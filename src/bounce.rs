use crate::discovery::{config_dir, get_conversation_detail, save_overlay, truncate_str};
use crate::models::{
    humanize_model_name, ConversationDetail, HandoffEnvelope, SendMessageRequest,
    StoredThreadOverlay, SwarmBounceRequest, SwarmSessionItem, SwarmStatus,
};
use crate::ops::{create_worktree, inspect_branch_diff, merge_worktree_branch, remove_worktree};
use crate::runner::{dispatch_turn, RunnerState};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Extracts structured file references, tasks, and goals from a conversation.
pub fn distill_conversation_context(
    conv_id: &str,
    target_harness: &str,
    detail: &ConversationDetail,
) -> HandoffEnvelope {
    let source_harness = detail.summary.harness.clone();

    // 1. Goal extraction (first user prompt or summary title)
    let first_user_text = detail
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| m.text.clone())
        .unwrap_or_else(|| detail.summary.title.clone());

    let last_user_text = detail
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.text.clone())
        .unwrap_or_default();

    let goal = if !last_user_text.is_empty() && last_user_text != first_user_text {
        format!("{}: {}", first_user_text.trim(), last_user_text.trim())
    } else {
        first_user_text.trim().to_string()
    };

    // 2. Extract file mentions from messages and tool calls
    let mut files_set = HashSet::new();
    for msg in &detail.messages {
        extract_file_mentions_from_text(&msg.text, &mut files_set);
        if let Some(ref th) = msg.thinking {
            extract_file_mentions_from_text(th, &mut files_set);
        }
        for tool in &msg.tools {
            extract_file_mentions_from_text(&tool.summary, &mut files_set);
        }
    }
    let mut relevant_files: Vec<String> = files_set.into_iter().collect();
    relevant_files.sort();

    // 3. Extract task list items
    let mut open_tasks = Vec::new();
    let mut completed_tasks = Vec::new();
    for msg in &detail.messages {
        for line in msg.text.lines() {
            let tr = line.trim();
            if let Some(task) = tr.strip_prefix("- [ ]") {
                let cleaned = task.trim().to_string();
                if !open_tasks.contains(&cleaned) {
                    open_tasks.push(cleaned);
                }
            } else if let Some(task) = tr
                .strip_prefix("- [x]")
                .or_else(|| tr.strip_prefix("- [X]"))
            {
                let cleaned = task.trim().to_string();
                if !completed_tasks.contains(&cleaned) {
                    completed_tasks.push(cleaned);
                }
            }
        }
    }

    // 4. Summarize recent assistant insights
    let mut summary_points = Vec::new();
    for msg in detail.messages.iter().rev().take(4).rev() {
        if msg.role == "assistant" && !msg.text.starts_with("Process `") {
            // Char-boundary-safe truncation: harness text is full of emoji and
            // CJK, and a raw `&text[..300]` byte slice panics mid-codepoint.
            let snippet = if msg.text.chars().count() > 300 {
                truncate_str(&msg.text, 300)
            } else {
                msg.text.clone()
            };
            summary_points.push(snippet.trim().to_string());
        }
    }
    let context_summary = summary_points.join("\n---\n");

    HandoffEnvelope {
        source_conversation_id: conv_id.to_string(),
        source_harness,
        target_harness: target_harness.to_string(),
        goal,
        relevant_files,
        open_tasks,
        completed_tasks,
        context_summary,
    }
}

fn extract_file_mentions_from_text(text: &str, set: &mut HashSet<String>) {
    for word in text.split_whitespace() {
        let unquoted = word.trim_matches(|c: char| {
            c == '`'
                || c == '('
                || c == ')'
                || c == '['
                || c == ']'
                || c == '"'
                || c == '\''
                || c == '<'
                || c == '>'
        });
        let clean = unquoted.trim_end_matches(|c: char| {
            c == '.'
                || c == ','
                || c == ';'
                || c == ':'
                || c == '!'
                || c == '?'
                || c == ')'
                || c == ']'
                || c == '`'
        });
        let has_code_ext = clean.ends_with(".rs")
            || clean.ends_with(".ts")
            || clean.ends_with(".js")
            || clean.ends_with(".py")
            || clean.ends_with(".json")
            || clean.ends_with(".toml")
            || clean.ends_with(".md")
            || clean.ends_with(".yml")
            || clean.ends_with(".yaml");
        if (clean.contains('/') || clean.contains('.'))
            && has_code_ext
            && !clean.starts_with("http")
            && !clean.starts_with("git@")
        {
            set.insert(clean.to_string());
        }
    }
}

impl HandoffEnvelope {
    pub fn to_target_prompt(&self, custom_instructions: Option<&str>) -> String {
        let mut sections = Vec::new();
        sections.push(format!(
            "### 🔄 Swarm Handoff: {} ➔ {}",
            self.source_harness.to_uppercase(),
            self.target_harness.to_uppercase()
        ));

        sections.push(format!("**Primary Objective:**\n{}", self.goal));

        if !self.relevant_files.is_empty() {
            let files_fmt = self
                .relevant_files
                .iter()
                .take(15)
                .map(|f| format!("- `{}`", f))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("**Identified Workspace Files:**\n{}", files_fmt));
        }

        if !self.completed_tasks.is_empty() {
            let done_fmt = self
                .completed_tasks
                .iter()
                .map(|t| format!("- [x] {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("**Completed Milestones:**\n{}", done_fmt));
        }

        if !self.open_tasks.is_empty() {
            let open_fmt = self
                .open_tasks
                .iter()
                .map(|t| format!("- [ ] {}", t))
                .collect::<Vec<_>>()
                .join("\n");
            sections.push(format!("**Pending Tasks to Execute:**\n{}", open_fmt));
        }

        if !self.context_summary.is_empty() {
            sections.push(format!(
                "**Prior Context & Findings:**\n{}",
                self.context_summary
            ));
        }

        let default_instruction = match self.target_harness.as_str() {
            "keel" => "Execute the pending tasks in this isolated sandbox worktree. Use `keel vfs`, `patch_file`, and compiler/test diagnostics to verify your changes. Ensure tests pass before reporting completion.",
            "claude" => "Perform an architectural audit of the approach, review any diffs for safety, and refine the design.",
            "agy" => "Perform fast autonomous execution and verify the modified files.",
            _ => "Continue execution in this sandbox branch and verify results.",
        };

        let instructions = custom_instructions
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(default_instruction);

        sections.push(format!("**Instructions:**\n{}", instructions));

        sections.join("\n\n")
    }
}

pub struct SwarmManager {
    sessions: RwLock<HashMap<String, SwarmSessionItem>>,
    config_file: PathBuf,
}

impl SwarmManager {
    pub fn new() -> Arc<Self> {
        let conf_dir = config_dir();
        let config_file = conf_dir.join("swarm_sessions.json");
        let initial_sessions = load_persisted_sessions(&config_file).unwrap_or_default();

        Arc::new(Self {
            sessions: RwLock::new(initial_sessions),
            config_file,
        })
    }

    /// Executes an autonomous cross-harness context bounce into an isolated git worktree.
    pub async fn execute_bounce(
        &self,
        source_conv_id: &str,
        req: SwarmBounceRequest,
        runner: Arc<RunnerState>,
    ) -> Result<SwarmSessionItem, String> {
        let active = runner.active_snapshot().await;
        let detail = get_conversation_detail(source_conv_id, &active)
            .ok_or_else(|| format!("Source conversation `{}` not found", source_conv_id))?;

        let base_workdir = detail.summary.workdir.clone();
        let target_harness = req.target_harness.to_lowercase();
        // A bounce that stays inside the same harness keeps the source
        // thread's account; crossing harnesses starts on the target's
        // primary account.
        let inherit_profile = if target_harness == detail.summary.harness {
            detail.summary.profile.clone()
        } else {
            None
        };
        let (model_id, _model_label) =
            humanize_model_name(&target_harness, req.target_model.as_deref().unwrap_or(""));
        let effort = req.target_effort.unwrap_or_else(|| "high".to_string());

        let envelope = distill_conversation_context(source_conv_id, &target_harness, &detail);
        let prompt_text = envelope.to_target_prompt(req.custom_instructions.as_deref());

        let use_worktree = req.isolated_worktree.unwrap_or(true);
        let now = Utc::now().timestamp_millis();
        let short_id = &Uuid::new_v4().simple().to_string()[..6];

        let (target_workdir, branch_name) = if use_worktree {
            let slug = format!(
                "bounce-{}-{}-{}",
                envelope.source_harness, target_harness, short_id
            );
            let wt = create_worktree(&base_workdir, &slug)?;
            (wt.path, wt.branch)
        } else {
            (base_workdir.clone(), "HEAD".to_string())
        };

        let new_conv_id = format!("helm:swarm_{}", Uuid::new_v4().simple());
        let title = format!(
            "Swarm ({} ➔ {}) · {}",
            envelope.source_harness.to_uppercase(),
            target_harness.to_uppercase(),
            detail.summary.title
        );

        let overlay = StoredThreadOverlay {
            id: new_conv_id.clone(),
            title: Some(title),
            pinned: true,
            harness: Some(target_harness.clone()),
            model: Some(model_id.clone()),
            effort: Some(effort.clone()),
            workdir: Some(target_workdir.clone()),
            profile: inherit_profile.clone(),
            created_at: now,
            updated_at: now,
            appended_messages: Vec::new(),
            native_session: None,
        };
        save_overlay(&overlay);

        let send_req = SendMessageRequest {
            text: prompt_text,
            harness: Some(target_harness.clone()),
            model: Some(model_id),
            effort: Some(effort),
            workdir: Some(target_workdir.clone()),
            profile: inherit_profile,
            images: Vec::new(),
        };

        dispatch_turn(runner, new_conv_id.clone(), send_req)
            .await
            .map_err(|e| format!("Failed to dispatch target harness: {}", e))?;

        let swarm_id = format!("swm_{}", short_id);
        let session = SwarmSessionItem {
            id: swarm_id.clone(),
            source_conversation_id: source_conv_id.to_string(),
            target_conversation_id: new_conv_id,
            source_harness: envelope.source_harness.clone(),
            target_harness,
            worktree_path: target_workdir,
            branch: branch_name,
            base_workdir,
            status: SwarmStatus::Running,
            envelope,
            created_at: now,
            updated_at: now,
        };

        {
            let mut s = self.sessions.write().await;
            s.insert(swarm_id, session.clone());
            if !cfg!(test) {
                save_persisted_sessions(&self.config_file, &s);
            }
        }

        Ok(session)
    }

    pub async fn list_sessions(&self) -> Vec<SwarmSessionItem> {
        let s = self.sessions.read().await;
        let mut list: Vec<SwarmSessionItem> = s.values().cloned().collect();
        list.sort_by_key(|item| item.created_at);
        list.reverse();
        list
    }

    pub async fn get_session(&self, swarm_id: &str) -> Option<SwarmSessionItem> {
        let s = self.sessions.read().await;
        s.get(swarm_id).cloned()
    }

    /// Inspects the diff of the swarm branch relative to the base HEAD.
    pub async fn diff_session(&self, swarm_id: &str) -> Result<String, String> {
        let s = self.sessions.read().await;
        let session = s
            .get(swarm_id)
            .ok_or_else(|| format!("Swarm session `{}` not found", swarm_id))?;

        if session.branch == "HEAD" || session.branch.is_empty() {
            return Err("Session has no worktree branch".to_string());
        }

        inspect_branch_diff(&session.base_workdir, &session.branch)
    }

    /// Merges the swarm worktree branch back into base and cleans up the sandbox worktree.
    pub async fn merge_session(&self, swarm_id: &str) -> Result<String, String> {
        let mut s = self.sessions.write().await;
        let session = s
            .get_mut(swarm_id)
            .ok_or_else(|| format!("Swarm session `{}` not found", swarm_id))?;

        if session.branch == "HEAD" || session.branch.is_empty() {
            return Err("Cannot merge in-place non-worktree session".to_string());
        }

        let merge_res = merge_worktree_branch(&session.base_workdir, &session.branch)?;
        let _ = remove_worktree(&session.base_workdir, &session.worktree_path);

        session.status = SwarmStatus::Merged;
        session.updated_at = Utc::now().timestamp_millis();

        if !cfg!(test) {
            save_persisted_sessions(&self.config_file, &s);
        }

        Ok(merge_res)
    }

    /// Discards the swarm worktree branch without merging.
    pub async fn discard_session(&self, swarm_id: &str) -> Result<(), String> {
        let mut s = self.sessions.write().await;
        let session = s
            .get_mut(swarm_id)
            .ok_or_else(|| format!("Swarm session `{}` not found", swarm_id))?;

        if session.branch != "HEAD" && !session.branch.is_empty() {
            let _ = remove_worktree(&session.base_workdir, &session.worktree_path);
        }

        session.status = SwarmStatus::Discarded;
        session.updated_at = Utc::now().timestamp_millis();

        if !cfg!(test) {
            save_persisted_sessions(&self.config_file, &s);
        }

        Ok(())
    }
}

fn load_persisted_sessions(path: &Path) -> Option<HashMap<String, SwarmSessionItem>> {
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_persisted_sessions(path: &Path, sessions: &HashMap<String, SwarmSessionItem>) {
    if let Ok(text) = serde_json::to_string_pretty(sessions) {
        if let Err(e) = crate::discovery::atomic_write(path, &text) {
            eprintln!("Failed to persist swarm sessions: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ChatMessage, ConversationSummary};

    #[test]
    fn test_distill_context() {
        let detail = ConversationDetail {
            summary: ConversationSummary {
                id: "claude:1".to_string(),
                raw_id: "1".to_string(),
                title: "Refactor Rust Storage".to_string(),
                subtitle: "/home/ada/project".to_string(),
                workdir: "/home/ada/project".to_string(),
                harness: "claude".to_string(),
                profile: None,
                profile_label: String::new(),
                model: "opus".to_string(),
                model_label: "Opus 5.5".to_string(),
                effort: "high".to_string(),
                status: "idle".to_string(),
                running_tasks_count: 0,
                running_task_label: None,
                updated_at: 1789000000,
                pinned: false,
                last_preview: "Done".to_string(),
                pr_links: Vec::new(),
                pending_approval: None,
            },
            messages: vec![
                ChatMessage {
                    id: "m1".into(),
                    role: "user".into(),
                    harness: "claude".into(),
                    model: None,
                    timestamp: "2026-11-03T00:00:00Z".into(),
                    text: "We need to fix the memory buffer in `src/storage.rs` and verify `tests/storage_test.rs`.".into(),
                    thinking: None,
                    tools: Vec::new(),
                    images: Vec::new(),
                    streaming: false,
                    blocks: Vec::new(),
                },
                ChatMessage {
                    id: "m2".into(),
                    role: "assistant".into(),
                    harness: "claude".into(),
                    model: Some("Opus 5.5".into()),
                    timestamp: "2026-11-03T00:01:00Z".into(),
                    text: "Here is the plan:\n- [x] Analyze memory leak in `src/storage.rs`\n- [ ] Rewrite buffer flush\n- [ ] Run test suite".into(),
                    thinking: Some("Inspecting crates/keel-agent/src/lib.rs for reference".into()),
                    tools: Vec::new(),
                    images: Vec::new(),
                    streaming: false,
                    blocks: Vec::new(),
                },
            ],
            running_tasks: Vec::new(),
            truncated: 0,
            total_messages: 2,
        };

        let env = distill_conversation_context("claude:1", "keel", &detail);
        assert_eq!(env.source_harness, "claude");
        assert_eq!(env.target_harness, "keel");
        assert!(env.relevant_files.contains(&"src/storage.rs".to_string()));
        assert!(env
            .relevant_files
            .contains(&"tests/storage_test.rs".to_string()));
        assert!(env
            .relevant_files
            .contains(&"crates/keel-agent/src/lib.rs".to_string()));
        assert_eq!(
            env.completed_tasks,
            vec!["Analyze memory leak in `src/storage.rs`"]
        );
        assert_eq!(
            env.open_tasks,
            vec!["Rewrite buffer flush", "Run test suite"]
        );

        let prompt = env.to_target_prompt(None);
        assert!(prompt
            .to_uppercase()
            .contains("SWARM HANDOFF: CLAUDE ➔ KEEL"));
        assert!(prompt.contains("Rewrite buffer flush"));
        assert!(prompt.contains("keel vfs"));
    }
}
