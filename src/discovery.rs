use crate::models::{
    humanize_model_name, ChatMessage, ConversationDetail, ConversationSummary, GitFileStat,
    RunningTaskItem, StoredThreadOverlay, ToolSummaryItem, WorkspaceInfo,
};
use crate::profiles;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

static OVERLAY_LOCK: Mutex<()> = Mutex::new(());

pub fn home_dir() -> PathBuf {
    // When HOME is unset (e.g. some service units), fall back to the current
    // working directory rather than a hard-coded user path.
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
}

pub fn home_str() -> String {
    home_dir().to_string_lossy().into_owned()
}

pub fn home_projects_str() -> String {
    let p = home_dir().join("Projects");
    p.to_string_lossy().into_owned()
}

/// A setting from the environment: `MOEDECK_<NAME>`, or the pre-rename
/// `HELM_<NAME>` so existing service files keep working. Empty counts as unset.
pub fn setting(name: &str) -> Option<String> {
    [format!("MOEDECK_{name}"), format!("HELM_{name}")]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty())
}

/// `~/.config/moedeck`, unless an install from before the rename already
/// keeps its chats, accounts and passcode in `~/.config/helm`. Never moved:
/// extra accounts' sign-ins live inside it.
pub fn config_dir() -> PathBuf {
    let dir = setting("CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let config = home_dir().join(".config");
        let (current, legacy) = (config.join("moedeck"), config.join("helm"));
        if !current.exists() && legacy.exists() {
            legacy
        } else {
            current
        }
    });
    let _ = fs::create_dir_all(dir.join("threads"));
    let _ = fs::create_dir_all(dir.join("uploads"));
    restrict_to_owner(&dir);
    dir
}

/// Chats, uploads and extra accounts' sign-ins live under the config dir, so
/// other users on the machine must not be able to read it.
#[cfg(unix)]
fn restrict_to_owner(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = fs::metadata(dir) {
        if meta.permissions().mode() & 0o077 != 0 {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
    }
}

#[cfg(not(unix))]
fn restrict_to_owner(_dir: &Path) {}

/// Chats hidden from the drawer. Only MoE Deck forgets them: native session files
/// and overlays stay where they are, and unarchiving brings a chat back.
pub fn archived_ids() -> Vec<String> {
    fs::read(config_dir().join("archived.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn set_archived(id: &str, archived: bool) -> std::io::Result<()> {
    let mut ids = archived_ids();
    ids.retain(|existing| existing != id);
    if archived {
        ids.push(id.to_string());
    }
    let path = config_dir().join("archived.json");
    let staged = path.with_extension("json.tmp");
    fs::write(&staged, serde_json::to_vec_pretty(&ids)?)?;
    fs::rename(staged, path)
}

pub fn is_safe_session_id(raw_id: &str) -> bool {
    !raw_id.is_empty()
        && raw_id.len() <= 128
        && raw_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn sanitize_workdir(raw: &str) -> Option<PathBuf> {
    if raw == "." {
        return std::env::current_dir().ok();
    }
    let canon = fs::canonicalize(raw).ok()?;
    if !canon.is_dir() {
        return None;
    }
    let home = fs::canonicalize(home_dir()).unwrap_or_else(|_| home_dir());
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    if canon.starts_with(&home) || canon.starts_with("/tmp") || canon.starts_with(&cwd) {
        Some(canon)
    } else {
        None
    }
}

pub fn load_overlays() -> HashMap<String, StoredThreadOverlay> {
    let _guard = OVERLAY_LOCK.lock().ok();
    load_overlays_locked()
}

/// Caller must hold `OVERLAY_LOCK`.
fn load_overlays_locked() -> HashMap<String, StoredThreadOverlay> {
    let mut map = HashMap::new();
    let dir = config_dir().join("threads");
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("json")
                && !p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with(".tmp."))
                    .unwrap_or(false)
            {
                if let Ok(content) = fs::read_to_string(&p) {
                    if let Ok(overlay) = serde_json::from_str::<StoredThreadOverlay>(&content) {
                        map.insert(overlay.id.clone(), overlay);
                    }
                }
            }
        }
    }
    map
}

pub fn save_overlay(overlay: &StoredThreadOverlay) {
    let _guard = OVERLAY_LOCK.lock().ok();
    save_overlay_locked(overlay);
}

/// Caller must hold `OVERLAY_LOCK`.
fn save_overlay_locked(overlay: &StoredThreadOverlay) {
    let safe_name: String = overlay
        .id
        .replace(':', "_")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(128)
        .collect();
    if safe_name.is_empty() {
        return;
    }
    let threads_dir = config_dir().join("threads");
    let path = threads_dir.join(format!("{}.json", safe_name));
    let tmp_path = threads_dir.join(format!(".tmp.{}.{}.json", safe_name, Uuid::new_v4()));
    if let Ok(json) = serde_json::to_string_pretty(overlay) {
        if fs::write(&tmp_path, json).is_ok() {
            let _ = fs::rename(&tmp_path, &path);
        }
    }
}

pub fn atomic_write(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(
        ".tmp.{}.{}",
        Utc::now().timestamp_millis(),
        Uuid::new_v4()
    ));
    fs::write(&tmp_path, contents)?;
    match fs::rename(&tmp_path, path) {
        ok @ Ok(_) => ok,
        Err(e) => {
            let _ = fs::remove_file(&tmp_path);
            Err(e)
        }
    }
}

/// Store (or clear, when `native_id` is empty) the native CLI session id a
/// thread adopted from its harness runs, so future turns resume in place.
pub fn set_overlay_native_session(conv_id: &str, native_id: &str) {
    let _guard = OVERLAY_LOCK.lock().ok();
    let mut overlays = load_overlays_locked();
    if let Some(ov) = overlays.get_mut(conv_id) {
        let next = if native_id.trim().is_empty() {
            None
        } else {
            Some(native_id.trim().to_string())
        };
        if ov.native_session != next {
            ov.native_session = next;
            ov.updated_at = Utc::now().timestamp_millis();
            save_overlay_locked(ov);
        }
    }
}

/// Native session id a thread currently delegates to, if any.
pub fn overlay_native_session(conv_id: &str) -> Option<String> {
    let overlays = load_overlays();
    overlays
        .get(conv_id)
        .and_then(|ov| ov.native_session.clone())
}

#[derive(Debug, Clone)]
struct ClaudeActiveMeta {
    cwd: String,
    status: String,
}

/// Canonical (id, label) pair for a profile reference, re-exported for the
/// API layer.
pub fn profile_display(profile: Option<&str>) -> (Option<String>, String) {
    profiles::profile_display(profile)
}

fn load_claude_session_metas() -> HashMap<String, ClaudeActiveMeta> {
    let mut metas = HashMap::new();
    for (_, home) in profiles::harness_homes("claude") {
        let sessions_dir = home.join(".claude").join("sessions");
        if let Ok(entries) = fs::read_dir(sessions_dir) {
            for entry in entries.flatten() {
                if let Ok(text) = fs::read_to_string(entry.path()) {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        if let Some(sid) = val.get("sessionId").and_then(|v| v.as_str()) {
                            let cwd = val
                                .get("cwd")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&home_str())
                                .to_string();
                            let status = val
                                .get("status")
                                .and_then(|v| v.as_str())
                                .unwrap_or("idle")
                                .to_string();
                            metas.insert(sid.to_string(), ClaudeActiveMeta { cwd, status });
                        }
                    }
                }
            }
        }
    }
    metas
}

fn adopted_native_aliases(
    overlays: &HashMap<String, StoredThreadOverlay>,
) -> std::collections::HashSet<String> {
    overlays
        .values()
        .filter(|o| o.id.starts_with("helm:"))
        .filter_map(|o| {
            let harness = o.harness.as_deref()?;
            let native = o.native_session.as_deref().filter(|id| !id.is_empty())?;
            Some(profiles::make_conv_id(
                harness,
                o.profile.as_deref(),
                native,
            ))
        })
        .collect()
}

pub fn list_all_conversations(
    active_runner_ids: &HashMap<String, String>,
) -> Vec<ConversationSummary> {
    let overlays = load_overlays();
    let claude_metas = load_claude_session_metas();
    let mut list = Vec::new();

    // 1. Discover Claude Code sessions (primary account + extra profiles)
    for (profile, home) in profiles::harness_homes("claude") {
        let is_primary = profile.is_none();
        let claude_proj_root = home.join(".claude").join("projects");
        let Ok(proj_dirs) = fs::read_dir(&claude_proj_root) else {
            continue;
        };
        let mut jsonl_files: Vec<(PathBuf, i64)> = Vec::new();
        for pdir in proj_dirs.flatten() {
            if pdir.path().is_dir() {
                if let Ok(files) = fs::read_dir(pdir.path()) {
                    for f in files.flatten() {
                        let path = f.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            let mtime = f
                                .metadata()
                                .ok()
                                .and_then(|m| m.modified().ok())
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            jsonl_files.push((path, mtime));
                        }
                    }
                }
            }
        }
        jsonl_files.sort_by_key(|b| std::cmp::Reverse(b.1));
        for (path, mtime) in jsonl_files
            .into_iter()
            .take(if is_primary { 12 } else { 6 })
        {
            if let Some(summary) = summarize_claude_jsonl(
                &path,
                mtime,
                profile.as_deref(),
                &claude_metas,
                &overlays,
                active_runner_ids,
            ) {
                list.push(summary);
            }
        }
    }

    // 2. Discover Antigravity (agy) sessions across every account profile
    for (profile, home) in profiles::harness_homes("agy") {
        let is_primary = profile.is_none();
        let agy_brain_root = home.join(".gemini").join("antigravity-cli").join("brain");
        let Ok(brain_dirs) = fs::read_dir(&agy_brain_root) else {
            continue;
        };
        let mut agy_files: Vec<(String, PathBuf, i64)> = Vec::new();
        for bdir in brain_dirs.flatten() {
            let transcript = bdir
                .path()
                .join(".system_generated")
                .join("logs")
                .join("transcript.jsonl");
            if transcript.exists() {
                let mtime = fs::metadata(&transcript)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let raw_id = bdir.file_name().to_string_lossy().to_string();
                agy_files.push((raw_id, transcript, mtime));
            }
        }
        agy_files.sort_by_key(|b| std::cmp::Reverse(b.2));
        for (raw_id, transcript, _mtime) in
            agy_files.into_iter().take(if is_primary { 16 } else { 8 })
        {
            if let Some(summary) = summarize_agy_transcript(
                &raw_id,
                &transcript,
                profile.as_deref(),
                &overlays,
                active_runner_ids,
            ) {
                list.push(summary);
            }
        }
    }

    // 3. Discover Codex sessions (primary + profiles)
    for (profile, home) in profiles::harness_homes("codex") {
        let codex_idx = home.join(".codex").join("session_index.jsonl");
        let Ok(content) = fs::read_to_string(&codex_idx) else {
            continue;
        };
        for line in content.lines().rev().take(6) {
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                if let Some(raw_id) = val.get("id").and_then(|v| v.as_str()) {
                    let id = profiles::make_conv_id("codex", profile.as_deref(), raw_id);
                    let (_, profile_label) = profile_display(profile.as_deref());
                    let default_title = val
                        .get("thread_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Codex Session")
                        .to_string();
                    let ov = overlays.get(&id);
                    let title = ov.and_then(|o| o.title.clone()).unwrap_or(default_title);
                    let harness = ov
                        .and_then(|o| o.harness.clone())
                        .unwrap_or_else(|| "codex".to_string());
                    let (native_model, native_effort, native_workdir) =
                        codex_context(raw_id, &home);
                    let raw_model = ov.and_then(|o| o.model.clone()).unwrap_or(native_model);
                    let (model, model_label) = humanize_model_name(&harness, &raw_model);
                    let effort = ov.and_then(|o| o.effort.clone()).unwrap_or(native_effort);
                    let workdir = ov.and_then(|o| o.workdir.clone()).unwrap_or(native_workdir);
                    let is_running = active_runner_ids.contains_key(&id);
                    let updated_at = ov.map(|o| o.updated_at).unwrap_or(1789000000000);
                    list.push(ConversationSummary {
                        id: id.clone(),
                        raw_id: raw_id.to_string(),
                        title,
                        subtitle: format_subtitle(&workdir),
                        workdir,
                        harness,
                        profile: profile.clone(),
                        profile_label,
                        model,
                        model_label,
                        effort,
                        status: if is_running {
                            "busy".to_string()
                        } else {
                            "idle".to_string()
                        },
                        running_tasks_count: if is_running { 1 } else { 0 },
                        running_task_label: active_runner_ids.get(&id).cloned(),
                        updated_at,
                        pinned: ov.map(|o| o.pinned).unwrap_or(false),
                        last_preview: ov
                            .and_then(|o| o.appended_messages.last())
                            .map(preview_of)
                            .unwrap_or_else(|| "Codex thread".to_string()),
                        pr_links: Vec::new(),
                        pending_approval: None,
                    });
                }
            }
        }
    }

    // 4. Discover OpenCode sessions from every account's opencode.db
    for (profile, home) in profiles::harness_homes("opencode") {
        for summary in
            discover_opencode_sessions(&overlays, active_runner_ids, profile.as_deref(), &home)
        {
            if !list.iter().any(|c| c.id == summary.id) {
                list.push(summary);
            }
        }
    }

    // 5. Discover Keel sessions from ~/.keel/sessions/*.jsonl (all accounts)
    for (profile, home) in profiles::harness_homes("keel") {
        for summary in
            discover_keel_sessions(&overlays, active_runner_ids, profile.as_deref(), &home)
        {
            if !list.iter().any(|c| c.id == summary.id) {
                list.push(summary);
            }
        }
    }

    // 6. Add pure MoE Deck threads (created directly in MoE Deck). Include fresh,
    // message-less threads too — otherwise "+ New" creates a thread that is
    // invisible until the first message is sent.
    for (id, ov) in &overlays {
        if id.starts_with("helm:") && !list.iter().any(|c| &c.id == id) {
            let harness = ov.harness.clone().unwrap_or_else(|| "claude".to_string());
            let raw_model = ov.model.clone().unwrap_or_else(|| "opus".to_string());
            let (model, model_label) = humanize_model_name(&harness, &raw_model);
            let effort = ov.effort.clone().unwrap_or_else(|| "medium".to_string());
            let workdir = ov.workdir.clone().unwrap_or_else(home_projects_str);
            let (profile, profile_label) = profile_display(ov.profile.as_deref());
            let is_running = active_runner_ids.contains_key(id);
            let title = resolve_thread_title(ov, &[]);
            let last_preview = ov
                .appended_messages
                .last()
                .map(preview_of)
                .unwrap_or_else(|| "Ready for commands".to_string());
            list.push(ConversationSummary {
                id: id.clone(),
                raw_id: id.trim_start_matches("helm:").to_string(),
                title,
                subtitle: format_subtitle(&workdir),
                workdir,
                harness,
                profile,
                profile_label,
                model,
                model_label,
                effort,
                status: if is_running {
                    "busy".to_string()
                } else {
                    "idle".to_string()
                },
                running_tasks_count: if is_running { 1 } else { 0 },
                running_task_label: active_runner_ids.get(id).cloned(),
                updated_at: ov.updated_at,
                pinned: ov.pinned,
                last_preview,
                pr_links: Vec::new(),
                pending_approval: None,
            });
        }
    }

    // An adopted CLI transcript backs a MoE Deck thread; don't show it as a second conversation.
    let aliases = adopted_native_aliases(&overlays);
    list.retain(|conversation| !aliases.contains(&conversation.id));

    // Sort: pinned first, then busy/recent
    list.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });

    list
}

fn summarize_opencode_single_session(
    raw_id: &str,
    db_path: &Path,
    profile: Option<&str>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    if !raw_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let rows = opencode_session_rows(db_path, Some(raw_id), 1);
    let row = rows.first()?;
    let id = profiles::make_conv_id("opencode", profile, raw_id);
    let (_, profile_label) = profile_display(profile);
    let ov = overlays.get(&id);
    let default_title = row
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("OpenCode Session")
        .to_string();
    let title = ov.and_then(|o| o.title.clone()).unwrap_or(default_title);
    let workdir = ov
        .and_then(|o| o.workdir.clone())
        .or_else(|| {
            row.get("directory")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(home_str);
    let harness = ov
        .and_then(|o| o.harness.clone())
        .unwrap_or_else(|| "opencode".to_string());
    let raw_model = ov
        .and_then(|o| o.model.clone())
        .unwrap_or_else(|| opencode_model_from_row(row));
    let (model, model_label) = humanize_model_name(&harness, &raw_model);
    let effort = ov
        .and_then(|o| o.effort.clone())
        .unwrap_or_else(|| "medium".to_string());
    let is_running = active_runner_ids.contains_key(&id);
    let updated_at = ov
        .map(|o| o.updated_at)
        .or_else(|| row.get("time_updated").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    Some(ConversationSummary {
        id: id.clone(),
        raw_id: raw_id.to_string(),
        title,
        subtitle: format_subtitle(&workdir),
        workdir,
        harness,
        profile: profile.map(|s| s.to_string()),
        profile_label,
        model,
        model_label,
        effort,
        status: if is_running {
            "busy".to_string()
        } else {
            "idle".to_string()
        },
        running_tasks_count: if is_running { 1 } else { 0 },
        running_task_label: active_runner_ids.get(&id).cloned(),
        updated_at,
        pinned: ov.map(|o| o.pinned).unwrap_or(false),
        last_preview: ov
            .and_then(|o| o.appended_messages.last())
            .map(preview_of)
            .unwrap_or_else(|| "OpenCode session".to_string()),
        pr_links: Vec::new(),
        pending_approval: None,
    })
}

pub fn find_conversation_summary(
    id: &str,
    active_runner_ids: &HashMap<String, String>,
    overlays: &HashMap<String, StoredThreadOverlay>,
) -> Option<ConversationSummary> {
    let overlay = overlays.get(id);
    let (harness_seg, profile_seg, raw_id) = profiles::parse_conv_id(id);

    if id.starts_with("helm:") {
        let ov = overlay?;
        let harness = ov.harness.clone().unwrap_or_else(|| "claude".to_string());
        let raw_model = ov.model.clone().unwrap_or_else(|| "opus".to_string());
        let (model, model_label) = humanize_model_name(&harness, &raw_model);
        let effort = ov.effort.clone().unwrap_or_else(|| "medium".to_string());
        let workdir = ov.workdir.clone().unwrap_or_else(home_str);
        let (profile, profile_label) = profile_display(ov.profile.as_deref());
        let is_running = active_runner_ids.contains_key(id);
        return Some(ConversationSummary {
            id: id.to_string(),
            raw_id: id.trim_start_matches("helm:").to_string(),
            title: resolve_thread_title(ov, &[]),
            subtitle: format_subtitle(&workdir),
            workdir,
            harness,
            profile,
            profile_label,
            model,
            model_label,
            effort,
            status: if is_running {
                "busy".to_string()
            } else {
                "idle".to_string()
            },
            running_tasks_count: if is_running { 1 } else { 0 },
            running_task_label: active_runner_ids.get(id).cloned(),
            updated_at: ov.updated_at,
            pinned: ov.pinned,
            last_preview: ov
                .appended_messages
                .last()
                .map(preview_of)
                .unwrap_or_else(|| "Ready for commands".to_string()),
            pr_links: Vec::new(),
            pending_approval: None,
        });
    }

    if let Some(h) = harness_seg.as_deref() {
        if !is_safe_session_id(&raw_id) {
            return None;
        }
        let home = match profile_seg.as_deref() {
            None => home_dir(),
            Some(pid) => match profiles::get_profile(pid) {
                Some(p) if p.harness == h => profiles::profile_home(pid),
                _ => return None,
            },
        };

        match h {
            "claude" => {
                if let Some(path) = find_claude_jsonl(&raw_id, &home) {
                    let mtime = fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    let metas = load_claude_session_metas();
                    return summarize_claude_jsonl(
                        &path,
                        mtime,
                        profile_seg.as_deref(),
                        &metas,
                        overlays,
                        active_runner_ids,
                    );
                }
            }
            "agy" => {
                let transcript = home
                    .join(".gemini")
                    .join("antigravity-cli")
                    .join("brain")
                    .join(&raw_id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl");
                if transcript.exists() {
                    return summarize_agy_transcript(
                        &raw_id,
                        &transcript,
                        profile_seg.as_deref(),
                        overlays,
                        active_runner_ids,
                    );
                }
            }
            "keel" => {
                let path = home
                    .join(".keel")
                    .join("sessions")
                    .join(format!("{}.jsonl", raw_id));
                if path.exists() {
                    let mtime = fs::metadata(&path)
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);
                    return summarize_keel_session(
                        &raw_id,
                        &path,
                        mtime,
                        profile_seg.as_deref(),
                        overlays,
                        active_runner_ids,
                    );
                }
            }
            "opencode" => {
                let db_path = home
                    .join(".local")
                    .join("share")
                    .join("opencode")
                    .join("opencode.db");
                if db_path.exists() {
                    if let Some(sum) = summarize_opencode_single_session(
                        &raw_id,
                        &db_path,
                        profile_seg.as_deref(),
                        overlays,
                        active_runner_ids,
                    ) {
                        return Some(sum);
                    }
                }
            }
            "codex" => {
                let codex_idx = home.join(".codex").join("session_index.jsonl");
                if let Ok(content) = fs::read_to_string(&codex_idx) {
                    for line in content.lines().rev() {
                        if let Ok(val) = serde_json::from_str::<Value>(line) {
                            if val.get("id").and_then(|v| v.as_str()) == Some(&raw_id) {
                                let default_title = val
                                    .get("thread_name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Codex Session")
                                    .to_string();
                                let (_, profile_label) = profile_display(profile_seg.as_deref());
                                let ov = overlays.get(id);
                                let title =
                                    ov.and_then(|o| o.title.clone()).unwrap_or(default_title);
                                let harness = ov
                                    .and_then(|o| o.harness.clone())
                                    .unwrap_or_else(|| "codex".to_string());
                                let (native_model, native_effort, native_workdir) =
                                    codex_context(&raw_id, &home);
                                let raw_model =
                                    ov.and_then(|o| o.model.clone()).unwrap_or(native_model);
                                let (model, model_label) =
                                    humanize_model_name(&harness, &raw_model);
                                let effort =
                                    ov.and_then(|o| o.effort.clone()).unwrap_or(native_effort);
                                let workdir =
                                    ov.and_then(|o| o.workdir.clone()).unwrap_or(native_workdir);
                                let is_running = active_runner_ids.contains_key(id);
                                let updated_at = ov.map(|o| o.updated_at).unwrap_or(1789000000000);
                                return Some(ConversationSummary {
                                    id: id.to_string(),
                                    raw_id: raw_id.clone(),
                                    title,
                                    subtitle: format_subtitle(&workdir),
                                    workdir,
                                    harness,
                                    profile: profile_seg.clone(),
                                    profile_label,
                                    model,
                                    model_label,
                                    effort,
                                    status: if is_running {
                                        "busy".to_string()
                                    } else {
                                        "idle".to_string()
                                    },
                                    running_tasks_count: if is_running { 1 } else { 0 },
                                    running_task_label: active_runner_ids.get(id).cloned(),
                                    updated_at,
                                    pinned: ov.map(|o| o.pinned).unwrap_or(false),
                                    last_preview: ov
                                        .and_then(|o| o.appended_messages.last())
                                        .map(preview_of)
                                        .unwrap_or_else(|| "Codex thread".to_string()),
                                    pr_links: Vec::new(),
                                    pending_approval: None,
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Fallback if an overlay exists
    let ov = overlay?;
    let harness = ov.harness.clone().unwrap_or_else(|| "claude".to_string());
    let raw_model = ov.model.clone().unwrap_or_else(|| "opus".to_string());
    let (model, model_label) = humanize_model_name(&harness, &raw_model);
    let effort = ov.effort.clone().unwrap_or_else(|| "medium".to_string());
    let workdir = ov.workdir.clone().unwrap_or_else(home_str);
    let (profile, profile_label) = profile_display(ov.profile.as_deref());
    let is_running = active_runner_ids.contains_key(id);
    Some(ConversationSummary {
        id: id.to_string(),
        raw_id: id.trim_start_matches("helm:").to_string(),
        title: resolve_thread_title(ov, &[]),
        subtitle: format_subtitle(&workdir),
        workdir,
        harness,
        profile,
        profile_label,
        model,
        model_label,
        effort,
        status: if is_running {
            "busy".to_string()
        } else {
            "idle".to_string()
        },
        running_tasks_count: if is_running { 1 } else { 0 },
        running_task_label: active_runner_ids.get(id).cloned(),
        updated_at: ov.updated_at,
        pinned: ov.pinned,
        last_preview: "Ready for commands".to_string(),
        pr_links: Vec::new(),
        pending_approval: None,
    })
}

pub fn get_conversation_detail(
    id: &str,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationDetail> {
    get_conversation_detail_tail(id, active_runner_ids, None)
}

/// Like [`get_conversation_detail`], but optionally returns only the last
/// `tail` messages. Phones rarely need a 1 MB transcript on every poll; the
/// summary carries `truncated`/`total_messages` so the UI can offer a
/// "load full history" control.
pub fn get_conversation_detail_tail(
    id: &str,
    active_runner_ids: &HashMap<String, String>,
    tail: Option<usize>,
) -> Option<ConversationDetail> {
    let overlays = load_overlays();
    let overlay = overlays.get(id);
    let mut summary = find_conversation_summary(id, active_runner_ids, &overlays)?;

    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut native_omitted = 0;
    let (harness_seg, profile_seg, raw_id) = profiles::parse_conv_id(id);
    if let Some(h) = harness_seg.as_deref() {
        if !is_safe_session_id(&raw_id) {
            return None;
        }
        // Resolve which account's home holds this session's transcript.
        let home = match profile_seg.as_deref() {
            None => home_dir(),
            Some(pid) => match profiles::get_profile(pid) {
                Some(p) if p.harness == h => profiles::profile_home(pid),
                _ => return None,
            },
        };
        messages = match h {
            "claude" => parse_claude_messages(&find_claude_jsonl(&raw_id, &home)?),
            "agy" => {
                let transcript = home
                    .join(".gemini")
                    .join("antigravity-cli")
                    .join("brain")
                    .join(&raw_id)
                    .join(".system_generated")
                    .join("logs")
                    .join("transcript.jsonl");
                parse_agy_messages(&transcript)
            }
            "codex" => match find_codex_rollout(&raw_id, &home) {
                Some(rollout) => parse_codex_messages(&rollout),
                None => Vec::new(),
            },
            "opencode" => {
                let parsed = parse_opencode_messages(&raw_id, &home);
                native_omitted =
                    opencode_message_count(&raw_id, &home).saturating_sub(parsed.len());
                parsed
            }
            "keel" => {
                let path = home
                    .join(".keel")
                    .join("sessions")
                    .join(format!("{}.jsonl", raw_id));
                if path.exists() {
                    parse_keel_messages(&path)
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        };
    }

    let native_opencode = harness_seg.as_deref() == Some("opencode");
    let is_actively_running = active_runner_ids.contains_key(id);

    if let Some(ov) = overlay {
        if native_opencode {
            merge_opencode_overlay(&mut messages, &ov.appended_messages, native_omitted > 0);
        } else {
            for m in &ov.appended_messages {
                // Avoid duplicating a message the transcript already ingested.
                let already_present = messages.iter().any(|existing| {
                    existing.role == m.role
                        && normalize_chat_text(&existing.text) == normalize_chat_text(&m.text)
                });
                if !already_present || (m.role == "assistant" && m.streaming) {
                    messages.push(m.clone());
                }
            }
        }
    }

    for m in &mut messages {
        m.text = readable_text(m);
    }

    // Reconcile any orphaned `streaming: true` "Thinking…" placeholders when no runner task is active
    if !is_actively_running {
        for m in &mut messages {
            if m.streaming {
                m.streaming = false;
                if m.text == "Thinking…" || m.text == "Working…" {
                    m.text = "Interrupted before completion (gateway restarted or CLI exited). Resend your prompt below to continue.".to_string();
                }
            }
        }
    }

    // Backfill a real title for threads that were created before titles were
    // derived from the first prompt (and for native sessions whose first user
    // message only recently became readable).
    if summary.title.trim().is_empty() || summary.title == "New Thread" {
        if let Some(first) = messages
            .iter()
            .find(|m| m.role == "user" && !m.text.trim().is_empty())
        {
            summary.title = clean_title_from_prompt(&first.text);
        }
    }

    let total_messages = messages.len() + native_omitted;
    let tail_omitted = match tail {
        Some(limit) if messages.len() > limit => messages.len() - limit,
        _ => 0,
    };
    let truncated = tail_omitted + native_omitted;
    if tail_omitted > 0 {
        messages = messages.split_off(tail_omitted);
    }
    if tail.is_some() {
        crate::timeline::trim_old_tool_io(&mut messages, 1);
    }

    let mut running_tasks = Vec::new();
    if summary.status == "busy" || summary.running_tasks_count > 0 {
        let label = summary
            .running_task_label
            .clone()
            .unwrap_or_else(|| "Active background execution".to_string());
        running_tasks.push(RunningTaskItem {
            id: format!("task-{}", summary.raw_id),
            conversation_id: summary.id.clone(),
            harness: summary.harness.clone(),
            title: label.clone(),
            detail: format!("Working in {}", summary.workdir),
            status: "running".to_string(),
            updated_at: summary.updated_at,
        });
    }

    Some(ConversationDetail {
        summary,
        messages,
        running_tasks,
        truncated,
        total_messages,
    })
}

/// Reconcile only copies actually present in SQLite. Gateway failures, Stop
/// and handoffs may never reach that database and must remain visible.
fn merge_opencode_overlay(native: &mut Vec<ChatMessage>, overlay: &[ChatMessage], capped: bool) {
    let native_len = native.len();
    let mut cursor = 0;
    let mut matched_user = false;
    for message in overlay {
        let text = normalize_chat_text(&message.text);
        let found = native[cursor..native_len].iter().position(|m| {
            m.role == message.role
                && m.harness == message.harness
                && normalize_chat_text(&m.text) == text
                && match (
                    chrono::DateTime::parse_from_rfc3339(&m.timestamp),
                    chrono::DateTime::parse_from_rfc3339(&message.timestamp),
                ) {
                    (Ok(native_time), Ok(overlay_time)) => native_time >= overlay_time,
                    _ => true,
                }
        });
        if let Some(offset) = found {
            cursor += offset + 1;
            if message.role == "user" {
                matched_user = true;
            }
            continue;
        }
        // Do not reintroduce overlay history older than the bounded DB tail.
        if capped
            && cursor == 0
            && native.first().is_some_and(|first| {
                match (
                    chrono::DateTime::parse_from_rfc3339(&first.timestamp),
                    chrono::DateTime::parse_from_rfc3339(&message.timestamp),
                ) {
                    (Ok(first), Ok(message)) => message < first,
                    _ => false,
                }
            })
        {
            continue;
        }
        if message.role == "user" {
            matched_user = false;
        }
        // The native transcript supersedes a streaming snapshot left by a
        // gateway restart, or the same in-flight assistant turn's snapshot.
        if message.streaming
            && message.harness == "opencode"
            && matched_user
            && native[cursor..native_len]
                .iter()
                .take_while(|m| m.role != "user")
                .any(|m| m.role == "assistant")
        {
            continue;
        }
        native.push(message.clone());
    }
}

fn opencode_message_count(raw_id: &str, home: &Path) -> usize {
    let path = home.join(".local/share/opencode/opencode.db");
    let count = |table: &str, filter: &str| {
        opencode_rows(
            &path,
            &format!(
                "SELECT COUNT(*) AS total FROM {table} WHERE session_id = '{raw_id}' {filter};"
            ),
        )
        .and_then(|rows| rows.first()?.get("total")?.as_u64())
        .unwrap_or(0) as usize
    };
    let v2 = count("session_message", "AND type IN ('user', 'assistant')");
    if v2 > 0 {
        v2
    } else {
        count("message", "")
    }
}

/// Run a read-only `sqlite3 -json` query. `None` means the query itself failed
/// (for example, the table does not exist in this OpenCode version); an empty
/// result set is `Some(vec![])`.
fn opencode_rows(db_path: &Path, sql: &str) -> Option<Vec<Value>> {
    let out = Command::new("sqlite3")
        .arg("-readonly")
        .arg("-json")
        .arg(db_path)
        .arg(sql)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    if out.stdout.iter().all(|byte| byte.is_ascii_whitespace()) {
        return Some(Vec::new());
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// OpenCode 2.x stores sessions in `session_v2` (the v1 `session` table is
/// frozen at the upgrade point). Prefer v2 and fall back for older installs.
fn opencode_session_rows(db_path: &Path, where_id: Option<&str>, limit: usize) -> Vec<Value> {
    let filter = where_id
        .map(|id| format!("WHERE id = '{id}'"))
        .unwrap_or_else(|| "WHERE parent_id IS NULL".to_string());
    let v2 = format!(
        "SELECT id, title, directory, model, time_updated FROM session_v2 {filter} ORDER BY time_updated DESC LIMIT {limit};"
    );
    let v1 = format!(
        "SELECT id, title, directory, model, time_updated FROM session {filter} ORDER BY time_updated DESC LIMIT {limit};"
    );
    match opencode_rows(db_path, &v2) {
        Some(rows) if !rows.is_empty() => rows,
        _ => opencode_rows(db_path, &v1).unwrap_or_default(),
    }
}

/// OpenCode stores `model` as a JSON object string
/// (`{"id":…,"providerID":…,"variant":…}`) in both v1 and v2. Return the
/// `provider/model` form MoE Deck passes back to `--model`.
fn opencode_model_from_row(row: &Value) -> String {
    let raw = row.get("model").and_then(|v| v.as_str()).unwrap_or("");
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|model| {
            let provider = model
                .get("providerID")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let id = model
                .get("modelID")
                .or_else(|| model.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !provider.is_empty() && !id.is_empty() {
                Some(format!("{provider}/{id}"))
            } else if !id.is_empty() {
                Some(id.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "default".to_string())
}

fn discover_opencode_sessions(
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
    profile: Option<&str>,
    home: &Path,
) -> Vec<ConversationSummary> {
    let db_path = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let rows = opencode_session_rows(&db_path, None, 12);

    let mut summaries = Vec::new();
    for row in rows {
        let Some(raw_id) = row.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let id = profiles::make_conv_id("opencode", profile, raw_id);
        let (_, profile_label) = profile_display(profile);
        let ov = overlays.get(&id);
        let default_title = row
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("OpenCode Session")
            .to_string();
        let title = ov.and_then(|o| o.title.clone()).unwrap_or(default_title);
        let workdir = ov
            .and_then(|o| o.workdir.clone())
            .or_else(|| {
                row.get("directory")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(home_str);
        let harness = ov
            .and_then(|o| o.harness.clone())
            .unwrap_or_else(|| "opencode".to_string());

        let parsed_model = opencode_model_from_row(&row);

        let raw_model = ov.and_then(|o| o.model.clone()).unwrap_or(parsed_model);
        let (model, model_label) = humanize_model_name(&harness, &raw_model);
        let effort = ov
            .and_then(|o| o.effort.clone())
            .unwrap_or_else(|| "high".to_string());
        let time_updated = row
            .get("time_updated")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let updated_at = ov
            .map(|o| o.updated_at.max(time_updated))
            .unwrap_or(time_updated);
        let is_running = active_runner_ids.contains_key(&id);

        summaries.push(ConversationSummary {
            id: id.clone(),
            raw_id: raw_id.to_string(),
            title: title.clone(),
            subtitle: format_subtitle(&workdir),
            workdir,
            harness,
            profile: profile.map(|s| s.to_string()),
            profile_label,
            model,
            model_label,
            effort,
            status: if is_running {
                "busy".to_string()
            } else {
                "idle".to_string()
            },
            running_tasks_count: if is_running { 1 } else { 0 },
            running_task_label: active_runner_ids.get(&id).cloned(),
            updated_at,
            pinned: ov.map(|o| o.pinned).unwrap_or(false),
            last_preview: ov
                .and_then(|o| o.appended_messages.last())
                .map(preview_of)
                .unwrap_or(title),
            pr_links: Vec::new(),
            pending_approval: None,
        });
    }

    summaries
}

/// OpenCode 2.x: one row per user/assistant message in `session_message`,
/// with the turn's blocks inlined in the JSON `data.content` array.
fn parse_opencode_v2_messages(raw_id: &str, db_path: &Path) -> Option<Vec<ChatMessage>> {
    let query = format!(
        "SELECT seq, type, data FROM (SELECT seq, type, data FROM session_message WHERE session_id = '{raw_id}' AND type IN ('user', 'assistant') ORDER BY seq DESC LIMIT 400) ORDER BY seq ASC;"
    );
    let rows = opencode_rows(db_path, &query)?;
    let mut messages = Vec::new();
    for row in rows {
        let Some(role) = row.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(data) = row.get("data").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(data) else {
            continue;
        };
        let mut texts = Vec::new();
        let mut thinkings = Vec::new();
        let mut tools = Vec::new();
        if role == "user" {
            if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
                let text = strip_opencode_quotes(text);
                if !text.is_empty() {
                    texts.push(text);
                }
            }
        } else if let Some(content) = data.get("content").and_then(|v| v.as_array()) {
            for part in content {
                match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.trim().is_empty() {
                                texts.push(text.trim().to_string());
                            }
                        }
                    }
                    "reasoning" => {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                            if !text.trim().is_empty() {
                                thinkings.push(text.trim().to_string());
                            }
                        }
                    }
                    "tool" => {
                        let name = part
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let state = part.get("state");
                        let summary = state
                            .and_then(|s| {
                                s.get("title").and_then(|v| v.as_str()).or_else(|| {
                                    s.get("input").and_then(|input| {
                                        input
                                            .get("command")
                                            .or_else(|| input.get("filePath"))
                                            .or_else(|| input.get("path"))
                                            .or_else(|| input.get("description"))
                                            .and_then(|v| v.as_str())
                                    })
                                })
                            })
                            .map(|s| truncate_str(s, 68))
                            .unwrap_or_else(|| name.clone());
                        let status = match state
                            .and_then(|s| s.get("status"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                        {
                            "completed" => "done",
                            "error" => "denied",
                            _ => "running",
                        };
                        tools.push(ToolSummaryItem {
                            name,
                            summary,
                            status: status.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
        let combined_text = texts.join("\n\n");
        let thinking = if thinkings.is_empty() {
            None
        } else {
            Some(thinkings.join("\n\n"))
        };
        if combined_text.is_empty() && thinking.is_none() && tools.is_empty() {
            continue;
        }
        let raw_model = data
            .get("model")
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let (_, model_label) = humanize_model_name("opencode", raw_model);
        let seq = row.get("seq").and_then(|v| v.as_i64()).unwrap_or(0);
        messages.push(ChatMessage {
            id: format!("v2-{seq}"),
            role: role.to_string(),
            harness: "opencode".to_string(),
            model: if role == "assistant" {
                Some(model_label)
            } else {
                None
            },
            timestamp: data["time"]["created"]
                .as_i64()
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
            text: combined_text,
            thinking,
            tools,
            images: Vec::new(),
            streaming: false,
            blocks: Vec::new(),
        });
    }
    Some(messages)
}

pub fn parse_opencode_messages(raw_id: &str, home: &Path) -> Vec<ChatMessage> {
    if !raw_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Vec::new();
    }
    let db_path = home
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db");
    if !db_path.exists() {
        return Vec::new();
    }

    if let Some(messages) = parse_opencode_v2_messages(raw_id, &db_path) {
        if !messages.is_empty() {
            return messages;
        }
    }

    let msg_query = format!(
        "SELECT id, time_created, data FROM (SELECT id, time_created, data FROM message WHERE session_id = '{}' ORDER BY time_created DESC LIMIT 250) ORDER BY time_created ASC;",
        raw_id
    );
    let part_query = format!(
        "SELECT id, message_id, data FROM (SELECT id, message_id, data, time_created FROM part WHERE session_id = '{}' ORDER BY time_created DESC, id DESC LIMIT 1000) ORDER BY time_created ASC, id ASC;",
        raw_id
    );

    let Ok(msg_out) = Command::new("sqlite3")
        .arg("-json")
        .arg(&db_path)
        .arg(&msg_query)
        .output()
    else {
        return Vec::new();
    };
    let Ok(part_out) = Command::new("sqlite3")
        .arg("-json")
        .arg(&db_path)
        .arg(&part_query)
        .output()
    else {
        return Vec::new();
    };

    let msg_rows: Vec<Value> = serde_json::from_slice(&msg_out.stdout).unwrap_or_default();
    let part_rows: Vec<Value> = serde_json::from_slice(&part_out.stdout).unwrap_or_default();

    let mut parts_by_msg: HashMap<String, Vec<Value>> = HashMap::new();
    for p in part_rows {
        if let (Some(mid), Some(d_str)) = (
            p.get("message_id").and_then(|v| v.as_str()),
            p.get("data").and_then(|v| v.as_str()),
        ) {
            if let Ok(d_val) = serde_json::from_str::<Value>(d_str) {
                parts_by_msg.entry(mid.to_string()).or_default().push(d_val);
            }
        }
    }

    let mut messages = Vec::new();
    for m in msg_rows {
        let Some(mid) = m.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(d_str) = m.get("data").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(d_val) = serde_json::from_str::<Value>(d_str) else {
            continue;
        };
        let role = d_val
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("assistant")
            .to_string();
        let raw_model = d_val
            .get("modelID")
            .and_then(|v| v.as_str())
            .unwrap_or("default");
        let (_, model_label) = humanize_model_name("opencode", raw_model);

        let mut texts = Vec::new();
        let mut thinkings = Vec::new();
        let mut tools = Vec::new();

        if let Some(parts) = parts_by_msg.get(mid) {
            for part in parts {
                let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if ptype == "text" {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        if !t.trim().is_empty() {
                            texts.push(t.trim().to_string());
                        }
                    }
                } else if ptype == "reasoning" {
                    if let Some(th) = part.get("text").and_then(|v| v.as_str()) {
                        if !th.trim().is_empty() {
                            thinkings.push(th.trim().to_string());
                        }
                    }
                } else if ptype == "tool" {
                    let name = part
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    let summary = part
                        .get("state")
                        .and_then(|st| {
                            st.get("title").and_then(|v| v.as_str()).or_else(|| {
                                st.get("input").and_then(|inp| {
                                    inp.get("command")
                                        .or_else(|| inp.get("filePath"))
                                        .and_then(|v| v.as_str())
                                })
                            })
                        })
                        .map(|s| truncate_str(s, 68))
                        .unwrap_or_else(|| name.clone());
                    tools.push(ToolSummaryItem {
                        name,
                        summary,
                        status: "done".to_string(),
                    });
                }
            }
        }

        let combined_text = texts.join("\n\n");
        let thinking = if thinkings.is_empty() {
            None
        } else {
            Some(thinkings.join("\n\n"))
        };

        if !combined_text.is_empty() || thinking.is_some() || !tools.is_empty() {
            messages.push(ChatMessage {
                id: mid.to_string(),
                role: role.clone(),
                harness: "opencode".to_string(),
                model: if role == "assistant" {
                    Some(model_label)
                } else {
                    None
                },
                timestamp: String::new(),
                text: combined_text,
                thinking,
                tools,
                images: Vec::new(),
                streaming: false,
                blocks: Vec::new(),
            });
        }
    }

    messages
}

/// Inspect only the bounded tail; old or missing metadata must not invent a model.
fn codex_context(raw_id: &str, home: &Path) -> (String, String, String) {
    let fallback = (
        "default".into(),
        "default".into(),
        home.to_string_lossy().into_owned(),
    );
    let Some(path) = find_codex_rollout(raw_id, home) else {
        return fallback;
    };
    let Ok(mut file) = File::open(path) else {
        return fallback;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(1024 * 1024)));
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return fallback;
    }
    codex_context_from_tail(&String::from_utf8_lossy(&bytes), fallback)
}

fn codex_context_from_tail(
    tail: &str,
    fallback: (String, String, String),
) -> (String, String, String) {
    for line in tail.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("turn_context") {
            continue;
        }
        let payload = &value["payload"];
        return (
            payload["model"].as_str().unwrap_or(&fallback.0).to_string(),
            payload["effort"]
                .as_str()
                .or_else(|| payload["reasoning_effort"].as_str())
                .unwrap_or(&fallback.1)
                .to_string(),
            payload["cwd"].as_str().unwrap_or(&fallback.2).to_string(),
        );
    }
    fallback
}

fn find_codex_rollout(raw_id: &str, home: &Path) -> Option<PathBuf> {
    if !is_safe_session_id(raw_id) {
        return None;
    }
    let sessions_root = home.join(".codex").join("sessions");
    let suffix = format!("{}.jsonl", raw_id);
    find_file_with_suffix(&sessions_root, &suffix, 4)
}

fn find_file_with_suffix(dir: &Path, suffix: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 || !dir.is_dir() {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file_with_suffix(&p, suffix, depth - 1) {
                return Some(found);
            }
        } else if p.to_string_lossy().ends_with(suffix) {
            return Some(p);
        }
    }
    None
}

pub fn parse_codex_messages(path: &Path) -> Vec<ChatMessage> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending_tools: Vec<ToolSummaryItem> = Vec::new();

    let mut current_model: Option<String> = None;
    for line in reader.lines().map_while(Result::ok) {
        if line.len() > 32768
            && !line.contains("\"item_completed\"")
            && !line.contains("\"turn_context\"")
            && !line.contains("\"response_item\"")
        {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if val["type"].as_str() == Some("turn_context") {
            current_model = val["payload"]["model"].as_str().map(str::to_string);
        }
        let ts = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if val["type"].as_str() == Some("response_item")
            && val["payload"]["type"].as_str() == Some("message")
        {
            let payload = &val["payload"];
            let role = payload["role"].as_str().unwrap_or("");
            if matches!(role, "user" | "assistant") {
                let text = payload["content"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                if !text.is_empty() {
                    messages.push(ChatMessage {
                        id: payload["id"]
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("codex-{}", messages.len())),
                        role: role.into(),
                        harness: "codex".into(),
                        model: if role == "assistant" {
                            current_model.clone()
                        } else {
                            None
                        },
                        timestamp: ts.clone(),
                        text,
                        thinking: None,
                        tools: std::mem::take(&mut pending_tools),
                        images: vec![],
                        streaming: false,
                        blocks: Vec::new(),
                    });
                }
            }
            continue;
        }
        if val.get("type").and_then(|v| v.as_str()) == Some("event_msg") {
            let Some(payload) = val.get("payload") else {
                continue;
            };
            if payload.get("type").and_then(|v| v.as_str()) != Some("item_completed") {
                continue;
            }
            let Some(item) = payload.get("item") else {
                continue;
            };
            let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if itype == "UserMessage" {
                let mut parts = Vec::new();
                if let Some(arr) = item.get("content").and_then(|v| v.as_array()) {
                    for c in arr {
                        if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                            if !t.trim().is_empty() {
                                parts.push(t.trim().to_string());
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    messages.push(ChatMessage {
                        id: item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("codex-u")
                            .to_string(),
                        role: "user".to_string(),
                        harness: "codex".to_string(),
                        model: None,
                        timestamp: ts,
                        text: parts.join("\n\n"),
                        thinking: None,
                        tools: Vec::new(),
                        images: Vec::new(),
                        streaming: false,
                        blocks: Vec::new(),
                    });
                }
            } else if itype == "CommandExecution" {
                let summary = item
                    .get("command")
                    .and_then(|c| {
                        c.as_array().and_then(|arr| {
                            arr.last().and_then(|v| v.as_str()).map(|s| s.to_string())
                        })
                    })
                    .map(|s| truncate_str(s.lines().next().unwrap_or(&s), 68))
                    .unwrap_or_else(|| "exec".to_string());
                pending_tools.push(ToolSummaryItem {
                    name: "Bash".to_string(),
                    summary,
                    status: "done".to_string(),
                });
            } else if itype == "AgentMessage" {
                let mut parts = Vec::new();
                if let Some(arr) = item.get("content").and_then(|v| v.as_array()) {
                    for c in arr {
                        if let Some(t) = c.get("text").and_then(|v| v.as_str()) {
                            if !t.trim().is_empty() {
                                parts.push(t.trim().to_string());
                            }
                        }
                    }
                }
                if !parts.is_empty() {
                    messages.push(ChatMessage {
                        id: item
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("codex-a")
                            .to_string(),
                        role: "assistant".to_string(),
                        harness: "codex".to_string(),
                        model: current_model.clone(),
                        timestamp: ts,
                        text: parts.join("\n\n"),
                        thinking: None,
                        tools: std::mem::take(&mut pending_tools),
                        images: Vec::new(),
                        streaming: false,
                        blocks: Vec::new(),
                    });
                }
            }
        }
    }

    messages
}

fn discover_keel_sessions(
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
    profile: Option<&str>,
    home: &Path,
) -> Vec<ConversationSummary> {
    let sessions_dir = home.join(".keel").join("sessions");
    if !sessions_dir.exists() {
        return Vec::new();
    }
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                files.push((p, mtime));
            }
        }
    }
    files.sort_by_key(|b| std::cmp::Reverse(b.1));

    let mut summaries = Vec::new();
    for (path, mtime) in files.into_iter().take(12) {
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if let Some(sum) =
            summarize_keel_session(stem, &path, mtime, profile, overlays, active_runner_ids)
        {
            summaries.push(sum);
        }
    }
    summaries
}

fn summarize_keel_session(
    raw_id: &str,
    path: &Path,
    mtime: i64,
    profile: Option<&str>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    let id = profiles::make_conv_id("keel", profile, raw_id);
    let (_, profile_label) = profile_display(profile);
    let ov = overlays.get(&id);

    let mut first_user_prompt = None;
    let mut last_preview_text = None;
    let mut max_ts = mtime;

    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(&line) {
                if let Some(ts) = val.get("ts").and_then(|v| v.as_i64()) {
                    if ts > max_ts {
                        max_ts = ts;
                    }
                }
                let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                let data = &val["data"];
                match kind {
                    "user" => {
                        if let Some(txt) = data.get("text").and_then(|v| v.as_str()) {
                            if first_user_prompt.is_none() && !txt.trim().is_empty() {
                                first_user_prompt = Some(txt.to_string());
                            }
                            last_preview_text = Some(truncate_str(txt.trim(), 90));
                        }
                    }
                    "assistant" => {
                        if let Some(txt) = data.get("text").and_then(|v| v.as_str()) {
                            if !txt.trim().is_empty() {
                                last_preview_text = Some(truncate_str(txt.trim(), 90));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let default_title = first_user_prompt
        .as_deref()
        .map(clean_title_from_prompt)
        .unwrap_or_else(|| "Keel Session".to_string());
    let title = ov.and_then(|o| o.title.clone()).unwrap_or(default_title);

    let workdir = ov.and_then(|o| o.workdir.clone()).unwrap_or_else(home_str);
    let harness = ov
        .and_then(|o| o.harness.clone())
        .unwrap_or_else(|| "keel".to_string());
    let raw_model = ov
        .and_then(|o| o.model.clone())
        .unwrap_or_else(|| "bonsai-9b".to_string());
    let (model, model_label) = humanize_model_name(&harness, &raw_model);
    let effort = ov
        .and_then(|o| o.effort.clone())
        .unwrap_or_else(|| "medium".to_string());
    let is_running = active_runner_ids.contains_key(&id);
    let updated_at = ov.map(|o| o.updated_at).unwrap_or(max_ts);
    let last_preview = ov
        .and_then(|o| o.appended_messages.last())
        .map(preview_of)
        .or(last_preview_text)
        .unwrap_or_else(|| "Keel session".to_string());

    Some(ConversationSummary {
        id: id.clone(),
        raw_id: raw_id.to_string(),
        title,
        subtitle: format_subtitle(&workdir),
        workdir,
        harness,
        profile: profile.map(|s| s.to_string()),
        profile_label,
        model,
        model_label,
        effort,
        status: if is_running {
            "busy".to_string()
        } else {
            "idle".to_string()
        },
        running_tasks_count: if is_running { 1 } else { 0 },
        running_task_label: active_runner_ids.get(&id).cloned(),
        updated_at,
        pinned: ov.map(|o| o.pinned).unwrap_or(false),
        last_preview,
        pr_links: Vec::new(),
        pending_approval: None,
    })
}

pub fn parse_keel_messages(path: &Path) -> Vec<ChatMessage> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let reader = BufReader::new(file);
    let mut messages = Vec::new();

    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ts_millis = val.get("ts").and_then(|v| v.as_i64()).unwrap_or(0);
        let timestamp = chrono::DateTime::from_timestamp_millis(ts_millis)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| Utc::now().to_rfc3339());
        let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let data = &val["data"];

        match kind {
            "user" => {
                let text = data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                messages.push(ChatMessage {
                    id: format!("kl-u-{}", messages.len()),
                    role: "user".to_string(),
                    harness: "keel".to_string(),
                    model: None,
                    timestamp,
                    text,
                    thinking: None,
                    tools: Vec::new(),
                    images: Vec::new(),
                    streaming: false,
                    blocks: Vec::new(),
                });
            }
            "assistant" => {
                let text = data
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut tools = Vec::new();
                if let Some(tcs) = data.get("tool_calls").and_then(|v| v.as_array()) {
                    for tc in tcs {
                        let name = tc
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let args = tc.get("args").or_else(|| tc.get("arguments"));
                        let summary = args
                            .and_then(|a| {
                                a.get("path")
                                    .or_else(|| a.get("file"))
                                    .or_else(|| a.get("command"))
                                    .or_else(|| a.get("query"))
                                    .or_else(|| a.get("fact"))
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .or_else(|| serde_json::to_string(a).ok())
                            })
                            .map(|s| truncate_str(&s, 64))
                            .unwrap_or_else(|| name.clone());
                        tools.push(ToolSummaryItem {
                            name,
                            summary,
                            status: "done".to_string(),
                        });
                    }
                }
                messages.push(ChatMessage {
                    id: format!("kl-a-{}", messages.len()),
                    role: "assistant".to_string(),
                    harness: "keel".to_string(),
                    model: Some("Bonsai 2 9B (Local)".to_string()),
                    timestamp,
                    text,
                    thinking: None,
                    tools,
                    images: Vec::new(),
                    streaming: false,
                    blocks: Vec::new(),
                });
            }
            _ => {}
        }
    }

    messages
}

fn find_claude_jsonl(raw_id: &str, home: &Path) -> Option<PathBuf> {
    if !is_safe_session_id(raw_id) {
        return None;
    }
    let claude_proj_root = home.join(".claude").join("projects");
    if let Ok(proj_dirs) = fs::read_dir(&claude_proj_root) {
        for pdir in proj_dirs.flatten() {
            let candidate = pdir.path().join(format!("{}.jsonl", raw_id));
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn summarize_claude_jsonl(
    path: &Path,
    mtime: i64,
    profile: Option<&str>,
    metas: &HashMap<String, ClaudeActiveMeta>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    let raw_id = path.file_stem()?.to_string_lossy().to_string();
    let id = profiles::make_conv_id("claude", profile, &raw_id);
    let (_, profile_label) = profile_display(profile);
    let ov = overlays.get(&id);

    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();

    // Read first 24 KB to extract initial user message / title
    let mut head_buf = vec![0u8; file_len.min(24576) as usize];
    let _ = file.read(&mut head_buf);
    let head_str = String::from_utf8_lossy(&head_buf);

    let mut initial_title = None;
    for line in head_str.lines() {
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            if val.get("type").and_then(|v| v.as_str()) == Some("custom-title") {
                if let Some(ct) = val.get("customTitle").and_then(|v| v.as_str()) {
                    if !ct.trim().is_empty() {
                        initial_title = Some(ct.trim().to_string());
                        break;
                    }
                }
            }
            if initial_title.is_none()
                && val.get("type").and_then(|v| v.as_str()) == Some("user")
                && val.get("isMeta").and_then(|v| v.as_bool()) != Some(true)
            {
                if let Some(txt) = extract_claude_user_text(&val) {
                    let trimmed = txt.trim();
                    if trimmed.starts_with("[Recent thread context]") {
                        return None;
                    }
                    if !trimmed.is_empty() && !trimmed.starts_with('<') {
                        initial_title = Some(clean_title_from_prompt(trimmed));
                        break;
                    }
                }
            }
        }
    }

    // Read tail 3 MB for latest model, status, preview, PR links, and active tool
    let tail_offset = file_len.saturating_sub(3 * 1024 * 1024);
    let _ = file.seek(SeekFrom::Start(tail_offset));
    let mut tail_bytes = Vec::new();
    let _ = file.read_to_end(&mut tail_bytes);
    let tail_str = String::from_utf8_lossy(&tail_bytes);

    let mut detected_model = "claude-opus-5-5".to_string();
    let mut last_preview = String::new();
    let mut pr_links = Vec::new();
    let mut last_tool_desc: Option<String> = None;

    for (idx, line) in tail_str.lines().enumerate() {
        if tail_offset > 0 && idx == 0 {
            continue;
        }
        if line.len() > 24576 && line.contains("\"tool_result\"") {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            let t = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if t == "pr-link" {
                if let Some(url) = val
                    .get("url")
                    .or_else(|| val.get("prUrl"))
                    .and_then(|v| v.as_str())
                {
                    if !pr_links.contains(&url.to_string()) {
                        pr_links.push(url.to_string());
                    }
                }
            } else if t == "assistant" {
                if let Some(m) = val
                    .get("message")
                    .and_then(|m| m.get("model"))
                    .and_then(|v| v.as_str())
                {
                    detected_model = m.to_string();
                }
                if let Some(arr) = val
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for item in arr {
                        let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if itype == "text" {
                            if let Some(txt) = item.get("text").and_then(|v| v.as_str()) {
                                if !txt.trim().is_empty() {
                                    last_preview = truncate_str(txt.trim(), 100);
                                }
                            }
                        } else if itype == "tool_use" {
                            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("Tool");
                            let desc = item
                                .get("input")
                                .and_then(|inp| {
                                    inp.get("description")
                                        .or_else(|| inp.get("command"))
                                        .or_else(|| inp.get("file_path"))
                                })
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            last_tool_desc = Some(if desc.is_empty() {
                                name.to_string()
                            } else {
                                format!("{}: {}", name, truncate_str(desc, 48))
                            });
                        }
                    }
                }
            } else if t == "user" && val.get("isMeta").and_then(|v| v.as_bool()) != Some(true) {
                if let Some(txt) = extract_claude_user_text(&val) {
                    let trimmed = txt.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('<') {
                        last_preview = truncate_str(trimmed, 100);
                    }
                }
            }
        }
    }

    let meta = metas.get(&raw_id);
    let workdir = ov
        .and_then(|o| o.workdir.clone())
        .or_else(|| meta.map(|m| m.cwd.clone()))
        .unwrap_or_else(home_str);

    let meta_busy = meta.map(|m| m.status == "busy").unwrap_or(false);
    let runner_busy = active_runner_ids.contains_key(&id);
    let is_busy = meta_busy || runner_busy;

    let harness = ov
        .and_then(|o| o.harness.clone())
        .unwrap_or_else(|| "claude".to_string());
    let raw_model = ov.and_then(|o| o.model.clone()).unwrap_or(detected_model);
    let (model, model_label) = humanize_model_name(&harness, &raw_model);
    let effort = ov
        .and_then(|o| o.effort.clone())
        .unwrap_or_else(|| "medium".to_string());

    let title = ov
        .and_then(|o| o.title.clone())
        .or(initial_title)
        .unwrap_or_else(|| {
            let short: String = raw_id.chars().take(8).collect();
            format!("Claude · {short}")
        });

    let updated_at = ov.map(|o| o.updated_at.max(mtime)).unwrap_or(mtime);

    let running_task_label = if let Some(lbl) = active_runner_ids.get(&id) {
        Some(lbl.clone())
    } else if is_busy {
        last_tool_desc.or_else(|| Some("Active background task".to_string()))
    } else {
        None
    };

    Some(ConversationSummary {
        id: id.clone(),
        raw_id: raw_id.clone(),
        title,
        subtitle: format_subtitle(&workdir),
        workdir,
        harness,
        profile: profile.map(|s| s.to_string()),
        profile_label,
        model,
        model_label,
        effort,
        status: if is_busy {
            "busy".to_string()
        } else {
            "idle".to_string()
        },
        running_tasks_count: if is_busy { 1 } else { 0 },
        running_task_label,
        updated_at,
        pinned: ov.map(|o| o.pinned).unwrap_or(false),
        last_preview: if last_preview.is_empty() {
            "Claude Code session".to_string()
        } else {
            last_preview
        },
        pr_links,
        pending_approval: None,
    })
}

fn summarize_agy_transcript(
    raw_id: &str,
    transcript: &Path,
    profile: Option<&str>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    let id = profiles::make_conv_id("agy", profile, raw_id);
    let (_, profile_label) = profile_display(profile);
    let ov = overlays.get(&id);
    let mtime = fs::metadata(transcript)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut file = File::open(transcript).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut initial_title: Option<String> = None;
    let mut last_preview = String::new();
    let mut last_tool: Option<String> = None;

    if file_len <= 131072 {
        let mut buf = Vec::new();
        let _ = file.read_to_end(&mut buf);
        let s = String::from_utf8_lossy(&buf);
        for line in s.lines() {
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                let stype = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if stype == "USER_INPUT" {
                    if let Some(raw_content) = val.get("content").and_then(|v| v.as_str()) {
                        let clean = extract_agy_user_request(raw_content);
                        if clean.starts_with("[Recent thread context]") {
                            return None;
                        }
                        if !clean.is_empty() {
                            if initial_title.is_none()
                                && clean.to_lowercase() != "okay does this reach you?"
                            {
                                initial_title = Some(clean_title_from_prompt(&clean));
                            }
                            last_preview = truncate_str(&clean, 100);
                        }
                    }
                } else if stype == "PLANNER_RESPONSE" {
                    if let Some(c) = val.get("content").and_then(|v| v.as_str()) {
                        if !c.trim().is_empty() {
                            last_preview = truncate_str(c.trim(), 100);
                        }
                    }
                    if let Some(tcs) = val.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            if let Some(summary) = tc
                                .get("args")
                                .or_else(|| tc.get("arguments"))
                                .and_then(|a| a.get("toolSummary"))
                                .and_then(|v| v.as_str())
                            {
                                last_tool = Some(summary.to_string());
                            }
                        }
                    }
                }
            }
        }
    } else {
        // Read head 32 KB for title
        let mut head_buf = vec![0u8; 32768];
        let _ = file.read(&mut head_buf);
        let head_str = String::from_utf8_lossy(&head_buf);
        for line in head_str.lines() {
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                let stype = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if stype == "USER_INPUT" {
                    if let Some(raw_content) = val.get("content").and_then(|v| v.as_str()) {
                        let clean = extract_agy_user_request(raw_content);
                        if clean.starts_with("[Recent thread context]") {
                            return None;
                        }
                        if !clean.is_empty() {
                            if initial_title.is_none()
                                && clean.to_lowercase() != "okay does this reach you?"
                            {
                                initial_title = Some(clean_title_from_prompt(&clean));
                            }
                            last_preview = truncate_str(&clean, 100);
                            break;
                        }
                    }
                }
            }
        }
        // Read tail 96 KB for latest preview & tool
        let tail_offset = file_len.saturating_sub(98304);
        let _ = file.seek(SeekFrom::Start(tail_offset));
        let mut tail_bytes = Vec::new();
        let _ = file.read_to_end(&mut tail_bytes);
        let tail_str = String::from_utf8_lossy(&tail_bytes);
        for (idx, line) in tail_str.lines().enumerate() {
            if tail_offset > 0 && idx == 0 {
                continue;
            }
            if let Ok(val) = serde_json::from_str::<Value>(line) {
                let stype = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if stype == "USER_INPUT" {
                    if let Some(raw_content) = val.get("content").and_then(|v| v.as_str()) {
                        let clean = extract_agy_user_request(raw_content);
                        if !clean.is_empty() {
                            last_preview = truncate_str(&clean, 100);
                        }
                    }
                } else if stype == "PLANNER_RESPONSE" {
                    if let Some(c) = val.get("content").and_then(|v| v.as_str()) {
                        if !c.trim().is_empty() {
                            last_preview = truncate_str(c.trim(), 100);
                        }
                    }
                    if let Some(tcs) = val.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in tcs {
                            if let Some(summary) = tc
                                .get("args")
                                .or_else(|| tc.get("arguments"))
                                .and_then(|a| a.get("toolSummary"))
                                .and_then(|v| v.as_str())
                            {
                                last_tool = Some(summary.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    let workdir = ov.and_then(|o| o.workdir.clone()).unwrap_or_else(home_str);
    let harness = ov
        .and_then(|o| o.harness.clone())
        .unwrap_or_else(|| "agy".to_string());
    let raw_model = ov
        .and_then(|o| o.model.clone())
        .unwrap_or_else(|| "default".to_string());
    let (model, model_label) = humanize_model_name(&harness, &raw_model);
    let effort = ov
        .and_then(|o| o.effort.clone())
        .unwrap_or_else(|| "medium".to_string());

    let is_busy = active_runner_ids.contains_key(&id);
    let title = ov
        .and_then(|o| o.title.clone())
        .or_else(|| {
            if raw_id.starts_with("bdf89862") {
                Some("MoE Deck Gateway & Setup".to_string())
            } else {
                initial_title
            }
        })
        .unwrap_or_else(|| "Antigravity Session".to_string());

    Some(ConversationSummary {
        id: id.clone(),
        raw_id: raw_id.to_string(),
        title,
        subtitle: format_subtitle(&workdir),
        workdir,
        harness,
        profile: profile.map(|s| s.to_string()),
        profile_label,
        model,
        model_label,
        effort,
        status: if is_busy {
            "busy".to_string()
        } else {
            "idle".to_string()
        },
        running_tasks_count: if is_busy { 1 } else { 0 },
        running_task_label: active_runner_ids.get(&id).cloned().or(last_tool),
        updated_at: ov.map(|o| o.updated_at.max(mtime)).unwrap_or(mtime),
        // Antigravity sessions are ordinary recent sessions, not pins. Pinning
        // every discovered session made the whole list look permanently stuck.
        pinned: ov.map(|o| o.pinned).unwrap_or(false),
        last_preview,
        pr_links: Vec::new(),
        pending_approval: None,
    })
}

pub fn parse_claude_messages(path: &Path) -> Vec<ChatMessage> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    // Read last 12 MB and skip giant base64 tool_result lines before JSON parsing
    let offset = file_len.saturating_sub(12 * 1024 * 1024);
    let _ = file.seek(SeekFrom::Start(offset));
    let mut byte_buf = Vec::new();
    let _ = file.read_to_end(&mut byte_buf);
    let buf = String::from_utf8_lossy(&byte_buf);

    // One assistant message per turn (prompt to prompt), holding every text
    // update, thinking block and tool call in the order they happened.
    let mut messages: Vec<ChatMessage> = Vec::new();

    for (idx, line) in buf.lines().enumerate() {
        if offset > 0 && idx == 0 {
            continue; // skip partial first line
        }
        if line.len() > 24576 && line.contains("\"tool_result\"") {
            continue;
        }
        let val: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = val
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let content = val
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());

        if t == "user" && val.get("isMeta").and_then(|v| v.as_bool()) != Some(true) {
            if let (Some(arr), Some(turn)) = (
                content,
                messages.last_mut().filter(|m| m.role == "assistant"),
            ) {
                if crate::timeline::apply_claude_tool_results(&mut turn.blocks, arr) > 0 {
                    continue;
                }
            }
            if let Some(user_text) = extract_claude_user_text(&val) {
                let clean = user_text.trim();
                if !clean.is_empty() && !clean.starts_with('<') {
                    messages.push(ChatMessage {
                        id: val
                            .get("uuid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("u")
                            .to_string(),
                        role: "user".to_string(),
                        harness: "claude".to_string(),
                        model: None,
                        timestamp: ts,
                        text: clean.to_string(),
                        thinking: None,
                        tools: Vec::new(),
                        images: extract_claude_user_images(&val),
                        streaming: false,
                        blocks: Vec::new(),
                    });
                }
            }
        } else if t == "assistant" {
            let Some(arr) = content else { continue };
            let raw_model = val
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|v| v.as_str())
                .unwrap_or("claude-opus-5-5");
            let (_, model_label) = humanize_model_name("claude", raw_model);

            if messages.last().is_none_or(|m| m.role != "assistant") {
                messages.push(ChatMessage {
                    id: val
                        .get("uuid")
                        .and_then(|v| v.as_str())
                        .unwrap_or("a")
                        .to_string(),
                    role: "assistant".to_string(),
                    harness: "claude".to_string(),
                    model: None,
                    timestamp: String::new(),
                    text: String::new(),
                    thinking: None,
                    tools: Vec::new(),
                    images: Vec::new(),
                    streaming: false,
                    blocks: Vec::new(),
                });
            }
            let turn = messages.last_mut().expect("assistant turn");
            crate::timeline::push_claude_assistant(&mut turn.blocks, arr, "done");
            turn.timestamp = ts;
            turn.model = Some(model_label);

            for item in arr {
                if item.get("type").and_then(|v| v.as_str()) != Some("tool_use")
                    || item.get("name").and_then(|v| v.as_str()) != Some("Read")
                {
                    continue;
                }
                if let Some(fp) = item
                    .get("input")
                    .and_then(|i| i.get("file_path"))
                    .and_then(|v| v.as_str())
                {
                    if is_image_path(fp) && Path::new(fp).exists() {
                        let url = format!("/api/local-image?path={}", url_encode(fp));
                        if !turn.images.contains(&url) {
                            turn.images.push(url);
                        }
                    }
                }
            }
        }
    }

    messages.retain(|m| m.role != "assistant" || !m.blocks.is_empty());
    for m in &mut messages {
        crate::timeline::sync_legacy(m);
    }

    let len = messages.len();
    if len > 45 {
        messages.split_off(len - 45)
    } else {
        messages
    }
}

pub fn parse_agy_messages(transcript: &Path) -> Vec<ChatMessage> {
    let file = match File::open(transcript) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending_tools: Vec<ToolSummaryItem> = Vec::new();
    let mut pending_thinking: Vec<String> = Vec::new();
    let mut last_ts = String::new();

    for line in reader.lines().map_while(Result::ok) {
        let val: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let stype = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let ts = val
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if !ts.is_empty() {
            last_ts = ts.clone();
        }

        if stype == "USER_INPUT" {
            if !pending_tools.is_empty() || !pending_thinking.is_empty() {
                if let Some(last) = messages.last_mut().filter(|m| m.role == "assistant") {
                    last.tools.append(&mut pending_tools);
                    if let Some(th) = last.thinking.as_mut() {
                        if !pending_thinking.is_empty() {
                            th.push_str("\n\n");
                            th.push_str(&pending_thinking.join("\n\n"));
                        }
                    } else if !pending_thinking.is_empty() {
                        last.thinking = Some(pending_thinking.join("\n\n"));
                    }
                    pending_thinking.clear();
                } else {
                    messages.push(ChatMessage {
                        id: "agy-a-prev".to_string(),
                        role: "assistant".to_string(),
                        harness: "agy".to_string(),
                        model: None,
                        timestamp: ts.clone(),
                        text: String::new(),
                        thinking: if pending_thinking.is_empty() {
                            None
                        } else {
                            Some(pending_thinking.join("\n\n"))
                        },
                        tools: std::mem::take(&mut pending_tools),
                        images: Vec::new(),
                        streaming: false,
                        blocks: Vec::new(),
                    });
                    pending_thinking.clear();
                }
            }

            let raw = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let clean = extract_agy_user_request(raw);
            let mut images = Vec::new();
            if let Some(media_arr) = val.get("media").and_then(|v| v.as_array()) {
                for m in media_arr {
                    if let Some(uri) = m.get("uri").and_then(|v| v.as_str()) {
                        let path = uri.trim_start_matches("file://");
                        if Path::new(path).exists() {
                            images.push(format!("/api/local-image?path={}", url_encode(path)));
                        }
                    }
                }
            }
            if !clean.is_empty() || !images.is_empty() {
                messages.push(ChatMessage {
                    id: format!(
                        "agy-u-{}",
                        val.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0)
                    ),
                    role: "user".to_string(),
                    harness: "agy".to_string(),
                    model: None,
                    timestamp: ts,
                    text: clean,
                    thinking: None,
                    tools: Vec::new(),
                    images,
                    streaming: false,
                    blocks: Vec::new(),
                });
            }
        } else if stype == "PLANNER_RESPONSE" {
            if let Some(th) = val.get("thinking").and_then(|v| v.as_str()) {
                if !th.trim().is_empty() {
                    pending_thinking.push(th.trim().to_string());
                }
            }
            if let Some(tcs) = val.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let name = tc
                        .get("name")
                        .or_else(|| tc.get("function").and_then(|f| f.get("name")))
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool")
                        .to_string();
                    let args = tc.get("args").or_else(|| tc.get("arguments"));
                    let summary = args
                        .and_then(|a| {
                            a.get("toolSummary")
                                .or_else(|| a.get("query"))
                                .or_else(|| a.get("Path"))
                                .or_else(|| a.get("CommandLine"))
                        })
                        .and_then(|v| v.as_str())
                        .map(|s| truncate_str(s, 72))
                        .unwrap_or_else(|| name.clone());
                    pending_tools.push(ToolSummaryItem {
                        name,
                        summary,
                        status: "done".to_string(),
                    });
                }
            }
            if let Some(content) = val.get("content").and_then(|v| v.as_str()) {
                if !content.trim().is_empty() {
                    let thinking_str = if pending_thinking.is_empty() {
                        None
                    } else {
                        Some(pending_thinking.join("\n\n"))
                    };
                    pending_thinking.clear();
                    messages.push(ChatMessage {
                        id: format!(
                            "agy-a-{}",
                            val.get("step_index").and_then(|v| v.as_i64()).unwrap_or(0)
                        ),
                        role: "assistant".to_string(),
                        harness: "agy".to_string(),
                        model: None,
                        timestamp: ts,
                        text: content.trim().to_string(),
                        thinking: thinking_str,
                        tools: std::mem::take(&mut pending_tools),
                        images: Vec::new(),
                        streaming: false,
                        blocks: Vec::new(),
                    });
                }
            }
        }
    }

    if !pending_tools.is_empty() || !pending_thinking.is_empty() {
        if let Some(last) = messages.last_mut().filter(|m| m.role == "assistant") {
            last.tools.extend(pending_tools);
            if !pending_thinking.is_empty() {
                if let Some(th) = last.thinking.as_mut() {
                    th.push_str("\n\n");
                    th.push_str(&pending_thinking.join("\n\n"));
                } else {
                    last.thinking = Some(pending_thinking.join("\n\n"));
                }
            }
        } else {
            messages.push(ChatMessage {
                id: "agy-a-active".to_string(),
                role: "assistant".to_string(),
                harness: "agy".to_string(),
                model: None,
                timestamp: last_ts,
                text: String::new(),
                thinking: if pending_thinking.is_empty() {
                    None
                } else {
                    Some(pending_thinking.join("\n\n"))
                },
                tools: pending_tools,
                images: Vec::new(),
                streaming: true,
                blocks: Vec::new(),
            });
        }
    }

    messages
}

fn extract_claude_user_text(val: &Value) -> Option<String> {
    let content = val.get("message")?.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut texts = Vec::new();
        for item in arr {
            if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    texts.push(t.to_string());
                }
            }
        }
        if !texts.is_empty() {
            return Some(texts.join("\n"));
        }
    }
    None
}

fn extract_claude_user_images(val: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(arr) = val
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for item in arr {
            if item.get("type").and_then(|v| v.as_str()) == Some("image") {
                if let Some(path) = item.get("path").and_then(|v| v.as_str()) {
                    out.push(format!("/api/local-image?path={}", url_encode(path)));
                }
            }
        }
    }
    out
}

pub fn extract_agy_user_request(raw: &str) -> String {
    if let Some(start) = raw.find("<USER_REQUEST>") {
        let after = &raw[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after.find("</USER_REQUEST>") {
            return after[..end].trim().to_string();
        }
    }
    raw.trim().to_string()
}

pub fn clean_title_from_prompt(text: &str) -> String {
    let mut candidate = text.trim();
    while candidate.starts_with('#')
        || candidate.starts_with('*')
        || candidate.starts_with('-')
        || candidate.starts_with('>')
        || candidate.starts_with('/')
    {
        candidate = candidate
            .trim_start_matches('#')
            .trim_start_matches('*')
            .trim_start_matches('-')
            .trim_start_matches('>')
            .trim_start_matches('/')
            .trim();
    }
    let first_line = candidate.lines().next().unwrap_or(candidate).trim();
    let sentence_end = first_line
        .find(['.', '?', '!', '\n'])
        .unwrap_or(first_line.len());
    let sub = first_line[..sentence_end].trim();
    let base = if sub.len() >= 4 {
        sub
    } else if !first_line.is_empty() {
        first_line
    } else {
        "New Thread"
    };
    let truncated = truncate_str(base, 34);
    if truncated.trim().is_empty() {
        "New Thread".to_string()
    } else {
        truncated
    }
}

pub fn resolve_thread_title(overlay: &StoredThreadOverlay, messages: &[ChatMessage]) -> String {
    if let Some(t) = &overlay.title {
        let trimmed = t.trim();
        if !trimmed.is_empty() && trimmed != "New Thread" {
            return trimmed.to_string();
        }
    }
    let first_user = overlay
        .appended_messages
        .iter()
        .chain(messages.iter())
        .find(|m| m.role == "user" && !m.text.trim().is_empty());
    if let Some(user_msg) = first_user {
        return clean_title_from_prompt(&user_msg.text);
    }
    "New Thread".to_string()
}

pub fn format_subtitle(workdir: &str) -> String {
    let home = home_str();
    let home_name = home_dir()
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_string())
        .unwrap_or_else(|| "~".to_string());
    let clean = workdir.trim_end_matches('/');
    if clean == home {
        home_name
    } else if let Some(name) = Path::new(workdir).file_name().and_then(|n| n.to_str()) {
        format!("{} · {}", home_name, name)
    } else {
        home_name
    }
}

/// OpenCode's CLI stores an argv prompt with a literal pair of surrounding
/// quotes (`"Reply with exactly: ok"`). Strip that one layer so the cockpit
/// shows the prompt as typed and duplicate detection can match the overlay.
fn strip_opencode_quotes(text: &str) -> String {
    let trimmed = text.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(trimmed)
        .to_string()
}

/// Compare prompts across the native transcript and MoE Deck's overlay without
/// being sensitive to that quoting or to collapsed whitespace.
fn normalize_chat_text(text: &str) -> String {
    strip_opencode_quotes(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drawer preview of a stored message, with harness errors made readable.
fn preview_of(m: &ChatMessage) -> String {
    truncate_str(&readable_text(m), 90)
}

/// Stored failures predate the readable wording; rewrite them on the way out.
fn readable_text(m: &ChatMessage) -> String {
    if m.role != "assistant" || m.streaming {
        return m.text.clone();
    }
    let at = chrono::DateTime::parse_from_rfc3339(&m.timestamp)
        .map(|t| t.with_timezone(&chrono::Local))
        .unwrap_or_else(|_| chrono::Local::now());
    crate::completion::readable_failure(&m.harness, &m.text, at)
}

pub fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let collected: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}…", collected)
    } else {
        collected
    }
}

fn is_image_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".webp")
        || lower.ends_with(".gif")
}

fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub fn list_projects() -> Vec<WorkspaceInfo> {
    let mut out = Vec::new();
    let home = home_dir();
    out.push(inspect_workspace(&home.to_string_lossy()));

    let proj_dir = home.join("Projects");
    if let Ok(entries) = fs::read_dir(&proj_dir) {
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_dir()
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| !n.starts_with('.') && n != "node_modules")
                        .unwrap_or(false)
            })
            .collect();
        dirs.sort();
        for d in dirs.into_iter().take(24) {
            out.push(inspect_workspace(&d.to_string_lossy()));
        }
    }
    out
}

pub fn inspect_workspace(workdir: &str) -> WorkspaceInfo {
    let Some(canon) = sanitize_workdir(workdir) else {
        return WorkspaceInfo {
            workdir: workdir.to_string(),
            name: "workspace".to_string(),
            is_git: false,
            branch: None,
            modified_files: Vec::new(),
            recent_commits: Vec::new(),
            worktrees: Vec::new(),
        };
    };
    let clean_dir = canon.to_string_lossy().to_string();
    let name = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    let git_dir = canon.join(".git");
    if !git_dir.exists() {
        return WorkspaceInfo {
            workdir: workdir.to_string(),
            name,
            is_git: false,
            branch: None,
            modified_files: Vec::new(),
            recent_commits: Vec::new(),
            worktrees: Vec::new(),
        };
    }

    let branch = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let mut modified_files = Vec::new();
    if let Ok(st_out) = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("status")
        .arg("--porcelain")
        .arg("-uall")
        .output()
    {
        if st_out.status.success() {
            let text = String::from_utf8_lossy(&st_out.stdout);
            for line in text.lines().take(25) {
                if line.len() > 3 {
                    let status = line[..2].trim().to_string();
                    let path = line[3..].trim().to_string();
                    modified_files.push(GitFileStat {
                        status: if status.is_empty() {
                            "M".to_string()
                        } else {
                            status
                        },
                        path,
                    });
                }
            }
        }
    }

    let mut recent_commits = Vec::new();
    if let Ok(log_out) = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("log")
        .arg("-n")
        .arg("4")
        .arg("--oneline")
        .output()
    {
        if log_out.status.success() {
            let text = String::from_utf8_lossy(&log_out.stdout);
            for line in text.lines() {
                if !line.trim().is_empty() {
                    recent_commits.push(line.trim().to_string());
                }
            }
        }
    }

    let worktrees = crate::ops::list_worktrees(&clean_dir);

    WorkspaceInfo {
        workdir: workdir.to_string(),
        name,
        is_git: true,
        branch,
        modified_files,
        recent_commits,
        worktrees,
    }
}

pub fn inspect_git_diff(workdir: &str) -> String {
    let Some(canon) = sanitize_workdir(workdir) else {
        return "Invalid or unauthorized workspace path.".to_string();
    };
    if !canon.join(".git").exists() {
        return "Not a git repository.".to_string();
    }
    let clean_dir = canon.to_string_lossy().to_string();

    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("diff")
        .arg("HEAD")
        .output()
    {
        if out.status.success() {
            let diff = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !diff.is_empty() {
                return truncate_str(&diff, 32000);
            }
        }
    }

    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("show")
        .arg("--stat")
        .arg("--patch")
        .arg("-n")
        .arg("1")
        .arg("HEAD")
        .output()
    {
        if out.status.success() {
            let show = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !show.is_empty() {
                return format!(
                    "# Working tree clean — showing latest commit (HEAD):\n\n{}",
                    truncate_str(&show, 32000)
                );
            }
        }
    }

    "No uncommitted changes or commits found.".to_string()
}

pub fn list_workspace_files(workdir: &str, query: &str) -> Vec<String> {
    let Some(root) = sanitize_workdir(workdir) else {
        return Vec::new();
    };
    let clean_dir = root.to_string_lossy().to_string();
    let q_lower = query.to_lowercase();
    let mut results = Vec::new();

    // Fast git ls-files first if inside a git repo
    if root.join(".git").exists() {
        if let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(&clean_dir)
            .arg("ls-files")
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty()
                        && (q_lower.is_empty() || trimmed.to_lowercase().contains(&q_lower))
                    {
                        results.push(trimmed.to_string());
                        if results.len() >= 18 {
                            return results;
                        }
                    }
                }
                return results;
            }
        }
    }

    // Fallback shallow directory scan (up to depth 2)
    collect_files_shallow(&root, &root, &q_lower, 2, &mut results);
    results
}

fn collect_files_shallow(
    base: &Path,
    current: &Path,
    q_lower: &str,
    depth: usize,
    out: &mut Vec<String>,
) {
    if depth == 0 || out.len() >= 18 {
        return;
    }
    let Ok(entries) = fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= 18 {
            return;
        }
        let p = entry.path();
        let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname.starts_with('.') || fname == "node_modules" || fname == "target" {
            continue;
        }
        if let Ok(rel) = p.strip_prefix(base) {
            let rel_str = rel.to_string_lossy().to_string();
            if q_lower.is_empty() || rel_str.to_lowercase().contains(q_lower) {
                out.push(if p.is_dir() {
                    format!("{}/", rel_str)
                } else {
                    rel_str
                });
            }
        }
        if p.is_dir() {
            collect_files_shallow(base, &p, q_lower, depth - 1, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn config_dir_is_made_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("moedeck-perm-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o775)).unwrap();
        restrict_to_owner(&dir);
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn test_set_overlay_native_session_does_not_deadlock() {
        // Regression: this used to re-enter OVERLAY_LOCK (load_overlays and
        // save_overlay lock again), deadlocking the first call and freezing
        // the whole daemon as every request then pinned a worker thread.
        set_overlay_native_session("helm:nonexistent-test-thread", "ses_test123");
        assert_eq!(overlay_native_session("helm:nonexistent-test-thread"), None);
        set_overlay_native_session("helm:nonexistent-test-thread", "");
    }

    #[test]
    fn test_opencode_model_from_row_unwraps_json() {
        let row: Value = serde_json::from_str(
            r#"{"model":"{\"id\":\"glm-5.3\",\"providerID\":\"opencode-go\",\"variant\":\"high\"}"}"#,
        )
        .unwrap();
        assert_eq!(opencode_model_from_row(&row), "opencode-go/glm-5.3");
        let bare: Value = serde_json::from_str(r#"{"model":"{\"id\":\"glm-5.3\"}"}"#).unwrap();
        assert_eq!(opencode_model_from_row(&bare), "glm-5.3");
        let missing: Value = serde_json::from_str(r#"{"model":null}"#).unwrap();
        assert_eq!(opencode_model_from_row(&missing), "default");
    }

    #[test]
    fn test_strip_opencode_quotes_and_normalize() {
        assert_eq!(
            strip_opencode_quotes("\"Reply with exactly: ok\""),
            "Reply with exactly: ok"
        );
        assert_eq!(strip_opencode_quotes("  plain  "), "plain");
        assert_eq!(strip_opencode_quotes("\"unbalanced"), "\"unbalanced");
        assert_eq!(normalize_chat_text(" a  b\n c "), "a b c");
        assert_eq!(normalize_chat_text("\"a  b\""), "a b");
    }

    /// OpenCode 2.x writes `session_v2` + `session_message`; parsing it must
    /// yield the same user/assistant/tool shape MoE Deck renders for v1.
    #[test]
    fn test_parse_opencode_v2_session_messages() {
        if Command::new("sqlite3").arg("--version").output().is_err() {
            return; // the daemon already needs sqlite3; skip on bare machines
        }
        let dir = std::env::temp_dir().join(format!("helm-oc-v2-{}", Uuid::new_v4()));
        fs::create_dir_all(dir.join(".local/share/opencode")).unwrap();
        let db = dir.join(".local/share/opencode/opencode.db");
        let assistant = serde_json::json!({
            "model": {"id": "glm-5.3", "providerID": "opencode-go"},
            "content": [
                {"type": "reasoning", "text": "checking"},
                {"type": "tool", "name": "shell",
                 "state": {"status": "completed", "input": {"command": "echo hi"}}},
                {"type": "text", "text": "all done"}
            ]
        })
        .to_string();
        let user = serde_json::json!({"text": "run echo"}).to_string();
        let status = Command::new("sqlite3")
            .arg(&db)
            .arg(format!(
                "CREATE TABLE session_v2 (id text, title text, directory text, model text, parent_id text, time_updated integer);\
                 CREATE TABLE session_message (session_id text, type text, seq integer, data text);\
                 INSERT INTO session_message VALUES ('ses_v2_test','user',1,'{user}');\
                 INSERT INTO session_message VALUES ('ses_v2_test','assistant',2,'{assistant}');\
                 INSERT INTO session_message VALUES ('ses_v2_test','idle',3,'{{}}');"
            ))
            .status()
            .unwrap();
        assert!(status.success());
        let messages = parse_opencode_messages("ses_v2_test", &dir);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text, "run echo");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text, "all done");
        assert_eq!(messages[1].thinking.as_deref(), Some("checking"));
        assert_eq!(messages[1].tools[0].name, "shell");
        assert_eq!(messages[1].tools[0].summary, "echo hi");
        assert_eq!(messages[1].tools[0].status, "done");
    }

    #[test]
    fn test_extract_agy_user_request() {
        let raw = "Header\n<USER_REQUEST>\nFix the mobile dock padding\n</USER_REQUEST>\nFooter";
        assert_eq!(extract_agy_user_request(raw), "Fix the mobile dock padding");
    }

    #[test]
    fn test_clean_title_from_prompt() {
        assert_eq!(
            clean_title_from_prompt("Fix the mobile dock padding. Also add CI."),
            "Fix the mobile dock padding"
        );
        assert_eq!(
            clean_title_from_prompt("# Add support for dark mode"),
            "Add support for dark mode"
        );
        assert_eq!(
            clean_title_from_prompt("## Refactor Rust Storage\nDetails here"),
            "Refactor Rust Storage"
        );
        assert_eq!(
            clean_title_from_prompt("/review audit PR"),
            "review audit PR"
        );
        assert_eq!(clean_title_from_prompt(""), "New Thread");
        assert_eq!(clean_title_from_prompt("   "), "New Thread");
    }

    #[test]
    fn test_resolve_thread_title() {
        let overlay = StoredThreadOverlay {
            id: "helm:123".to_string(),
            title: Some("Explicit Title".to_string()),
            pinned: false,
            harness: Some("claude".to_string()),
            model: None,
            effort: None,
            workdir: None,
            profile: None,
            created_at: 0,
            updated_at: 0,
            appended_messages: Vec::new(),
            native_session: None,
        };
        assert_eq!(resolve_thread_title(&overlay, &[]), "Explicit Title");

        let overlay_default = StoredThreadOverlay {
            id: "helm:124".to_string(),
            title: Some("New Thread".to_string()),
            pinned: false,
            harness: Some("claude".to_string()),
            model: None,
            effort: None,
            workdir: None,
            profile: None,
            created_at: 0,
            updated_at: 0,
            appended_messages: vec![ChatMessage {
                id: "m1".to_string(),
                role: "user".to_string(),
                harness: "claude".to_string(),
                model: None,
                timestamp: "2026-11-03T00:00:00Z".to_string(),
                text: "# Implement OAuth login flow".to_string(),
                thinking: None,
                tools: Vec::new(),
                images: Vec::new(),
                streaming: false,
                blocks: Vec::new(),
            }],
            native_session: None,
        };
        assert_eq!(
            resolve_thread_title(&overlay_default, &[]),
            "Implement OAuth login flow"
        );
    }

    #[test]
    fn test_inspect_workspace_and_files() {
        let info = inspect_workspace(".");
        assert!(!info.name.is_empty());
        let files = list_workspace_files(".", "Cargo");
        assert!(files.iter().any(|f| f.contains("Cargo.toml")));
        let diff = inspect_git_diff(".");
        assert!(!diff.is_empty());
    }

    #[test]
    fn test_session_id_and_workdir_validation() {
        assert!(is_safe_session_id("bdf89862-dce2-465f-b4f6-7b18f9843744"));
        assert!(is_safe_session_id("ses_e62ffc059ffeULYF6tG1SAEapS"));
        assert!(!is_safe_session_id("../etc/passwd"));
        assert!(!is_safe_session_id("foo/bar"));
        assert!(!is_safe_session_id(""));

        // Unauthorized system directories must be rejected
        assert!(sanitize_workdir("/etc").is_none());
        assert!(sanitize_workdir("/root").is_none());
        assert!(sanitize_workdir(".").is_some());
    }

    #[test]
    fn test_parse_keel_messages() {
        let dir = std::env::temp_dir().join(format!("helm-keel-test-{}", Uuid::new_v4()));
        let _ = fs::create_dir_all(&dir);
        let log_file = dir.join("session.jsonl");

        let content = r#"{"ts":1793650000000,"kind":"user","data":{"text":"Implement feature X"}}
{"ts":1793650001000,"kind":"assistant","data":{"role":"assistant","text":"Starting task","tool_calls":[{"id":"call_1","name":"patch_file","arguments":{"path":"src/lib.rs"}}]}}
{"ts":1793650002000,"kind":"tool","data":{"id":"call_1","content":"Applied patch"}}
{"ts":1793650003000,"kind":"assistant","data":{"role":"assistant","text":"All done!","tool_calls":[]}}
"#;
        fs::write(&log_file, content).unwrap();

        let msgs = parse_keel_messages(&log_file);
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "Implement feature X");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].tools.len(), 1);
        assert_eq!(msgs[1].tools[0].name, "patch_file");
        assert_eq!(msgs[1].tools[0].summary, "src/lib.rs");
        assert_eq!(msgs[2].role, "assistant");
        assert_eq!(msgs[2].text, "All done!");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_turn_keeps_short_updates_and_tool_output_in_order() {
        let path = std::env::temp_dir().join(format!("helm-claude-{}.jsonl", Uuid::new_v4()));
        let lines = [
            r#"{"type":"user","uuid":"u1","message":{"content":"Fix the test"}}"#,
            r#"{"type":"assistant","uuid":"a1","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Let me check."}]}}"#,
            r#"{"type":"assistant","uuid":"a2","message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","uuid":"r1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"1 failed"}]}}"#,
            r#"{"type":"assistant","uuid":"a3","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"Fixed it."}]}}"#,
        ];
        fs::write(&path, lines.join("\n")).unwrap();
        let msgs = parse_claude_messages(&path);
        let _ = fs::remove_file(&path);

        assert_eq!(msgs.len(), 2, "tool results must not become user messages");
        let turn = &msgs[1];
        assert_eq!(turn.text, "Let me check.\n\nFixed it.");
        assert_eq!(turn.blocks.len(), 3);
        assert!(matches!(
            &turn.blocks[1],
            crate::models::TurnBlock::Tool { output: Some(o), status, .. } if o == "1 failed" && status == "error"
        ));
        assert_eq!(turn.tools.len(), 1, "legacy slots stay filled for the TUI");
        assert_eq!(turn.tools[0].status, "error");
    }
}

#[cfg(test)]
mod codex_context_tests {
    #[test]
    fn latest_native_context_wins_over_old_model_and_workdir() {
        let tail = concat!(
            "partial line\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","effort":"low","cwd":"/old"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol","effort":"ultra","cwd":"/project"}}"#
        );
        assert_eq!(
            super::codex_context_from_tail(
                tail,
                ("default".into(), "default".into(), "/home".into())
            ),
            ("gpt-6.1-sol".into(), "ultra".into(), "/project".into())
        );
    }
}

#[cfg(test)]
mod codex_native_model_tests {
    #[test]
    fn response_item_uses_actual_turn_model_and_ignores_system_messages() {
        let path =
            std::env::temp_dir().join(format!("helm-codex-model-{}.jsonl", uuid::Uuid::new_v4()));
        std::fs::write(&path, concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"text":"hidden"}]}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Verified."}]}}"#
        )).unwrap();
        let messages = super::parse_codex_messages(&path);
        std::fs::remove_file(path).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].model.as_deref(), Some("gpt-6.1-sol"));
    }
}

#[cfg(test)]
mod adopted_alias_tests {
    #[test]
    fn adopted_alias_is_account_scoped_and_never_hides_the_helm_thread() {
        let overlay:crate::models::StoredThreadOverlay=serde_json::from_value(serde_json::json!({"id":"helm:proof","harness":"codex","profile":"codex-2","native_session":"native-uuid","pinned":false,"created_at":0,"updated_at":0})).unwrap();
        let aliases = super::adopted_native_aliases(&std::collections::HashMap::from([(
            overlay.id.clone(),
            overlay,
        )]));
        assert!(aliases.contains("codex:codex-2:native-uuid"));
        assert!(!aliases.contains("codex:native-uuid"));
        assert!(!aliases.contains("helm:proof"));
    }
}
