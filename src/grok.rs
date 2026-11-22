//! Grok Build CLI (`grok`) headless adapter.
//!
//! Non-interactive turns use print mode and newline-delimited JSON:
//! <https://docs.x.ai/build/cli/headless-scripting>.
//! Sessions on disk: <https://docs.x.ai/build/features/sessions>.
//! Permissions: <https://docs.x.ai/build/features/permissions>.
//!
//! Sign in with `grok login` (or `grok login --device-auth` on a machine
//! without a browser), or export `XAI_API_KEY` for the service user. The key
//! stays in the environment; it is never passed as an argument.
//!
//! `--session-id` names a new UUID. It does not resume. Resume is `--resume`.
//! The headless page's shorter "create or resume" wording loses to the
//! sessions page and the CLI reference, which both say a new session only.

use crate::discovery::{self, truncate_str};
use crate::models::{
    ChatMessage, ConversationSummary, SafetyMode, StoredThreadOverlay, ToolSummaryItem,
};
use crate::profiles;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const BINARY: &str = "grok";

/// Built-in read-only tool ids from the headless guide. `--tools` is an
/// allowlist; anything else (shell, edit, write) is removed for that turn.
pub const READ_ONLY_TOOLS: &str = "read_file,grep,list_dir";

pub struct Turn<'a> {
    pub safety: SafetyMode,
    pub model: &'a str,
    pub effort: &'a str,
    pub workdir: &'a str,
    pub session: Option<&'a str>,
    pub prompt: &'a str,
}

impl Turn<'_> {
    /// Argv after the binary. `grok -p` does not pause for a phone approval:
    /// the streaming-json stream is one-way, and tool approvals that can wait
    /// belong to ACP (`grok agent stdio`), which MoE Deck does not drive.
    ///
    /// Safe is read-only (`dontAsk` plus a read-only tool allowlist and the
    /// `read-only` sandbox). Standard selects `auto`, which in a
    /// non-interactive `grok -p` runs what the classifier allows and reports
    /// the rest back to the model instead of prompting. Yolo passes
    /// `--always-approve`. Deny rules in `~/.grok/config.toml` still win.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "--no-auto-update".to_string(),
            "--output-format".to_string(),
            "streaming-json".to_string(),
            "--cwd".to_string(),
            self.workdir.to_string(),
        ];
        match self.safety {
            SafetyMode::Safe => {
                args.extend([
                    "--permission-mode".to_string(),
                    "dontAsk".to_string(),
                    "--sandbox".to_string(),
                    "read-only".to_string(),
                    "--tools".to_string(),
                    READ_ONLY_TOOLS.to_string(),
                ]);
            }
            SafetyMode::Standard => {
                args.extend(["--permission-mode".to_string(), "auto".to_string()]);
            }
            SafetyMode::Yolo => {
                args.push("--always-approve".to_string());
            }
        }
        if !self.model.is_empty() && self.model != "default" {
            args.push("--model".to_string());
            args.push(self.model.to_string());
        }
        if matches!(
            self.effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            args.push("--effort".to_string());
            args.push(self.effort.to_string());
        }
        if let Some(session) = self.session {
            args.push("--resume".to_string());
            args.push(session.to_string());
        }
        args.push("-p".to_string());
        args.push(self.prompt.to_string());
        args
    }
}

pub struct ToolProgress {
    pub name: String,
    pub summary: String,
    pub status: &'static str,
    /// False when an update names no tool. The caller then closes the latest
    /// running tool instead of adding a second row.
    pub named: bool,
}

pub enum StreamEffect {
    Text(String),
    Thought(String),
    Tool(ToolProgress),
}

/// One streaming-json line. `None` means this line belongs to another harness.
/// Tool events are progress only: print mode has already run or skipped the
/// tool before the line is printed, so this must not open a phone approval.
pub fn stream_effect(value: &Value) -> Option<StreamEffect> {
    let kind = value.get("type").and_then(|v| v.as_str())?;
    match kind {
        "text" => {
            let text = value.get("data").and_then(|v| v.as_str())?;
            Some(StreamEffect::Text(text.to_string()))
        }
        "thought" => {
            let text = value.get("data").and_then(|v| v.as_str()).unwrap_or("");
            Some(StreamEffect::Thought(text.to_string()))
        }
        "tool_call" => {
            if value.get("toolName").is_none() && value.get("rawInput").is_none() {
                return None;
            }
            Some(StreamEffect::Tool(tool_progress(value, true)))
        }
        "tool_call_update" => Some(StreamEffect::Tool(tool_progress(value, false))),
        _ => None,
    }
}

fn tool_label(value: &Value) -> Option<&str> {
    value
        .get("toolName")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value
                .pointer("/_meta/x.ai~1tool/name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            value
                .get("title")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
}

fn tool_progress(value: &Value, started: bool) -> ToolProgress {
    let label = tool_label(value);
    let named = label.is_some();
    let name = label.unwrap_or("tool").to_string();
    let raw = value.get("rawInput");
    let summary = raw
        .and_then(|args| {
            args.get("path")
                .or_else(|| args.get("file_path"))
                .or_else(|| args.get("command"))
                .or_else(|| args.get("cmd"))
                .or_else(|| args.get("query"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.clone());
    let status = match value.get("status").and_then(|v| v.as_str()) {
        Some("completed" | "done" | "success") => "done",
        Some("failed" | "error" | "cancelled" | "canceled") => "error",
        Some("in_progress" | "pending" | "running") => "running",
        _ if started => "running",
        _ => "done",
    };
    ToolProgress {
        name,
        summary: truncate_str(&summary, 64),
        status,
        named,
    }
}

/// `end.sessionId` is the id `--resume` accepts. Also accept the
/// Claude-shaped `system`/`init` line in case a build emits one.
pub fn session_id_from_line(value: &Value) -> Option<&str> {
    let kind = value.get("type").and_then(|v| v.as_str())?;
    let id = if kind == "end" {
        value
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| value.get("session_id").and_then(|v| v.as_str()))
    } else if kind == "system" && value.get("subtype").and_then(|v| v.as_str()) == Some("init") {
        value.get("session_id").and_then(|v| v.as_str())
    } else {
        None
    }?;
    uuid::Uuid::parse_str(id).ok().map(|_| id)
}

/// Grok state directory. The default account follows absolute `GROK_HOME`;
/// every other account uses `~/.grok` inside its own home (profiles set
/// `GROK_HOME` to that directory so a daemon-level value cannot leak).
pub fn state_dir(home: &Path) -> PathBuf {
    if home == discovery::home_dir() {
        if let Ok(configured) = std::env::var("GROK_HOME") {
            let configured = PathBuf::from(configured.trim());
            if configured.is_absolute() {
                return configured;
            }
        }
    }
    home.join(".grok")
}

pub struct SessionFile {
    pub raw_id: String,
    pub title: String,
    pub workdir: String,
    pub model: String,
    pub updated_at: i64,
    pub updates: Option<PathBuf>,
    pub preview: String,
}

pub fn listings(home: &Path) -> Vec<SessionFile> {
    let root = state_dir(home).join("sessions");
    let Ok(groups) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for group in groups.flatten() {
        if !group.path().is_dir() {
            continue;
        }
        let Ok(sessions) = fs::read_dir(group.path()) else {
            continue;
        };
        for session in sessions.flatten() {
            if session.path().is_dir() {
                if let Some(file) = read_session(&session.path()) {
                    out.push(file);
                }
            }
        }
    }
    out.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
    out
}

fn read_session(dir: &Path) -> Option<SessionFile> {
    let summary_path = dir.join("summary.json");
    let text = fs::read_to_string(&summary_path).ok()?;
    let summary: Value = serde_json::from_str(&text).ok()?;
    let kind = summary
        .get("session_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if kind.starts_with("subagent") {
        return None;
    }
    let dir_id = dir.file_name()?.to_str()?;
    let info_id = summary
        .pointer("/info/id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let raw_id = if discovery::is_safe_session_id(info_id) {
        info_id.to_string()
    } else if discovery::is_safe_session_id(dir_id) {
        dir_id.to_string()
    } else {
        return None;
    };
    let updates = dir.join("updates.jsonl");
    let updates = updates.is_file().then_some(updates);
    let title = first_nonempty(&[
        summary.get("generated_title").and_then(|v| v.as_str()),
        summary.get("session_summary").and_then(|v| v.as_str()),
    ]);
    let title = if title.is_empty() {
        updates
            .as_ref()
            .and_then(|path| first_user_text(path))
            .map(|text| discovery::clean_title_from_prompt(&text))
            .unwrap_or_else(|| "Grok session".to_string())
    } else {
        title
    };
    let preview = first_nonempty(&[
        summary.get("last_turn_summary").and_then(|v| v.as_str()),
        summary.get("session_summary").and_then(|v| v.as_str()),
        Some(title.as_str()),
    ]);
    let workdir = summary
        .pointer("/info/cwd")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let model =
        json_string(summary.get("current_model_id")).unwrap_or_else(|| "default".to_string());
    let updated_at = json_time_ms(summary.get("updated_at"))
        .or_else(|| json_time_ms(summary.get("created_at")))
        .unwrap_or_else(|| file_mtime_ms(&summary_path));
    Some(SessionFile {
        raw_id,
        title,
        workdir,
        model,
        updated_at,
        updates,
        preview: truncate_str(preview.trim(), 90),
    })
}

fn first_nonempty(candidates: &[Option<&str>]) -> String {
    candidates
        .iter()
        .copied()
        .filter_map(|item| {
            let text = item?.trim();
            (!text.is_empty()).then(|| text.to_string())
        })
        .next()
        .unwrap_or_default()
}

fn json_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let object = value.as_object()?;
    for key in ["id", "model", "0"] {
        if let Some(text) = object.get(key).and_then(|v| v.as_str()) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn json_time_ms(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(n) = value.as_i64() {
        return Some(normalize_epoch_ms(n));
    }
    let text = value.as_str()?.trim();
    if let Ok(n) = text.parse::<i64>() {
        return Some(normalize_epoch_ms(n));
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn normalize_epoch_ms(n: i64) -> i64 {
    if n > 0 && n < 10_000_000_000 {
        n * 1000
    } else {
        n
    }
}

fn file_mtime_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn first_user_text(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(40).map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let update = update_body(&value);
        if session_kind(update) == "user_message_chunk" {
            let text = chunk_text(update).unwrap_or_default();
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    None
}

pub fn discover(
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
    profile: Option<&str>,
    home: &Path,
    limit: usize,
) -> Vec<ConversationSummary> {
    listings(home)
        .into_iter()
        .filter_map(|session| session_summary(session, profile, overlays, active_runner_ids))
        .take(limit)
        .collect()
}

pub fn session_summary_by_id(
    home: &Path,
    raw_id: &str,
    profile: Option<&str>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    listings(home)
        .into_iter()
        .find(|session| session.raw_id == raw_id)
        .and_then(|session| session_summary(session, profile, overlays, active_runner_ids))
}

fn session_summary(
    session: SessionFile,
    profile: Option<&str>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    active_runner_ids: &HashMap<String, String>,
) -> Option<ConversationSummary> {
    let id = profiles::make_conv_id("grok", profile, &session.raw_id);
    let (_, profile_label) = discovery::profile_display(profile);
    let ov = overlays.get(&id);
    let workdir = ov
        .and_then(|o| o.workdir.clone())
        .filter(|dir| !dir.is_empty())
        .or_else(|| {
            let dir = session.workdir.trim();
            (!dir.is_empty()).then(|| dir.to_string())
        })
        .unwrap_or_else(discovery::home_str);
    let title = ov
        .and_then(|o| o.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(session.title);
    let harness = ov
        .and_then(|o| o.harness.clone())
        .unwrap_or_else(|| "grok".to_string());
    let raw_model = ov.and_then(|o| o.model.clone()).unwrap_or(session.model);
    let (model, model_label) = crate::models::humanize_model_name(&harness, &raw_model);
    let effort = ov
        .and_then(|o| o.effort.clone())
        .unwrap_or_else(|| "default".to_string());
    let is_running = active_runner_ids.contains_key(&id);
    let updated_at = ov
        .map(|o| o.updated_at)
        .filter(|ts| *ts > 0)
        .unwrap_or(session.updated_at);
    let last_preview = ov
        .and_then(|o| o.appended_messages.last())
        .map(|message| truncate_str(message.text.trim(), 90))
        .filter(|text| !text.is_empty())
        .unwrap_or(session.preview);
    Some(ConversationSummary {
        id: id.clone(),
        raw_id: session.raw_id,
        title,
        subtitle: discovery::format_subtitle(&workdir),
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
        can_stop: false,
    })
}

pub fn messages(home: &Path, raw_id: &str) -> Vec<ChatMessage> {
    let Some(session) = listings(home)
        .into_iter()
        .find(|session| session.raw_id == raw_id)
    else {
        return Vec::new();
    };
    let Some(path) = session.updates else {
        return Vec::new();
    };
    parse_updates(&path)
}

/// ACP `updates.jsonl` (and a streaming-json line, if one was copied in).
/// One user turn per prompt, then one assistant turn until `turn_completed`.
pub fn parse_updates(path: &Path) -> Vec<ChatMessage> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    let mut assistant: Option<Draft> = None;
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let update = update_body(&value);
        let kind = session_kind(update);
        let timestamp = line_timestamp(&value);
        match kind.as_str() {
            "user_message_chunk" => {
                flush_assistant(&mut messages, &mut assistant);
                let text = chunk_text(update).unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                if let Some(last) = messages.last_mut() {
                    if last.role == "user" {
                        last.text.push_str(&text);
                        continue;
                    }
                }
                messages.push(chat_message(
                    "user",
                    &text,
                    None,
                    Vec::new(),
                    &timestamp,
                    messages.len(),
                ));
            }
            "agent_message_chunk" | "text" => {
                let draft = assistant.get_or_insert_with(|| Draft::new(timestamp.clone()));
                if let Some(text) = chunk_text(update).or_else(|| {
                    (kind == "text")
                        .then(|| update.get("data").and_then(|v| v.as_str()).unwrap_or(""))
                        .map(|s| s.to_string())
                }) {
                    draft.text.push_str(&text);
                }
            }
            "agent_thought_chunk" | "thought" => {
                let draft = assistant.get_or_insert_with(|| Draft::new(timestamp.clone()));
                if let Some(text) = chunk_text(update).or_else(|| {
                    update
                        .get("data")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                }) {
                    if !draft.thinking.is_empty() {
                        draft.thinking.push_str("\n\n");
                    }
                    draft.thinking.push_str(text.trim());
                }
            }
            "tool_call" => {
                let draft = assistant.get_or_insert_with(|| Draft::new(timestamp.clone()));
                let progress = tool_progress(update, true);
                draft.tools.push(ToolSummaryItem {
                    name: progress.name,
                    summary: progress.summary,
                    status: "running".to_string(),
                });
            }
            "tool_call_update" => {
                let progress = tool_progress(update, false);
                if let Some(draft) = assistant.as_mut() {
                    if let Some(tool) = draft.tools.iter_mut().rev().find(|t| t.status == "running")
                    {
                        tool.status = progress.status.to_string();
                        if progress.named && tool.name == "tool" {
                            tool.name = progress.name;
                            tool.summary = progress.summary;
                        }
                    }
                }
            }
            "turn_completed" | "end" => flush_assistant(&mut messages, &mut assistant),
            _ => {}
        }
    }
    flush_assistant(&mut messages, &mut assistant);
    messages
}

struct Draft {
    timestamp: String,
    text: String,
    thinking: String,
    tools: Vec<ToolSummaryItem>,
}

impl Draft {
    fn new(timestamp: String) -> Self {
        Self {
            timestamp,
            text: String::new(),
            thinking: String::new(),
            tools: Vec::new(),
        }
    }
}

fn flush_assistant(messages: &mut Vec<ChatMessage>, assistant: &mut Option<Draft>) {
    let Some(draft) = assistant.take() else {
        return;
    };
    if draft.text.trim().is_empty() && draft.thinking.trim().is_empty() && draft.tools.is_empty() {
        return;
    }
    let thinking = (!draft.thinking.trim().is_empty()).then_some(draft.thinking);
    messages.push(chat_message(
        "assistant",
        &draft.text,
        thinking,
        draft.tools,
        &draft.timestamp,
        messages.len(),
    ));
}

fn chat_message(
    role: &str,
    text: &str,
    thinking: Option<String>,
    tools: Vec<ToolSummaryItem>,
    timestamp: &str,
    index: usize,
) -> ChatMessage {
    ChatMessage {
        id: format!("grok-{role}-{index}"),
        role: role.to_string(),
        harness: "grok".to_string(),
        model: None,
        timestamp: timestamp.to_string(),
        text: text.to_string(),
        thinking,
        tools,
        images: Vec::new(),
        streaming: false,
        blocks: Vec::new(),
    }
}

fn update_body(value: &Value) -> &Value {
    value.pointer("/params/update").unwrap_or(value)
}

fn session_kind(update: &Value) -> String {
    update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn chunk_text(update: &Value) -> Option<String> {
    let content = update.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    content
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn line_timestamp(value: &Value) -> String {
    json_time_ms(value.get("timestamp"))
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
}

/// A live outside `grok` is mid-turn until `turn_completed` (or streaming `end`).
pub fn turn_open(tail: &str) -> bool {
    let mut open = false;
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let update = update_body(&value);
        match session_kind(update).as_str() {
            "turn_completed" | "end" => open = false,
            "user_message_chunk" | "tool_call" => open = true,
            _ => {}
        }
    }
    open
}

pub fn last_tool(tail: &str) -> Option<(String, String)> {
    let mut found = None;
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let update = update_body(&value);
        if session_kind(update) == "tool_call" {
            let progress = tool_progress(update, true);
            found = Some((progress.name, progress.summary));
        }
    }
    found
}

/// `grok models` prints one id per line, a JSON list of ids / `{id,name}`,
/// or the human catalog (`Default model:` plus `*` / `-` rows).
pub fn parse_model_list(output: &str) -> Vec<crate::models::ModelOption> {
    let trimmed = output.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            let mut models = Vec::new();
            collect_models(&value, &mut models, true);
            if !models.is_empty() {
                return models;
            }
        }
    }
    let lines = crate::settings::parse_native("grok", output);
    if !lines.is_empty() {
        return lines;
    }
    parse_human_catalog(output)
}

/// The installed CLI's catalog is prose, not one id per line:
/// `Default model: grok-4.7` and rows like `* grok-4.7 (default)`.
fn parse_human_catalog(output: &str) -> Vec<crate::models::ModelOption> {
    let mut models = Vec::new();
    for raw in output.lines() {
        let line = raw.trim();
        let id = if let Some(rest) = line.strip_prefix("Default model:") {
            rest.split_whitespace().next().unwrap_or("")
        } else if let Some(rest) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- ")) {
            rest.split_whitespace().next().unwrap_or("")
        } else {
            continue;
        };
        if crate::settings::valid_model_id(id) {
            push_model(&mut models, id, id);
        }
    }
    models
}

fn collect_models(value: &Value, out: &mut Vec<crate::models::ModelOption>, top: bool) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_models(item, out, false);
            }
        }
        Value::String(id) if crate::settings::valid_model_id(id) => push_model(out, id, id),
        Value::Object(map) if !top => {
            if let Some(id) = map
                .get("id")
                .and_then(|v| v.as_str())
                .or_else(|| map.get("model").and_then(|v| v.as_str()))
            {
                if crate::settings::valid_model_id(id) {
                    let label = map.get("name").and_then(|v| v.as_str()).unwrap_or(id);
                    push_model(out, id, label);
                }
            }
        }
        Value::Object(map) => {
            for key in ["models", "data"] {
                if let Some(child) = map.get(key) {
                    collect_models(child, out, false);
                }
            }
        }
        _ => {}
    }
}

fn push_model(out: &mut Vec<crate::models::ModelOption>, id: &str, label: &str) {
    if out.iter().any(|model| model.id == id) {
        return;
    }
    out.push(crate::models::ModelOption {
        id: id.to_string(),
        label: label.to_string(),
        default_effort: "default".to_string(),
        efforts: vec!["default".to_string()],
        base_url: None,
        api_key_env: None,
        thinking: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(safety: SafetyMode, model: &str, effort: &str, session: Option<&str>) -> Vec<String> {
        Turn {
            safety,
            model,
            effort,
            workdir: "/tmp/proj",
            session,
            prompt: "Reply SESSION_OK",
        }
        .args()
    }

    fn has_pair(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2)
            .any(|pair| pair[0] == left && pair[1] == right)
    }

    #[test]
    fn modes_are_read_only_auto_or_always_approve_and_never_carry_a_key() {
        let safe = turn(SafetyMode::Safe, "default", "default", None);
        assert!(has_pair(&safe, "--permission-mode", "dontAsk"));
        assert!(has_pair(&safe, "--sandbox", "read-only"));
        assert!(has_pair(&safe, "--tools", READ_ONLY_TOOLS));
        assert!(!safe
            .iter()
            .any(|arg| arg == "--always-approve" || arg == "--yolo"));
        assert!(has_pair(&safe, "--output-format", "streaming-json"));
        assert!(safe.iter().any(|arg| arg == "--no-auto-update"));
        assert!(has_pair(&safe, "--cwd", "/tmp/proj"));
        assert!(has_pair(&safe, "-p", "Reply SESSION_OK"));

        let standard = turn(SafetyMode::Standard, "grok-4.7", "high", Some("abc"));
        assert!(has_pair(&standard, "--permission-mode", "auto"));
        assert!(!standard.iter().any(|arg| arg == "--always-approve"));
        assert!(!standard.iter().any(|arg| arg == "--sandbox"));
        assert!(has_pair(&standard, "--model", "grok-4.7"));
        assert!(has_pair(&standard, "--effort", "high"));
        assert!(has_pair(&standard, "--resume", "abc"));
        assert!(!standard.iter().any(|arg| arg.contains("session-id")));
        assert!(!standard
            .iter()
            .any(|arg| arg == "--api-key" || arg.contains("XAI_API_KEY") || arg.contains("xai-")));

        let yolo = turn(SafetyMode::Yolo, "default", "ultra", None);
        assert!(yolo.iter().any(|arg| arg == "--always-approve"));
        assert!(!yolo.iter().any(|arg| arg == "--permission-mode"));
        assert!(!yolo.iter().any(|arg| arg == "--effort"));
    }

    #[test]
    fn stream_events_append_text_and_name_tools() {
        match stream_effect(&json!({"type":"text","data":"SESSION_"})).unwrap() {
            StreamEffect::Text(text) => assert_eq!(text, "SESSION_"),
            _ => panic!("text"),
        }
        match stream_effect(&json!({"type":"thought","data":"checking"})).unwrap() {
            StreamEffect::Thought(text) => assert_eq!(text, "checking"),
            _ => panic!("thought"),
        }
        match stream_effect(&json!({
            "type":"tool_call",
            "toolName":"read_file",
            "title":"Read",
            "status":"in_progress",
            "rawInput":{"path":"README.md"}
        }))
        .unwrap()
        {
            StreamEffect::Tool(tool) => {
                assert_eq!(tool.name, "read_file");
                assert_eq!(tool.summary, "README.md");
                assert_eq!(tool.status, "running");
                assert!(tool.named);
            }
            _ => panic!("tool"),
        }
        match stream_effect(
            &json!({"type":"tool_call_update","toolCallId":"t1","status":"completed"}),
        )
        .unwrap()
        {
            StreamEffect::Tool(tool) => {
                assert_eq!(tool.status, "done");
                assert!(!tool.named);
            }
            _ => panic!("update"),
        }
        assert!(stream_effect(&json!({"type":"tool_call","data":{"name":"shell"}})).is_none());
        assert_eq!(
            session_id_from_line(&json!({
                "type":"end",
                "stopReason":"end_turn",
                "sessionId":"c6b62c6f-7ead-4fd6-9922-e952131177ff"
            })),
            Some("c6b62c6f-7ead-4fd6-9922-e952131177ff")
        );
        assert_eq!(
            session_id_from_line(&json!({"type":"end","sessionId":"../escape"})),
            None
        );
    }

    #[test]
    fn listings_skip_subagents_and_messages_follow_the_transcript() {
        let home = std::env::temp_dir().join(format!("moedeck-grok-test-{}", uuid::Uuid::new_v4()));
        let sid = "11111111-1111-4111-8111-111111111111";
        let dir = home.join(".grok/sessions/%2Ftmp%2Fproj").join(sid);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("summary.json"),
            json!({
                "info": {"id": sid, "cwd": "/tmp/proj"},
                "generated_title": "Grok proof",
                "session_summary": "A saved chat",
                "last_turn_summary": "Disk answer",
                "current_model_id": "grok-4.7",
                "updated_at": "2026-11-01T00:00:00Z",
                "session_kind": "parent"
            })
            .to_string(),
        )
        .unwrap();
        let updates = [
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Disk prompt"}}},"timestamp":"2026-11-01T00:00:00Z"}).to_string(),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"looking"}}},"timestamp":"2026-11-01T00:00:01Z"}).to_string(),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolName":"read_file","rawInput":{"path":"README.md"},"status":"in_progress"}},"timestamp":"2026-11-01T00:00:02Z"}).to_string(),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","status":"completed"}},"timestamp":"2026-11-01T00:00:03Z"}).to_string(),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Disk answer"}}},"timestamp":"2026-11-01T00:00:04Z"}).to_string(),
            json!({"method":"session/update","params":{"update":{"sessionUpdate":"turn_completed"}},"timestamp":"2026-11-01T00:00:05Z"}).to_string(),
        ];
        fs::write(dir.join("updates.jsonl"), updates.join("\n")).unwrap();
        let sub = home.join(".grok/sessions/%2Ftmp%2Fproj/22222222-2222-4222-8222-222222222222");
        fs::create_dir_all(&sub).unwrap();
        fs::write(
            sub.join("summary.json"),
            r#"{"info":{"id":"22222222-2222-4222-8222-222222222222","cwd":"/tmp/proj"},"generated_title":"child","session_kind":"subagent"}"#,
        )
        .unwrap();

        let listed = listings(&home);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].raw_id, sid);
        assert_eq!(listed[0].title, "Grok proof");
        assert_eq!(listed[0].workdir, "/tmp/proj");
        assert_eq!(listed[0].model, "grok-4.7");
        assert_eq!(listed[0].preview, "Disk answer");
        let msgs = messages(&home, sid);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "Disk prompt");
        assert_eq!(msgs[1].text, "Disk answer");
        assert_eq!(msgs[1].thinking.as_deref(), Some("looking"));
        assert_eq!(msgs[1].tools.len(), 1);
        assert_eq!(msgs[1].tools[0].name, "read_file");
        assert_eq!(msgs[1].tools[0].status, "done");
        let tail = fs::read_to_string(dir.join("updates.jsonl")).unwrap();
        assert!(!turn_open(&tail));
        assert!(turn_open(&updates[..updates.len() - 1].join("\n")));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn model_list_accepts_lines_and_json() {
        let lines = parse_model_list("grok-4.7\nnot an id because of spaces\n");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].id, "grok-4.7");
        let json_list = parse_model_list(
            r#"{"models":[{"id":"grok-build","name":"Grok Build"},{"id":"grok-4.7","name":"Grok 4.7"}]}"#,
        );
        assert_eq!(json_list.len(), 2);
        assert_eq!(json_list[0].label, "Grok Build");
    }

    #[test]
    fn model_list_accepts_the_human_catalog() {
        let output = "\
You are logged in with grok.com.

Default model: grok-4.7

Available models:
  * grok-4.7 (default)
  - grok-4.7-build-fast
  - grok-4.6
  - grok-4.5
";
        let ids: Vec<_> = parse_model_list(output)
            .into_iter()
            .map(|model| model.id)
            .collect();
        assert_eq!(
            ids,
            ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]
        );
    }
}
