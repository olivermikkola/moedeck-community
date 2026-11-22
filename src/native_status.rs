//! Observe native Codex turns without claiming ownership of their processes.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn codex_session_running(raw_id: &str, home: &Path) -> bool {
    let root = crate::codex::state_dir(home).join("sessions");
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let suffix = format!("{raw_id}.jsonl");
    open_codex_rollouts().iter().any(|path| {
        path.starts_with(&root)
            && path.to_string_lossy().ends_with(&suffix)
            && codex_is_running(path)
    })
}

/// A transcript timestamp alone is not liveness: require an open transcript
/// in a Codex process and an unfinished turn. Never signal a native process.
pub fn codex_has_writer(path: &Path) -> bool {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    open_codex_rollouts().contains(&path)
}

pub fn codex_is_running(path: &Path) -> bool {
    if !codex_has_writer(path) {
        return false;
    }
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(1024 * 1024)));
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return false;
    }
    unfinished_turn(&String::from_utf8_lossy(&bytes))
}

fn unfinished_turn(tail: &str) -> bool {
    for line in tail.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"] != "event_msg" {
            continue;
        }
        match value["payload"]["type"].as_str() {
            Some("task_complete" | "turn_aborted" | "task_cancelled") => return false,
            Some("task_started") => return true,
            _ => {}
        }
    }
    // An open writer whose turn extends beyond the bounded tail has not
    // recorded completion in that tail. A closed writer never reaches here.
    true
}

#[cfg(target_os = "linux")]
fn open_codex_rollouts() -> std::collections::HashSet<PathBuf> {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    type Snapshot = Option<(Instant, HashSet<PathBuf>)>;
    static CACHE: OnceLock<Mutex<Snapshot>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some((at, paths)) = cache.as_ref() {
        if at.elapsed() < Duration::from_millis(100) {
            return paths.clone();
        }
    }
    let mut paths = HashSet::new();
    if let Ok(processes) = std::fs::read_dir("/proc") {
        for process in processes.flatten() {
            if !process
                .file_name()
                .to_string_lossy()
                .chars()
                .all(|c| c.is_ascii_digit())
            {
                continue;
            }
            let root = process.path();
            let name = std::fs::read_to_string(root.join("comm")).unwrap_or_default();
            if !name.trim().starts_with("codex") {
                continue;
            }
            if let Ok(fds) = std::fs::read_dir(root.join("fd")) {
                for fd in fds.flatten() {
                    if let Ok(path) = std::fs::read_link(fd.path()) {
                        if path
                            .file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with("rollout-"))
                        {
                            paths.insert(path);
                        }
                    }
                }
            }
        }
    }
    *cache = Some((Instant::now(), paths.clone()));
    paths
}

#[cfg(not(target_os = "linux"))]
fn open_codex_rollouts() -> std::collections::HashSet<PathBuf> {
    // No process-backed observation is available on this platform.
    std::collections::HashSet::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_and_abort_end_the_native_turn_even_with_later_usage_records() {
        let start = r#"{"type":"event_msg","payload":{"type":"task_started"}}"#;
        let done = r#"{"type":"event_msg","payload":{"type":"task_complete"}}"#;
        let abort = r#"{"type":"event_msg","payload":{"type":"turn_aborted"}}"#;
        assert!(unfinished_turn(start));
        assert!(!unfinished_turn(&format!(
            "{start}\n{done}\n{{\"type\":\"token_usage_record\"}}"
        )));
        assert!(!unfinished_turn(&format!("{start}\n{abort}")));
        assert!(unfinished_turn(&format!("{start}\n{done}\n{start}")));
    }
    #[test]
    fn an_unfinished_transcript_without_a_live_codex_writer_is_not_running() {
        assert!(!codex_is_running(Path::new("/missing/rollout-stale.jsonl")));
    }
}
