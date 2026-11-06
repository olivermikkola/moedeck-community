//! Multi-account support for harnesses.
//!
//! MoE Deck normally drives each harness CLI (`agy`, `claude`, …) with the one
//! Google/Claude/… account baked into the user's real `~/.gemini`, `~/.claude`,
//! etc. This module lets a single MoE Deck daemon run *several* accounts of the
//! *same* harness in parallel by giving extra accounts their own "shadow home":
//! a directory that mirrors the real `$HOME` via symlinks but keeps the
//! harness's credential/state paths as private, real directories. Launching the
//! CLI with `HOME=<shadow>` gives it an isolated, independently-authenticated
//! profile while still seeing things like `~/.gitconfig` and `~/Projects`.
//!
//! The *default* account is simply the real home — profiles never touch it.

use crate::discovery::{atomic_write, config_dir, home_dir};
use crate::models::resolve_bin_path;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A registered extra account for one harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountProfile {
    /// Stable id like `agy-2`. Also usable as a conversation-id segment.
    pub id: String,
    pub harness: String,
    pub label: String,
    pub created_at: i64,
}

/// Serialisable profile view returned by the HTTP API.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileItem {
    pub id: String,
    pub harness: String,
    pub label: String,
    pub home: String,
    pub logged_in: bool,
}

fn registry_path() -> PathBuf {
    config_dir().join("profiles.json")
}

pub fn profiles_base_dir() -> PathBuf {
    config_dir().join("profiles")
}

/// Profile ids are deliberately tiny: lowercase alphanumerics and dashes only.
/// They end up inside conversation ids and filesystem paths.
pub fn sanitize_profile_id(raw: &str) -> Option<String> {
    let id = raw.trim().to_lowercase();
    if id.is_empty()
        || id.len() > 48
        || !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    Some(id)
}

/// Harness-relative paths that must be isolated (private) per account.
/// Everything else in `$HOME` is symlinked through unchanged.
pub fn isolation_paths(harness: &str) -> &'static [&'static str] {
    match harness {
        "claude" => &[".claude", ".claude.json"],
        "agy" => &[".gemini", ".antigravity", ".antigravity-ide"],
        "codex" => &[".codex"],
        "opencode" => &[
            ".local/share/opencode",
            ".local/state/opencode",
            ".config/opencode",
        ],
        "keel" => &[".keel"],
        _ => &[".helm-unknown-harness"],
    }
}

pub(crate) fn is_harness(harness: &str) -> bool {
    matches!(harness, "claude" | "agy" | "codex" | "opencode" | "keel")
}

/// Load the profile registry. A missing/corrupt file yields an empty list.
pub fn load_profiles() -> Vec<AccountProfile> {
    let path = registry_path();
    let Ok(content) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<AccountProfile>>(&content).unwrap_or_default()
}

pub fn get_profile(id: &str) -> Option<AccountProfile> {
    let id = sanitize_profile_id(id)?;
    load_profiles().into_iter().find(|p| p.id == id)
}

pub fn profiles_for_harness(harness: &str) -> Vec<AccountProfile> {
    load_profiles()
        .into_iter()
        .filter(|p| p.harness == harness)
        .collect()
}

fn save_profiles(profiles: &[AccountProfile]) -> Result<(), String> {
    let json = serde_json::to_string_pretty(profiles)
        .map_err(|e| format!("Failed to serialise profiles: {e}"))?;
    atomic_write(&registry_path(), &json).map_err(|e| format!("Failed to save profiles: {e}"))
}

/// Shadow-home directory for a profile id (does not create it).
pub fn profile_home(id: &str) -> PathBuf {
    profiles_base_dir().join(id).join("home")
}

/// Every home to scan for a harness: the real home (default account) first,
/// then each registered profile's shadow home. Returns `(profile_id, home)`;
/// `None` id means the default account.
pub fn harness_homes(harness: &str) -> Vec<(Option<String>, PathBuf)> {
    let mut out = vec![(None, home_dir())];
    for p in profiles_for_harness(harness) {
        let home = profile_home(&p.id);
        if home.is_dir() {
            out.push((Some(p.id), home));
        }
    }
    out
}

/// True when `rel` is an isolated leaf path for this harness.
fn is_isolated_leaf(harness: &str, rel: &Path) -> bool {
    isolation_paths(harness)
        .iter()
        .any(|iso| Path::new(iso) == rel)
}

/// Build (or incrementally refresh) the shadow home for `harness`: symlink
/// every entry of `real_home` into `shadow_home` except the account-state
/// paths in `isolation_paths`, which stay as private real directories.
fn link_tree(real_home: &Path, shadow_home: &Path, harness: &str) -> io::Result<()> {
    fs::create_dir_all(shadow_home)?;
    let mut need_real_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for iso in isolation_paths(harness) {
        let rel = Path::new(iso);
        // Ancestors of the isolated leaf are real dirs in the shadow home.
        for anc in rel.ancestors().skip(1) {
            if !anc.as_os_str().is_empty() {
                need_real_dirs.insert(anc.to_path_buf());
            }
        }
        let leaf = shadow_home.join(rel);
        if leaf.symlink_metadata().is_ok() {
            continue;
        }
        match fs::symlink_metadata(real_home.join(rel)) {
            // A file-typed isolated leaf (e.g. `.claude.json`): leave it
            // absent so the CLI recreates it privately on first run.
            Ok(md) if md.is_file() => {}
            _ => {
                fs::create_dir_all(&leaf)?;
            }
        }
    }

    // Walk real home top-level (and the inside of each real shadow dir) and
    // symlink everything that is neither isolated nor an isolated ancestor.
    let mut queue: Vec<PathBuf> = vec![PathBuf::new()]; // "" = home root
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    while let Some(rel) = queue.pop() {
        if !visited.insert(rel.clone()) {
            continue;
        }
        let real_dir = if rel.as_os_str().is_empty() {
            real_home.to_path_buf()
        } else {
            real_home.join(&rel)
        };
        let Ok(entries) = fs::read_dir(&real_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let child_rel = if rel.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                rel.join(&name)
            };
            if child_rel.starts_with(".config/moedeck") || child_rel.starts_with(".config/helm") {
                // Never mirror MoE Deck's own config (either name) into the shadow home.
                continue;
            }
            let shadow_child = shadow_home.join(&child_rel);
            if shadow_child.symlink_metadata().is_ok() {
                // Already present (real dir we made, or link from an earlier pass).
                if need_real_dirs.contains(&child_rel) {
                    queue.push(child_rel);
                }
                continue;
            }
            if need_real_dirs.contains(&child_rel) {
                fs::create_dir_all(&shadow_child)?;
                queue.push(child_rel);
            } else if !is_isolated_leaf(harness, &child_rel) {
                let real_child = real_home.join(&child_rel);
                #[cfg(unix)]
                std::os::unix::fs::symlink(&real_child, &shadow_child)?;
                #[cfg(not(unix))]
                let _ = real_child; // symlinks are unix-only; shadow home is a no-op elsewhere
            }
        }
        // Real dirs we created may still need children from a *missing* real
        // counterpart (e.g. `.gemini` does not exist yet) — create_dir_all on
        // the isolated leaf already covered that.
    }
    Ok(())
}

/// Create (or refresh) the shadow home for `harness`/`id`.
pub fn ensure_shadow_home(harness: &str, id: &str) -> Result<PathBuf, String> {
    let id = sanitize_profile_id(id).ok_or("Invalid profile id")?;
    if !is_harness(harness) {
        return Err(format!("Unknown harness `{harness}`"));
    }
    let home = profile_home(&id);
    let real = home_dir();
    link_tree(&real, &home, harness).map_err(|e| format!("Failed to build shadow home: {e}"))?;
    Ok(home)
}

/// Compute the env-var overrides a profiled child process needs.
pub fn child_env_overrides(harness: &str, id: &str) -> Vec<(String, String)> {
    let home = profile_home(id);
    let home_str = home.to_string_lossy().to_string();
    let mut env = vec![("HOME".to_string(), home_str.clone())];

    // Mirror XDG locations that live under the real home so apps don't escape
    // the shadow tree when the daemon environment sets them explicitly.
    for (var, _rel) in [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
    ] {
        if let Ok(cur) = std::env::var(var) {
            if let Ok(rest) = Path::new(&cur).strip_prefix(home_dir()) {
                let mapped = home.join(rest);
                env.push((var.to_string(), mapped.to_string_lossy().to_string()));
            } else {
                env.push((var.to_string(), cur));
            }
        }
    }

    // Native per-profile hooks for CLIs that honour their own config-dir env.
    match harness {
        "claude" => env.push((
            "CLAUDE_CONFIG_DIR".to_string(),
            home.join(".claude").to_string_lossy().to_string(),
        )),
        "codex" => env.push((
            "CODEX_HOME".to_string(),
            home.join(".codex").to_string_lossy().to_string(),
        )),
        // agy reads its login from the desktop keyring before its own token
        // file, and the keyring is shared by every HOME. Hide the session bus
        // so it falls back to the token file inside the shadow home;
        // otherwise every agy profile signs in as the default account.
        "agy" => env.push((
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            NO_KEYRING_BUS.to_string(),
        )),
        _ => {}
    }
    env
}

/// A session-bus address that never connects, so keyring lookups fail fast.
const NO_KEYRING_BUS: &str = "unix:path=/nonexistent/helm-no-keyring";

/// Does this home look like it has live credentials for the harness?
pub fn home_looks_logged_in(harness: &str, home: &Path) -> bool {
    for rel in isolation_paths(harness) {
        let p = home.join(rel);
        let Ok(md) = fs::symlink_metadata(&p) else {
            continue;
        };
        if md.is_file() {
            // e.g. `.claude.json`
            if fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false) {
                return true;
            }
            continue;
        }
        if !md.is_dir() {
            continue;
        }
        if has_any_file(&p, 4) {
            return true;
        }
    }
    false
}

fn has_any_file(dir: &Path, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() {
            return true;
        }
        if p.is_dir() && has_any_file(&p, depth - 1) {
            return true;
        }
    }
    false
}

/// Public profile list for the API/bootstrap payload.
pub fn profile_items() -> Vec<ProfileItem> {
    load_profiles()
        .into_iter()
        .map(|p| {
            let home = profile_home(&p.id);
            ProfileItem {
                id: p.id,
                harness: p.harness.clone(),
                label: p.label,
                logged_in: home_looks_logged_in(&p.harness, &home),
                home: home.to_string_lossy().to_string(),
            }
        })
        .collect()
}

/// Register the next account for `harness` and build its shadow home.
pub fn add_profile(harness: &str) -> Result<AccountProfile, String> {
    if !is_harness(harness) {
        return Err(format!("Unknown harness `{harness}`"));
    }
    let existing: Vec<String> = load_profiles()
        .into_iter()
        .filter(|p| p.harness == harness)
        .map(|p| p.id)
        .collect();
    let mut n = 2usize;
    let id = loop {
        let cand = format!("{harness}-{n}");
        if !existing.contains(&cand) && get_profile(&cand).is_none() {
            break cand;
        }
        n += 1;
        if n > 99 {
            return Err("Too many profiles for this harness".to_string());
        }
    };
    ensure_shadow_home(harness, &id)?;
    let label = format!("Account #{n}");
    let profile = AccountProfile {
        id,
        harness: harness.to_string(),
        label,
        created_at: Utc::now().timestamp_millis(),
    };
    let mut all = load_profiles();
    all.push(profile.clone());
    save_profiles(&all)?;
    Ok(profile)
}

/// Remove a profile from the registry. The shadow home (and any sessions it
/// recorded) stays on disk unless `purge_data` is set, so nothing is silently
/// destroyed.
pub fn remove_profile(id: &str, purge_data: bool) -> Result<(), String> {
    let id = sanitize_profile_id(id).ok_or("Invalid profile id")?;
    let all = load_profiles();
    if !all.iter().any(|p| p.id == id) {
        return Err(format!("Profile `{id}` not found"));
    }
    save_profiles(
        &all.iter()
            .filter(|p| p.id != id)
            .cloned()
            .collect::<Vec<_>>(),
    )?;
    if purge_data {
        let _ = fs::remove_dir_all(profiles_base_dir().join(&id));
    }
    Ok(())
}

/// The argv used to log an account in from a real terminal:
/// `HOME=<shadow> <binary>`. Surfaced in the UI and used by `moedeck login`.
pub fn login_command(harness: &str, id: &str) -> String {
    let bin = resolve_bin_path(harness)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| harness.to_string());
    format!("HOME={} {}", profile_home(id).to_string_lossy(), bin)
}

/// `moedeck login <profile-id>`: exec the harness CLI interactively with the
/// profile's shadow home so its OAuth flow writes credentials there.
pub fn exec_profile_login(profile_id: &str) -> anyhow::Result<()> {
    let profile = get_profile(profile_id).ok_or_else(|| {
        anyhow::anyhow!("No profile `{profile_id}` — create one in the cockpit first (harness pill → + Add account)")
    })?;
    let home = ensure_shadow_home(&profile.harness, &profile.id).map_err(|e| anyhow::anyhow!(e))?;
    let bin = resolve_bin_path(&profile.harness)
        .ok_or_else(|| anyhow::anyhow!("`{}` binary not found on this machine", profile.harness))?;
    println!(
        "Logging `{}` into account [{}] — finish the sign-in flow in this terminal.",
        profile.harness, profile.id
    );
    println!("Credentials will be stored under {}", home.display());
    let status = std::process::Command::new(bin)
        .envs(child_env_overrides(&profile.harness, &profile.id))
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()?;
    println!("Login CLI exited with {status}.");
    Ok(())
}

/// Build conversation ids: default account → `agy:<raw>`, named profile →
/// `agy:<profile>:<raw>`.
pub fn make_conv_id(harness: &str, profile: Option<&str>, raw_id: &str) -> String {
    match profile {
        None | Some("") | Some("default") => format!("{harness}:{raw_id}"),
        Some(p) => format!("{harness}:{p}:{raw_id}"),
    }
}

/// Split a conversation id into `(harness, profile, raw_session_id)`. Ids
/// without a profile segment (and foreign `helm:*` thread ids) round-trip with
/// `profile == None`.
pub fn parse_conv_id(id: &str) -> (Option<String>, Option<String>, String) {
    let head = id.split(':').next().unwrap_or("");
    if !is_harness(head) {
        return (None, None, id.to_string());
    }
    // `<harness>:<profile>:<raw>` has 3 segments and harness session ids never
    // contain ':', so splitn(3) is safe.
    let segs: Vec<&str> = id.splitn(3, ':').collect();
    match segs.as_slice() {
        [h, raw] => (Some(h.to_string()), None, raw.to_string()),
        [h, mid, raw] => {
            // Only treat `mid` as a profile when it looks like one and `raw`
            // is non-empty; anything else keeps the legacy whole-rest form.
            if sanitize_profile_id(mid).is_some() && !raw.is_empty() {
                (Some(h.to_string()), Some(mid.to_string()), raw.to_string())
            } else {
                (Some(h.to_string()), None, format!("{mid}:{raw}"))
            }
        }
        _ => (Some(head.to_string()), None, String::new()),
    }
}

/// Registry sanity check used at daemon start (self-heals nothing, just logs).
pub fn registry_summary() -> String {
    let all = load_profiles();
    if all.is_empty() {
        return "none".to_string();
    }
    let mut per: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in &all {
        *per.entry(p.harness.clone()).or_default() += 1;
    }
    per.iter()
        .map(|(h, n)| format!("{h}×{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Remove stale entries whose JSON was hand-edited into nonsense.
pub fn prune_invalid_registry() {
    let path = registry_path();
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    let Ok(parsed) = serde_json::from_str::<Vec<Value>>(&content) else {
        return;
    };
    let valid: Vec<AccountProfile> = parsed
        .into_iter()
        .filter_map(|v| serde_json::from_value::<AccountProfile>(v).ok())
        .filter(|p| {
            is_harness(&p.harness) && sanitize_profile_id(&p.id).as_deref() == Some(p.id.as_str())
        })
        .collect();
    if serde_json::to_string_pretty(&valid)
        .map(|j| j != content)
        .unwrap_or(false)
    {
        let _ = atomic_write(
            &path,
            &serde_json::to_string_pretty(&valid).unwrap_or_default(),
        );
    }
}

/// Canonical profile id + human label for API/summary payloads.
pub fn profile_display(profile: Option<&str>) -> (Option<String>, String) {
    match profile.and_then(sanitize_profile_id) {
        None => (None, String::new()),
        Some(pid) => {
            let label = get_profile(&pid)
                .map(|p| p.label)
                .unwrap_or_else(|| format!("account `{pid}`"));
            (Some(pid), label)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conv_id_roundtrip() {
        assert_eq!(make_conv_id("agy", None, "abc123"), "agy:abc123");
        assert_eq!(make_conv_id("agy", Some("default"), "abc"), "agy:abc");
        assert_eq!(make_conv_id("agy", Some("agy-2"), "abc"), "agy:agy-2:abc");
        let (h, p, raw) = parse_conv_id("agy:agy-2:abc123");
        assert_eq!(h.as_deref(), Some("agy"));
        assert_eq!(p.as_deref(), Some("agy-2"));
        assert_eq!(raw, "abc123");
        let (h, p, raw) = parse_conv_id("agy:abc123");
        assert_eq!(h.as_deref(), Some("agy"));
        assert_eq!(p, None);
        assert_eq!(raw, "abc123");
        // helm threads and unknown prefixes are not harness ids
        let (h, _, _) = parse_conv_id("helm:1234");
        assert_eq!(h, None);
    }

    #[test]
    fn test_sanitize_profile_id() {
        assert_eq!(sanitize_profile_id("agy-2").as_deref(), Some("agy-2"));
        assert_eq!(sanitize_profile_id(" AGY-2 "), Some("AGY-2".to_lowercase()));
        assert_eq!(sanitize_profile_id("../evil"), None);
        assert_eq!(sanitize_profile_id("a:b"), None);
        assert_eq!(sanitize_profile_id(""), None);
    }

    #[test]
    fn test_agy_profile_hides_shared_keyring() {
        let env = child_env_overrides("agy", "agy-2");
        assert!(env
            .iter()
            .any(|(k, v)| k == "DBUS_SESSION_BUS_ADDRESS" && v == NO_KEYRING_BUS));
        let env = child_env_overrides("claude", "claude-2");
        assert!(!env.iter().any(|(k, _)| k == "DBUS_SESSION_BUS_ADDRESS"));
    }

    #[test]
    fn test_shadow_home_links_and_isolates() {
        let tmp = std::env::temp_dir().join(format!("helm-profile-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let real_home = tmp.join("realhome");
        let gemini = real_home.join(".gemini/antigravity-cli/brain/conv1");
        fs::create_dir_all(&gemini).unwrap();
        fs::write(real_home.join(".gitconfig"), "[user]\n").unwrap();
        fs::create_dir_all(real_home.join("Projects/demo")).unwrap();
        fs::create_dir_all(real_home.join(".local/share/something")).unwrap();

        // agy isolates .gemini entirely but must symlink the rest.
        let shadow = tmp.join("shadow");
        link_tree(&real_home, &shadow, "agy").unwrap();

        assert!(fs::read_link(shadow.join(".gitconfig")).is_ok());
        assert!(shadow.join("Projects/demo").exists());
        // .gemini is a real dir and contains no transcript from the default account
        let md = fs::symlink_metadata(shadow.join(".gemini")).unwrap();
        assert!(md.is_dir());
        assert!(!shadow.join(".gemini/antigravity-cli").exists());
        // .local stays reachable through a symlink (not isolated for agy)
        assert!(fs::read_link(shadow.join(".local")).is_ok());

        let _ = fs::remove_dir_all(&tmp);
    }
}
