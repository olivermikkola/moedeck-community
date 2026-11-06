use crate::approval::{analyze_risk, ApprovalManager};
use crate::discovery::{
    clean_title_from_prompt, get_conversation_detail, home_dir, is_safe_session_id, load_overlays,
    overlay_native_session, sanitize_workdir, save_overlay, set_overlay_native_session,
    truncate_str,
};
use crate::models::{
    humanize_model_name, resolve_bin_path, ApprovalDecisionKind, ChatMessage, SafetyMode,
    SendMessageRequest, StoredThreadOverlay, ToolSummaryItem, TurnBlock,
};
use crate::profiles;
use chrono::Utc;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

/// Final outcome of an in-flight harness turn, delivered to callers that
/// requested a completion channel (e.g. the autonomous sprint engine).
#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub success: bool,
    pub text: String,
}

pub struct RunnerState {
    pub active_tasks: RwLock<HashMap<String, String>>,
    pub run_ids: RwLock<HashMap<String, Uuid>>,
    pub abort_handles: Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    pub child_stdins: Mutex<HashMap<String, Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>>>,
    pub approvals: Arc<ApprovalManager>,
    pub remote_controls: Arc<crate::claude_rc::RemoteControls>,
    /// Idempotency ledger for repeatable deliveries (offline queue replay etc.)
    seen_keys: Mutex<HashMap<String, i64>>,
    native_approval_runs: RwLock<HashMap<String, Uuid>>,
}

impl RunnerState {
    pub fn new() -> Arc<Self> {
        Self::with_approvals(ApprovalManager::new())
    }

    pub fn with_approvals(approvals: Arc<ApprovalManager>) -> Arc<Self> {
        Arc::new(Self {
            active_tasks: RwLock::new(HashMap::new()),
            run_ids: RwLock::new(HashMap::new()),
            abort_handles: Mutex::new(HashMap::new()),
            child_stdins: Mutex::new(HashMap::new()),
            approvals,
            remote_controls: Arc::new(crate::claude_rc::RemoteControls::default()),
            seen_keys: Mutex::new(HashMap::new()),
            native_approval_runs: RwLock::new(HashMap::new()),
        })
    }

    /// Dedupes retries of the same delivery key (offline queue replay).
    /// Registers the key and returns `true` when it has never been seen
    /// (within the retention window); `false` when it was already processed.
    pub async fn check_idempotency(&self, key: &str) -> bool {
        const RETENTION_MS: i64 = 30 * 60 * 1000; // 30 minutes
        let now = Utc::now().timestamp_millis();
        let mut seen = self.seen_keys.lock().await;
        seen.retain(|_, ts| now - *ts < RETENTION_MS);
        match seen.get(key) {
            Some(_) => false,
            None => {
                seen.insert(key.to_string(), now);
                true
            }
        }
    }

    pub async fn active_snapshot(&self) -> HashMap<String, String> {
        self.active_tasks.read().await.clone()
    }

    pub async fn stop_conversation(&self, conv_id: &str) -> bool {
        // Signal the stop before closing stdin: the Codex app-server exits
        // cleanly on EOF, which must not read as a finished turn.
        let stop = self.abort_handles.lock().await.remove(conv_id);
        let stopped = stop.map(|tx| tx.send(())).is_some();
        self.native_approval_runs.write().await.remove(conv_id);
        self.child_stdins.lock().await.remove(conv_id);
        self.approvals.clear_conversation(conv_id).await;
        if stopped {
            self.active_tasks.write().await.remove(conv_id);
            self.run_ids.write().await.remove(conv_id);
            return true;
        }
        false
    }

    pub async fn finish_run(&self, conv_id: &str, run_id: Uuid) {
        let current_id = self.run_ids.read().await.get(conv_id).copied();
        if current_id == Some(run_id) {
            self.native_approval_runs.write().await.remove(conv_id);
            self.run_ids.write().await.remove(conv_id);
            self.active_tasks.write().await.remove(conv_id);
            self.abort_handles.lock().await.remove(conv_id);
            self.child_stdins.lock().await.remove(conv_id);
            self.approvals.clear_conversation(conv_id).await;
        }
    }

    async fn native_approval_run(&self, conv_id: &str) -> Option<Uuid> {
        let registered = self
            .native_approval_runs
            .read()
            .await
            .get(conv_id)
            .copied()?;
        (self.run_ids.read().await.get(conv_id).copied() == Some(registered)).then_some(registered)
    }

    async fn send_stdin_for_run(
        &self,
        conv_id: &str,
        run_id: Uuid,
        text: &str,
    ) -> Result<(), String> {
        let runs = self.run_ids.read().await;
        if runs.get(conv_id).copied() != Some(run_id) {
            return Err("Approval belongs to an earlier run".into());
        }
        self.send_stdin(conv_id, text).await
    }

    pub async fn send_stdin(&self, conv_id: &str, text: &str) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let stdins = self.child_stdins.lock().await;
        if let Some(stdin_arc) = stdins.get(conv_id) {
            let mut guard = stdin_arc.lock().await;
            guard
                .write_all(text.as_bytes())
                .await
                .map_err(|e| format!("Failed to write to stdin: {}", e))?;
            guard
                .flush()
                .await
                .map_err(|e| format!("Failed to flush stdin: {}", e))?;
            Ok(())
        } else {
            Err(format!("No active stdin found for `{}`", conv_id))
        }
    }
}

pub fn sanitize_effort(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "default" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra" => {
            raw.trim().to_lowercase()
        }
        _ => "medium".to_string(),
    }
}

pub fn sanitize_harness(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "claude" | "agy" | "codex" | "opencode" | "keel" => raw.trim().to_lowercase(),
        _ => "claude".to_string(),
    }
}

/// Harnesses that need MoE Deck's piped stdin: the native approval broker writes
/// `y\n`/`n\n` there, transcript-driven CLIs read interactive answers from
/// it, and the Codex app-server speaks JSON-RPC over it. `opencode run` drains stdin to EOF
/// before doing any work when stdin is not a TTY — keeping that pipe open makes
/// every OpenCode turn hang with zero output.
fn harness_wants_stdin(harness: &str) -> bool {
    harness != "opencode"
}

/// Ensures a user prompt starting with `-` is never parsed as a CLI option flag.
pub fn safe_prompt_arg(prompt: &str) -> String {
    if prompt.starts_with('-') {
        format!(" {}", prompt)
    } else {
        prompt.to_string()
    }
}

pub async fn dispatch_turn(
    runner: Arc<RunnerState>,
    conv_id: String,
    req: SendMessageRequest,
) -> Result<ChatMessage, String> {
    dispatch_turn_with_outcome(runner, conv_id, req)
        .await
        .map(|(msg, _rx)| msg)
}

/// Same as `dispatch_turn`, but additionally returns a one-shot receiver that
/// resolves to the turn's final outcome when the spawned harness process
/// finishes (success = exit status ok and not user-stopped).
pub async fn dispatch_turn_with_outcome(
    runner: Arc<RunnerState>,
    conv_id: String,
    req: SendMessageRequest,
) -> Result<(ChatMessage, tokio::sync::oneshot::Receiver<TurnOutcome>), String> {
    if let Some(workdir) = &req.workdir {
        if sanitize_workdir(workdir).is_none() {
            return Err("Choose an existing project folder on this computer.".into());
        }
    }
    // Stop any previous in-flight turn on the same conversation before starting a new one
    let _ = runner.stop_conversation(&conv_id).await;

    let active_snapshot = runner.active_snapshot().await;
    let cid_for_detail = conv_id.clone();
    // Transcript crawling is heavy sync I/O — keep it off the async workers.
    let existing_detail = tokio::task::spawn_blocking(move || {
        get_conversation_detail(&cid_for_detail, &active_snapshot)
    })
    .await
    .unwrap_or(None);

    let mut overlays = load_overlays();
    let now_ms = Utc::now().timestamp_millis();
    let now_iso = Utc::now().to_rfc3339();

    let (base_harness, base_model, base_effort, base_workdir, session_busy) =
        if let Some(ref d) = existing_detail {
            (
                d.summary.harness.clone(),
                d.summary.model.clone(),
                d.summary.effort.clone(),
                d.summary.workdir.clone(),
                d.summary.status == "busy",
            )
        } else {
            (
                "claude".to_string(),
                "opus".to_string(),
                "medium".to_string(),
                crate::discovery::home_str(),
                false,
            )
        };

    let target_harness = sanitize_harness(&req.harness.clone().unwrap_or(base_harness));
    let safety_mode = runner.approvals.get_safety_mode().await;
    let opencode_config = if target_harness == "opencode" && safety_mode == SafetyMode::Safe {
        Some(opencode_safe_config(
            std::env::var("OPENCODE_CONFIG_CONTENT").ok().as_deref(),
        )?)
    } else {
        None
    };
    let raw_model = req.model.clone().unwrap_or(base_model);
    let (model_id, model_label) = humanize_model_name(&target_harness, &raw_model);
    let effort = sanitize_effort(&req.effort.clone().unwrap_or(base_effort));
    let raw_workdir = req.workdir.clone().unwrap_or(base_workdir.clone());
    let workdir = sanitize_workdir(&raw_workdir)
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| {
            "This conversation's project folder no longer exists. Choose a project to continue."
                .to_string()
        })?;

    // Multi-account: the conversation id may encode the owning account
    // (`agy:<profile>:<raw session>`), and the request can override it.
    let (conv_harness, conv_profile_seg, conv_raw_id) = profiles::parse_conv_id(&conv_id);
    let native_profile = if conv_harness.as_deref() == Some(target_harness.as_str()) {
        conv_profile_seg
    } else {
        None
    };

    let mut overlay = overlays
        .remove(&conv_id)
        .unwrap_or_else(|| StoredThreadOverlay {
            id: conv_id.clone(),
            title: None,
            pinned: false,
            harness: Some(target_harness.clone()),
            model: Some(model_id.clone()),
            effort: Some(effort.clone()),
            workdir: Some(workdir.clone()),
            profile: None,
            created_at: now_ms,
            updated_at: now_ms,
            appended_messages: Vec::new(),
            native_session: None,
        });

    // Which account runs this turn? A discovered session's id is authoritative
    // (its transcript physically lives in that account's home), then an
    // explicit pick from the cockpit, then whatever the thread was bound to.
    // A profile only sticks when it belongs to the *target* harness, so
    // mid-thread harness switches never carry a stale account over.
    let requested_profile = req
        .profile
        .as_deref()
        .and_then(profiles::sanitize_profile_id);
    let effective_profile = native_profile
        .or(requested_profile)
        .or_else(|| overlay.profile.clone())
        .filter(|pid| {
            profiles::get_profile(pid)
                .map(|p| p.harness == target_harness)
                .unwrap_or(false)
        });
    if !crate::codex::binding_matches(
        &overlay,
        &target_harness,
        effective_profile.as_deref(),
        &workdir,
    ) {
        overlay.native_session = None;
        runner.approvals.reset_authorizations(&conv_id).await;
    }
    overlay.harness = Some(target_harness.clone());
    overlay.model = Some(model_id.clone());
    overlay.effort = Some(effort.clone());
    overlay.workdir = Some(workdir.clone());
    overlay.updated_at = now_ms;

    overlay.profile = effective_profile.clone();

    // Build context handoff whenever switching harnesses or when the underlying session
    // may be locked by another live terminal/webapp process.
    let mut prompt_with_context = req.text.trim().to_string();
    if let Some(ref detail) = existing_detail {
        if !detail.messages.is_empty() {
            let mut ctx_lines = Vec::new();
            for m in detail.messages.iter().rev().take(6).rev() {
                if m.text.starts_with("Process `") || m.text == "Thinking…" || m.text == "Working…"
                {
                    continue;
                }
                let speaker = if m.role == "user" {
                    "User".to_string()
                } else {
                    format!("Assistant ({})", m.harness)
                };
                ctx_lines.push(format!("{}: {}", speaker, truncate_str(&m.text, 450)));
            }
            if !ctx_lines.is_empty() {
                prompt_with_context = format!(
                    "[Recent thread context]\n{}\n\n[User message]\n{}",
                    ctx_lines.join("\n---\n"),
                    req.text.trim()
                );
            }
        }
    }

    let mut display_images = Vec::new();
    let mut local_image_paths = Vec::new();
    for img in &req.images {
        display_images.push(img.clone());
        if let Some(local_p) = resolve_uploaded_image_path(img) {
            local_image_paths.push(local_p);
        }
    }
    if !local_image_paths.is_empty() {
        let joined = local_image_paths.join(", ");
        prompt_with_context = format!(
            "{}\n\n[Attached image file(s) on local disk: {}]",
            prompt_with_context, joined
        );
    }

    let user_msg = ChatMessage {
        id: format!("u-{}", Uuid::new_v4()),
        role: "user".to_string(),
        harness: target_harness.clone(),
        model: None,
        timestamp: now_iso.clone(),
        text: req.text.trim().to_string(),
        thinking: None,
        tools: Vec::new(),
        images: display_images,
        streaming: false,
        blocks: Vec::new(),
    };

    // Give a fresh thread a real name from its first prompt instead of
    // leaving every new thread called "New Thread". Slash commands are not
    // useful titles.
    let needs_title = overlay
        .title
        .as_deref()
        .map(|t| t.trim().is_empty() || t == "New Thread")
        .unwrap_or(true);
    if needs_title && !req.text.trim().is_empty() && !req.text.trim().starts_with('/') {
        overlay.title = Some(clean_title_from_prompt(&req.text));
    }

    // Final-outcome channel for listeners (e.g. the sprint engine). For the
    // `/new` short-circuit below no turns are dispatched, so the receiver
    // resolves only when a real turn task completes.
    let (outcome_tx_task, outcome_rx) = tokio::sync::oneshot::channel::<TurnOutcome>();

    // UI slash command: reset the thread's adopted native session (OpenCode
    // & friends). Handled here so it never reaches the agent as prompt text.
    if req.text.trim().eq_ignore_ascii_case("/new") {
        overlay.appended_messages.push(user_msg.clone());
        let now_reply_iso = Utc::now().to_rfc3339();
        overlay.appended_messages.push(ChatMessage {
            id: format!("a-{}", Uuid::new_v4()),
            role: "assistant".to_string(),
            harness: target_harness.clone(),
            model: Some(model_label.clone()),
            timestamp: now_reply_iso,
            text: handle_reset_command(&conv_id),
            thinking: None,
            tools: Vec::new(),
            images: Vec::new(),
            streaming: false,
            blocks: Vec::new(),
        });
        save_overlay(&overlay);
        return Ok((user_msg, outcome_rx));
    }

    let assistant_msg_id = format!("a-{}", Uuid::new_v4());
    let placeholder_assistant = ChatMessage {
        id: assistant_msg_id.clone(),
        role: "assistant".to_string(),
        harness: target_harness.clone(),
        model: Some(model_label.clone()),
        timestamp: now_iso,
        text: "Thinking…".to_string(),
        thinking: None,
        tools: Vec::new(),
        images: Vec::new(),
        streaming: true,
        blocks: Vec::new(),
    };

    overlay.appended_messages.push(user_msg.clone());
    overlay.appended_messages.push(placeholder_assistant);
    save_overlay(&overlay);

    let run_id = Uuid::new_v4();
    let (abort_tx, mut abort_rx) = tokio::sync::oneshot::channel::<()>();
    runner.run_ids.write().await.insert(conv_id.clone(), run_id);
    runner
        .abort_handles
        .lock()
        .await
        .insert(conv_id.clone(), abort_tx);
    let account_tag = effective_profile
        .as_deref()
        .map(|p| format!(" [{p}]"))
        .unwrap_or_default();
    runner.active_tasks.write().await.insert(
        conv_id.clone(),
        format!(
            "Running {} · {}{}",
            target_harness.to_uppercase(),
            model_label,
            account_tag
        ),
    );

    let runner_clone = runner.clone();
    let conv_id_clone = conv_id.clone();
    let raw_user_text = safe_prompt_arg(req.text.trim());
    let prompt_with_context = safe_prompt_arg(&prompt_with_context);

    // Resume native sessions in place when the thread id (or an adopted
    // overlay id) names one — profiled sessions carry the account in their
    // conversation id, so `<harness>:<profile>:<raw>` resumes under that
    // account's home.
    let native_raw = if conv_harness.as_deref() == Some(target_harness.as_str())
        && is_safe_session_id(&conv_raw_id)
    {
        Some(conv_raw_id.clone())
    } else {
        None
    };
    let claude_native = if target_harness == "claude" {
        native_raw
            .clone()
            .or_else(|| overlay_native_session(&conv_id))
            .filter(|id| Uuid::parse_str(id).is_ok())
    } else {
        None
    };
    let agy_native = if target_harness == "agy" {
        native_raw
            .clone()
            .or_else(|| overlay_native_session(&conv_id))
            .filter(|id| Uuid::parse_str(id).is_ok())
    } else {
        None
    };
    let keel_native = if target_harness == "keel" {
        native_raw.clone()
    } else {
        None
    };
    let opencode_native = if target_harness == "opencode" {
        native_raw
            .clone()
            .or_else(|| overlay_native_session(&conv_id).filter(|id| is_safe_session_id(id)))
    } else {
        None
    };

    let codex_native = if target_harness == "codex" && !session_busy && workdir == base_workdir {
        native_raw
            .clone()
            .or_else(|| overlay_native_session(&conv_id))
            .filter(|id| Uuid::parse_str(id).is_ok())
    } else {
        None
    };
    let codex_handoff = existing_detail
        .as_ref()
        .is_some_and(|d| d.summary.harness != target_harness);

    tokio::spawn(async move {
        let mut outcome_tx = Some(outcome_tx_task);
        // The daemon's own home builds the enriched PATH (absolute); the child
        // process is re-rooted to its account's shadow home below.
        let real_home = home_dir().to_string_lossy().to_string();
        let enriched_path = format!(
            "{}/.local/bin:{}/.npm-global/bin:{}/.gemini/antigravity-cli/bin:/usr/local/bin:/usr/bin:/bin",
            real_home, real_home, real_home
        );
        let account_home = effective_profile
            .as_ref()
            .map(|pid| profiles::profile_home(pid))
            .unwrap_or_else(home_dir);
        let profile_env: Vec<(String, String)> = match &effective_profile {
            Some(pid) => profiles::child_env_overrides(&target_harness, pid),
            None => Vec::new(),
        };

        let bin_str = |name: &str| -> String {
            resolve_bin_path(name)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| name.to_string())
        };

        let codex_run = crate::codex::Run {
            session: codex_native.as_deref(),
            model: &model_id,
            effort: &effort,
            workdir: &workdir,
            images: &local_image_paths,
            prompt: if codex_native.is_some() && !codex_handoff {
                &raw_user_text
            } else {
                &prompt_with_context
            },
            safety: safety_mode,
        };
        let mut codex_turn = crate::codex::Turn::default();

        let mut cmd = match target_harness.as_str() {
            "claude" => {
                let mut c = Command::new(bin_str("claude"));
                c.arg("--output-format").arg("stream-json").arg("--verbose");
                if safety_mode == SafetyMode::Yolo {
                    c.arg("--dangerously-skip-permissions");
                }
                if model_id != "default" {
                    c.arg("--model").arg(&model_id);
                }
                if effort != "default" {
                    c.arg("--effort").arg(&effort);
                }
                if !session_busy {
                    if let Some(raw_id) = &claude_native {
                        c.arg("--resume").arg(raw_id);
                        c.arg("-p").arg(&raw_user_text);
                    } else {
                        c.arg("-p").arg(&prompt_with_context);
                    }
                } else {
                    c.arg("-p").arg(&prompt_with_context);
                }
                c
            }
            "agy" => {
                let mut c = Command::new(bin_str("agy"));
                c.arg("--output-format").arg("stream-json");
                if safety_mode == SafetyMode::Yolo {
                    c.arg("--dangerously-skip-permissions");
                }
                if model_id != "default" {
                    c.arg("--model").arg(&model_id);
                }
                if effort != "default" {
                    c.arg("--effort").arg(&effort);
                }
                if !session_busy {
                    if let Some(raw_id) = &agy_native {
                        c.arg("--conversation").arg(raw_id);
                        c.arg("-p").arg(&raw_user_text);
                    } else {
                        c.arg("-p").arg(&prompt_with_context);
                    }
                } else {
                    c.arg("-p").arg(&prompt_with_context);
                }
                c
            }
            "codex" => {
                let mut c = Command::new(bin_str("codex"));
                c.args(crate::codex::Run::args());
                c
            }
            "keel" => {
                let mut c = Command::new(bin_str("keel"));
                c.arg("run").arg("--json");
                match safety_mode {
                    SafetyMode::Yolo => {
                        c.arg("--yolo");
                    }
                    SafetyMode::Safe => {
                        c.arg("--read-only");
                    }
                    SafetyMode::Standard => {
                        c.arg("--safety-mode").arg("standard");
                    }
                }
                c.arg("--root").arg(&workdir);

                let keel_sid = keel_native
                    .or_else(|| overlay_native_session(&conv_id_clone))
                    .unwrap_or_else(|| {
                        let sid = format!("kl_{}", Uuid::new_v4().simple());
                        set_overlay_native_session(&conv_id_clone, &sid);
                        sid
                    });

                let sess_path = account_home
                    .join(".keel")
                    .join("sessions")
                    .join(format!("{}.jsonl", keel_sid));
                c.arg("--log").arg(&sess_path);
                if sess_path.exists() && !session_busy {
                    c.arg("--resume").arg(&sess_path);
                    c.arg(&raw_user_text);
                } else {
                    c.arg(&prompt_with_context);
                }

                // Provider metadata is data, not a hard-coded model match.
                // Any OpenAI-compatible endpoint can be added in Settings;
                // "default" deliberately leaves Keel's own config in control.
                if model_id != "default" && !model_id.is_empty() {
                    c.arg("--model").arg(&model_id);
                }
                if let Some(model) = crate::settings::find_model("keel", &model_id) {
                    if let Some(url) = model
                        .base_url
                        .as_deref()
                        .map(str::trim)
                        .filter(|u| !u.is_empty())
                    {
                        c.arg("--base-url").arg(url);
                    }
                    if let Some(env_name) = model
                        .api_key_env
                        .as_deref()
                        .map(str::trim)
                        .filter(|n| !n.is_empty())
                    {
                        if let Ok(key) = std::env::var(env_name) {
                            let key = key.trim();
                            if !key.is_empty() {
                                c.arg("--api-key").arg(key);
                            }
                        }
                    }
                    if let Some(thinking) = model.thinking {
                        c.env("KEEL_THINKING", if thinking { "1" } else { "0" });
                    }
                }
                c
            }
            _ => {
                // A private server must load this turn's policy, instead of
                // reusing the shared server's allow-all environment.
                let mut c = Command::new(bin_str("opencode"));
                c.arg("run").arg("--format").arg("json").arg("--thinking");
                if let Some(config) = &opencode_config {
                    c.arg("--standalone").env("OPENCODE_CONFIG_CONTENT", config);
                }
                if safety_mode != SafetyMode::Safe {
                    c.arg("--auto");
                }
                if model_id != "default" && !model_id.is_empty() {
                    c.arg("--model").arg(&model_id);
                }
                for img_p in &local_image_paths {
                    c.arg("--file").arg(img_p);
                }
                if let Some(native_id) = opencode_native.as_ref() {
                    c.arg("--session").arg(native_id);
                    c.arg(&raw_user_text);
                } else {
                    c.arg(&prompt_with_context);
                }
                c
            }
        };

        #[cfg(unix)]
        cmd.process_group(0);

        cmd.current_dir(&workdir)
            .env("PATH", &enriched_path)
            .stdin(if harness_wants_stdin(&target_harness) {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &profile_env {
            cmd.env(key, value);
        }

        let mut child = match cmd.spawn() {
            Ok(ch) => ch,
            Err(e) => {
                let err_text = format!("Failed to start `{}`: {}", target_harness, e);
                if let Some(tx) = outcome_tx.take() {
                    let _ = tx.send(TurnOutcome {
                        success: false,
                        text: err_text.clone(),
                    });
                }
                update_overlay_assistant(
                    &conv_id_clone,
                    &assistant_msg_id,
                    err_text,
                    None,
                    Vec::new(),
                    Vec::new(),
                    false,
                );
                runner_clone.finish_run(&conv_id_clone, run_id).await;
                return;
            }
        };

        if let Some(stdin) = child.stdin.take() {
            runner_clone.child_stdins.lock().await.insert(
                conv_id_clone.clone(),
                Arc::new(tokio::sync::Mutex::new(stdin)),
            );
        }

        if target_harness == "codex" {
            for line in codex_run.opening() {
                let _ = runner_clone
                    .send_stdin(&conv_id_clone, &format!("{line}\n"))
                    .await;
            }
        }

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut reader = BufReader::new(stdout).lines();

        let stderr_handle = tokio::spawn(crate::completion::stderr_tail(stderr));
        let mut completion = crate::completion::Completion::default();

        let mut accumulated_text = String::new();
        let mut accumulated_thinking: Option<String> = None;
        let mut accumulated_tools: Vec<ToolSummaryItem> = Vec::new();
        let mut accumulated_blocks: Vec<TurnBlock> = Vec::new();
        let mut captured_session: Option<String> = None;

        loop {
            tokio::select! {
                _ = &mut abort_rx => {
                    completion.stopped = true;
                    #[cfg(unix)]
                    if let Some(pid) = child.id() {
                        unsafe {
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                    }
                    let _ = child.kill().await;
                    break;
                }
                line_res = reader.next_line() => {
                    match line_res {
                        Ok(Some(line)) => {
                            if runner_clone.run_ids.read().await.get(&conv_id_clone).copied() != Some(run_id) { continue; }
                            if target_harness == "codex" {
                                let actions = codex_turn.on_line(&codex_run, &line, &mut accumulated_blocks, &mut accumulated_tools);
                                run_codex_actions(actions, &conv_id_clone, run_id, &runner_clone, &mut completion, &mut accumulated_tools, codex_native.is_none()).await;
                                accumulated_text = crate::timeline::joined_text(&accumulated_blocks);
                                accumulated_thinking = codex_thinking(&accumulated_blocks);
                            } else {
                                completion.observe(&target_harness, &line);
                                parse_stream_line_for_run(
                                    &line,
                                    &mut accumulated_text,
                                    &mut accumulated_thinking,
                                    &mut accumulated_tools,
                                    &mut accumulated_blocks,
                                    &conv_id_clone,
                                    &runner_clone,
                                    Some(run_id),
                                ).await;
                            }
                            if target_harness == "opencode" && captured_session.is_none() {
                                if let Some(sid) = extract_opencode_session_id(&line) {
                                    if opencode_native.is_none() {
                                        set_overlay_native_session(&conv_id_clone, &sid);
                                    }
                                    captured_session = Some(sid);
                                }
                            }
                            if matches!(target_harness.as_str(), "claude" | "agy") && captured_session.is_none() {
                                if let Some(sid) = extract_native_session_id(&target_harness, &line) {
                                    if runner_clone.run_ids.read().await.get(&conv_id_clone).copied() == Some(run_id) {
                                        set_overlay_native_session(&conv_id_clone, &sid);
                                        captured_session = Some(sid);
                                    }
                                }
                            }
                            let display = if accumulated_text.trim().is_empty() {
                                if let Some(last_t) = accumulated_tools.last() {
                                    format!("Running `{}`: {}…", last_t.name, last_t.summary)
                                } else if accumulated_thinking.is_some() {
                                    "Thinking…".to_string()
                                } else {
                                    "Working…".to_string()
                                }
                            } else {
                                accumulated_text.clone()
                            };
                            update_overlay_assistant(
                                &conv_id_clone,
                                &assistant_msg_id,
                                display,
                                accumulated_thinking.clone(),
                                accumulated_tools.clone(),
                                live_blocks(&accumulated_blocks, &accumulated_tools, true),
                                true,
                            );
                        }
                        Ok(None) => break,
                        Err(error) => {
                            completion.failure = Some(format!("Could not read harness output: {error}"));
                            #[cfg(unix)]
                            if let Some(pid) = child.id() { unsafe { libc::kill(-(pid as i32), libc::SIGKILL); } }
                            let _ = child.kill().await;
                            break;
                        },
                    }
                }
            }
        }

        // Stop is signalled before stdin closes, so an exit on EOF that raced
        // the signal still counts as stopped.
        if abort_rx.try_recv().is_ok() {
            completion.stopped = true;
        }
        let status = child.wait().await;
        let stderr_out = stderr_handle.await.unwrap_or_default();

        let process_error = match &status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(format!("Process `{target_harness}` exited with {status}.")),
            Err(error) => Some(format!("Could not wait for `{target_harness}`: {error}")),
        };
        let partial_len = accumulated_text.len();
        let (success, accumulated_text) =
            completion.finish(process_error, accumulated_text, &stderr_out);
        let accumulated_text = if success {
            accumulated_text
        } else {
            crate::completion::readable_failure(
                &target_harness,
                &accumulated_text,
                chrono::Local::now(),
            )
        };
        // The phone draws the timeline when there is one; the failure belongs in it too.
        if !success && !accumulated_blocks.is_empty() {
            let note = accumulated_text
                .get(partial_len..)
                .unwrap_or(&accumulated_text);
            accumulated_blocks.push(TurnBlock::Text {
                text: note.trim_start_matches(['\n', '-']).trim().to_string(),
            });
        }
        for tool in &mut accumulated_tools {
            if tool.status == "running" {
                tool.status = if success { "done" } else { "error" }.into();
            }
        }

        update_overlay_assistant(
            &conv_id_clone,
            &assistant_msg_id,
            accumulated_text.clone(),
            accumulated_thinking,
            accumulated_tools.clone(),
            live_blocks(&accumulated_blocks, &accumulated_tools, false),
            false,
        );

        if let Some(tx) = outcome_tx.take() {
            let _ = tx.send(TurnOutcome {
                success,
                text: accumulated_text,
            });
        }
        runner_clone.finish_run(&conv_id_clone, run_id).await;
    });

    Ok((user_msg, outcome_rx))
}

async fn process_tool_execution(
    name: &str,
    summary: &str,
    args: Option<&Value>,
    conv_id: &str,
    runner: &Arc<RunnerState>,
    accumulated_tools: &mut Vec<ToolSummaryItem>,
) {
    // Risk analysis sees the raw args payload, not just the model-written
    // one-line description: `description: "tidying up"` + `command: "rm -rf"`
    // must not classify as harmless.
    let risk = analyze_risk(name, summary, args);
    let details = args.and_then(|v| {
        serde_json::to_string(v)
            .ok()
            .map(|s| truncate_str(&s, 300).to_string())
            .filter(|s| !s.is_empty())
    });

    let pre_auth = runner
        .approvals
        .is_pre_authorized(conv_id, name, summary, risk)
        .await;

    if pre_auth {
        runner
            .active_tasks
            .write()
            .await
            .insert(conv_id.to_string(), format!("{}: {}", name, summary));
        accumulated_tools.push(ToolSummaryItem {
            name: name.to_string(),
            summary: summary.to_string(),
            status: "done".to_string(),
        });
        return;
    }

    // Interactive HITL Approval Required!
    runner.active_tasks.write().await.insert(
        conv_id.to_string(),
        format!("⚠️ Waiting for approval: {}: {}", name, summary),
    );
    accumulated_tools.push(ToolSummaryItem {
        name: name.to_string(),
        summary: summary.to_string(),
        status: "waiting_approval".to_string(),
    });

    let harness = conv_id.split(':').next().unwrap_or("agent");
    let decision = runner
        .approvals
        .request_approval(conv_id, harness, name, summary, details, risk, 300)
        .await;

    match decision {
        ApprovalDecisionKind::AllowOnce | ApprovalDecisionKind::AllowAlways => {
            let _ = runner.send_stdin(conv_id, "y\n").await;
            if let Some(last) = accumulated_tools.last_mut() {
                last.status = "done".to_string();
            }
            runner
                .active_tasks
                .write()
                .await
                .insert(conv_id.to_string(), format!("{}: {}", name, summary));
        }
        ApprovalDecisionKind::Deny => {
            let _ = runner.send_stdin(conv_id, "n\n").await;
            if let Some(last) = accumulated_tools.last_mut() {
                last.status = "denied".to_string();
            }
        }
    }
}

/// Carry out what the Codex app-server asked for. Approvals block this run's
/// reader, exactly like Codex itself waits for the answer.
async fn run_codex_actions(
    actions: Vec<crate::codex::Action>,
    conv_id: &str,
    run_id: Uuid,
    runner: &Arc<RunnerState>,
    completion: &mut crate::completion::Completion,
    tools: &mut [ToolSummaryItem],
    adopt: bool,
) {
    use crate::codex::Action;
    for action in actions {
        match action {
            Action::Send(line) => {
                let _ = runner
                    .send_stdin_for_run(conv_id, run_id, &format!("{line}\n"))
                    .await;
            }
            Action::Adopt(thread) => {
                if adopt && runner.run_ids.read().await.get(conv_id).copied() == Some(run_id) {
                    set_overlay_native_session(conv_id, &thread);
                }
            }
            Action::Approve {
                id,
                tool,
                target,
                args,
            } => {
                let risk = analyze_risk(&tool, &target, Some(&args));
                runner
                    .active_tasks
                    .write()
                    .await
                    .insert(conv_id.into(), format!("Waiting for approval: {tool}"));
                let decision = runner
                    .approvals
                    .request_native_approval(
                        conv_id,
                        "codex",
                        &tool,
                        &target,
                        serde_json::to_string(&args).ok(),
                        risk,
                        300,
                    )
                    .await;
                let allow = decision != ApprovalDecisionKind::Deny;
                if !allow {
                    if let Some(last) = tools.last_mut() {
                        last.status = "denied".into();
                    }
                }
                let line = crate::codex::answer(id, allow);
                let _ = runner
                    .send_stdin_for_run(conv_id, run_id, &format!("{line}\n"))
                    .await;
            }
            Action::Fail(reason) => completion.failure = Some(reason),
            // EOF on stdin is how the app-server knows to exit.
            Action::Finished => {
                runner.child_stdins.lock().await.remove(conv_id);
            }
        }
    }
}

fn codex_thinking(blocks: &[TurnBlock]) -> Option<String> {
    let parts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            TurnBlock::Thinking { text } if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Test helper: parse a line for whatever run is current.
#[cfg(test)]
pub async fn parse_stream_line(
    line: &str,
    accumulated_text: &mut String,
    accumulated_thinking: &mut Option<String>,
    accumulated_tools: &mut Vec<ToolSummaryItem>,
    conv_id: &str,
    runner: &Arc<RunnerState>,
) {
    let run_id = runner.run_ids.read().await.get(conv_id).copied();
    parse_stream_line_for_run(
        line,
        accumulated_text,
        accumulated_thinking,
        accumulated_tools,
        &mut Vec::new(),
        conv_id,
        runner,
        run_id,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn parse_stream_line_for_run(
    line: &str,
    accumulated_text: &mut String,
    accumulated_thinking: &mut Option<String>,
    accumulated_tools: &mut Vec<ToolSummaryItem>,
    accumulated_blocks: &mut Vec<TurnBlock>,
    conv_id: &str,
    runner: &Arc<RunnerState>,
    expected_run: Option<Uuid>,
) {
    if let Some(expected) = expected_run {
        if runner.run_ids.read().await.get(conv_id).copied() != Some(expected) {
            return;
        }
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        let etype = val
            .get("type")
            .or_else(|| val.get("kind"))
            .or_else(|| val.get("event"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // AGY 1.5 wraps the payload under the event name. Its final response
        // is authoritative; step updates carry deltas rather than snapshots.
        if val.get("event").is_some() {
            if etype == "result" {
                if let Some(text) = val["result"]["response"].as_str() {
                    *accumulated_text = text.to_string();
                }
                return;
            }
            if etype == "step_update" {
                let step = &val["step_update"];
                if let Some(text) = step["text_delta"].as_str() {
                    accumulated_text.push_str(text);
                }
                if let Some(thinking) = step["thinking_delta"].as_str() {
                    accumulated_thinking
                        .get_or_insert_with(String::new)
                        .push_str(thinking);
                }
                if step["step_type"] == "tool" {
                    let info = &step["tool_info"];
                    let name = step["tool_name"]
                        .as_str()
                        .or_else(|| info["name"].as_str())
                        .unwrap_or("tool");
                    let parameters = &info["parameters"];
                    let summary = parameters["Path"]
                        .as_str()
                        .or_else(|| parameters["CommandLine"].as_str())
                        .or_else(|| parameters["query"].as_str())
                        .unwrap_or(name);
                    let status = match step["state"].as_str() {
                        Some("DONE") => "done",
                        Some("ERROR" | "FAILED") => "error",
                        _ => "running",
                    };
                    record_tool_progress(
                        accumulated_tools,
                        name,
                        &truncate_str(summary, 64),
                        status,
                    );
                }
                return;
            }
        }

        if etype == "capabilities" && val["approval_protocol"].as_str() == Some("stdio-line-v1") {
            if let Some(run_id) = expected_run {
                runner
                    .native_approval_runs
                    .write()
                    .await
                    .insert(conv_id.into(), run_id);
            }
            return;
        }
        if etype == "approval_request" && val["protocol"].as_str() == Some("stdio-line-v1") {
            let Some(run_id) = runner
                .native_approval_run(conv_id)
                .await
                .filter(|id| Some(*id) == expected_run)
            else {
                return;
            };
            let tool = val["tool"].as_str().unwrap_or("");
            if tool.is_empty()
                || val["id"].as_str().unwrap_or("").is_empty()
                || !val["arguments"].is_object()
            {
                let _ = runner.send_stdin_for_run(conv_id, run_id, "deny\n").await;
                return;
            }
            let args = &val["arguments"];
            let target = args["command"]
                .as_str()
                .or_else(|| args["path"].as_str())
                .unwrap_or(tool);
            let risk = analyze_risk(tool, target, Some(args));
            if let Some(last) = accumulated_tools.last_mut() {
                last.status = "waiting_approval".into();
            }
            runner
                .active_tasks
                .write()
                .await
                .insert(conv_id.into(), format!("Waiting for approval: {tool}"));
            let decision = runner
                .approvals
                .request_native_approval(
                    conv_id,
                    "keel",
                    tool,
                    target,
                    serde_json::to_string(args).ok(),
                    risk,
                    300,
                )
                .await;
            let response = match decision {
                ApprovalDecisionKind::AllowOnce => "allow\n",
                // Keel's "always" grants the whole tool. MoE Deck remembers only
                // this exact call and answers each matching native request.
                ApprovalDecisionKind::AllowAlways => "allow\n",
                ApprovalDecisionKind::Deny => "deny\n",
            };
            let _ = runner.send_stdin_for_run(conv_id, run_id, response).await;
            if let Some(last) = accumulated_tools.last_mut() {
                last.status = if decision == ApprovalDecisionKind::Deny {
                    "denied"
                } else {
                    "running"
                }
                .into();
            }
            return;
        }
        if etype == "tool_result" {
            if let Some(last) = accumulated_tools.last_mut() {
                if last.status != "denied" {
                    last.status = if val["is_error"] == true || val["status"] == "error" {
                        "error"
                    } else {
                        "done"
                    }
                    .into();
                }
            }
            return;
        }

        // Keel turn_summary event
        if etype == "turn_summary" {
            if let Some(sum_text) = val
                .get("summary")
                .or_else(|| val.get("data").and_then(|d| d.get("text")))
                .and_then(|v| v.as_str())
            {
                if !sum_text.trim().is_empty() {
                    *accumulated_text = sum_text.to_string();
                }
            }
            return;
        }

        // 1. Check top-level thinking (present in agy stream-json step_update)
        if let Some(th) = val.get("thinking").and_then(|v| v.as_str()) {
            append_thinking(accumulated_thinking, th);
        }

        // 2. Check OpenCode `part` object or top-level `reasoning` / `tool` / `tool_use`
        if let Some(part) = val.get("part") {
            parse_opencode_part(
                part,
                accumulated_text,
                accumulated_thinking,
                accumulated_tools,
                conv_id,
                runner,
            )
            .await;
            return;
        }
        if etype == "reasoning" {
            // Keel streams reasoning token by token (" user", " the"), so keep
            // each delta verbatim: no trimming, no paragraph breaks, no dedup.
            if let Some(th) = val
                .get("content")
                .or_else(|| val.get("text"))
                .and_then(|v| v.as_str())
            {
                accumulated_thinking
                    .get_or_insert_with(String::new)
                    .push_str(th);
            }
            return;
        }
        if etype == "text" {
            if let Some(txt) = val
                .get("content")
                .or_else(|| val.get("text"))
                .or_else(|| val.get("data").and_then(|d| d.get("text")))
                .and_then(|v| v.as_str())
            {
                accumulated_text.push_str(txt);
            }
            return;
        }
        if etype == "done" {
            if let Some(ans) = val.get("answer").and_then(|v| v.as_str()) {
                if !ans.trim().is_empty() {
                    *accumulated_text = ans.to_string();
                }
            } else if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
                *accumulated_text = format!("Error: {}", err);
            }
            return;
        }
        if etype == "tool_call" {
            let data = val.get("data").unwrap_or(&val);
            let name = data
                .get("name")
                .or_else(|| data.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("tool")
                .to_string();
            let summary = data
                .get("arguments")
                .or_else(|| data.get("args"))
                .and_then(|args| {
                    if let Some(s) = args.as_str() {
                        Some(s.to_string())
                    } else {
                        args.get("path")
                            .or_else(|| args.get("file"))
                            .or_else(|| args.get("command"))
                            .or_else(|| args.get("query"))
                            .or_else(|| args.get("fact"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| serde_json::to_string(args).ok())
                    }
                })
                .map(|s| truncate_str(&s, 64))
                .unwrap_or_else(|| name.clone());
            let args_val = data.get("arguments").or_else(|| data.get("args"));
            if runner.native_approval_run(conv_id).await.is_some() {
                accumulated_tools.push(ToolSummaryItem {
                    name,
                    summary,
                    status: "running".into(),
                });
                return;
            }

            process_tool_execution(
                &name,
                &summary,
                args_val,
                conv_id,
                runner,
                accumulated_tools,
            )
            .await;
            return;
        }
        if etype == "tool_use" || etype == "tool" {
            parse_opencode_part(
                &val,
                accumulated_text,
                accumulated_thinking,
                accumulated_tools,
                conv_id,
                runner,
            )
            .await;
            return;
        }

        // 4. Claude Code `assistant` message
        if etype == "assistant" {
            if let Some(arr) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                crate::timeline::push_claude_assistant(accumulated_blocks, arr, "running");
                let mut turn_texts = Vec::new();
                for item in arr {
                    let itype = item.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    if itype == "text" {
                        if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                            turn_texts.push(t.to_string());
                        }
                    } else if itype == "thinking" {
                        if let Some(th) = item.get("thinking").and_then(|v| v.as_str()) {
                            append_thinking(accumulated_thinking, th);
                        }
                    } else if itype == "tool_use" {
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Tool")
                            .to_string();
                        let summary = item
                            .get("input")
                            .and_then(|inp| {
                                inp.get("description")
                                    .or_else(|| inp.get("toolSummary"))
                                    .or_else(|| inp.get("command"))
                                    .or_else(|| inp.get("file_path"))
                            })
                            .and_then(|v| v.as_str())
                            .map(|s| truncate_str(s, 64))
                            .unwrap_or_else(|| name.clone());
                        process_tool_execution(
                            &name,
                            &summary,
                            item.get("input"),
                            conv_id,
                            runner,
                            accumulated_tools,
                        )
                        .await;
                    }
                }
                if !turn_texts.is_empty() {
                    *accumulated_text = crate::timeline::joined_text(accumulated_blocks);
                }
            }
        } else if etype == "user" && !accumulated_blocks.is_empty() {
            // Claude Code tool results arrive as `user` events.
            if let Some(arr) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                crate::timeline::apply_claude_tool_results(accumulated_blocks, arr);
            }
        } else if etype == "step_update"
            || val.get("tool_info").is_some()
            || val.get("tool_calls").is_some()
        {
            // Antigravity (`agy`) stream-json
            if let Some(tinfo) = val.get("tool_info") {
                let name = tinfo
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let summary = tinfo
                    .get("summary")
                    .or_else(|| tinfo.get("command"))
                    .or_else(|| {
                        tinfo
                            .get("parameters")
                            .and_then(|p| p.get("command").or_else(|| p.get("Path")))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| truncate_str(s, 64))
                    .unwrap_or_else(|| name.clone());
                process_tool_execution(
                    &name,
                    &summary,
                    Some(tinfo),
                    conv_id,
                    runner,
                    accumulated_tools,
                )
                .await;
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
                        .map(|s| truncate_str(s, 64))
                        .unwrap_or_else(|| name.clone());
                    process_tool_execution(
                        &name,
                        &summary,
                        Some(tc),
                        conv_id,
                        runner,
                        accumulated_tools,
                    )
                    .await;
                }
            }
            if let Some(cnt) = val
                .get("content")
                .or_else(|| val.get("delta"))
                .and_then(|v| v.as_str())
            {
                if !cnt.trim().is_empty() {
                    accumulated_text.push_str(cnt);
                }
            }
        } else if etype == "result" {
            // With a timeline, the final answer is already its last text block.
            if let Some(res_txt) = val
                .get("result")
                .and_then(|v| v.as_str())
                .filter(|_| crate::timeline::joined_text(accumulated_blocks).is_empty())
            {
                if !res_txt.trim().is_empty() {
                    *accumulated_text = res_txt.to_string();
                }
            }
        } else if let Some(txt) = val
            .get("content")
            .or_else(|| val.get("text"))
            .or_else(|| val.get("delta"))
            .and_then(|v| {
                v.as_str().or_else(|| {
                    v.get("text")
                        .or_else(|| v.get("content"))
                        .and_then(|inner| inner.as_str())
                })
            })
        {
            let is_delta = val.get("delta").is_some() || etype.contains("delta");
            if !txt.is_empty() {
                if !is_delta && !accumulated_text.is_empty() && !txt.starts_with('\n') {
                    accumulated_text.push('\n');
                }
                accumulated_text.push_str(txt);
            }
        }
    } else {
        let is_prompt = trimmed.contains("[y/N]")
            || trimmed.contains("[Y/n]")
            || trimmed.contains("[y/n]")
            || trimmed.contains("Approve tool call")
            || trimmed.contains("Permission required");

        if is_prompt {
            process_tool_execution(
                "interactive_prompt",
                trimmed,
                None,
                conv_id,
                runner,
                accumulated_tools,
            )
            .await;
            return;
        }

        if !accumulated_text.is_empty() {
            accumulated_text.push('\n');
        }
        accumulated_text.push_str(trimmed);
    }
}

async fn parse_opencode_part(
    part: &Value,
    accumulated_text: &mut String,
    accumulated_thinking: &mut Option<String>,
    accumulated_tools: &mut Vec<ToolSummaryItem>,
    conv_id: &str,
    runner: &Arc<RunnerState>,
) {
    let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if ptype == "reasoning" {
        if let Some(th) = part.get("text").and_then(|v| v.as_str()) {
            append_thinking(accumulated_thinking, th);
        }
    } else if ptype == "text" {
        if let Some(txt) = part.get("text").and_then(|v| v.as_str()) {
            if !txt.trim().is_empty() {
                *accumulated_text = txt.trim().to_string();
            }
        }
    } else if ptype == "tool" || ptype == "tool_use" {
        let name = part
            .get("tool")
            .or_else(|| part.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("tool")
            .to_string();
        let state_obj = part.get("state");
        let summary = state_obj
            .and_then(|st| {
                st.get("title").and_then(|v| v.as_str()).or_else(|| {
                    st.get("input").and_then(|inp| {
                        inp.get("command")
                            .or_else(|| inp.get("filePath"))
                            .or_else(|| inp.get("path"))
                            .and_then(|v| v.as_str())
                    })
                })
            })
            .map(|s| truncate_str(s, 64))
            .unwrap_or_else(|| name.clone());
        // OpenCode emits tool progress/results, not a blocking stdin approval.
        // Its native permissions must enforce policy before execution.
        let status = match state_obj.and_then(|s| s["status"].as_str()) {
            Some("completed") => "done",
            Some("error") => "error",
            _ => "running",
        };
        record_tool_progress(accumulated_tools, &name, &summary, status);
        runner
            .active_tasks
            .write()
            .await
            .insert(conv_id.into(), "Tool progress".into());
    }
}

fn record_tool_progress(tools: &mut Vec<ToolSummaryItem>, name: &str, summary: &str, status: &str) {
    if status != "running" {
        if let Some(tool) = tools
            .iter_mut()
            .rev()
            .find(|t| t.name == name && t.summary == summary && t.status == "running")
        {
            tool.status = status.into();
            return;
        }
    }
    tools.push(ToolSummaryItem {
        name: name.into(),
        summary: summary.into(),
        status: status.into(),
    });
}

fn append_thinking(accumulated_thinking: &mut Option<String>, new_chunk: &str) {
    let clean = new_chunk.trim();
    if clean.is_empty() {
        return;
    }
    if let Some(acc) = accumulated_thinking.as_mut() {
        if !acc.contains(clean) {
            acc.push_str("\n\n");
            acc.push_str(clean);
        }
    } else {
        *accumulated_thinking = Some(clean.to_string());
    }
}

/// Tool blocks get their status from the result once it arrives. Until then
/// the approval state tracked in `tools` (same order) is what the user needs.
fn live_blocks(blocks: &[TurnBlock], tools: &[ToolSummaryItem], streaming: bool) -> Vec<TurnBlock> {
    let mut tool_idx = 0;
    blocks
        .iter()
        .cloned()
        .map(|mut b| {
            if let TurnBlock::Tool { output, status, .. } = &mut b {
                let legacy = tools.get(tool_idx).map(|t| t.status.as_str());
                tool_idx += 1;
                if status == "running" && output.is_none() {
                    if let Some(s @ ("waiting_approval" | "denied")) = legacy {
                        *status = s.to_string();
                    } else if !streaming {
                        *status = "done".to_string();
                    }
                }
            }
            b
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn update_overlay_assistant(
    conv_id: &str,
    msg_id: &str,
    text: String,
    thinking: Option<String>,
    tools: Vec<ToolSummaryItem>,
    blocks: Vec<TurnBlock>,
    streaming: bool,
) {
    let mut overlays = load_overlays();
    if let Some(ov) = overlays.get_mut(conv_id) {
        ov.updated_at = Utc::now().timestamp_millis();
        if let Some(msg) = ov.appended_messages.iter_mut().find(|m| m.id == msg_id) {
            msg.text = text;
            msg.thinking = thinking;
            msg.tools = tools;
            msg.blocks = blocks;
            msg.streaming = streaming;
        }
        save_overlay(ov);
    }
}

/// OpenCode's non-interactive JSONL stream tags every line with the session it
/// belongs to. Use the first `ses_…` id seen to adopt the session for the
/// thread, so later turns can resume instead of re-pasting context.
fn extract_opencode_session_id(line: &str) -> Option<String> {
    let val = serde_json::from_str::<Value>(line).ok()?;
    val.get("sessionID")
        .and_then(|v| v.as_str())
        .or_else(|| {
            val.get("part")
                .and_then(|p| p.get("sessionID"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| s.starts_with("ses_"))
        .map(|s| s.to_string())
}

fn extract_native_session_id(harness: &str, line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line).ok()?;
    let id = match harness {
        "claude" if value["type"] == "system" && value["subtype"] == "init" => {
            value["session_id"].as_str()
        }
        "agy" if value["event"] == "init" => value["conversation_id"].as_str(),
        _ => None,
    }?;
    Uuid::parse_str(id).ok().map(|_| id.to_string())
}

/// OpenCode V2 policies run after agent permissions and saved approvals.
/// Deny unknown tools too: shell, edits, subagents and MCP cannot bypass Safe
/// through an agent's allow rule. Keep existing provider/config overrides.
fn opencode_safe_config(inline: Option<&str>) -> Result<String, String> {
    let mut config: Value = serde_json::from_str(inline.unwrap_or("{}"))
        .map_err(|_| "OpenCode inline configuration is invalid JSON".to_string())?;
    let object = config
        .as_object_mut()
        .ok_or("OpenCode inline configuration must be an object")?;
    let policies = object
        .entry("policies")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or("OpenCode policies must be an array")?;
    let existing_denials: Vec<Value> = policies
        .iter()
        .filter(|p| p["action"] == "tool.use" && p["effect"] == "deny")
        .cloned()
        .collect();
    policies.push(serde_json::json!({"action":"tool.use","resource":"*","effect":"deny"}));
    for action in ["read", "glob", "grep", "external_directory"] {
        policies.push(serde_json::json!({"action":"tool.use","resource":format!("{action}:*"),"effect":"allow"}));
    }
    // A Safe read exception must never lift an existing explicit denial.
    policies.extend(existing_denials);
    Ok(config.to_string())
}

#[cfg(test)]
mod parity_tests {
    use super::*;

    #[test]
    fn native_identity_accepts_only_valid_init_events() {
        let id = Uuid::new_v4().to_string();
        for (harness, value) in [
            (
                "claude",
                serde_json::json!({"type":"system","subtype":"init","session_id":id}),
            ),
            (
                "agy",
                serde_json::json!({"event":"init","conversation_id":id}),
            ),
        ] {
            assert_eq!(
                extract_native_session_id(harness, &value.to_string()),
                Some(id.clone())
            );
        }
        assert_eq!(
            extract_native_session_id("agy", r#"{"event":"result","conversation_id":"../escape"}"#),
            None
        );
        assert_eq!(
            extract_native_session_id(
                "claude",
                r#"{"type":"system","subtype":"init","session_id":"../escape"}"#
            ),
            None
        );
    }

    #[test]
    fn safe_config_preserves_provider_settings_and_stricter_denials() {
        let original = serde_json::json!({"model":"provider/model","policies":[{"action":"tool.use","resource":"read:*.env","effect":"deny"}]});
        let config: Value =
            serde_json::from_str(&opencode_safe_config(Some(&original.to_string())).unwrap())
                .unwrap();
        assert_eq!(config["model"], "provider/model");
        assert_eq!(
            config["policies"].as_array().unwrap().last().unwrap()["resource"],
            "read:*.env"
        );
        assert!(opencode_safe_config(Some("not JSON")).is_err());
        assert!(opencode_safe_config(Some(r#"{"policies":{}}"#)).is_err());
    }

    #[tokio::test]
    async fn native_tool_progress_never_opens_a_retrospective_approval() {
        let runner = RunnerState::new();
        runner.approvals.set_safety_mode(SafetyMode::Safe).await;
        let mut text = String::new();
        let mut thinking = None;
        let mut tools = Vec::new();
        for line in [
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"ACTIVE","tool_name":"read_file","tool_info":{"parameters":{"Path":"proof.txt"}}}}"#,
            r#"{"event":"step_update","step_update":{"step_type":"tool","state":"DONE","tool_name":"read_file","tool_info":{"parameters":{"Path":"proof.txt"}}}}"#,
            r#"{"type":"tool_use","part":{"type":"tool","tool":"shell","state":{"status":"completed","input":{"command":"cargo publish"}}}}"#,
        ] {
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                parse_stream_line(
                    line,
                    &mut text,
                    &mut thinking,
                    &mut tools,
                    "helm:proof",
                    &runner,
                ),
            )
            .await
            .unwrap();
        }
        assert!(runner.approvals.list_pending().await.is_empty());
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().all(|t| t.status == "done"));
    }
}

/// UI-level session reset: stop delegating this thread to its native CLI
/// session so the next turn starts a fresh one. Only meaningful for threads
/// MoE Deck created itself; threads bound to a discovered native session
/// (`claude:*`, `agy:*`, `opencode:*`, `codex:*`, `keel:*`) are owned by that session and
/// can only be replaced by starting a new thread.
pub fn handle_reset_command(conv_id: &str) -> String {
    match conv_id.split(':').next() {
        Some("claude") | Some("agy") | Some("codex") | Some("opencode") | Some("keel") => format!(
            "ℹ️ This thread lives in a native `{0}` session, so `/new` can't reset it in place. \
Create a new thread in MoE Deck to start fresh.",
            conv_id.split(':').next().unwrap_or("keel")
        ),
        _ => {
            set_overlay_native_session(conv_id, "");
            "🔄 Session reset. Your next message starts a brand-new harness session.".to_string()
        }
    }
}

fn resolve_uploaded_image_path(url: &str) -> Option<String> {
    if let Some(fname) = url.strip_prefix("/api/uploads/") {
        let safe: String = fname
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
            .collect();
        let p = crate::discovery::config_dir().join("uploads").join(safe);
        if p.exists() {
            return Some(p.to_string_lossy().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the OpenCode hang: a child that drains stdin to EOF before
    /// answering never finishes while MoE Deck keeps the write end open. The
    /// chosen stdin policy must let this process exit on its own.
    #[tokio::test]
    async fn harness_stdin_policy_does_not_deadlock_opencode() {
        for harness in ["opencode"] {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "cat >/dev/null; printf '%s' ready"]);
            cmd.stdin(if harness_wants_stdin(harness) {
                Stdio::piped()
            } else {
                Stdio::null()
            });
            cmd.stdout(Stdio::piped()).stderr(Stdio::null());
            let child = cmd.spawn().unwrap();
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(5), child.wait_with_output())
                    .await
                    .unwrap_or_else(|_| panic!("{harness} waited for stdin EOF"))
                    .unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout), "ready");
        }
        // Harnesses with the native approval protocol keep the piped stdin.
        assert!(harness_wants_stdin("keel"));
        assert!(harness_wants_stdin("claude"));
        assert!(harness_wants_stdin("agy"));
    }

    #[test]
    fn test_extract_opencode_session_id() {
        // Full stream line from `opencode run --format json`
        let line = r#"{"type":"step_start","timestamp":1793651816851,"sessionID":"ses_19dfab99","part":{"id":"prt_x","sessionID":"ses_19dfab99","messageID":"msg_y","type":"step-start"}}"#;
        assert_eq!(
            extract_opencode_session_id(line).as_deref(),
            Some("ses_19dfab99")
        );
        // Only the part carries the id
        let line2 = r#"{"type":"step","part":{"type":"reasoning","sessionID":"ses_partonly"}}"#;
        assert_eq!(
            extract_opencode_session_id(line2).as_deref(),
            Some("ses_partonly")
        );
        // Non-JSON lines (e.g. tool output barging in) yield None
        assert_eq!(extract_opencode_session_id("noise"), None);
    }

    #[tokio::test]
    async fn test_parse_stream_line_opencode_and_agy_and_keel() {
        let runner = RunnerState::new();
        // This test focuses on *parsing*: the agy `run_command` fixture is
        // (correctly) classified as a Medium-risk exec tool now and would
        // block on a real HITL prompt, so run the broker in Yolo here. The
        // prompting path itself is covered by test_hitl_tool_approval_interception.
        runner.approvals.set_safety_mode(SafetyMode::Yolo).await;
        let mut text = String::new();
        let mut thinking = None;
        let mut tools = Vec::new();

        // 1. AGY step_update with thinking and tool_calls
        let agy_line = r#"{"type":"step_update","thinking":"Checking files in repo","tool_calls":[{"name":"run_command","args":{"toolSummary":"Git status"}}]}"#;
        parse_stream_line(
            agy_line,
            &mut text,
            &mut thinking,
            &mut tools,
            "agy:123",
            &runner,
        )
        .await;
        assert_eq!(thinking.as_deref(), Some("Checking files in repo"));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "run_command");
        assert_eq!(tools[0].summary, "Git status");

        // 2. OpenCode reasoning part
        let oc_reasoning = r#"{"type":"step","part":{"type":"reasoning","text":"Analyzing OpenCode session state"}}"#;
        parse_stream_line(
            oc_reasoning,
            &mut text,
            &mut thinking,
            &mut tools,
            "opencode:1",
            &runner,
        )
        .await;
        assert!(thinking
            .as_deref()
            .unwrap_or("")
            .contains("Analyzing OpenCode session state"));

        text = "Earlier text.".into();

        // 4. Keel NDJSON chunks (reasoning, tool_call, text, done)
        let mut keel_thinking = None;
        for delta in [
            "The", " user", " wants", " the", " fix", " the", " same", " way.",
        ] {
            parse_stream_line(
                &serde_json::json!({"type":"reasoning","content":delta}).to_string(),
                &mut text,
                &mut keel_thinking,
                &mut tools,
                "keel:1",
                &runner,
            )
            .await;
        }
        assert_eq!(
            keel_thinking.as_deref(),
            Some("The user wants the fix the same way.")
        );

        let keel_reasoning =
            r#"{"type":"reasoning","content":"Planning solution with epistemic memory"}"#;
        parse_stream_line(
            keel_reasoning,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        assert!(thinking
            .as_deref()
            .unwrap_or("")
            .contains("Planning solution with epistemic memory"));

        let keel_tool = r#"{"type":"tool_call","name":"patch_file","arguments":{"path":"src/main.rs","command":"patch"}}"#;
        parse_stream_line(
            keel_tool,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[1].name, "patch_file");
        assert_eq!(tools[1].summary, "src/main.rs");

        let keel_text = r#"{"type":"text","content":"Applying "}"#;
        parse_stream_line(
            keel_text,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        let keel_text2 = r#"{"type":"text","content":"patch now."}"#;
        parse_stream_line(
            keel_text2,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        assert_eq!(text, "Earlier text.Applying patch now.");

        let keel_done = r#"{"type":"done","status":"ok","answer":"Task successfully finished."}"#;
        parse_stream_line(
            keel_done,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        assert_eq!(text, "Task successfully finished.");

        // 5. Keel turn_summary event
        let keel_summary =
            r#"{"kind":"turn_summary","data":{"text":"Finished turn: 1 file modified"}}"#;
        parse_stream_line(
            keel_summary,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:1",
            &runner,
        )
        .await;
        assert_eq!(text, "Finished turn: 1 file modified");
    }

    #[tokio::test]
    async fn test_idempotency_dedupe_and_retention() {
        let runner = RunnerState::new();
        assert!(runner.check_idempotency("offline_1").await);
        assert!(!runner.check_idempotency("offline_1").await); // replay → dedup
        assert!(runner.check_idempotency("offline_2").await);
        // Keys for different items do not collide
        assert!(!runner.check_idempotency("offline_2").await);
        assert!(runner.check_idempotency("offline_3").await);
    }

    #[tokio::test]
    async fn test_cli_argument_sanitization_and_run_id_isolation() {
        assert_eq!(sanitize_effort("high"), "high");
        assert_eq!(sanitize_effort("--evil-flag"), "medium");
        assert_eq!(sanitize_harness("opencode"), "opencode");
        assert_eq!(sanitize_harness("keel"), "keel");
        assert_eq!(sanitize_harness("../../bin/sh"), "claude");
        assert_eq!(safe_prompt_arg("--version"), " --version");
        assert_eq!(safe_prompt_arg("Normal prompt"), "Normal prompt");

        // Verify finish_run only cleans up if run_id matches active run_id
        let runner = RunnerState::new();
        let old_run = Uuid::new_v4();
        let new_run = Uuid::new_v4();
        runner
            .run_ids
            .write()
            .await
            .insert("helm:1".to_string(), new_run);
        runner
            .active_tasks
            .write()
            .await
            .insert("helm:1".to_string(), "Running new turn".to_string());

        // Old run finishing late must NOT clobber the new run
        runner.finish_run("helm:1", old_run).await;
        assert!(runner.active_tasks.read().await.contains_key("helm:1"));

        // New run finishing cleans up properly
        runner.finish_run("helm:1", new_run).await;
        assert!(!runner.active_tasks.read().await.contains_key("helm:1"));
    }

    #[tokio::test]
    async fn test_hitl_tool_approval_interception() {
        let approvals = ApprovalManager::new();
        approvals.set_safety_mode(SafetyMode::Safe).await;
        let runner = RunnerState::with_approvals(approvals.clone());

        let mut text = String::new();
        let mut thinking = None;
        let mut tools = Vec::new();

        let runner_clone = runner.clone();
        tokio::spawn(async move {
            // Wait for tool to register pending approval
            for _ in 0..50 {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                let pending = runner_clone.approvals.list_pending().await;
                if !pending.is_empty() {
                    let appr_id = &pending[0].id;
                    assert_eq!(pending[0].tool_name, "shell");
                    assert!(pending[0].target.contains("cargo publish"));
                    let _ = runner_clone
                        .approvals
                        .decide(appr_id, ApprovalDecisionKind::AllowOnce)
                        .await;
                    break;
                }
            }
        });

        let dangerous_call =
            r#"{"type":"tool_call","name":"shell","arguments":{"command":"cargo publish"}}"#;
        parse_stream_line(
            dangerous_call,
            &mut text,
            &mut thinking,
            &mut tools,
            "keel:test",
            &runner,
        )
        .await;

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "shell");
        assert_eq!(tools[0].status, "done");
    }

    #[tokio::test]
    async fn test_interactive_terminal_prompt_interception() {
        let approvals = ApprovalManager::new();
        approvals.set_safety_mode(SafetyMode::Safe).await;
        let runner = RunnerState::with_approvals(approvals.clone());

        let mut text = String::new();
        let mut thinking = None;
        let mut tools = Vec::new();

        let runner_clone = runner.clone();
        tokio::spawn(async move {
            for _ in 0..50 {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                let pending = runner_clone.approvals.list_pending().await;
                if !pending.is_empty() {
                    let appr_id = &pending[0].id;
                    assert_eq!(pending[0].tool_name, "interactive_prompt");
                    let _ = runner_clone
                        .approvals
                        .decide(appr_id, ApprovalDecisionKind::AllowOnce)
                        .await;
                    break;
                }
            }
        });

        parse_stream_line(
            "Execute destructive command? [y/N]",
            &mut text,
            &mut thinking,
            &mut tools,
            "claude:prompt",
            &runner,
        )
        .await;

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "interactive_prompt");
        assert_eq!(tools[0].status, "done");
    }
}

#[cfg(test)]
mod native_approval_tests {
    use super::*;

    #[tokio::test]
    async fn advertised_tool_progress_waits_only_for_the_real_native_request() {
        for (decision, expected) in [
            (ApprovalDecisionKind::AllowOnce, "allow"),
            (ApprovalDecisionKind::AllowAlways, "allow"),
            (ApprovalDecisionKind::Deny, "deny"),
        ] {
            let runner = RunnerState::new();
            let run = Uuid::new_v4();
            runner
                .run_ids
                .write()
                .await
                .insert("helm:native-proof".into(), run);
            let mut child = Command::new("sh")
                .args(["-c", "read answer; printf '%s' \"$answer\""])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            runner.child_stdins.lock().await.insert(
                "helm:native-proof".into(),
                Arc::new(Mutex::new(child.stdin.take().unwrap())),
            );
            let mut text = String::new();
            let mut thinking = None;
            let mut tools = Vec::new();
            parse_stream_line(
                r#"{"type":"capabilities","approval_protocol":"stdio-line-v1"}"#,
                &mut text,
                &mut thinking,
                &mut tools,
                "helm:native-proof",
                &runner,
            )
            .await;
            tokio::time::timeout(std::time::Duration::from_secs(1),parse_stream_line(r#"{"type":"tool_call","id":"call-1","name":"shell","arguments":{"command":"cargo publish"}}"#,&mut text,&mut thinking,&mut tools,"helm:native-proof",&runner)).await.unwrap();
            assert!(runner.approvals.list_pending().await.is_empty());
            assert_eq!(tools[0].status, "running");
            let worker = runner.clone();
            let awaiting = tokio::spawn(async move {
                parse_stream_line(r#"{"type":"approval_request","protocol":"stdio-line-v1","id":"native-1","tool_call_id":"call-1","tool":"shell","arguments":{"command":"cargo publish"}}"#,&mut text,&mut thinking,&mut tools,"helm:native-proof",&worker).await;
                tools
            });
            let request = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let Some(request) = runner.approvals.list_pending().await.into_iter().next()
                    {
                        break request;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(request.target, "cargo publish");
            runner
                .approvals
                .decide(&request.id, decision)
                .await
                .unwrap();
            let tools = awaiting.await.unwrap();
            assert_eq!(tools.len(), 1);
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(2), child.wait_with_output())
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn an_old_approval_cannot_write_to_a_replacement_run() {
        let runner = RunnerState::new();
        let old = Uuid::new_v4();
        let current = Uuid::new_v4();
        runner
            .run_ids
            .write()
            .await
            .insert("helm:replace".into(), current);
        let mut child = Command::new("sh")
            .args(["-c", "read answer; printf '%s' \"$answer\""])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        runner.child_stdins.lock().await.insert(
            "helm:replace".into(),
            Arc::new(Mutex::new(child.stdin.take().unwrap())),
        );
        assert!(runner
            .send_stdin_for_run("helm:replace", old, "deny\n")
            .await
            .is_err());
        runner
            .send_stdin_for_run("helm:replace", current, "allow\n")
            .await
            .unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(output.stdout, b"allow");
    }
}

#[cfg(test)]
mod late_native_frame_tests {
    #[tokio::test]
    async fn late_capability_frame_cannot_register_a_new_run_as_its_own() {
        let runner = super::RunnerState::new();
        let old = uuid::Uuid::new_v4();
        let current = uuid::Uuid::new_v4();
        runner
            .run_ids
            .write()
            .await
            .insert("helm:race".into(), current);
        let mut text = String::new();
        let mut thinking = None;
        let mut tools = Vec::new();
        super::parse_stream_line_for_run(
            r#"{"type":"capabilities","approval_protocol":"stdio-line-v1"}"#,
            &mut text,
            &mut thinking,
            &mut tools,
            &mut Vec::new(),
            "helm:race",
            &runner,
            Some(old),
        )
        .await;
        assert!(runner.native_approval_run("helm:race").await.is_none());
        assert!(runner.approvals.list_pending().await.is_empty());
    }

    #[tokio::test]
    async fn claude_stream_keeps_updates_tools_and_results_in_order() {
        use crate::models::TurnBlock;
        let runner = super::RunnerState::new();
        let (mut text, mut thinking, mut tools, mut blocks) =
            (String::new(), None, Vec::new(), Vec::new());
        for line in [
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Let me read the config."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/tmp/helm.toml"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"port = 8080"}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"The port is 8080."}]}}"#,
            r#"{"type":"result","subtype":"success","result":"The port is 8080."}"#,
        ] {
            super::parse_stream_line_for_run(
                line,
                &mut text,
                &mut thinking,
                &mut tools,
                &mut blocks,
                "claude:timeline",
                &runner,
                None,
            )
            .await;
        }
        assert_eq!(text, "Let me read the config.\n\nThe port is 8080.");
        let kinds: Vec<&str> = blocks
            .iter()
            .map(|b| match b {
                TurnBlock::Text { .. } => "text",
                TurnBlock::Thinking { .. } => "thinking",
                TurnBlock::Tool { .. } => "tool",
            })
            .collect();
        assert_eq!(kinds, ["text", "tool", "text"]);
        let TurnBlock::Tool { output, status, .. } = &blocks[1] else {
            unreachable!()
        };
        assert_eq!(output.as_deref(), Some("port = 8080"));
        assert_eq!(status, "done");
    }
}
