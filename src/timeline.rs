//! Ordered turn timeline: text updates, thinking and tool calls as they
//! happened, shared by the transcript readers and the live runner.
use crate::discovery::truncate_str;
use crate::models::{ChatMessage, ToolSummaryItem, TurnBlock};
use serde_json::Value;

/// Cap for a tool's input or output. Enough to read a diff or test failure on
/// a phone without shipping megabytes on every poll.
pub const TOOL_IO_CAP: usize = 4000;

fn cap(text: &str) -> String {
    let total = text.chars().count();
    if total <= TOOL_IO_CAP {
        return text.to_string();
    }
    let head: String = text.chars().take(TOOL_IO_CAP).collect();
    format!("{head}\n… ({} more characters)", total - TOOL_IO_CAP)
}

/// One-line label for a Claude Code tool call.
pub fn claude_tool_summary(name: &str, input: Option<&Value>) -> String {
    input
        .and_then(|inp| {
            inp.get("description")
                .or_else(|| inp.get("command"))
                .or_else(|| inp.get("file_path"))
                .or_else(|| inp.get("pattern"))
                .or_else(|| inp.get("url"))
                .or_else(|| inp.get("query"))
                .or_else(|| inp.get("prompt"))
        })
        .and_then(|v| v.as_str())
        .map(|s| truncate_str(s.lines().next().unwrap_or(s), 72))
        .unwrap_or_else(|| name.to_string())
}

/// The full input a user wants to see when expanding a tool line.
pub fn tool_input_text(input: Option<&Value>) -> Option<String> {
    let input = input?;
    if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
        return Some(cap(cmd));
    }
    if input.as_object().is_some_and(|o| o.is_empty()) {
        return None;
    }
    serde_json::to_string_pretty(input).ok().map(|s| cap(&s))
}

/// Text of a `tool_result` content field (string or array of parts).
pub fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => cap(s.trim_end()),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for p in parts {
                match p.get("type").and_then(|v| v.as_str()) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(|v| v.as_str()) {
                            out.push(t.trim_end().to_string());
                        }
                    }
                    Some("image") => out.push("[image]".to_string()),
                    _ => {}
                }
            }
            cap(&out.join("\n"))
        }
        _ => String::new(),
    }
}

/// Append the blocks of one Claude `assistant` content array. Tool calls start
/// as `initial_status` ("done" for history, "running" for a live run).
pub fn push_claude_assistant(blocks: &mut Vec<TurnBlock>, content: &[Value], initial_status: &str) {
    for item in content {
        match item.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    if !t.trim().is_empty() {
                        blocks.push(TurnBlock::Text {
                            text: t.trim().to_string(),
                        });
                    }
                }
            }
            "thinking" => {
                if let Some(t) = item.get("thinking").and_then(|v| v.as_str()) {
                    if !t.trim().is_empty() {
                        blocks.push(TurnBlock::Thinking {
                            text: t.trim().to_string(),
                        });
                    }
                }
            }
            "tool_use" => {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Tool")
                    .to_string();
                let input = item.get("input");
                blocks.push(TurnBlock::Tool {
                    id: item.get("id").and_then(|v| v.as_str()).map(str::to_string),
                    summary: claude_tool_summary(&name, input),
                    input: tool_input_text(input),
                    output: None,
                    status: initial_status.to_string(),
                    name,
                });
            }
            _ => {}
        }
    }
}

/// Attach the `tool_result` parts of a Claude `user` content array to their
/// tool calls. Returns how many results matched.
pub fn apply_claude_tool_results(blocks: &mut [TurnBlock], content: &[Value]) -> usize {
    let mut matched = 0;
    for part in content {
        if part.get("type").and_then(|v| v.as_str()) != Some("tool_result") {
            continue;
        }
        let Some(use_id) = part.get("tool_use_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let is_error = part.get("is_error").and_then(|v| v.as_bool()) == Some(true);
        let text = tool_result_text(part.get("content"));
        for block in blocks.iter_mut().rev() {
            if let TurnBlock::Tool {
                id: Some(id),
                output,
                status,
                ..
            } = block
            {
                if id == use_id {
                    *output = Some(text.clone()).filter(|t| !t.is_empty());
                    *status = if is_error { "error" } else { "done" }.to_string();
                    matched += 1;
                    break;
                }
            }
        }
    }
    matched
}

/// All text blocks, in order. This is the message's plain `text`.
pub fn joined_text(blocks: &[TurnBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            TurnBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Fill the legacy `text`/`thinking`/`tools` slots from `blocks`, for clients
/// (TUI, notifications, handoff) that read only those.
pub fn sync_legacy(msg: &mut ChatMessage) {
    if msg.blocks.is_empty() {
        return;
    }
    msg.text = joined_text(&msg.blocks);
    let thinking: Vec<&str> = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            TurnBlock::Thinking { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    msg.thinking = (!thinking.is_empty()).then(|| thinking.join("\n\n"));
    msg.tools = msg
        .blocks
        .iter()
        .filter_map(|b| match b {
            TurnBlock::Tool {
                name,
                summary,
                status,
                ..
            } => Some(ToolSummaryItem {
                name: name.clone(),
                summary: summary.clone(),
                status: status.clone(),
            }),
            _ => None,
        })
        .collect();
}

/// Phones poll the tail every second while a turn runs. Keep full tool input
/// and output only for the newest `keep` assistant turns; the full-history
/// view still carries all of it.
pub fn trim_old_tool_io(messages: &mut [ChatMessage], keep: usize) {
    for m in messages
        .iter_mut()
        .rev()
        .filter(|m| m.role == "assistant")
        .skip(keep)
    {
        for b in &mut m.blocks {
            if let TurnBlock::Tool { input, output, .. } = b {
                *input = None;
                *output = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_order_and_attaches_results() {
        let mut blocks = Vec::new();
        push_claude_assistant(
            &mut blocks,
            json!([{"type":"text","text":"Let me check."},
                   {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test","description":"Run tests"}}])
            .as_array()
            .unwrap(),
            "running",
        );
        push_claude_assistant(
            &mut blocks,
            json!([{"type":"text","text":"Tests pass."}])
                .as_array()
                .unwrap(),
            "running",
        );
        let matched = apply_claude_tool_results(
            &mut blocks,
            json!([{"type":"tool_result","tool_use_id":"t1","is_error":true,
                    "content":[{"type":"text","text":"1 failed"}]}])
            .as_array()
            .unwrap(),
        );
        assert_eq!(matched, 1);
        assert_eq!(blocks.len(), 3);
        assert_eq!(
            blocks[1],
            TurnBlock::Tool {
                id: Some("t1".into()),
                name: "Bash".into(),
                summary: "Run tests".into(),
                input: Some("cargo test".into()),
                output: Some("1 failed".into()),
                status: "error".into(),
            }
        );
        assert_eq!(joined_text(&blocks), "Let me check.\n\nTests pass.");
    }

    #[test]
    fn caps_huge_output() {
        let big = "x".repeat(TOOL_IO_CAP + 10);
        let out = tool_result_text(Some(&Value::String(big)));
        assert!(out.ends_with("(10 more characters)"));
    }
}
