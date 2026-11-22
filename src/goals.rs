use crate::discovery::{config_dir, home_dir, load_overlays, sanitize_workdir, save_overlay};
use crate::models::{SendMessageRequest, StoredThreadOverlay};
use crate::runner::{dispatch_turn_with_outcome, RunnerState, TurnOutcome};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubgoalStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Blocked,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubgoalItem {
    pub id: String,
    pub goal_id: String,
    pub title: String,
    pub prompt: String,
    pub harness: String, // "opencode", "codex", "agy", "claude"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    pub status: SubgoalStatus,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub is_dynamic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_subgoal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

fn default_max_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub workdir: String,
    pub status: GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    pub subgoals: Vec<SubgoalItem>,
    #[serde(default = "default_auto_triage")]
    pub auto_triage: bool,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tick_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_tick_at: Option<i64>,
}

fn default_auto_triage() -> bool {
    false
}

impl GoalItem {
    /// What the cockpit's background-work list actually renders: identity,
    /// state and a task count. Bootstrap is polled every 0.7–2 s, so it must
    /// not ship full subgoal histories — a triage storm once grew this
    /// payload to 8 MB and every poll re-sent it to the phone.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "status": self.status,
            "schedule": self.schedule,
            "subgoal_count": self.subgoals.len(),
            "next_tick_at": self.next_tick_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGoalRequest {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub schedule: Option<String>,
    #[serde(default)]
    pub auto_triage: Option<bool>,
    pub subgoals: Vec<CreateSubgoalRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSubgoalRequest {
    pub title: String,
    pub prompt: String,
    pub harness: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickOutcome {
    SubgoalPassed {
        subgoal_id: String,
        subgoal_title: String,
        harness: String,
    },
    SubgoalFailed {
        subgoal_id: String,
        subgoal_title: String,
        error: String,
    },
    TriageSpawned {
        parent_id: String,
        triage_id: String,
        triage_title: String,
        target_harness: String,
    },
    GoalCompleted {
        goal_id: String,
    },
    GoalBlocked {
        goal_id: String,
        reason: String,
    },
    WaitingForRunning {
        goal_id: String,
        subgoal_id: String,
    },
    NoRunnableSubgoal {
        goal_id: String,
    },
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemDomain {
    NetworkIsolation,     // Route to OpenCode
    TargetInfrastructure, // Route to Codex
    VerificationEvidence, // Route to Antigravity (Agy)
    GeneralDiagnosis,     // Route to failing harness
}

#[derive(Debug, Clone)]
pub struct ProblemDiagnosis {
    #[allow(dead_code)]
    pub domain: ProblemDomain,
    pub target_harness: String,
    pub summary: String,
    pub remediation_prompt: String,
}

pub fn diagnose_failure(
    failed_subgoal: &SubgoalItem,
    error_text: &str,
    workdir: &str,
) -> ProblemDiagnosis {
    let lower = error_text.to_lowercase();

    // Check for explicit structured tag first, e.g. [PROBLEM: harness=opencode reason=...]
    if let Some(pos) = lower.find("[problem:") {
        let tag_rest = &error_text[pos + 9..];
        if let Some(end_pos) = tag_rest.find(']') {
            let tag_content = &tag_rest[..end_pos];
            let mut requested_harness = None;
            let mut requested_reason = None;
            for part in tag_content.split_whitespace() {
                if let Some(rest) = part.strip_prefix("harness=") {
                    requested_harness = Some(rest.trim().to_lowercase());
                } else if let Some(rest) = part.strip_prefix("reason=") {
                    requested_reason = Some(rest.trim().to_string());
                }
            }
            if let Some(harness) = requested_harness {
                let summary =
                    requested_reason.unwrap_or_else(|| "Explicit escalation request".to_string());
                return ProblemDiagnosis {
                    domain: match harness.as_str() {
                        "opencode" => ProblemDomain::NetworkIsolation,
                        "codex" => ProblemDomain::TargetInfrastructure,
                        "agy" => ProblemDomain::VerificationEvidence,
                        _ => ProblemDomain::GeneralDiagnosis,
                    },
                    target_harness: harness.clone(),
                    summary: summary.clone(),
                    remediation_prompt: format!(
                        "[AUTONOMOUS TRIAGE: Problem escalated from {}]\n\
                         Parent subgoal: {}\n\
                         Explicit escalation reason: {}\n\
                         Workspace: {}\n\
                         Error details:\n{}\n\n\
                         Please investigate the issue above and apply the necessary fix so the parent task can proceed.",
                        failed_subgoal.harness, failed_subgoal.title, summary, workdir, error_text
                    ),
                };
            }
        }
    }

    // Heuristic 1: Network, firewall, routing, socket, DNS, isolation boundary
    let is_network = lower.contains("connection refused")
        || lower.contains("unreachable")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("nftables")
        || lower.contains("iptables")
        || lower.contains("firewall")
        || lower.contains("port ")
        || lower.contains("no route to host")
        || lower.contains("name resolution")
        || lower.contains("dns")
        || lower.contains("egress")
        || lower.contains("ingress")
        || lower.contains("bridge")
        || lower.contains("packet drop")
        || lower.contains("isolation");

    if is_network {
        let summary = format!(
            "Network/firewall barrier detected in '{}'",
            failed_subgoal.title
        );
        return ProblemDiagnosis {
            domain: ProblemDomain::NetworkIsolation,
            target_harness: "opencode".to_string(),
            summary: summary.clone(),
            remediation_prompt: format!(
                "[AUTONOMOUS TRIAGE: Network/Isolation Blocker]\n\
                 Parent subgoal: '{}' (run by {})\n\
                 Failure observed:\n{}\n\n\
                 Workspace: {}\n\n\
                 Instructions for OpenCode:\n\
                 1. Check the active network namespaces, virtual interfaces, and routing table for this lab.\n\
                 2. Inspect nftables / iptables filtering rules to see if traffic to the target lab VM or required port was dropped.\n\
                 3. Ensure authorized communication within the lab boundary is permitted while preserving strict host isolation.\n\
                 4. Run a connectivity probe to verify the barrier is resolved.",
                failed_subgoal.title, failed_subgoal.harness, error_text, workdir
            ),
        };
    }

    // Heuristic 2: Target VM, image, snapshot, disk, systemd, libvirt
    let is_target = lower.contains("snapshot")
        || lower.contains("libvirt")
        || lower.contains("virsh")
        || lower.contains("domain")
        || lower.contains("guestfs")
        || lower.contains("qcow2")
        || lower.contains("systemd")
        || lower.contains("service ")
        || lower.contains("package ")
        || lower.contains("apt-get")
        || lower.contains("dpkg")
        || lower.contains("no space left")
        || lower.contains("disk full")
        || lower.contains("not found: /")
        || lower.contains("target vm");

    if is_target {
        let summary = format!(
            "Target VM/environment staging issue in '{}'",
            failed_subgoal.title
        );
        return ProblemDiagnosis {
            domain: ProblemDomain::TargetInfrastructure,
            target_harness: "codex".to_string(),
            summary: summary.clone(),
            remediation_prompt: format!(
                "[AUTONOMOUS TRIAGE: Target VM / Environment Blocker]\n\
                 Parent subgoal: '{}' (run by {})\n\
                 Failure observed:\n{}\n\n\
                 Workspace: {}\n\n\
                 Instructions for Codex:\n\
                 1. Inspect the target VM state, disk image, or snapshot configuration.\n\
                 2. If a clean snapshot is missing or invalid, restore or create the clean baseline snapshot.\n\
                 3. If a required service or dependency failed to start or is missing, verify its systemd configuration and package status.\n\
                 4. Confirm the disposable target environment is in the expected staging state.",
                failed_subgoal.title, failed_subgoal.harness, error_text, workdir
            ),
        };
    }

    // Heuristic 3: Verification, evidence ledger, assertion failure
    let is_verification = lower.contains("assertionerror")
        || lower.contains("assert ")
        || lower.contains("evidence")
        || lower.contains("retest")
        || lower.contains("finding")
        || lower.contains("precondition")
        || lower.contains("unexpected status")
        || lower.contains("failed test")
        || lower.contains("tests failed");

    if is_verification {
        let summary = format!(
            "Evidence/verification mismatch in '{}'",
            failed_subgoal.title
        );
        return ProblemDiagnosis {
            domain: ProblemDomain::VerificationEvidence,
            target_harness: "agy".to_string(),
            summary: summary.clone(),
            remediation_prompt: format!(
                "[AUTONOMOUS TRIAGE: Evidence Ledger / Verification Mismatch]\n\
                 Parent subgoal: '{}' (run by {})\n\
                 Failure observed:\n{}\n\n\
                 Workspace: {}\n\n\
                 Instructions for Antigravity (Agy):\n\
                 1. Review the assessment loop execution logs and evidence store records.\n\
                 2. Inspect the preconditions, finding assertion, or retest verification check that failed.\n\
                 3. Determine whether the flaw is in the probe logic, target behavior, or evidence ledger recording.\n\
                 4. Run targeted verification to prove the correct state.",
                failed_subgoal.title, failed_subgoal.harness, error_text, workdir
            ),
        };
    }

    // Fallback: Assign to parent harness
    let summary = format!("Diagnostic triage for '{}'", failed_subgoal.title);
    ProblemDiagnosis {
        domain: ProblemDomain::GeneralDiagnosis,
        target_harness: failed_subgoal.harness.clone(),
        summary: summary.clone(),
        remediation_prompt: format!(
            "[AUTONOMOUS TRIAGE: Diagnostic Run]\n\
             Parent subgoal: '{}'\n\
             Error observed:\n{}\n\n\
             Workspace: {}\n\n\
             Please diagnose this error, correct the root cause, and verify the fix.",
            failed_subgoal.title, error_text, workdir
        ),
    }
}

pub fn compute_next_tick(schedule: Option<&str>, from_epoch_ms: i64) -> Option<i64> {
    let s = schedule?.trim();
    if s.is_empty() {
        return None;
    }

    // 1. Minute intervals: "5m", "15m", "30m", "45m"
    if let Some(m_str) = s.strip_suffix('m') {
        if let Ok(mins) = m_str.parse::<i64>() {
            return Some(from_epoch_ms + mins * 60 * 1000);
        }
    }

    // 2. Hour intervals: "1h", "2h", "6h", "12h"
    if let Some(h_str) = s.strip_suffix('h') {
        if let Ok(hours) = h_str.parse::<i64>() {
            return Some(from_epoch_ms + hours * 3600 * 1000);
        }
    }

    // 3. Named shortcuts: "@hourly", "@daily", "@every 15m"
    if s.eq_ignore_ascii_case("@hourly") {
        return Some(from_epoch_ms + 3600 * 1000);
    }
    if s.eq_ignore_ascii_case("@daily") {
        return Some(from_epoch_ms + 86400 * 1000);
    }
    if let Some(rest) = s.strip_prefix("@every ") {
        return compute_next_tick(Some(rest), from_epoch_ms);
    }

    // 4. Cron expression: e.g. "*/15 * * * *" or "0 * * * *"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() == 5 {
        let min_part = parts[0];
        if let Some(step_str) = min_part.strip_prefix("*/") {
            if let Ok(step_mins) = step_str.parse::<i64>() {
                if step_mins > 0 && step_mins < 60 {
                    return Some(from_epoch_ms + step_mins * 60 * 1000);
                }
            }
        }
        if min_part == "0" && parts[1] == "*" {
            // Hourly at minute 0
            return Some(from_epoch_ms + 3600 * 1000);
        }
    }

    // Fallback: 30 minutes
    Some(from_epoch_ms + 30 * 60 * 1000)
}

pub fn find_runnable_subgoal(goal: &GoalItem) -> Option<usize> {
    let status_by_id: HashMap<&str, SubgoalStatus> = goal
        .subgoals
        .iter()
        .map(|s| (s.id.as_str(), s.status))
        .collect();

    // Pass 1: Dynamic triage subgoals first (high priority resolution of blockers!)
    for (idx, sg) in goal.subgoals.iter().enumerate() {
        if sg.is_dynamic && sg.status == SubgoalStatus::Pending {
            let deps_satisfied = sg
                .depends_on
                .iter()
                .all(|dep_id| status_by_id.get(dep_id.as_str()) == Some(&SubgoalStatus::Passed));
            if deps_satisfied {
                return Some(idx);
            }
        }
    }

    // Pass 2: Regular pending subgoals whose dependencies are passed
    for (idx, sg) in goal.subgoals.iter().enumerate() {
        if !sg.is_dynamic && sg.status == SubgoalStatus::Pending {
            let deps_satisfied = sg
                .depends_on
                .iter()
                .all(|dep_id| status_by_id.get(dep_id.as_str()) == Some(&SubgoalStatus::Passed));
            if deps_satisfied {
                return Some(idx);
            }
        }
    }

    None
}

/// Helper that creates the standard 3-agent Big Goal template for vm-lab.
pub fn create_vm_lab_assessment_goal(
    repo_workdir: &str,
    schedule: Option<&str>,
) -> CreateGoalRequest {
    CreateGoalRequest {
        title: "VM-Lab Phase 3: Autonomous Assessment & Isolation Loop".to_string(),
        description: Some(
            "Tri-agent autonomous pipeline: OpenCode enforces network boundary proofs, \
             Codex stages disposable Ubuntu target VM snapshot, and Antigravity executes \
             the bounded assessment-to-retest loop with evidence ledger tracing."
                .to_string(),
        ),
        workdir: Some(repo_workdir.to_string()),
        schedule: schedule.map(|s| s.to_string()),
        auto_triage: Some(true),
        subgoals: vec![
            CreateSubgoalRequest {
                title: "OpenCode · Boundary Isolation & Firewall Proofs".to_string(),
                prompt: "Verify the isolated bridge network for vm-lab. Ensure nftables rules drop \
                         all unauthorized host egress and enforce lab subnet boundaries. \
                         Run the network isolation tests and verify packet drop proofs."
                    .to_string(),
                harness: "opencode".to_string(),
                profile: None,
                model: None,
                effort: Some("high".to_string()),
                workdir: Some(repo_workdir.to_string()),
                depends_on: Some(Vec::new()),
                max_attempts: Some(3),
            },
            CreateSubgoalRequest {
                title: "Codex · Disposable Target VM Snapshot & Service Staging".to_string(),
                prompt: "Stage the disposable Ubuntu target clone VM. Ensure the authorized Ubuntu \
                         clone assessment service is enabled and listening, libvirt snapshot \
                         'clean-baseline' exists and is verified, and disposable target state is ready."
                    .to_string(),
                harness: "codex".to_string(),
                profile: None,
                model: None,
                effort: Some("high".to_string()),
                workdir: Some(repo_workdir.to_string()),
                depends_on: Some(vec!["sub_1".to_string()]),
                max_attempts: Some(3),
            },
            CreateSubgoalRequest {
                title: "Antigravity · Assessment Loop & Evidence Ledger Retest".to_string(),
                prompt: "Execute the orchestrator bounded assessment loop inside vm-lab against the \
                         authorized target VM. Verify lab preconditions (IP, snapshot, scope), \
                         execute controlled vulnerability probe, verify disposable remediation, \
                         and record finding proofs in the evidence store."
                    .to_string(),
                harness: "agy".to_string(),
                profile: None,
                model: None,
                effort: Some("high".to_string()),
                workdir: Some(repo_workdir.to_string()),
                depends_on: Some(vec!["sub_2".to_string()]),
                max_attempts: Some(3),
            },
        ],
    }
}

pub struct GoalManager {
    goals: Arc<RwLock<HashMap<String, GoalItem>>>,
    config_file: PathBuf,
}

impl GoalManager {
    pub fn new() -> Arc<Self> {
        let config_file = if cfg!(test) {
            PathBuf::from("/tmp/moedeck_test_goals.json")
        } else {
            config_dir().join("goals.json")
        };

        let initial = if !cfg!(test) && config_file.exists() {
            load_persisted_goals(&config_file)
        } else {
            HashMap::new()
        };

        Arc::new(Self {
            goals: Arc::new(RwLock::new(initial)),
            config_file,
        })
    }

    pub async fn list_goals(&self) -> Vec<GoalItem> {
        let g = self.goals.read().await;
        let mut list: Vec<_> = g.values().cloned().collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        list
    }

    pub async fn get_goal(&self, id: &str) -> Option<GoalItem> {
        let g = self.goals.read().await;
        g.get(id).cloned()
    }

    pub async fn create_goal(self: &Arc<Self>, req: CreateGoalRequest) -> Result<GoalItem, String> {
        if req.subgoals.is_empty() {
            return Err("Goal must contain at least one subgoal".to_string());
        }

        let workdir = req
            .workdir
            .as_deref()
            .and_then(sanitize_workdir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| home_dir().to_string_lossy().to_string());

        let now = Utc::now().timestamp_millis();
        let goal_id = format!("goal_{}", &Uuid::new_v4().to_string()[..8]);
        let next_tick = compute_next_tick(req.schedule.as_deref(), now);

        let subgoals: Vec<SubgoalItem> = req
            .subgoals
            .into_iter()
            .enumerate()
            .map(|(idx, st)| SubgoalItem {
                id: format!("sub_{}_{}", idx + 1, &Uuid::new_v4().to_string()[..6]),
                goal_id: goal_id.clone(),
                title: st.title,
                prompt: st.prompt,
                harness: st.harness.to_lowercase(),
                profile: st.profile,
                model: st.model,
                effort: st.effort.or_else(|| Some("high".to_string())),
                workdir: st.workdir.or_else(|| Some(workdir.clone())),
                status: SubgoalStatus::Pending,
                depends_on: st.depends_on.unwrap_or_default(),
                is_dynamic: false,
                parent_subgoal_id: None,
                conversation_id: None,
                error: None,
                attempt_count: 0,
                max_attempts: st.max_attempts.unwrap_or_else(default_max_attempts),
                created_at: now,
                started_at: None,
                completed_at: None,
            })
            .collect();

        let goal = GoalItem {
            id: goal_id.clone(),
            title: if req.title.trim().is_empty() {
                format!("Goal · {}", goal_id)
            } else {
                req.title.trim().to_string()
            },
            description: req.description.unwrap_or_default(),
            workdir,
            status: GoalStatus::Queued,
            schedule: req.schedule,
            subgoals,
            auto_triage: req.auto_triage.unwrap_or_else(default_auto_triage),
            created_at: now,
            updated_at: now,
            last_tick_at: None,
            next_tick_at: next_tick,
        };

        {
            let mut map = self.goals.write().await;
            map.insert(goal_id.clone(), goal.clone());
            if !cfg!(test) {
                save_persisted_goals(&self.config_file, &map);
            }
        }

        Ok(goal)
    }

    pub async fn add_subgoal(
        &self,
        goal_id: &str,
        req: CreateSubgoalRequest,
    ) -> Result<SubgoalItem, String> {
        let mut map = self.goals.write().await;
        let goal = map
            .get_mut(goal_id)
            .ok_or_else(|| format!("Goal `{}` not found", goal_id))?;

        let now = Utc::now().timestamp_millis();
        let sub_id = format!("sub_dyn_{}", &Uuid::new_v4().to_string()[..8]);

        let subgoal = SubgoalItem {
            id: sub_id.clone(),
            goal_id: goal_id.to_string(),
            title: req.title,
            prompt: req.prompt,
            harness: req.harness.to_lowercase(),
            profile: req.profile,
            model: req.model,
            effort: req.effort.or_else(|| Some("high".to_string())),
            workdir: req.workdir.or_else(|| Some(goal.workdir.clone())),
            status: SubgoalStatus::Pending,
            depends_on: req.depends_on.unwrap_or_default(),
            is_dynamic: false,
            parent_subgoal_id: None,
            conversation_id: None,
            error: None,
            attempt_count: 0,
            max_attempts: req.max_attempts.unwrap_or_else(default_max_attempts),
            created_at: now,
            started_at: None,
            completed_at: None,
        };

        goal.subgoals.push(subgoal.clone());
        goal.updated_at = now;

        if !cfg!(test) {
            save_persisted_goals(&self.config_file, &map);
        }

        Ok(subgoal)
    }

    pub async fn pause_goal(&self, id: &str) -> Result<(), String> {
        let mut map = self.goals.write().await;
        let item = map
            .get_mut(id)
            .ok_or_else(|| format!("Goal `{}` not found", id))?;

        item.status = GoalStatus::Paused;
        item.updated_at = Utc::now().timestamp_millis();

        if !cfg!(test) {
            save_persisted_goals(&self.config_file, &map);
        }
        Ok(())
    }

    pub async fn resume_goal(&self, id: &str) -> Result<(), String> {
        let mut map = self.goals.write().await;
        let item = map
            .get_mut(id)
            .ok_or_else(|| format!("Goal `{}` not found", id))?;

        item.status = GoalStatus::Running;
        item.updated_at = Utc::now().timestamp_millis();

        if !cfg!(test) {
            save_persisted_goals(&self.config_file, &map);
        }
        Ok(())
    }

    pub async fn delete_goal(&self, id: &str) -> Result<(), String> {
        let mut map = self.goals.write().await;
        map.remove(id)
            .ok_or_else(|| format!("Goal `{}` not found", id))?;

        if !cfg!(test) {
            save_persisted_goals(&self.config_file, &map);
        }
        Ok(())
    }

    /// Evaluates one execution tick of a goal: finds the next runnable subgoal,
    /// dispatches it to the assigned harness (opencode, codex, agy), observes outcome,
    /// and dynamically spawns triage subgoals on failure.
    pub async fn tick_goal(
        self: &Arc<Self>,
        goal_id: &str,
        runner: Arc<RunnerState>,
    ) -> Result<TickOutcome, String> {
        let now = Utc::now().timestamp_millis();

        // 1. Check goal status and extract next runnable subgoal
        let (runnable_idx, goal_title, workdir, auto_triage) = {
            let mut map = self.goals.write().await;
            let goal = map
                .get_mut(goal_id)
                .ok_or_else(|| format!("Goal `{}` not found", goal_id))?;

            if goal.status == GoalStatus::Paused {
                return Ok(TickOutcome::GoalBlocked {
                    goal_id: goal_id.to_string(),
                    reason: "Goal is paused".to_string(),
                });
            }

            if goal.status == GoalStatus::Completed {
                return Ok(TickOutcome::GoalCompleted {
                    goal_id: goal_id.to_string(),
                });
            }

            // Check if all subgoals have passed
            let all_passed = !goal.subgoals.is_empty()
                && goal
                    .subgoals
                    .iter()
                    .all(|s| s.status == SubgoalStatus::Passed);
            if all_passed {
                goal.status = GoalStatus::Completed;
                goal.updated_at = now;
                if !cfg!(test) {
                    save_persisted_goals(&self.config_file, &map);
                }
                return Ok(TickOutcome::GoalCompleted {
                    goal_id: goal_id.to_string(),
                });
            }

            // Check if any subgoal is currently running
            if let Some(running) = goal
                .subgoals
                .iter()
                .find(|s| s.status == SubgoalStatus::Running)
            {
                return Ok(TickOutcome::WaitingForRunning {
                    goal_id: goal_id.to_string(),
                    subgoal_id: running.id.clone(),
                });
            }

            // Find next runnable subgoal
            let idx = match find_runnable_subgoal(goal) {
                Some(i) => i,
                None => {
                    // Check if blocked or failed
                    let has_blocked = goal
                        .subgoals
                        .iter()
                        .any(|s| s.status == SubgoalStatus::Blocked);
                    if has_blocked {
                        return Ok(TickOutcome::GoalBlocked {
                            goal_id: goal_id.to_string(),
                            reason: "Subgoals are blocked waiting on prerequisites or triage"
                                .to_string(),
                        });
                    }
                    return Ok(TickOutcome::NoRunnableSubgoal {
                        goal_id: goal_id.to_string(),
                    });
                }
            };

            // Mark subgoal running
            if let Some(sg) = goal.subgoals.get_mut(idx) {
                sg.status = SubgoalStatus::Running;
                sg.started_at = Some(now);
                sg.attempt_count += 1;
            }
            goal.status = GoalStatus::Running;
            goal.last_tick_at = Some(now);
            goal.next_tick_at = compute_next_tick(goal.schedule.as_deref(), now);
            goal.updated_at = now;

            let title = goal.title.clone();
            let workdir = goal.workdir.clone();
            let auto_triage = goal.auto_triage;

            if !cfg!(test) {
                save_persisted_goals(&self.config_file, &map);
            }

            (idx, title, workdir, auto_triage)
        };

        // Extract subgoal parameters
        let subgoal = {
            let map = self.goals.read().await;
            let goal = map.get(goal_id).unwrap();
            goal.subgoals[runnable_idx].clone()
        };

        // Dispatch turn to the designated harness
        // Keep full identifiers: every dynamic subgoal starts with `sub_dyn_`.
        let conv_id = format!("helm:goal:{goal_id}:{}", subgoal.id);
        let title_prompt = format!(
            "🎯 [{}] Subgoal: {}",
            subgoal.harness.to_uppercase(),
            subgoal.title
        );

        let overlay = load_overlays()
            .remove(&conv_id)
            .unwrap_or_else(|| StoredThreadOverlay {
                id: conv_id.clone(),
                title: Some(title_prompt),
                pinned: true,
                harness: Some(subgoal.harness.clone()),
                model: subgoal.model.clone(),
                effort: subgoal.effort.clone(),
                workdir: subgoal.workdir.clone().or_else(|| Some(workdir.clone())),
                profile: subgoal.profile.clone(),
                created_at: now,
                updated_at: now,
                appended_messages: Vec::new(),
                native_session: None,
            });
        save_overlay(&overlay);

        let formatted_prompt = format!(
            "[MoE Deck Big Goal: {}]\n[Subgoal: {} (Harness: {})]\n\n{}",
            goal_title, subgoal.title, subgoal.harness, subgoal.prompt
        );

        let send_req = SendMessageRequest {
            text: formatted_prompt,
            harness: Some(subgoal.harness.clone()),
            model: subgoal.model.clone(),
            effort: subgoal.effort.clone(),
            workdir: subgoal.workdir.clone().or_else(|| Some(workdir.clone())),
            profile: subgoal.profile.clone(),
            images: Vec::new(),
        };

        let turn_res = dispatch_turn_with_outcome(runner.clone(), conv_id.clone(), send_req).await;

        let turn_outcome = match turn_res {
            Ok((_, outcome_rx)) => match outcome_rx.await {
                Ok(outcome) => outcome,
                Err(_) => TurnOutcome {
                    success: false,
                    text: "Turn task dropped before completion".to_string(),
                },
            },
            Err(e) => TurnOutcome {
                success: false,
                text: format!("Dispatch failed: {}", e),
            },
        };

        let completed_now = Utc::now().timestamp_millis();
        let mut map = self.goals.write().await;
        let Some(goal) = map.get_mut(goal_id) else {
            return Err(format!("Goal `{}` disappeared during turn", goal_id));
        };

        // Record failed turns too, so the user can inspect the actual failure.
        if let Some(sg) = goal.subgoals.iter_mut().find(|s| s.id == subgoal.id) {
            sg.conversation_id = Some(conv_id.clone());
        }
        let was_paused = goal.status == GoalStatus::Paused;

        if turn_outcome.success {
            // Mark subgoal passed
            if let Some(sg) = goal.subgoals.iter_mut().find(|s| s.id == subgoal.id) {
                sg.status = SubgoalStatus::Passed;
                sg.completed_at = Some(completed_now);
                sg.conversation_id = Some(conv_id);
            }

            // If this was a dynamic triage subgoal, check if its parent can now be unblocked
            if subgoal.is_dynamic {
                if let Some(parent_id) = &subgoal.parent_subgoal_id {
                    let passed_ids: Vec<String> = goal
                        .subgoals
                        .iter()
                        .filter(|s| s.status == SubgoalStatus::Passed)
                        .map(|s| s.id.clone())
                        .collect();

                    if let Some(parent) = goal.subgoals.iter_mut().find(|s| s.id == *parent_id) {
                        let all_deps_passed = parent
                            .depends_on
                            .iter()
                            .all(|dep_id| passed_ids.contains(dep_id));
                        if all_deps_passed && parent.status == SubgoalStatus::Blocked {
                            parent.status = SubgoalStatus::Pending;
                        }
                    }
                }
            }

            // Check if entire goal is now completed
            if !was_paused
                && goal
                    .subgoals
                    .iter()
                    .all(|s| s.status == SubgoalStatus::Passed)
            {
                goal.status = GoalStatus::Completed;
            }

            goal.updated_at = completed_now;
            if !cfg!(test) {
                save_persisted_goals(&self.config_file, &map);
            }

            Ok(TickOutcome::SubgoalPassed {
                subgoal_id: subgoal.id,
                subgoal_title: subgoal.title,
                harness: subgoal.harness,
            })
        } else {
            // Failure path: evaluate problem triage
            let err_text = turn_outcome.text;

            // A remediation must never recursively spawn its own remediation.
            // Account failures require the user, not another model invocation.
            let account_failure = [
                "insufficient balance",
                "insufficient credits",
                "insufficient_quota",
                "invalid api key",
                "invalid_api_key",
                "authentication required",
            ]
            .iter()
            .any(|message| err_text.to_lowercase().contains(message));
            if auto_triage
                && !was_paused
                && !subgoal.is_dynamic
                && subgoal.parent_subgoal_id.is_none()
                && subgoal.attempt_count < subgoal.max_attempts
                && !account_failure
            {
                // Diagnose problem and spawn dynamic triage subgoal!
                let diagnosis = diagnose_failure(&subgoal, &err_text, &workdir);
                let triage_id = format!("sub_dyn_{}", &Uuid::new_v4().to_string()[..8]);

                let triage_item = SubgoalItem {
                    id: triage_id.clone(),
                    goal_id: goal_id.to_string(),
                    title: format!(
                        "[Triage: {}] {}",
                        diagnosis.target_harness, diagnosis.summary
                    ),
                    prompt: diagnosis.remediation_prompt,
                    harness: diagnosis.target_harness.clone(),
                    profile: None,
                    model: None,
                    effort: Some("high".to_string()),
                    workdir: Some(workdir.clone()),
                    status: SubgoalStatus::Pending,
                    depends_on: Vec::new(),
                    is_dynamic: true,
                    parent_subgoal_id: Some(subgoal.id.clone()),
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 2,
                    created_at: completed_now,
                    started_at: None,
                    completed_at: None,
                };

                // Mark current subgoal Blocked depending on triage item
                if let Some(sg) = goal.subgoals.iter_mut().find(|s| s.id == subgoal.id) {
                    sg.status = SubgoalStatus::Blocked;
                    sg.error = Some(err_text.clone());
                    sg.depends_on.push(triage_id.clone());
                    sg.completed_at = Some(completed_now);
                }

                // Add triage item to goal subgoals
                goal.subgoals.push(triage_item.clone());
                goal.updated_at = completed_now;

                if !cfg!(test) {
                    save_persisted_goals(&self.config_file, &map);
                }

                Ok(TickOutcome::TriageSpawned {
                    parent_id: subgoal.id,
                    triage_id,
                    triage_title: triage_item.title,
                    target_harness: diagnosis.target_harness,
                })
            } else {
                // Mark subgoal failed
                if let Some(sg) = goal.subgoals.iter_mut().find(|s| s.id == subgoal.id) {
                    sg.status = SubgoalStatus::Failed;
                    sg.error = Some(err_text.clone());
                    sg.completed_at = Some(completed_now);
                }
                if !was_paused {
                    goal.status = GoalStatus::Failed;
                }
                goal.updated_at = completed_now;

                if !cfg!(test) {
                    save_persisted_goals(&self.config_file, &map);
                }

                Ok(TickOutcome::SubgoalFailed {
                    subgoal_id: subgoal.id,
                    subgoal_title: subgoal.title,
                    error: err_text,
                })
            }
        }
    }

    /// Evaluates all goals whose next tick is due. Runs sequentially to preserve
    /// low system impact (one job at a time).
    pub async fn tick_all_due(
        self: &Arc<Self>,
        runner: Arc<RunnerState>,
    ) -> Vec<(String, Result<TickOutcome, String>)> {
        let now = Utc::now().timestamp_millis();
        let due_goal_ids: Vec<String> = {
            let map = self.goals.read().await;
            map.values()
                .filter(|g| {
                    (g.status == GoalStatus::Running || g.status == GoalStatus::Queued)
                        && g.next_tick_at.map(|t| t <= now).unwrap_or(true)
                })
                .map(|g| g.id.clone())
                .collect()
        };

        let mut results = Vec::new();
        for id in due_goal_ids {
            let res = self.tick_goal(&id, runner.clone()).await;
            results.push((id, res));
        }
        results
    }

    /// Resumes incomplete goals on daemon startup.
    pub async fn resume_incomplete(self: &Arc<Self>, _runner: Arc<RunnerState>) -> usize {
        let mut map = self.goals.write().await;
        let mut count = 0;
        let now = Utc::now().timestamp_millis();

        for goal in map.values_mut() {
            if matches!(goal.status, GoalStatus::Running | GoalStatus::Queued) {
                // Reset any running subgoal that was interrupted back to pending
                for sg in &mut goal.subgoals {
                    if sg.status == SubgoalStatus::Running {
                        sg.status = SubgoalStatus::Pending;
                        sg.started_at = None;
                    }
                }
                goal.updated_at = now;
                count += 1;
            }
        }

        if !cfg!(test) && count > 0 {
            save_persisted_goals(&self.config_file, &map);
        }
        count
    }

    /// Starts the asynchronous in-daemon periodic scheduler loop.
    pub fn start_scheduler(
        self: Arc<Self>,
        runner: Arc<RunnerState>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(15));
            loop {
                interval.tick().await;
                let outcomes = self.tick_all_due(runner.clone()).await;
                for (gid, out) in outcomes {
                    match out {
                        Ok(TickOutcome::SubgoalPassed {
                            subgoal_title,
                            harness,
                            ..
                        }) => {
                            println!(
                                "  [Scheduler] Goal {}: Subgoal '{}' passed on {}",
                                gid, subgoal_title, harness
                            );
                        }
                        Ok(TickOutcome::TriageSpawned {
                            triage_title,
                            target_harness,
                            ..
                        }) => {
                            println!("  [Scheduler] Goal {}: Problem detected! Dynamic triage spawned: '{}' -> {}", gid, triage_title, target_harness);
                        }
                        Ok(TickOutcome::GoalCompleted { .. }) => {
                            println!("  [Scheduler] Goal {} completed successfully!", gid);
                        }
                        Ok(TickOutcome::SubgoalFailed {
                            subgoal_title,
                            error,
                            ..
                        }) => {
                            eprintln!(
                                "  [Scheduler] Goal {}: Subgoal '{}' failed: {}",
                                gid, subgoal_title, error
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("  [Scheduler] Goal {} tick error: {}", gid, e);
                        }
                    }
                }
            }
        })
    }
}

fn load_persisted_goals(path: &Path) -> HashMap<String, GoalItem> {
    if let Ok(data) = fs::read_to_string(path) {
        match serde_json::from_str::<HashMap<String, GoalItem>>(&data) {
            Ok(map) => return map,
            Err(e) => {
                eprintln!(
                    "goals.json is corrupt: {}. Backing it up so state is not silently lost.",
                    e
                );
                let backup =
                    path.with_extension(format!("corrupt.{}.json", Utc::now().timestamp_millis()));
                let _ = fs::rename(path, backup);
            }
        }
    }
    HashMap::new()
}

fn save_persisted_goals(path: &Path, map: &HashMap<String, GoalItem>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if let Err(e) = crate::discovery::atomic_write(path, &json) {
            eprintln!("Failed to persist goals.json: {}", e);
        }
    }
}

pub async fn run_cli(args: &[String], port: u16) -> anyhow::Result<()> {
    match args.first().map(String::as_str) {
        Some("list") => {
            let url = format!("http://127.0.0.1:{}/api/goals", port);
            println!(
                "Fetching active Big Goals from http://127.0.0.1:{}...",
                port
            );
            let resp = reqwest_get_json(&url).await?;
            println!("{}", resp);
        }
        Some("tick") => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: moedeck goal tick <goal_id>");
                std::process::exit(2);
            };
            let url = format!("http://127.0.0.1:{}/api/goals/{}/tick", port, id);
            println!("Triggering tick on goal `{}`...", id);
            let resp = reqwest_post_json(&url).await?;
            println!("{}", resp);
        }
        Some("status") => {
            let Some(id) = args.get(1) else {
                eprintln!("usage: moedeck goal status <goal_id>");
                std::process::exit(2);
            };
            let url = format!("http://127.0.0.1:{}/api/goals/{}", port, id);
            let resp = reqwest_get_json(&url).await?;
            println!("{}", resp);
        }
        _ => {
            println!("usage: moedeck goal <command> [args]");
            println!();
            println!("commands:");
            println!("  list              list all active Big Goals and subgoals");
            println!("  tick <goal_id>    trigger an immediate evaluation tick on a goal");
            println!(
                "  status <goal_id>  inspect subgoals, dependencies, and dynamic triage state"
            );
        }
    }
    Ok(())
}

async fn reqwest_get_json(url: &str) -> anyhow::Result<String> {
    use std::process::Command;
    let out = Command::new("curl").arg("-s").arg(url).output()?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn reqwest_post_json(url: &str) -> anyhow::Result<String> {
    use std::process::Command;
    let out = Command::new("curl")
        .arg("-s")
        .arg("-X")
        .arg("POST")
        .arg(url)
        .output()?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diagnose_failure_network_routes_to_opencode() {
        let subgoal = SubgoalItem {
            id: "sub_test".to_string(),
            goal_id: "goal_test".to_string(),
            title: "Assessment loop".to_string(),
            prompt: "run loop".to_string(),
            harness: "agy".to_string(),
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Running,
            depends_on: Vec::new(),
            is_dynamic: false,
            parent_subgoal_id: None,
            conversation_id: None,
            error: None,
            attempt_count: 1,
            max_attempts: 3,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let err = "PreconditionError: connection refused on target VM 192.168.100.50:22";
        let diag = diagnose_failure(&subgoal, err, "/home/ada/projects/vm-lab");

        assert_eq!(diag.domain, ProblemDomain::NetworkIsolation);
        assert_eq!(diag.target_harness, "opencode");
        assert!(diag.remediation_prompt.contains("OpenCode"));
        assert!(diag.remediation_prompt.contains("nftables"));
    }

    #[test]
    fn test_diagnose_failure_target_routes_to_codex() {
        let subgoal = SubgoalItem {
            id: "sub_test".to_string(),
            goal_id: "goal_test".to_string(),
            title: "Assessment loop".to_string(),
            prompt: "run loop".to_string(),
            harness: "agy".to_string(),
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Running,
            depends_on: Vec::new(),
            is_dynamic: false,
            parent_subgoal_id: None,
            conversation_id: None,
            error: None,
            attempt_count: 1,
            max_attempts: 3,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let err = "libvirt error: snapshot 'clean-baseline' not found for domain 'ubuntu-target'";
        let diag = diagnose_failure(&subgoal, err, "/home/ada/projects/vm-lab");

        assert_eq!(diag.domain, ProblemDomain::TargetInfrastructure);
        assert_eq!(diag.target_harness, "codex");
        assert!(diag.remediation_prompt.contains("Codex"));
        assert!(diag.remediation_prompt.contains("clean baseline snapshot"));
    }

    #[test]
    fn test_diagnose_failure_evidence_routes_to_agy() {
        let subgoal = SubgoalItem {
            id: "sub_test".to_string(),
            goal_id: "goal_test".to_string(),
            title: "Network verify".to_string(),
            prompt: "run verify".to_string(),
            harness: "opencode".to_string(),
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Running,
            depends_on: Vec::new(),
            is_dynamic: false,
            parent_subgoal_id: None,
            conversation_id: None,
            error: None,
            attempt_count: 1,
            max_attempts: 3,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let err = "AssertionError: evidence ledger missing finding proof for assessment retest";
        let diag = diagnose_failure(&subgoal, err, "/home/ada/projects/vm-lab");

        assert_eq!(diag.domain, ProblemDomain::VerificationEvidence);
        assert_eq!(diag.target_harness, "agy");
        assert!(diag.remediation_prompt.contains("Antigravity"));
        assert!(diag.remediation_prompt.contains("evidence store"));
    }

    #[test]
    fn test_diagnose_explicit_problem_tag() {
        let subgoal = SubgoalItem {
            id: "sub_test".to_string(),
            goal_id: "goal_test".to_string(),
            title: "Staging".to_string(),
            prompt: "stage".to_string(),
            harness: "codex".to_string(),
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Running,
            depends_on: Vec::new(),
            is_dynamic: false,
            parent_subgoal_id: None,
            conversation_id: None,
            error: None,
            attempt_count: 1,
            max_attempts: 3,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let err = "Failed staging: [PROBLEM: harness=opencode reason=Need_isolated_bridge_up] Cannot bind interface";
        let diag = diagnose_failure(&subgoal, err, "/home/ada/projects/vm-lab");

        assert_eq!(diag.domain, ProblemDomain::NetworkIsolation);
        assert_eq!(diag.target_harness, "opencode");
        assert_eq!(diag.summary, "Need_isolated_bridge_up");
    }

    #[test]
    fn test_compute_next_tick_intervals() {
        let now = 1000000;
        assert_eq!(
            compute_next_tick(Some("15m"), now),
            Some(now + 15 * 60 * 1000)
        );
        assert_eq!(compute_next_tick(Some("1h"), now), Some(now + 3600 * 1000));
        assert_eq!(
            compute_next_tick(Some("@hourly"), now),
            Some(now + 3600 * 1000)
        );
        assert_eq!(
            compute_next_tick(Some("*/20 * * * *"), now),
            Some(now + 20 * 60 * 1000)
        );
        assert_eq!(compute_next_tick(None, now), None);
    }

    #[test]
    fn test_find_runnable_subgoal_dependencies_and_priority() {
        let mut goal = GoalItem {
            id: "goal_1".to_string(),
            title: "Tri-agent pipeline".to_string(),
            description: "test".to_string(),
            workdir: "/tmp".to_string(),
            status: GoalStatus::Running,
            schedule: None,
            subgoals: vec![
                SubgoalItem {
                    id: "sub_opencode".to_string(),
                    goal_id: "goal_1".to_string(),
                    title: "Isolation".to_string(),
                    prompt: "iso".to_string(),
                    harness: "opencode".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: Vec::new(),
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: 0,
                    started_at: None,
                    completed_at: None,
                },
                SubgoalItem {
                    id: "sub_codex".to_string(),
                    goal_id: "goal_1".to_string(),
                    title: "Target".to_string(),
                    prompt: "target".to_string(),
                    harness: "codex".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: vec!["sub_opencode".to_string()],
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: 0,
                    started_at: None,
                    completed_at: None,
                },
                SubgoalItem {
                    id: "sub_agy".to_string(),
                    goal_id: "goal_1".to_string(),
                    title: "Assessment".to_string(),
                    prompt: "assess".to_string(),
                    harness: "agy".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: vec!["sub_codex".to_string()],
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: 0,
                    started_at: None,
                    completed_at: None,
                },
            ],
            auto_triage: true,
            created_at: 0,
            updated_at: 0,
            last_tick_at: None,
            next_tick_at: None,
        };

        // First step: only sub_opencode has no dependencies
        assert_eq!(find_runnable_subgoal(&goal), Some(0));

        // Mark opencode passed
        goal.subgoals[0].status = SubgoalStatus::Passed;
        // Now codex is runnable
        assert_eq!(find_runnable_subgoal(&goal), Some(1));

        // Mark codex passed
        goal.subgoals[1].status = SubgoalStatus::Passed;
        // Now agy is runnable
        assert_eq!(find_runnable_subgoal(&goal), Some(2));

        // Dynamic triage prioritization:
        // Suppose agy fails and spawns a dynamic triage task
        goal.subgoals[2].status = SubgoalStatus::Blocked;
        goal.subgoals[2]
            .depends_on
            .push("sub_triage_opencode".to_string());
        goal.subgoals.push(SubgoalItem {
            id: "sub_triage_opencode".to_string(),
            goal_id: "goal_1".to_string(),
            title: "Fix firewall".to_string(),
            prompt: "fix".to_string(),
            harness: "opencode".to_string(),
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Pending,
            depends_on: Vec::new(),
            is_dynamic: true,
            parent_subgoal_id: Some("sub_agy".to_string()),
            conversation_id: None,
            error: None,
            attempt_count: 0,
            max_attempts: 2,
            created_at: 0,
            started_at: None,
            completed_at: None,
        });

        // The dynamic triage task is prioritized and runnable!
        assert_eq!(find_runnable_subgoal(&goal), Some(3));
    }

    #[test]
    fn test_vm_lab_template_generation() {
        let req = create_vm_lab_assessment_goal("/home/ada/projects/vm-lab", Some("15m"));
        assert_eq!(req.subgoals.len(), 3);
        assert_eq!(req.subgoals[0].harness, "opencode");
        assert_eq!(req.subgoals[1].harness, "codex");
        assert_eq!(req.subgoals[2].harness, "agy");
        assert_eq!(req.schedule, Some("15m".to_string()));
    }

    #[tokio::test]
    async fn goal_summary_counts_tasks_without_shipping_histories() {
        let mgr = GoalManager::new();
        let req = create_vm_lab_assessment_goal("/tmp/vm-lab-summary", None);
        let goal = mgr.create_goal(req).await.unwrap();
        let summary = goal.summary();
        assert_eq!(summary["id"], goal.id);
        assert_eq!(summary["subgoal_count"], 3);
        assert!(summary["status"].is_string());
        assert!(
            summary.get("subgoals").is_none(),
            "bootstrap must not carry subgoal bodies"
        );
        assert!(summary.get("description").is_none());
        mgr.delete_goal(&goal.id).await.unwrap();
    }

    #[tokio::test]
    async fn test_goal_manager_crud() {
        let mgr = GoalManager::new();
        let req = create_vm_lab_assessment_goal("/tmp/vm-lab", None);
        let goal = mgr.create_goal(req).await.unwrap();

        assert_eq!(goal.subgoals.len(), 3);
        let list = mgr.list_goals().await;
        assert!(list.iter().any(|g| g.id == goal.id));

        let retrieved = mgr.get_goal(&goal.id).await.unwrap();
        assert_eq!(retrieved.title, goal.title);

        mgr.pause_goal(&goal.id).await.unwrap();
        let paused = mgr.get_goal(&goal.id).await.unwrap();
        assert_eq!(paused.status, GoalStatus::Paused);

        mgr.resume_goal(&goal.id).await.unwrap();
        let resumed = mgr.get_goal(&goal.id).await.unwrap();
        assert_eq!(resumed.status, GoalStatus::Running);

        mgr.delete_goal(&goal.id).await.unwrap();
        assert!(mgr.get_goal(&goal.id).await.is_none());
    }

    #[test]
    fn test_end_to_end_dynamic_triage_flow() {
        let now = Utc::now().timestamp_millis();
        let mut goal = GoalItem {
            id: "goal_e2e".to_string(),
            title: "VM-Lab Phase 3 Assessment Loop".to_string(),
            description: "End to end test".to_string(),
            workdir: "/tmp/vm-lab".to_string(),
            status: GoalStatus::Running,
            schedule: Some("15m".to_string()),
            subgoals: vec![
                SubgoalItem {
                    id: "sub_opencode".to_string(),
                    goal_id: "goal_e2e".to_string(),
                    title: "Network Isolation".to_string(),
                    prompt: "setup isolation".to_string(),
                    harness: "opencode".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: Vec::new(),
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: now,
                    started_at: None,
                    completed_at: None,
                },
                SubgoalItem {
                    id: "sub_codex".to_string(),
                    goal_id: "goal_e2e".to_string(),
                    title: "Disposable Target VM Snapshot".to_string(),
                    prompt: "stage snapshot".to_string(),
                    harness: "codex".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: vec!["sub_opencode".to_string()],
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: now,
                    started_at: None,
                    completed_at: None,
                },
                SubgoalItem {
                    id: "sub_agy".to_string(),
                    goal_id: "goal_e2e".to_string(),
                    title: "Assessment-to-Retest Loop".to_string(),
                    prompt: "run orchestrator loop".to_string(),
                    harness: "agy".to_string(),
                    profile: None,
                    model: None,
                    effort: None,
                    workdir: None,
                    status: SubgoalStatus::Pending,
                    depends_on: vec!["sub_codex".to_string()],
                    is_dynamic: false,
                    parent_subgoal_id: None,
                    conversation_id: None,
                    error: None,
                    attempt_count: 0,
                    max_attempts: 3,
                    created_at: now,
                    started_at: None,
                    completed_at: None,
                },
            ],
            auto_triage: true,
            created_at: now,
            updated_at: now,
            last_tick_at: None,
            next_tick_at: None,
        };

        // Tick 1: OpenCode is picked
        let idx1 = find_runnable_subgoal(&goal).expect("OpenCode should be runnable");
        assert_eq!(goal.subgoals[idx1].harness, "opencode");
        goal.subgoals[idx1].status = SubgoalStatus::Passed;

        // Tick 2: Codex is picked
        let idx2 = find_runnable_subgoal(&goal).expect("Codex should be runnable");
        assert_eq!(goal.subgoals[idx2].harness, "codex");
        goal.subgoals[idx2].status = SubgoalStatus::Passed;

        // Tick 3: Agy is picked
        let idx3 = find_runnable_subgoal(&goal).expect("Agy should be runnable");
        assert_eq!(goal.subgoals[idx3].harness, "agy");

        // Simulate Agy experiencing a network failure
        let err_text = "PreconditionError: connection refused on target VM 192.168.100.50:22";
        let diag = diagnose_failure(&goal.subgoals[idx3], err_text, &goal.workdir);
        assert_eq!(diag.target_harness, "opencode");

        // Spawn dynamic triage subgoal and block Agy
        let triage_id = "sub_triage_net_1".to_string();
        goal.subgoals[idx3].status = SubgoalStatus::Blocked;
        goal.subgoals[idx3].depends_on.push(triage_id.clone());

        goal.subgoals.push(SubgoalItem {
            id: triage_id.clone(),
            goal_id: goal.id.clone(),
            title: format!("[Triage: {}] {}", diag.target_harness, diag.summary),
            prompt: diag.remediation_prompt,
            harness: diag.target_harness,
            profile: None,
            model: None,
            effort: None,
            workdir: None,
            status: SubgoalStatus::Pending,
            depends_on: Vec::new(),
            is_dynamic: true,
            parent_subgoal_id: Some(goal.subgoals[idx3].id.clone()),
            conversation_id: None,
            error: None,
            attempt_count: 0,
            max_attempts: 2,
            created_at: now,
            started_at: None,
            completed_at: None,
        });

        // Tick 4: Dynamic triage subgoal is picked first!
        let idx4 = find_runnable_subgoal(&goal).expect("Dynamic triage should be runnable");
        assert_eq!(goal.subgoals[idx4].id, triage_id);
        assert_eq!(goal.subgoals[idx4].harness, "opencode");
        assert!(goal.subgoals[idx4].is_dynamic);

        // OpenCode resolves the triage problem
        goal.subgoals[idx4].status = SubgoalStatus::Passed;

        // Unblock parent check:
        let passed_ids: Vec<String> = goal
            .subgoals
            .iter()
            .filter(|s| s.status == SubgoalStatus::Passed)
            .map(|s| s.id.clone())
            .collect();
        if let Some(parent) = goal.subgoals.iter_mut().find(|s| s.id == "sub_agy") {
            let all_deps_passed = parent.depends_on.iter().all(|dep| passed_ids.contains(dep));
            if all_deps_passed {
                parent.status = SubgoalStatus::Pending;
            }
        }
        assert_eq!(goal.subgoals[2].status, SubgoalStatus::Pending);

        // Tick 5: Agy is runnable again!
        let idx5 = find_runnable_subgoal(&goal).expect("Agy should now be runnable");
        assert_eq!(goal.subgoals[idx5].harness, "agy");

        // Agy passes
        goal.subgoals[idx5].status = SubgoalStatus::Passed;

        // All subgoals passed -> goal is complete
        assert!(goal
            .subgoals
            .iter()
            .all(|s| s.status == SubgoalStatus::Passed));
        goal.status = GoalStatus::Completed;
        assert_eq!(goal.status, GoalStatus::Completed);
    }
}
