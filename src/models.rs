use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
    pub default_effort: String,
    #[serde(default)]
    pub efforts: Vec<String>,
    /// OpenAI-compatible endpoint for harnesses that accept one (Keel).
    /// `None` means "let the harness decide from its own config".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Environment variable that holds this provider's API key. The key is
    /// read from the daemon environment at spawn time and never stored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Disable/enable model thinking tokens when the harness supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessInfo {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub binary: String,
    pub accent: String,
    pub accent_soft: String,
    pub available: bool,
    pub version: String,
    pub models: Vec<ModelOption>,
    pub efforts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSummaryItem {
    pub name: String,
    pub summary: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: String, // "user" | "assistant" | "system"
    pub harness: String,
    pub model: Option<String>,
    pub timestamp: String,
    pub text: String,
    #[serde(default)]
    pub thinking: Option<String>,
    pub tools: Vec<ToolSummaryItem>,
    pub images: Vec<String>,
    pub streaming: bool,
    /// The turn in the order it happened: text updates, thinking and tool
    /// calls interleaved. Empty for readers that still only fill the legacy
    /// `text`/`thinking`/`tools` slots; the UI falls back to those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<TurnBlock>,
}

/// One step of an assistant turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnBlock {
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    Tool {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        summary: String,
        /// Full tool input, pretty-printed and capped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        /// What the tool returned, capped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// "running" | "done" | "error" | "waiting_approval" | "denied"
        status: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningTaskItem {
    pub id: String,
    pub conversation_id: String,
    pub harness: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyMode {
    Safe,
    Standard,
    Yolo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    ApprovedOnce,
    ApprovedAlways,
    Denied,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    AllowOnce,
    AllowAlways,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequestItem {
    pub id: String,
    pub conversation_id: String,
    pub harness: String,
    pub tool_name: String,
    pub target: String,
    pub details: Option<String>,
    pub risk_level: RiskLevel,
    pub status: ApprovalStatus,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecisionRequest {
    pub decision: String, // "allow_once", "allow_always", "deny"
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyModeUpdateRequest {
    pub mode: String, // "safe", "standard", "yolo"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmBounceRequest {
    pub target_harness: String,
    pub target_model: Option<String>,
    pub target_effort: Option<String>,
    pub isolated_worktree: Option<bool>,
    pub custom_instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmStatus {
    Running,
    Completed,
    Merged,
    Failed,
    Discarded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffEnvelope {
    pub source_conversation_id: String,
    pub source_harness: String,
    pub target_harness: String,
    pub goal: String,
    pub relevant_files: Vec<String>,
    pub open_tasks: Vec<String>,
    pub completed_tasks: Vec<String>,
    pub context_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmSessionItem {
    pub id: String,
    pub source_conversation_id: String,
    pub target_conversation_id: String,
    pub source_harness: String,
    pub target_harness: String,
    pub worktree_path: String,
    pub branch: String,
    pub base_workdir: String,
    pub status: SwarmStatus,
    pub envelope: HandoffEnvelope,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: String,
    pub raw_id: String,
    pub title: String,
    pub subtitle: String,
    pub workdir: String,
    pub harness: String,
    /// Account profile this thread's harness runs under (`None` = primary).
    #[serde(default)]
    pub profile: Option<String>,
    /// Human label of the account (e.g. "Account #2"), empty when default.
    #[serde(default)]
    pub profile_label: String,
    pub model: String,
    pub model_label: String,
    pub effort: String,
    pub status: String, // "busy" | "idle"
    pub running_tasks_count: usize,
    pub running_task_label: Option<String>,
    pub updated_at: i64,
    pub pinned: bool,
    pub last_preview: String,
    pub pr_links: Vec<String>,
    #[serde(default)]
    pub pending_approval: Option<ApprovalRequestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDetail {
    pub summary: ConversationSummary,
    pub messages: Vec<ChatMessage>,
    pub running_tasks: Vec<RunningTaskItem>,
    /// Number of messages kept back because the client asked for a tail only.
    #[serde(default)]
    pub truncated: usize,
    /// Total number of messages available before truncation.
    #[serde(default)]
    pub total_messages: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendMessageRequest {
    pub text: String,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub workdir: Option<String>,
    /// Account profile id (see `profiles.rs`); `None`/`"default"` = primary
    /// account living in the real home.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub images: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConversationRequest {
    pub title: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub workdir: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
    pub pinned: Option<bool>,
    #[serde(default)]
    pub archived: Option<bool>,
    pub workdir: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitFileStat {
    pub status: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeItem {
    pub path: String,
    pub branch: String,
    pub head: String,
    pub is_main: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrStatusInfo {
    pub has_pr: bool,
    pub number: Option<u64>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub state: Option<String>,
    pub ci_state: String, // "passing" | "failing" | "pending" | "none"
    pub checks_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevPortItem {
    pub port: u16,
    pub process: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorktreeRequest {
    pub workdir: String,
    pub slug: String,
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktreeRequest {
    pub workdir: String,
    pub worktree_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrActionRequest {
    pub workdir: String,
    pub action: String, // "merge" | "create-pr" | "fix-ci"
    pub conversation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondOpinionRequest {
    pub harness: String,
    pub model: Option<String>,
    pub mode: Option<String>, // "review-diff" | "arena"
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribeRequest {
    pub audio_base64: Option<String>,
    pub format: Option<String>,
    pub speech_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", content = "payload", rename_all = "snake_case")]
pub enum VoiceIntent {
    StopTask,
    FixCi,
    CheckCi,
    ReviewDiff,
    SwarmBounce {
        target_harness: String,
        custom_instructions: Option<String>,
    },
    ChatPrompt(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTranscribeResponse {
    pub ok: bool,
    pub transcript: String,
    pub intent: VoiceIntent,
    pub engine: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SprintStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintStep {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SprintItem {
    pub id: String,
    pub title: String,
    pub workdir: String,
    pub harness: String,
    /// Account profile the sprint's turns run under (`None` = primary).
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub steps: Vec<SprintStep>,
    pub status: SprintStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSprintRequest {
    pub title: String,
    pub workdir: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    pub steps: Vec<CreateSprintStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSprintStep {
    pub name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workdir: String,
    pub name: String,
    pub is_git: bool,
    pub branch: Option<String>,
    pub modified_files: Vec<GitFileStat>,
    pub recent_commits: Vec<String>,
    #[serde(default)]
    pub worktrees: Vec<WorktreeItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredThreadOverlay {
    pub id: String,
    pub title: Option<String>,
    pub pinned: bool,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub workdir: Option<String>,
    /// Account profile id this thread is bound to (`None` = primary account).
    #[serde(default)]
    pub profile: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub appended_messages: Vec<ChatMessage>,
    /// Native CLI session id (e.g. OpenCode `ses_…`) adopted from a stateless
    /// run, so later turns in this thread resume in place instead of re-pasting
    /// thread context.
    #[serde(default)]
    pub native_session: Option<String>,
}

pub fn default_harnesses() -> Vec<HarnessInfo> {
    let efforts = vec![
        "low".to_string(),
        "medium".to_string(),
        "high".to_string(),
        "xhigh".to_string(),
        "max".to_string(),
    ];

    let mut harnesses = vec![
        HarnessInfo {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            short_name: "Claude".to_string(),
            binary: "claude".to_string(),
            accent: "#D97757".to_string(),
            accent_soft: "rgba(217, 119, 87, 0.16)".to_string(),
            available: check_bin("claude"),
            version: "2.1.309".to_string(),
            models: vec![
                crate::settings::default_model(),
                ModelOption {
                    id: "opus".to_string(),
                    label: "Opus (CLI alias)".to_string(),
                    default_effort: "medium".to_string(),
                    efforts: vec![],
                    base_url: None,
                    api_key_env: None,
                    thinking: None,
                },
                ModelOption {
                    id: "sonnet".to_string(),
                    label: "Sonnet (CLI alias)".to_string(),
                    default_effort: "medium".to_string(),
                    efforts: vec![],
                    base_url: None,
                    api_key_env: None,
                    thinking: None,
                },
                ModelOption {
                    id: "haiku".to_string(),
                    label: "Haiku (CLI alias)".to_string(),
                    default_effort: "low".to_string(),
                    efforts: vec![],
                    base_url: None,
                    api_key_env: None,
                    thinking: None,
                },
            ],
            efforts: efforts.clone(),
        },
        HarnessInfo {
            id: "agy".to_string(),
            name: "Antigravity".to_string(),
            short_name: "AGY".to_string(),
            binary: "agy".to_string(),
            accent: "#4F8FF7".to_string(),
            accent_soft: "rgba(79, 143, 247, 0.16)".to_string(),
            available: check_bin("agy"),
            version: "1.4.12".to_string(),
            models: crate::settings::native_models("agy"),
            efforts: vec!["default".into()],
        },
        HarnessInfo {
            id: "codex".to_string(),
            name: "Codex CLI".to_string(),
            short_name: "Codex".to_string(),
            binary: "codex".to_string(),
            accent: "#10A37F".to_string(),
            accent_soft: "rgba(16, 163, 127, 0.16)".to_string(),
            available: check_bin("codex"),
            version: "latest".to_string(),
            models: crate::codex::cached_models(),
            efforts: vec!["default".to_string()],
        },
        HarnessInfo {
            id: "opencode".to_string(),
            name: "OpenCode".to_string(),
            short_name: "OpenCode".to_string(),
            binary: "opencode".to_string(),
            accent: "#A855F7".to_string(),
            accent_soft: "rgba(168, 85, 247, 0.16)".to_string(),
            available: check_bin("opencode"),
            version: "2.2.5".to_string(),
            models: crate::settings::native_models("opencode"),
            efforts: vec!["default".into()],
        },
        HarnessInfo {
            id: "keel".to_string(),
            name: "Keel Harness".to_string(),
            short_name: "Keel".to_string(),
            binary: "keel".to_string(),
            accent: "#06B6D4".to_string(),
            accent_soft: "rgba(6, 182, 212, 0.16)".to_string(),
            available: check_bin("keel"),
            version: "0.0.1".to_string(),
            models: crate::settings::native_models("keel"),
            efforts: vec!["default".into()],
        },
    ];
    crate::settings::apply(&mut harnesses);
    harnesses
}

pub fn resolve_bin_path(name: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "/".to_string())
        });
    let candidates = [
        format!("{}/.local/bin/{}", home, name),
        format!("{}/.npm-global/bin/{}", home, name),
        format!("{}/.gemini/antigravity-cli/bin/{}", home, name),
        format!("/usr/local/bin/{}", name),
        format!("/usr/bin/{}", name),
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

fn check_bin(name: &str) -> bool {
    resolve_bin_path(name).is_some()
}

pub fn humanize_model_name(harness: &str, raw_model: &str) -> (String, String) {
    // Codex IDs are opaque. Never turn Sol/Terra or a future model into Astra.
    if harness == "codex" {
        let id = raw_model.trim();
        if id.is_empty() || id == "default" || id == "cli-default" {
            return ("default".into(), "Codex configured default".into());
        }
        let label = crate::codex::cached_models()
            .into_iter()
            .find(|m| m.id == id)
            .map(|m| m.label)
            .unwrap_or_else(|| id.to_string());
        return (id.to_string(), label);
    }
    if matches!(harness, "agy" | "opencode" | "keel") {
        let id = raw_model.trim();
        if id.is_empty() || id == "default" || id == "cli-default" {
            return ("default".into(), "Configured default".into());
        }
        let label = default_harnesses()
            .into_iter()
            .find(|h| h.id == harness)
            .and_then(|h| h.models.into_iter().find(|m| m.id == id))
            .map(|m| m.label)
            .unwrap_or_else(|| id.to_string());
        return (id.to_string(), label);
    }
    let m = raw_model.to_lowercase();
    if m.contains("opus-5-5") || m == "opus" {
        return ("opus".to_string(), "Opus 5.5".to_string());
    }
    if m.contains("sonnet-4-5") || m.contains("sonnet") {
        return ("sonnet".to_string(), "Sonnet 5.5".to_string());
    }
    if m.contains("haiku") {
        return ("haiku".to_string(), "Haiku 4.5".to_string());
    }
    if m.contains("argon") {
        return ("gemini-4-argon".to_string(), "Gemini 4 Argon".to_string());
    }
    if m.contains("glm-5.3") {
        return (
            "opencode/glm-5.3-flash-free".to_string(),
            "GLM 5.3 Flash".to_string(),
        );
    }
    if m.contains("kimi-k3.1") {
        return (
            "opencode-go/kimi-k3.1-pro".to_string(),
            "Kimi K3.1 Pro".to_string(),
        );
    }
    if m.contains("mimo-v2.6") {
        return (
            "opencode-go/mimo-v2.6-pro".to_string(),
            "MiMo v2.6 Pro".to_string(),
        );
    }
    if m.contains("deepseek-v4") {
        return (
            "opencode/deepseek-v4.1-flash".to_string(),
            "DeepSeek v4.1 Flash".to_string(),
        );
    }
    if m.contains("bonsai-9b") || m == "bonsai 2 9b (local)" {
        return ("bonsai-9b".to_string(), "Bonsai 2 9B (Local)".to_string());
    }
    if m.contains("bonsai-27b") || m == "bonsai 2 27b (local)" {
        return ("bonsai-27b".to_string(), "Bonsai 2 27B (Local)".to_string());
    }
    if m.contains("qwen-27b") || m == "qwen 3.8 27b (local)" {
        return ("qwen-27b".to_string(), "Qwen 3.8 27B (Local)".to_string());
    }
    if m.contains("deepseek-chat") || m.contains("deepseek-v3") {
        return ("deepseek-chat".to_string(), "DeepSeek-V3".to_string());
    }
    if m.contains("kat") {
        return (
            "kat-coder".to_string(),
            "Kat Coder (Local :8085)".to_string(),
        );
    }
    if m.contains("3.8") || (harness == "agy" && m.contains("flash")) {
        return (
            "gemini-3.8-flash".to_string(),
            "Gemini 3.8 Flash".to_string(),
        );
    }
    if m.contains("3.5") {
        return ("gemini-3.5-pro".to_string(), "Gemini 3.5 Pro".to_string());
    }
    if m.contains("gpt-6") || m.contains("astra") {
        return ("gpt-6-astra".to_string(), "GPT-6 Astra".to_string());
    }
    if m == "default" || m == "cli-default" || m.is_empty() {
        return (
            "default".to_string(),
            match harness {
                "codex" => "Codex default",
                "opencode" => "OpenCode Auto",
                _ => "CLI default",
            }
            .to_string(),
        );
    }
    if m.contains("5.6-sol") {
        return ("gpt-5.6-sol".to_string(), "GPT-5.6 Sol".to_string());
    }
    if m.contains("5.6-terra") {
        return ("gpt-5.6-terra".to_string(), "GPT-5.6 Terra".to_string());
    }
    if m.contains("5.6-luna") {
        return ("gpt-5.6-luna".to_string(), "GPT-5.6 Luna".to_string());
    }
    if m.contains("5.5") {
        return ("gpt-5.5".to_string(), "GPT-5.5".to_string());
    }
    match harness {
        "claude" => ("opus".to_string(), "Opus 5.5".to_string()),
        "agy" => ("gemini-4-argon".to_string(), "Gemini 4 Argon".to_string()),
        "codex" => ("gpt-5.6-sol".to_string(), "GPT-5.6 Sol".to_string()),
        "keel" => ("bonsai-9b".to_string(), "Bonsai 2 9B (Local)".to_string()),
        _ => ("default".to_string(), "OpenCode Auto".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_model_names() {
        assert_eq!(
            humanize_model_name("claude", "claude-opus-5-5-2026"),
            ("opus".to_string(), "Opus 5.5".to_string())
        );
        assert_eq!(
            humanize_model_name("agy", "gemini-4-argon"),
            ("gemini-4-argon".to_string(), "gemini-4-argon".to_string())
        );
        assert_eq!(
            humanize_model_name("codex", "gpt-5.6-terra").0,
            "gpt-5.6-terra"
        );
        assert_eq!(
            humanize_model_name("opencode", "opencode/glm-5.3-flash-free"),
            (
                "opencode/glm-5.3-flash-free".to_string(),
                "opencode/glm-5.3-flash-free".to_string()
            )
        );
        assert_eq!(
            humanize_model_name("keel", "bonsai-9b"),
            ("bonsai-9b".to_string(), "Bonsai 2 9B (Local)".to_string())
        );
    }

    #[test]
    fn test_default_harnesses_includes_all_five() {
        let harnesses = default_harnesses();
        let ids: Vec<&str> = harnesses.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "agy", "codex", "opencode", "keel"]);
    }
}
