//! Codex owns model availability and per-model reasoning capabilities. Read its
//! local catalog rather than maintaining a second, inevitably stale model list.
use crate::discovery::truncate_str;
use crate::models::{ModelOption, ToolSummaryItem, TurnBlock};
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn cached_models() -> Vec<ModelOption> {
    let root = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::discovery::home_dir().join(".codex"));
    let catalog = std::fs::read(root.join("models_cache.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    models_from_catalog(catalog.as_ref())
}

fn models_from_catalog(catalog: Option<&Value>) -> Vec<ModelOption> {
    let mut models = vec![ModelOption {
        id: "default".into(),
        label: "Codex configured default".into(),
        default_effort: "default".into(),
        efforts: vec!["default".into()],
        base_url: None,
        api_key_env: None,
        thinking: None,
    }];
    if let Some(entries) = catalog
        .and_then(|v| v.get("models"))
        .and_then(Value::as_array)
    {
        for entry in entries {
            if entry.get("visibility").and_then(Value::as_str) != Some("list") {
                continue;
            }
            let Some(id) = entry
                .get("slug")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let mut efforts = vec!["default".to_string()];
            if let Some(levels) = entry
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
            {
                for level in levels {
                    if let Some(effort) = level.get("effort").and_then(Value::as_str) {
                        if !efforts.iter().any(|e| e == effort) {
                            efforts.push(effort.to_string());
                        }
                    }
                }
            }
            models.push(ModelOption {
                id: id.to_string(),
                label: entry
                    .get("display_name")
                    .and_then(Value::as_str)
                    .unwrap_or(id)
                    .to_string(),
                default_effort: "default".into(),
                efforts,
                base_url: None,
                api_key_env: None,
                thinking: None,
            });
        }
    }
    models
}

/// No override means Codex retains the user's configured reasoning effort.
pub fn effort_override(effort: &str) -> Option<&str> {
    (effort != "default" && !effort.is_empty()).then_some(effort)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_preserves_ids_and_model_specific_capabilities() {
        let catalog = json!({"models":[
            {"slug":"gpt-6.1-sol","display_name":"GPT-6.1 Sol","visibility":"list",
             "supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},
            {"slug":"gpt-6-luna","visibility":"list",
             "supported_reasoning_levels":[{"effort":"medium"}]},
            {"slug":"internal-review","visibility":"hide"},
            {"visibility":"list"}
        ]});
        let models = models_from_catalog(Some(&catalog));
        assert_eq!(models.len(), 3);
        assert_eq!(models[1].id, "gpt-6.1-sol");
        assert_eq!(models[1].efforts, ["default", "low", "ultra"]);
        assert_eq!(models[2].efforts, ["default", "medium"]);
    }

    #[test]
    fn absent_or_invalid_catalog_only_offers_configured_default() {
        assert_eq!(models_from_catalog(None).len(), 1);
        assert_eq!(models_from_catalog(Some(&json!({"models":false}))).len(), 1);
        assert_eq!(effort_override("default"), None);
        assert_eq!(effort_override(""), None);
        assert_eq!(effort_override("ultra"), Some("ultra"));
    }

    #[test]
    fn model_identity_is_never_rewritten() {
        for id in ["gpt-6.1-sol", "gpt-6.1-terra", "gpt-6-luna", "future-model"] {
            assert_eq!(crate::models::humanize_model_name("codex", id).0, id);
        }
    }
}

/// One MoE Deck turn drives a private `codex app-server` over JSON-RPC on stdio.
/// Unlike `codex exec`, it streams the reply as it is written, reports each
/// command, edit and search as it starts, and asks before leaving the sandbox.
pub struct Run<'a> {
    pub session: Option<&'a str>,
    pub model: &'a str,
    pub effort: &'a str,
    pub workdir: &'a str,
    pub images: &'a [String],
    pub prompt: &'a str,
    pub safety: crate::models::SafetyMode,
}

const INITIALIZE: i64 = 0;
const OPEN_THREAD: i64 = 1;
const START_TURN: i64 = 2;

impl Run<'_> {
    pub fn args() -> Vec<String> {
        vec!["app-server".into()]
    }

    /// Standard asks MoE Deck before leaving the workspace sandbox; Safe and YOLO
    /// never ask, they are already fully fenced or fully open.
    fn thread_params(&self) -> Value {
        use crate::models::SafetyMode;
        let (sandbox, approval) = match self.safety {
            SafetyMode::Yolo => ("danger-full-access", "never"),
            SafetyMode::Safe => ("read-only", "never"),
            SafetyMode::Standard => ("workspace-write", "on-request"),
        };
        let mut params =
            json!({"cwd": self.workdir, "sandbox": sandbox, "approvalPolicy": approval});
        if self.model != "default" && !self.model.is_empty() {
            params["model"] = self.model.into();
        }
        params
    }

    /// The first lines MoE Deck writes: handshake, then open (or resume) the thread.
    pub fn opening(&self) -> Vec<Value> {
        let open = match self.session {
            Some(id) => {
                let mut params = self.thread_params();
                params["threadId"] = id.into();
                request(OPEN_THREAD, "thread/resume", params)
            }
            None => request(OPEN_THREAD, "thread/start", self.thread_params()),
        };
        vec![
            request(
                INITIALIZE,
                "initialize",
                json!({"clientInfo": {"name": "moedeck", "version": env!("CARGO_PKG_VERSION")}}),
            ),
            json!({"method": "initialized"}),
            open,
        ]
    }

    fn start_turn(&self, thread_id: &str, prompt: &str) -> Value {
        let mut input = vec![json!({"type": "text", "text": prompt})];
        input.extend(
            self.images
                .iter()
                .map(|path| json!({"type": "localImage", "path": path})),
        );
        let mut params = json!({"threadId": thread_id, "input": input});
        if let Some(effort) = effort_override(self.effort) {
            params["effort"] = effort.into();
        }
        request(START_TURN, "turn/start", params)
    }
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"id": id, "method": method, "params": params})
}

/// What the runner must do after one line from the app-server.
#[derive(Debug, PartialEq)]
pub enum Action {
    Send(Value),
    /// A new native thread exists; remember it so the next turn resumes it.
    Adopt(String),
    /// Codex is blocked until MoE Deck answers request `id` with `answer(...)`.
    Approve {
        id: Value,
        tool: String,
        target: String,
        args: Value,
    },
    Fail(String),
    /// The turn is over; close stdin so the app-server exits.
    Finished,
}

pub fn answer(id: Value, allow: bool) -> Value {
    json!({"id": id, "result": {"decision": if allow { "accept" } else { "decline" }}})
}

/// Folds app-server events into MoE Deck's ordered turn timeline. Tool blocks and
/// `tools` stay in the same order, as `runner::live_blocks` expects.
#[derive(Default)]
pub struct Turn {
    items: HashMap<String, usize>,
    finished: bool,
}

impl Turn {
    pub fn on_line(
        &mut self,
        run: &Run,
        line: &str,
        blocks: &mut Vec<TurnBlock>,
        tools: &mut Vec<ToolSummaryItem>,
    ) -> Vec<Action> {
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            return vec![];
        };
        if self.finished {
            return vec![];
        }
        let method = msg["method"].as_str().unwrap_or("");
        if method.is_empty() {
            return self.on_response(run, &msg);
        }
        if !msg["id"].is_null() {
            return self.on_request(method, &msg);
        }
        let params = &msg["params"];
        match method {
            "item/started" => self.item(&params["item"], false, blocks, tools),
            "item/completed" => self.item(&params["item"], true, blocks, tools),
            "item/agentMessage/delta" => {
                let at = self.block(params, blocks, || TurnBlock::Text {
                    text: String::new(),
                });
                if let TurnBlock::Text { text } = &mut blocks[at] {
                    text.push_str(params["delta"].as_str().unwrap_or(""));
                }
            }
            "item/reasoning/summaryTextDelta" => {
                let at = self.block(params, blocks, || TurnBlock::Thinking {
                    text: String::new(),
                });
                if let TurnBlock::Thinking { text } = &mut blocks[at] {
                    text.push_str(params["delta"].as_str().unwrap_or(""));
                }
            }
            // A new summary paragraph; only matters once the first one has words.
            "item/reasoning/summaryPartAdded" => {
                if let Some(&at) = params["itemId"].as_str().and_then(|id| self.items.get(id)) {
                    if let TurnBlock::Thinking { text } = &mut blocks[at] {
                        if !text.is_empty() {
                            text.push_str("\n\n");
                        }
                    }
                }
            }
            "item/commandExecution/outputDelta" => {
                if let Some(&at) = params["itemId"].as_str().and_then(|id| self.items.get(id)) {
                    if let TurnBlock::Tool { output, .. } = &mut blocks[at] {
                        let so_far = output.get_or_insert_with(String::new);
                        if so_far.len() < crate::timeline::TOOL_IO_CAP {
                            so_far.push_str(params["delta"].as_str().unwrap_or(""));
                        }
                    }
                }
            }
            "error" if params["willRetry"] != true => {
                return vec![Action::Fail(error_message(&params["error"]))];
            }
            "turn/completed" => {
                self.finished = true;
                let turn = &params["turn"];
                return match turn["status"].as_str() {
                    Some("failed") => vec![
                        Action::Fail(error_message(&turn["error"])),
                        Action::Finished,
                    ],
                    Some("interrupted") => {
                        vec![
                            Action::Fail("Codex interrupted the turn.".into()),
                            Action::Finished,
                        ]
                    }
                    _ => vec![Action::Finished],
                };
            }
            _ => {}
        }
        vec![]
    }

    fn on_response(&mut self, run: &Run, msg: &Value) -> Vec<Action> {
        let id = msg["id"].as_i64();
        // A failed resume stays visible instead of silently starting over.
        if let Some(error) = msg.get("error") {
            self.finished = true;
            return vec![Action::Fail(error_message(error)), Action::Finished];
        }
        if id != Some(OPEN_THREAD) {
            return vec![];
        }
        let Some(thread) = msg["result"]["thread"]["id"].as_str() else {
            self.finished = true;
            return vec![
                Action::Fail("Codex opened a thread without an id.".into()),
                Action::Finished,
            ];
        };
        let mut actions = Vec::new();
        if run.session.is_none() {
            actions.push(Action::Adopt(thread.to_string()));
        }
        actions.push(Action::Send(run.start_turn(thread, run.prompt)));
        actions
    }

    fn on_request(&mut self, method: &str, msg: &Value) -> Vec<Action> {
        let params = &msg["params"];
        let (tool, target) = match method {
            "item/commandExecution/requestApproval" => (
                "Bash",
                shell_body(params["command"].as_str().unwrap_or("command")),
            ),
            "item/fileChange/requestApproval" => (
                "Edit",
                params["reason"]
                    .as_str()
                    .or_else(|| params["grantRoot"].as_str())
                    .unwrap_or("file changes")
                    .to_string(),
            ),
            // MoE Deck has no UI for these yet; refusing beats leaving Codex stuck.
            _ => {
                return vec![Action::Send(json!({"id": msg["id"], "error": {
                    "code": -32601, "message": format!("MoE Deck does not support {method}")
                }}))]
            }
        };
        vec![Action::Approve {
            id: msg["id"].clone(),
            tool: tool.into(),
            target,
            args: params.clone(),
        }]
    }

    fn block(
        &mut self,
        params: &Value,
        blocks: &mut Vec<TurnBlock>,
        new: impl FnOnce() -> TurnBlock,
    ) -> usize {
        let id = params["itemId"].as_str().unwrap_or("").to_string();
        *self.items.entry(id).or_insert_with(|| {
            blocks.push(new());
            blocks.len() - 1
        })
    }

    fn item(
        &mut self,
        item: &Value,
        done: bool,
        blocks: &mut Vec<TurnBlock>,
        tools: &mut Vec<ToolSummaryItem>,
    ) {
        let kind = item["type"].as_str().unwrap_or("");
        let params = json!({"itemId": item["id"]});
        match kind {
            "agentMessage" => {
                let at = self.block(&params, blocks, || TurnBlock::Text {
                    text: String::new(),
                });
                if let (true, Some(full), TurnBlock::Text { text }) =
                    (done, item["text"].as_str(), &mut blocks[at])
                {
                    *text = full.to_string();
                }
            }
            // Codex often reasons without a summary; only show thinking that has words.
            "reasoning" if done => {
                let summary = item["summary"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    })
                    .unwrap_or_default();
                if summary.trim().is_empty() {
                    return;
                }
                let at = self.block(&params, blocks, || TurnBlock::Thinking {
                    text: String::new(),
                });
                if let TurnBlock::Thinking { text } = &mut blocks[at] {
                    if text.trim().is_empty() {
                        *text = summary;
                    }
                }
            }
            "commandExecution" | "fileChange" | "mcpToolCall" | "webSearch" => {
                let (name, summary, input) = tool_label(item);
                let known = self.items.contains_key(item["id"].as_str().unwrap_or(""));
                let at = self.block(&params, blocks, || TurnBlock::Tool {
                    id: item["id"].as_str().map(str::to_string),
                    name: name.clone(),
                    summary: summary.clone(),
                    input,
                    output: None,
                    status: "running".into(),
                });
                if !known {
                    tools.push(ToolSummaryItem {
                        name: name.clone(),
                        summary: summary.clone(),
                        status: "running".into(),
                    });
                }
                if !done {
                    return;
                }
                let failed = matches!(item["status"].as_str(), Some("failed"))
                    || item["exitCode"].as_i64().is_some_and(|code| code != 0)
                    || !item["error"].is_null();
                let status = match item["status"].as_str() {
                    Some("declined") => "denied",
                    _ if failed => "error",
                    _ => "done",
                };
                let tool_at = blocks[..at]
                    .iter()
                    .filter(|b| matches!(b, TurnBlock::Tool { .. }))
                    .count();
                if let Some(tool) = tools.get_mut(tool_at) {
                    tool.status = status.into();
                }
                if let TurnBlock::Tool {
                    output, status: s, ..
                } = &mut blocks[at]
                {
                    *s = status.into();
                    let full = match kind {
                        "commandExecution" => item["aggregatedOutput"].as_str().map(str::to_string),
                        "fileChange" => item["changes"].as_array().map(|changes| {
                            changes
                                .iter()
                                .filter_map(|c| c["diff"].as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        }),
                        "mcpToolCall" => Some(
                            item["error"]["message"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| item["result"].to_string()),
                        ),
                        _ => None,
                    };
                    if let Some(full) = full.filter(|t| !t.trim().is_empty()) {
                        *output = Some(crate::timeline::tool_result_text(Some(&Value::String(
                            full,
                        ))));
                    }
                }
            }
            _ => {}
        }
    }
}

fn tool_label(item: &Value) -> (String, String, Option<String>) {
    let one_line = |s: &str| truncate_str(s.lines().next().unwrap_or(s), 72);
    match item["type"].as_str().unwrap_or("") {
        "commandExecution" => {
            let command = shell_body(item["command"].as_str().unwrap_or("command"));
            ("Bash".into(), one_line(&command), Some(command))
        }
        "fileChange" => {
            let paths: Vec<&str> = item["changes"]
                .as_array()
                .map(|c| c.iter().filter_map(|c| c["path"].as_str()).collect())
                .unwrap_or_default();
            ("Edit".into(), one_line(&paths.join(", ")), None)
        }
        "mcpToolCall" => {
            let name = format!(
                "{}.{}",
                item["server"].as_str().unwrap_or("mcp"),
                item["tool"].as_str().unwrap_or("tool")
            );
            let input = crate::timeline::tool_input_text(Some(&item["arguments"]));
            (name.clone(), name, input)
        }
        _ => {
            let query = item["query"].as_str().unwrap_or("search");
            ("WebSearch".into(), one_line(query), None)
        }
    }
}

/// Codex reports `/bin/bash -lc 'cargo test'`; the phone should say `cargo test`.
fn shell_body(command: &str) -> String {
    const SHELLS: [&str; 4] = [
        "/bin/bash -lc ",
        "bash -lc ",
        "/bin/zsh -lc ",
        "/bin/sh -c ",
    ];
    let Some(body) = SHELLS.iter().find_map(|shell| command.strip_prefix(shell)) else {
        return command.to_string();
    };
    let quoted = |q: char| body.len() >= 2 && body.starts_with(q) && body.ends_with(q);
    if quoted('\'') {
        body[1..body.len() - 1].replace(r"'\''", "'")
    } else if quoted('"') {
        body[1..body.len() - 1].replace(r#"\""#, "\"")
    } else {
        body.to_string()
    }
}

fn error_message(error: &Value) -> String {
    error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .unwrap_or("Codex reported that this turn failed.")
        .chars()
        .take(500)
        .collect()
}

/// An adopted native thread belongs to its original adapter, account and root.
pub fn binding_matches(
    overlay: &crate::models::StoredThreadOverlay,
    harness: &str,
    profile: Option<&str>,
    workdir: &str,
) -> bool {
    overlay.harness.as_deref() == Some(harness)
        && overlay.profile.as_deref() == profile
        && overlay.workdir.as_deref() == Some(workdir)
}

#[cfg(test)]
mod app_server_tests {
    use super::*;
    use crate::models::SafetyMode;

    const ID: &str = "12345678-1234-4234-8234-123456789abc";

    fn run<'a>(session: Option<&'a str>, safety: SafetyMode) -> Run<'a> {
        Run {
            session,
            model: "gpt-6.1-sol",
            effort: "high",
            workdir: "/tmp/project with spaces",
            images: &[],
            prompt: "--literal prompt",
            safety,
        }
    }

    fn feed(
        turn: &mut Turn,
        run: &Run,
        lines: &[Value],
    ) -> (Vec<Action>, Vec<TurnBlock>, Vec<ToolSummaryItem>) {
        let (mut blocks, mut tools, mut actions) = (vec![], vec![], vec![]);
        for line in lines {
            actions.extend(turn.on_line(run, &line.to_string(), &mut blocks, &mut tools));
        }
        (actions, blocks, tools)
    }

    #[test]
    fn opening_resumes_the_native_thread_with_policy_model_and_workdir() {
        let opening = run(Some(ID), SafetyMode::Standard).opening();
        assert_eq!(opening[0]["method"], "initialize");
        assert_eq!(opening[1], json!({"method": "initialized"}));
        let open = &opening[2]["params"];
        assert_eq!(opening[2]["method"], "thread/resume");
        assert_eq!(open["threadId"], ID);
        assert_eq!(open["cwd"], "/tmp/project with spaces");
        assert_eq!(open["sandbox"], "workspace-write");
        assert_eq!(open["approvalPolicy"], "on-request");
        assert_eq!(open["model"], "gpt-6.1-sol");
        let yolo = run(None, SafetyMode::Yolo).opening();
        assert_eq!(yolo[2]["method"], "thread/start");
        assert_eq!(yolo[2]["params"]["sandbox"], "danger-full-access");
        assert_eq!(yolo[2]["params"]["approvalPolicy"], "never");
        assert_eq!(
            run(None, SafetyMode::Safe).opening()[2]["params"]["sandbox"],
            "read-only"
        );
    }

    #[test]
    fn resumed_turn_sends_the_raw_prompt_effort_and_images_verbatim() {
        let images = ["/tmp/a picture.png".to_string()];
        let mut r = run(Some(ID), SafetyMode::Standard);
        r.images = &images;
        let (actions, ..) = feed(
            &mut Turn::default(),
            &r,
            &[json!({"id": 1, "result": {"thread": {"id": ID}}})],
        );
        let [Action::Send(turn)] = &actions[..] else {
            panic!("{actions:?}")
        };
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], ID);
        assert_eq!(turn["params"]["effort"], "high");
        assert_eq!(
            turn["params"]["input"],
            json!([{"type": "text", "text": "--literal prompt"},
                   {"type": "localImage", "path": "/tmp/a picture.png"}])
        );
    }

    #[test]
    fn a_thread_codex_cannot_resume_is_a_visible_failure_not_a_new_thread() {
        let (actions, ..) = feed(
            &mut Turn::default(),
            &run(Some(ID), SafetyMode::Standard),
            &[json!({"id": 1, "error": {"code": -32600, "message": "no rollout found"}})],
        );
        assert_eq!(
            actions,
            [Action::Fail("no rollout found".into()), Action::Finished]
        );
    }

    #[test]
    fn text_streams_in_order_with_tools_and_thinking_between() {
        let r = run(None, SafetyMode::Standard);
        let delta = |id: &str, d: &str| json!({"method": "item/agentMessage/delta", "params": {"itemId": id, "delta": d}});
        let cmd = json!({"type": "commandExecution", "id": "c1", "command": "cargo test", "status": "inProgress"});
        let (actions, blocks, tools) = feed(
            &mut Turn::default(),
            &r,
            &[
                json!({"method": "item/reasoning/summaryTextDelta", "params": {"itemId": "r1", "delta": "Checking tests"}}),
                delta("m1", "Running "),
                delta("m1", "the tests."),
                json!({"method": "item/started", "params": {"item": cmd}}),
                json!({"method": "item/commandExecution/outputDelta", "params": {"itemId": "c1", "delta": "ok\n"}}),
                json!({"method": "item/completed", "params": {"item": {"type": "commandExecution", "id": "c1",
                "command": "cargo test", "status": "completed", "exitCode": 0, "aggregatedOutput": "test result: ok"}}}),
                delta("m2", "All green."),
                json!({"method": "turn/completed", "params": {"turn": {"status": "completed"}}}),
                delta("m2", " late"),
            ],
        );
        assert_eq!(actions, [Action::Finished]);
        assert_eq!(
            blocks[0],
            TurnBlock::Thinking {
                text: "Checking tests".into()
            }
        );
        assert_eq!(
            blocks[1],
            TurnBlock::Text {
                text: "Running the tests.".into()
            }
        );
        assert!(
            matches!(&blocks[2], TurnBlock::Tool { name, status, output: Some(o), .. }
            if name == "Bash" && status == "done" && o == "test result: ok")
        );
        assert_eq!(
            blocks[3],
            TurnBlock::Text {
                text: "All green.".into()
            }
        );
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].status, "done");
    }

    #[test]
    fn commands_show_what_ran_not_the_shell_wrapper_and_empty_thinking_is_hidden() {
        assert_eq!(shell_body("/bin/bash -lc ls"), "ls");
        assert_eq!(
            shell_body("/bin/bash -lc 'cat Cargo.toml main.rs'"),
            "cat Cargo.toml main.rs"
        );
        assert_eq!(shell_body(r"/bin/bash -lc 'echo '\''hi'\'''"), "echo 'hi'");
        assert_eq!(
            shell_body(r#"/bin/bash -lc "rg -g 'A*' \"x\"""#),
            r#"rg -g 'A*' "x""#
        );
        assert_eq!(shell_body("cargo test"), "cargo test");
        let r = run(None, SafetyMode::Standard);
        let reasoning = |method: &str, id: &str, summary: Value| json!({"method": method, "params": {"item": {"type": "reasoning", "id": id, "summary": summary}}});
        let (_, blocks, _) = feed(
            &mut Turn::default(),
            &r,
            &[
                reasoning("item/started", "r1", json!([])),
                json!({"method": "item/reasoning/summaryPartAdded", "params": {"itemId": "r1"}}),
                reasoning("item/completed", "r1", json!([])),
                reasoning("item/completed", "r2", json!(["Plan"])),
            ],
        );
        assert_eq!(
            blocks,
            [TurnBlock::Thinking {
                text: "Plan".into()
            }]
        );
    }

    #[test]
    fn approvals_failures_and_unknown_requests_are_answered() {
        let r = run(None, SafetyMode::Standard);
        let (actions, ..) = feed(
            &mut Turn::default(),
            &r,
            &[
                json!({"id": 7, "method": "item/commandExecution/requestApproval", "params": {"command": "/bin/bash -lc 'curl example.com'", "reason": "network"}}),
                json!({"id": 8, "method": "item/tool/requestUserInput", "params": {}}),
                json!({"method": "error", "params": {"error": {"message": "reconnecting"}, "willRetry": true}}),
                json!({"method": "turn/completed", "params": {"turn": {"status": "failed", "error": {"message": "quota exhausted"}}}}),
            ],
        );
        assert!(
            matches!(&actions[0], Action::Approve { id, tool, target, .. }
            if *id == json!(7) && tool == "Bash" && target == "curl example.com")
        );
        assert!(matches!(&actions[1], Action::Send(v) if v["id"] == 8 && v["error"].is_object()));
        assert_eq!(
            actions[2..],
            [Action::Fail("quota exhausted".into()), Action::Finished]
        );
        assert_eq!(
            answer(json!(7), true),
            json!({"id": 7, "result": {"decision": "accept"}})
        );
        assert_eq!(answer(json!(7), false)["result"]["decision"], "decline");
    }

    #[test]
    fn adopted_thread_is_invalidated_by_account_harness_or_workspace_changes() {
        let overlay: crate::models::StoredThreadOverlay=serde_json::from_value(serde_json::json!({"id":"helm:test","harness":"codex","profile":"codex-2","workdir":"/tmp/project","native_session":"old","pinned":false,"created_at":0,"updated_at":0,"appended_messages":[]})).unwrap();
        assert!(binding_matches(
            &overlay,
            "codex",
            Some("codex-2"),
            "/tmp/project"
        ));
        assert!(!binding_matches(
            &overlay,
            "agy",
            Some("codex-2"),
            "/tmp/project"
        ));
        assert!(!binding_matches(&overlay, "codex", None, "/tmp/project"));
        assert!(!binding_matches(
            &overlay,
            "codex",
            Some("codex-2"),
            "/tmp/other"
        ));
    }
}
