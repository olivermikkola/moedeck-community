//! Cursor Agent CLI (`cursor-agent`) headless adapter.
//!
//! The installer symlinks one binary as both `agent` and `cursor-agent`.
//! MoE Deck looks up `cursor-agent` only, so some other program named `agent`
//! is never treated as Cursor. Non-interactive runs use print mode and
//! newline-delimited JSON:
//! <https://cursor.com/docs/cli/headless>.
//!
//! Sign in with `cursor-agent login`, or export `CURSOR_API_KEY` for the
//! service user. The key stays in the environment; it is never passed as an
//! argument.

use crate::discovery::truncate_str;
use crate::models::SafetyMode;
use serde_json::Value;

pub const BINARY: &str = "cursor-agent";

pub struct Turn<'a> {
    pub safety: SafetyMode,
    pub model: &'a str,
    pub workdir: &'a str,
    pub session: Option<&'a str>,
    pub prompt: &'a str,
}

impl Turn<'_> {
    /// Argv after the binary. Print mode does not pause for a y/n prompt, so
    /// Safe selects ask mode (no edits) and every other safety mode passes
    /// `--force`, the same split OpenCode uses with `--auto`. Deny rules in
    /// `~/.cursor/cli-config.json` still override `--force`.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--trust".to_string(),
            "--workspace".to_string(),
            self.workdir.to_string(),
        ];
        if self.safety == SafetyMode::Safe {
            args.push("--mode".to_string());
            args.push("ask".to_string());
        } else {
            args.push("--force".to_string());
        }
        if !self.model.is_empty() && self.model != "default" {
            args.push("--model".to_string());
            args.push(self.model.to_string());
        }
        if let Some(session) = self.session {
            args.push("--resume".to_string());
            args.push(session.to_string());
        }
        args.push(self.prompt.to_string());
        args
    }
}

pub struct ToolProgress {
    pub name: String,
    pub summary: String,
    pub status: &'static str,
}

/// `--stream-partial-output` repeats assistant text in a flush event that
/// carries `model_call_id`. That flush is not new text.
pub fn skip_duplicate_assistant(value: &Value) -> bool {
    value.get("type").and_then(|v| v.as_str()) == Some("assistant")
        && value.get("model_call_id").is_some()
}

/// Cursor's `tool_call` events (`readToolCall`, `writeToolCall`, …) are
/// progress. Print mode has already allowed or skipped the tool before the
/// line is printed, so this must not open a phone approval.
pub fn tool_progress(value: &Value) -> Option<ToolProgress> {
    if value.get("type").and_then(|v| v.as_str()) != Some("tool_call") {
        return None;
    }
    let subtype = value.get("subtype").and_then(|v| v.as_str())?;
    let call = value.get("tool_call")?.as_object()?;
    let (key, body) = call.iter().next()?;
    let name = tool_label(key, body);
    let args = body.get("args").or_else(|| body.get("arguments"));
    let summary = tool_summary(&name, args);
    let status = match subtype {
        "started" => "running",
        "completed" => completed_status(body),
        _ => return None,
    };
    Some(ToolProgress {
        name,
        summary,
        status,
    })
}

fn tool_label(key: &str, body: &Value) -> String {
    if key == "function" {
        if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    let stem = key.strip_suffix("ToolCall").unwrap_or(key);
    let mut chars = stem.chars();
    let Some(first) = chars.next() else {
        return "tool".to_string();
    };
    let mut name: String = first.to_uppercase().collect();
    name.extend(chars);
    name
}

fn tool_summary(name: &str, args: Option<&Value>) -> String {
    let Some(args) = args else {
        return name.to_string();
    };
    if let Some(text) = args.as_str() {
        let text = text.trim();
        if text.is_empty() {
            return name.to_string();
        }
        return truncate_str(text, 64);
    }
    let summary = args
        .get("path")
        .or_else(|| args.get("file_path"))
        .or_else(|| args.get("command"))
        .or_else(|| args.get("cmd"))
        .or_else(|| args.get("query"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if summary.is_empty() {
        name.to_string()
    } else {
        truncate_str(summary, 64)
    }
}

fn completed_status(body: &Value) -> &'static str {
    let result = &body["result"];
    if result.get("error").is_some() || result.get("failure").is_some() {
        "error"
    } else {
        "done"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn turn(safety: SafetyMode, model: &str, session: Option<&str>) -> Vec<String> {
        Turn {
            safety,
            model,
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
    fn safe_asks_and_other_modes_force_without_putting_a_key_on_argv() {
        let safe = turn(SafetyMode::Safe, "default", None);
        assert!(has_pair(&safe, "--mode", "ask"));
        assert!(!safe.iter().any(|arg| arg == "--force"));
        let standard = turn(SafetyMode::Standard, "composer-2", Some("abc"));
        assert!(standard.iter().any(|arg| arg == "--force"));
        assert!(has_pair(&standard, "--model", "composer-2"));
        assert!(has_pair(&standard, "--resume", "abc"));
        assert!(has_pair(&standard, "--workspace", "/tmp/proj"));
        assert!(standard.iter().any(|arg| arg == "--trust"));
        assert!(standard.iter().any(|arg| arg == "-p"));
        assert!(has_pair(&standard, "--output-format", "stream-json"));
        assert_eq!(
            standard.last().map(String::as_str),
            Some("Reply SESSION_OK")
        );
        assert!(!standard.iter().any(|arg| arg == "--api-key"));
        assert!(turn(SafetyMode::Yolo, "default", None).contains(&"--force".to_string()));
    }

    #[test]
    fn tool_events_name_the_call_and_skip_duplicate_text() {
        let started = tool_progress(&json!({
            "type": "tool_call",
            "subtype": "started",
            "tool_call": {"readToolCall": {"args": {"path": "README.md"}}}
        }))
        .unwrap();
        assert_eq!(started.name, "Read");
        assert_eq!(started.summary, "README.md");
        assert_eq!(started.status, "running");
        let failed = tool_progress(&json!({
            "type": "tool_call",
            "subtype": "completed",
            "tool_call": {"writeToolCall": {"args": {"path": "a.rs"}, "result": {"error": {"message": "denied"}}}}
        }))
        .unwrap();
        assert_eq!(failed.name, "Write");
        assert_eq!(failed.status, "error");
        assert!(tool_progress(&json!({"type": "tool_call", "data": {"name": "shell"}})).is_none());
        assert!(skip_duplicate_assistant(&json!({
            "type": "assistant",
            "model_call_id": "call-1",
            "message": {"content": [{"type": "text", "text": "dup"}]}
        })));
        assert!(!skip_duplicate_assistant(&json!({
            "type": "assistant",
            "message": {"content": [{"type": "text", "text": "real"}]}
        })));
    }
}
