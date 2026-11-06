//! A turn's outcome comes from process/protocol state, never from answer wording.
//! Codex reports its outcome over JSON-RPC; see `codex::Action::Fail`.
use serde_json::Value;

#[derive(Default)]
pub struct Completion {
    pub stopped: bool,
    pub failure: Option<String>,
}
impl Completion {
    pub fn observe(&mut self, harness: &str, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let kind = value["type"]
            .as_str()
            .or_else(|| value["event"].as_str())
            .unwrap_or("");
        let failed = match harness {
            "agy" if value.get("event").is_some() => {
                kind == "error"
                    || (kind == "result"
                        && value["result"]["status"]
                            .as_str()
                            .is_some_and(|s| s != "SUCCESS"))
            }
            "claude" | "agy" => {
                kind == "result"
                    && (value["is_error"].as_bool() == Some(true)
                        || value["subtype"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("error")))
            }
            "keel" => kind == "done" && value["status"].as_str() == Some("error"),
            "opencode" => kind == "error",
            _ => false,
        };
        if !failed {
            return;
        }
        let message = value["error"]["message"]
            .as_str()
            .or_else(|| value["error"].as_str())
            .or_else(|| value["result"].as_str())
            .or_else(|| value["message"].as_str())
            .or_else(|| value["errors"][0].as_str())
            .or_else(|| value["result"]["error"].as_str())
            .or_else(|| value["result"]["response"].as_str())
            .unwrap_or("The harness reported that this turn failed.");
        self.failure = Some(message.chars().take(500).collect());
    }

    pub fn finish(
        &self,
        process_error: Option<String>,
        partial: String,
        stderr: &str,
    ) -> (bool, String) {
        let reason = if self.stopped {
            Some("Stopped by user.".into())
        } else {
            self.failure.clone().or(process_error)
        };
        let Some(reason) = reason else {
            return (
                true,
                if partial.trim().is_empty() {
                    "Done.".into()
                } else {
                    partial
                },
            );
        };
        let mut details = reason;
        if !self.stopped && !stderr.trim().is_empty() {
            details.push('\n');
            details.extend(stderr.trim().chars().take(500));
        }
        let text = if partial.trim().is_empty() {
            details
        } else {
            format!("{partial}{FAILURE_MARK}{details}")
        };
        (false, text)
    }
}

/// Drain stderr continuously so a chatty child cannot deadlock on a full pipe.
/// Keep only a bounded tail; a noisy command must not grow daemon memory forever.
pub async fn stderr_tail(mut stderr: impl tokio::io::AsyncRead + Unpin) -> String {
    use tokio::io::AsyncReadExt;
    const LIMIT: usize = 16 * 1024;
    let mut tail = Vec::with_capacity(LIMIT + 4096);
    let mut chunk = [0u8; 4096];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                tail.extend_from_slice(&chunk[..count]);
                if tail.len() > LIMIT {
                    tail.drain(..tail.len() - LIMIT);
                }
            }
        }
    }
    String::from_utf8_lossy(&tail).into_owned()
}

/// Separator `finish` puts between a partial answer and why the turn stopped.
pub const FAILURE_MARK: &str = "\n\n---\nTurn did not complete: ";

/// Rewrite the harness errors people actually hit (quota, usage limits, a
/// model server that is down) into one plain sentence, and drop raw JSON and
/// CLI noise. Only failure text is touched: the part after `FAILURE_MARK`, or
/// a whole message that starts with an error. Answers that merely quote an
/// error stay as written. `at` is when the error happened, for reset times.
pub fn readable_failure(harness: &str, text: &str, at: chrono::DateTime<chrono::Local>) -> String {
    let Some(i) = text.find(FAILURE_MARK) else {
        return clean_if_error(harness, text, at);
    };
    let head = clean_if_error(harness, &text[..i], at);
    let tail = clean(harness, &text[i + FAILURE_MARK.len()..], at);
    // A harness that printed its error as the answer and then failed with it.
    if head.trim() == tail.trim() {
        return tail;
    }
    format!("{head}{FAILURE_MARK}{tail}")
}

fn clean_if_error(harness: &str, text: &str, at: chrono::DateTime<chrono::Local>) -> String {
    let first = text.lines().next().unwrap_or("");
    let starts_with_error = first.starts_with("Process `")
        || first.starts_with("Error: ")
        || first.starts_with("error: ")
        || first.starts_with("AGY_ERROR:")
        || plain_error(harness, first, at).is_some();
    if starts_with_error {
        clean(harness, text, at)
    } else {
        text.to_string()
    }
}

fn clean(harness: &str, text: &str, at: chrono::DateTime<chrono::Local>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut rewritten = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("AGY_ERROR:") || trimmed == "Reading additional input from stdin..."
        {
            continue;
        }
        let line = match plain_error(harness, trimmed, at) {
            Some(plain) => {
                rewritten = true;
                plain
            }
            None => line.to_string(),
        };
        if line.trim().is_empty() || !out.contains(&line) {
            out.push(line);
        }
    }
    // "Process `agy` exited with exit status: 3." adds nothing once the cause is said.
    if rewritten {
        out.retain(|l| !(l.starts_with("Process `") && l.contains("exited with")));
    }
    out.join("\n").trim().to_string()
}

fn harness_name(harness: &str) -> &str {
    match harness {
        "claude" => "Claude",
        "agy" => "Antigravity",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        "keel" => "Keel",
        other => other,
    }
}

fn plain_error(harness: &str, line: &str, at: chrono::DateTime<chrono::Local>) -> Option<String> {
    let name = harness_name(harness);
    let lower = line.to_lowercase();
    // Antigravity: "Individual quota reached. … Resets in 135h21m56s."
    if lower.contains("quota reached") {
        let after = line.split("Resets in ").nth(1)?;
        let secs = parse_duration(after.split(['.', ' ']).next()?)?;
        let when = at + chrono::Duration::seconds(secs);
        return Some(format!(
            "{name} quota is used up. It resets in {} ({}).",
            human_duration(secs),
            when.format("%a %-d %b %H:%M")
        ));
    }
    // Codex: "You’ve hit your usage limit. Upgrade … or try again at Nov 10th, 2026 9:45 PM."
    if lower.contains("hit your usage limit") {
        let when = line
            .split("try again at ")
            .nth(1)?
            .trim()
            .trim_end_matches('.');
        return Some(format!("{name} usage limit reached. Try again at {when}."));
    }
    // Keel / OpenAI-compatible: "provider request failed: http://127.0.0.1:8085/v1/…: … Connection refused"
    if lower.contains("connection refused") {
        let url = line.split("request failed: ").nth(1)?.split(": ").next()?;
        let host = url.split("://").nth(1).unwrap_or(url).split('/').next()?;
        return Some(format!(
            "Can't reach the model server at {host} (connection refused). Is it running?"
        ));
    }
    None
}

/// "135h21m56s" → seconds.
fn parse_duration(text: &str) -> Option<i64> {
    let (mut total, mut number) = (0i64, String::new());
    for c in text.chars() {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        let n: i64 = number.parse().ok()?;
        number.clear();
        total += n * match c {
            'd' => 86_400,
            'h' => 3_600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    (number.is_empty() && total > 0).then_some(total)
}

fn human_duration(secs: i64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h) {
        (0, 0) => format!("{} min", m.max(1)),
        (0, _) => format!("{h} h {m} min"),
        _ => format!("{d} d {h} h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_failure_wins_over_partial_output_and_zero_exit() {
        for (harness, event) in [
            (
                "agy",
                r#"{"type":"result","is_error":true,"result":"quota exhausted"}"#,
            ),
            (
                "agy",
                r#"{"event":"result","result":{"status":"ERROR","response":"quota exhausted"}}"#,
            ),
            (
                "claude",
                r#"{"type":"result","subtype":"error_max_turns","errors":["quota exhausted"]}"#,
            ),
            (
                "keel",
                r#"{"type":"done","status":"error","error":"quota exhausted"}"#,
            ),
            (
                "opencode",
                r#"{"type":"error","error":{"message":"quota exhausted"}}"#,
            ),
        ] {
            let mut completion = Completion::default();
            completion.observe(harness, event);
            let (success, text) = completion.finish(None, "Partial answer".into(), "");
            assert!(!success, "{harness}");
            assert!(text.contains("Partial answer"));
            assert!(text.contains("quota exhausted"));
        }
    }
    #[test]
    fn wording_does_not_cancel_a_successful_turn_but_cancellation_does() {
        let text = "The earlier log says Stopped by user.";
        assert!(Completion::default().finish(None, text.into(), "").0);
        assert!(
            !Completion {
                stopped: true,
                failure: None
            }
            .finish(None, "Partial answer".into(), "")
            .0
        );
        let (success, text) = Completion::default().finish(
            Some("Process exited 1".into()),
            "Partial answer".into(),
            "compiler failed",
        );
        assert!(!success);
        assert!(text.contains("compiler failed"));
    }
    #[tokio::test]
    async fn noisy_stderr_is_drained_and_only_its_tail_retained() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(512);
        let producer = tokio::spawn(async move {
            writer.write_all(&vec![b'x'; 256 * 1024]).await.unwrap();
            writer.write_all(b"final error").await.unwrap();
        });
        let tail = tokio::time::timeout(std::time::Duration::from_secs(5), stderr_tail(reader))
            .await
            .unwrap();
        producer.await.unwrap();
        assert_eq!(tail.len(), 16 * 1024);
        assert!(tail.ends_with("final error"));
    }

    #[test]
    fn real_harness_errors_become_one_plain_sentence() {
        use chrono::TimeZone;
        let at = chrono::Local
            .with_ymd_and_hms(2026, 11, 6, 10, 0, 0)
            .unwrap();
        let agy = "Process `agy` exited with exit status: 3.\nerror: Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 135h21m56s.\nAGY_ERROR: {\"short_error\":\"RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 135h21m56s.\",\"error_code\":429}";
        assert_eq!(
            readable_failure("agy", agy, at),
            "Antigravity quota is used up. It resets in 5 d 15 h (Thu 12 Nov 01:21)."
        );
        let codex = "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at Nov 10th, 2026 9:45 PM.\nReading additional input from stdin...\nYou’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), visit https://chatgpt.com/settings/usage to purchase more credits or try again at Nov 10th, 2026 9:45 PM.";
        assert_eq!(
            readable_failure("codex", codex, at),
            "Codex usage limit reached. Try again at Nov 10th, 2026 9:45 PM."
        );
        let keel = "Partial answer\n\n---\nTurn did not complete: provider request failed: http://127.0.0.1:8085/v1/chat/completions: Connection Failed: Connect error: Connection refused (os error 111): Connection refused (os error 111)";
        assert_eq!(
            readable_failure("keel", keel, at),
            "Partial answer\n\n---\nTurn did not complete: Can't reach the model server at 127.0.0.1:8085 (connection refused). Is it running?"
        );
        let refused = "provider request failed: http://127.0.0.1:8085/v1/chat/completions: Connection Failed: Connect error: Connection refused (os error 111)";
        assert_eq!(
            readable_failure(
                "keel",
                &format!("Error: {refused}{FAILURE_MARK}{refused}"),
                at
            ),
            "Can't reach the model server at 127.0.0.1:8085 (connection refused). Is it running?"
        );
    }

    #[test]
    fn answers_that_quote_an_error_and_unknown_errors_are_left_alone() {
        let at = chrono::Local::now();
        let answer =
            "The AGY pane says \"Individual quota reached… Resets in 135h\", about 5½ days.";
        assert_eq!(readable_failure("claude", answer, at), answer);
        let unknown = "Process `opencode` exited with exit status: 1.";
        assert_eq!(readable_failure("opencode", unknown, at), unknown);
        assert_eq!(parse_duration("21m5s"), Some(1265));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(human_duration(1265), "21 min");
        assert_eq!(human_duration(3 * 3600 + 120), "3 h 2 min");
    }
}
