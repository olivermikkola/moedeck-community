use crate::discovery::{config_dir, home_dir, sanitize_workdir, save_overlay};
use crate::models::{
    CreateSprintRequest, CreateSprintStep, SendMessageRequest, SprintItem, SprintStatus,
    SprintStep, StepStatus, StoredThreadOverlay,
};
use crate::runner::{dispatch_turn_with_outcome, RunnerState, TurnOutcome};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Parses a Markdown task checklist into discrete executable sprint steps.
pub fn parse_markdown_checklist_to_steps(text: &str) -> Vec<CreateSprintStep> {
    let mut steps = Vec::new();
    for line in text.lines() {
        let tr = line.trim();
        if tr.is_empty() {
            continue;
        }
        let cleaned = if let Some(rest) = tr.strip_prefix("- [ ]") {
            rest.trim()
        } else if let Some(rest) = tr
            .strip_prefix("- [x]")
            .or_else(|| tr.strip_prefix("- [X]"))
        {
            rest.trim()
        } else if let Some(rest) = tr.strip_prefix("- ").or_else(|| tr.strip_prefix("* ")) {
            rest.trim()
        } else if let Some(pos) = tr.find(". ") {
            let prefix = &tr[..pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                tr[pos + 2..].trim()
            } else {
                tr
            }
        } else {
            tr
        };

        if !cleaned.is_empty() {
            steps.push(CreateSprintStep {
                name: cleaned.to_string(),
                prompt: cleaned.to_string(),
            });
        }
    }

    if steps.is_empty() && !text.trim().is_empty() {
        steps.push(CreateSprintStep {
            name: text.trim().to_string(),
            prompt: text.trim().to_string(),
        });
    }

    steps
}

#[derive(Clone)]
pub struct SprintManager {
    sprints: Arc<RwLock<HashMap<String, SprintItem>>>,
    config_file: PathBuf,
}

impl SprintManager {
    pub fn new() -> Arc<Self> {
        let config_file = if cfg!(test) {
            PathBuf::from("/tmp/helm_test_sprints.json")
        } else {
            config_dir().join("sprints.json")
        };

        let initial = if !cfg!(test) && config_file.exists() {
            load_persisted_sprints(&config_file)
        } else {
            HashMap::new()
        };
        Arc::new(Self {
            sprints: Arc::new(RwLock::new(initial)),
            config_file,
        })
    }

    pub async fn list_sprints(&self) -> Vec<SprintItem> {
        let s = self.sprints.read().await;
        let mut list: Vec<_> = s.values().cloned().collect();
        list.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        list
    }

    pub async fn get_sprint(&self, id: &str) -> Option<SprintItem> {
        let s = self.sprints.read().await;
        s.get(id).cloned()
    }

    pub async fn create_sprint(
        self: &Arc<Self>,
        req: CreateSprintRequest,
        runner: Arc<RunnerState>,
    ) -> Result<SprintItem, String> {
        if req.steps.is_empty() {
            return Err("Sprint must contain at least one step".to_string());
        }

        let workdir = req
            .workdir
            .as_deref()
            .and_then(sanitize_workdir)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| home_dir().to_string_lossy().to_string());

        let harness = req
            .harness
            .unwrap_or_else(|| "claude".to_string())
            .to_lowercase();
        let now = Utc::now().timestamp_millis();
        let sprint_id = format!("sprint_{}", &Uuid::new_v4().to_string()[..8]);

        // Optional account binding: only honoured when the profile exists and
        // belongs to the sprint's harness.
        let profile = req
            .profile
            .as_deref()
            .and_then(crate::profiles::sanitize_profile_id)
            .filter(|pid| {
                crate::profiles::get_profile(pid)
                    .map(|p| p.harness == harness)
                    .unwrap_or(false)
            });

        let steps: Vec<SprintStep> = req
            .steps
            .into_iter()
            .map(|st| SprintStep {
                id: Uuid::new_v4().to_string(),
                name: st.name,
                prompt: st.prompt,
                status: StepStatus::Pending,
                conversation_id: None,
                started_at: None,
                completed_at: None,
                error: None,
            })
            .collect();

        let sprint = SprintItem {
            id: sprint_id.clone(),
            title: if req.title.trim().is_empty() {
                format!("Sprint · {}", sprint_id)
            } else {
                req.title.trim().to_string()
            },
            workdir,
            harness,
            profile,
            model: req.model,
            steps,
            status: SprintStatus::Queued,
            created_at: now,
            updated_at: now,
        };

        {
            let mut map = self.sprints.write().await;
            map.insert(sprint_id.clone(), sprint.clone());
            if !cfg!(test) {
                save_persisted_sprints(&self.config_file, &map);
            }
        }

        // Spawn background autonomous execution loop
        let this = self.clone();
        tokio::spawn(async move {
            this.execute_sprint_loop(&sprint_id, runner, false).await;
        });

        Ok(sprint)
    }

    pub async fn cancel_sprint(&self, id: &str) -> Result<(), String> {
        let mut map = self.sprints.write().await;
        let item = map
            .get_mut(id)
            .ok_or_else(|| format!("Sprint `{}` not found", id))?;

        item.status = SprintStatus::Cancelled;
        item.updated_at = Utc::now().timestamp_millis();

        for step in &mut item.steps {
            if step.status == StepStatus::Running || step.status == StepStatus::Pending {
                step.status = StepStatus::Skipped;
            }
        }

        if !cfg!(test) {
            save_persisted_sprints(&self.config_file, &map);
        }
        Ok(())
    }

    pub async fn delete_sprint(&self, id: &str) -> Result<(), String> {
        let mut map = self.sprints.write().await;
        map.remove(id)
            .ok_or_else(|| format!("Sprint `{}` not found", id))?;

        if !cfg!(test) {
            save_persisted_sprints(&self.config_file, &map);
        }
        Ok(())
    }

    /// Re-arm sprints that were queued or running when the daemon restarted.
    /// Called once at boot; returns how many sprints were resumed.
    pub async fn resume_incomplete(self: &Arc<Self>, runner: Arc<RunnerState>) -> usize {
        let ids: Vec<String> = {
            let mut map = self.sprints.write().await;
            let ids: Vec<String> = map
                .values()
                .filter(|s| matches!(s.status, SprintStatus::Queued | SprintStatus::Running))
                .map(|s| s.id.clone())
                .collect();
            if ids.is_empty() {
                return 0;
            }
            for id in &ids {
                if let Some(s) = map.get_mut(id) {
                    s.status = SprintStatus::Running;
                    s.updated_at = Utc::now().timestamp_millis();
                }
            }
            ids
        };

        let count = ids.len();
        if !cfg!(test) {
            let map = self.sprints.read().await;
            save_persisted_sprints(&self.config_file, &map);
        }
        for id in ids {
            let this = self.clone();
            let runner = runner.clone();
            tokio::spawn(async move {
                this.execute_sprint_loop(&id, runner, true).await;
            });
        }
        count
    }

    async fn execute_sprint_loop(&self, sprint_id: &str, runner: Arc<RunnerState>, resume: bool) {
        {
            let mut map = self.sprints.write().await;
            if let Some(s) = map.get_mut(sprint_id) {
                s.status = SprintStatus::Running;
                s.updated_at = Utc::now().timestamp_millis();
            }
            if !cfg!(test) {
                save_persisted_sprints(&self.config_file, &map);
            }
        }

        let sprint_meta = {
            let map = self.sprints.read().await;
            map.get(sprint_id).cloned()
        };
        let Some(sprint) = sprint_meta else { return };

        // Create a single shared conversation thread for this sprint
        let conv_id = format!("helm:sprint:{}", &Uuid::new_v4().to_string()[..8]);
        let now = Utc::now().timestamp_millis();
        let overlay = StoredThreadOverlay {
            id: conv_id.clone(),
            title: Some(format!("🏃 {}", sprint.title)),
            pinned: true,
            harness: Some(sprint.harness.clone()),
            model: sprint.model.clone(),
            effort: Some("high".to_string()),
            workdir: Some(sprint.workdir.clone()),
            profile: sprint.profile.clone(),
            created_at: now,
            updated_at: now,
            appended_messages: Vec::new(),
            native_session: None,
        };
        save_overlay(&overlay);

        let total_steps = sprint.steps.len();

        for step_idx in 0..total_steps {
            // Check if sprint was cancelled or already passed (on resume)
            {
                let map = self.sprints.read().await;
                let s = match map.get(sprint_id) {
                    Some(s) => s,
                    None => return,
                };
                if s.status == SprintStatus::Cancelled {
                    return;
                }
                if resume && s.steps.get(step_idx).map(|s| s.status) == Some(StepStatus::Passed) {
                    continue;
                }
            }

            // Mark step running
            {
                let mut map = self.sprints.write().await;
                if let Some(s) = map.get_mut(sprint_id) {
                    if let Some(step) = s.steps.get_mut(step_idx) {
                        step.status = StepStatus::Running;
                        step.started_at = Some(Utc::now().timestamp_millis());
                        step.conversation_id = Some(conv_id.clone());
                    }
                    s.updated_at = Utc::now().timestamp_millis();
                }
                if !cfg!(test) {
                    save_persisted_sprints(&self.config_file, &map);
                }
            }

            // Sprint may have been deleted while between locks — bail out
            // instead of panicking on `unwrap()`.
            let (step_name, step_prompt) = match self
                .sprints
                .read()
                .await
                .get(sprint_id)
                .and_then(|s| s.steps.get(step_idx))
            {
                Some(st) => (st.name.clone(), st.prompt.clone()),
                None => return,
            };

            let formatted_prompt = format!(
                "[Sprint Step {}/{}: {}]\n{}",
                step_idx + 1,
                total_steps,
                step_name,
                step_prompt
            );

            let send_req = SendMessageRequest {
                text: formatted_prompt,
                harness: Some(sprint.harness.clone()),
                model: sprint.model.clone(),
                effort: Some("high".to_string()),
                workdir: Some(sprint.workdir.clone()),
                profile: sprint.profile.clone(),
                images: Vec::new(),
            };

            let turn_res =
                dispatch_turn_with_outcome(runner.clone(), conv_id.clone(), send_req).await;

            // Wait for the real turn outcome (process exit status) instead of
            // guessing from a busy-state string — see dispatch_turn_with_outcome.
            let turn_outcome = match turn_res {
                Ok((_, outcome_rx)) => match outcome_rx.await {
                    Ok(outcome) => outcome,
                    Err(_) => TurnOutcome {
                        success: false,
                        text: "Turn task dropped before completion".to_string(),
                    },
                },
                Err(e) => {
                    let mut map = self.sprints.write().await;
                    if let Some(s) = map.get_mut(sprint_id) {
                        if let Some(step) = s.steps.get_mut(step_idx) {
                            step.status = StepStatus::Failed;
                            step.error = Some(e);
                            step.completed_at = Some(Utc::now().timestamp_millis());
                        }
                        s.status = SprintStatus::Failed;
                        s.updated_at = Utc::now().timestamp_millis();
                    }
                    if !cfg!(test) {
                        save_persisted_sprints(&self.config_file, &map);
                    }
                    return;
                }
            };
            let step_success = turn_outcome.success;
            let turn_error = if step_success {
                None
            } else {
                Some(turn_outcome.text)
            };

            // Mark step passed/failed
            {
                let mut map = self.sprints.write().await;
                if let Some(s) = map.get_mut(sprint_id) {
                    if let Some(step) = s.steps.get_mut(step_idx) {
                        step.status = if step_success {
                            StepStatus::Passed
                        } else {
                            StepStatus::Failed
                        };
                        step.error = turn_error.clone();
                        step.completed_at = Some(Utc::now().timestamp_millis());
                    }
                    if !step_success {
                        s.status = SprintStatus::Failed;
                    }
                    s.updated_at = Utc::now().timestamp_millis();
                }
                if !cfg!(test) {
                    save_persisted_sprints(&self.config_file, &map);
                }
            }

            if !step_success {
                return;
            }
        }

        // Mark sprint completed
        {
            let mut map = self.sprints.write().await;
            if let Some(s) = map.get_mut(sprint_id) {
                s.status = SprintStatus::Completed;
                s.updated_at = Utc::now().timestamp_millis();
            }
            if !cfg!(test) {
                save_persisted_sprints(&self.config_file, &map);
            }
        }
    }
}

fn load_persisted_sprints(path: &Path) -> HashMap<String, SprintItem> {
    if let Ok(data) = fs::read_to_string(path) {
        match serde_json::from_str::<HashMap<String, SprintItem>>(&data) {
            Ok(map) => return map,
            Err(e) => {
                eprintln!(
                    "sprints.json is corrupt: {}. Backing it up so state is not silently lost.",
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

fn save_persisted_sprints(path: &Path, map: &HashMap<String, SprintItem>) {
    if let Ok(json) = serde_json::to_string_pretty(map) {
        if let Err(e) = crate::discovery::atomic_write(path, &json) {
            eprintln!("Failed to persist sprints.json: {}", e);
        }
    }
}

#[cfg(test)]
#[test]
fn test_atomic_persisted_sprint_write_round_trip() {
    use crate::models::SprintStep;
    use std::path::PathBuf as PB;

    let path = PB::from("/tmp/helm_test_sprints_atomic.json");
    let _ = fs::remove_file(&path);

    let now = Utc::now().timestamp_millis();
    let mut map = HashMap::new();
    map.insert(
        "sprint_x".to_string(),
        SprintItem {
            id: "sprint_x".to_string(),
            title: "Persist test".to_string(),
            workdir: "/home".to_string(),
            harness: "claude".to_string(),
            profile: None,
            model: None,
            steps: vec![SprintStep {
                id: "st1".to_string(),
                name: "a".to_string(),
                prompt: "a".to_string(),
                status: StepStatus::Pending,
                conversation_id: None,
                started_at: None,
                completed_at: None,
                error: None,
            }],
            status: SprintStatus::Queued,
            created_at: now,
            updated_at: now,
        },
    );

    save_persisted_sprints(&path, &map);
    // A persisted write must not leave tmp debris behind.
    let dir = path.parent().unwrap().read_dir().unwrap();
    assert!(dir
        .filter_map(|e| e.ok())
        .all(|e| !e.file_name().to_string_lossy().starts_with(".tmp.")));

    let loaded = load_persisted_sprints(&path);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded["sprint_x"].title, "Persist test");

    // Corrupt file: must not silently pretend everything is fine.
    fs::write(&path, "{ not json").unwrap();
    assert_eq!(
        load_persisted_sprints(&path).len(),
        0,
        "corrupt file should be quarantined, not silent-empty"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_markdown_checklist_to_steps() {
        let md = "- [ ] Step 1: Implement shadow VFS\n- [x] Step 2: Write unit tests\n3. Step 3: Run clippy and format\n* Step 4: Create PR";
        let steps = parse_markdown_checklist_to_steps(md);
        assert_eq!(steps.len(), 4);
        assert_eq!(steps[0].name, "Step 1: Implement shadow VFS");
        assert_eq!(steps[1].name, "Step 2: Write unit tests");
        assert_eq!(steps[2].name, "Step 3: Run clippy and format");
        assert_eq!(steps[3].name, "Step 4: Create PR");
    }

    #[tokio::test]
    async fn test_sprint_manager_lifecycle() {
        let mgr = SprintManager::new();
        let runner = RunnerState::new();

        let req = CreateSprintRequest {
            title: "Test Sprint".to_string(),
            workdir: None,
            harness: Some("claude".to_string()),
            model: None,
            profile: None,
            steps: vec![CreateSprintStep {
                name: "Step A".to_string(),
                prompt: "Hello".to_string(),
            }],
        };

        let sprint = mgr.create_sprint(req, runner).await.unwrap();
        assert_eq!(sprint.title, "Test Sprint");
        assert_eq!(sprint.steps.len(), 1);

        let retrieved = mgr.get_sprint(&sprint.id).await;
        assert!(retrieved.is_some());

        mgr.cancel_sprint(&sprint.id).await.unwrap();
        let cancelled = mgr.get_sprint(&sprint.id).await.unwrap();
        assert_eq!(cancelled.status, SprintStatus::Cancelled);
    }
}
