use crate::discovery::{sanitize_workdir, truncate_str};
use crate::models::{DevPortItem, PrStatusInfo, WorktreeItem};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn sanitize_slug(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                '\0'
            }
        })
        .filter(|c| *c != '\0')
        .take(40)
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub fn list_worktrees(workdir: &str) -> Vec<WorktreeItem> {
    let Some(canon) = sanitize_workdir(workdir) else {
        return Vec::new();
    };
    let clean_dir = canon.to_string_lossy().to_string();
    let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("worktree")
        .arg("list")
        .arg("--porcelain")
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_worktree_porcelain(&text)
}

pub fn parse_worktree_porcelain(text: &str) -> Vec<WorktreeItem> {
    let mut items = Vec::new();
    let mut cur_path = String::new();
    let mut cur_head = String::new();
    let mut cur_branch = String::new();

    for line in text.lines().chain(std::iter::once("")) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !cur_path.is_empty() {
                let is_main = items.is_empty();
                items.push(WorktreeItem {
                    path: std::mem::take(&mut cur_path),
                    branch: if cur_branch.is_empty() {
                        "detached".to_string()
                    } else {
                        std::mem::take(&mut cur_branch)
                    },
                    head: cur_head.chars().take(7).collect(),
                    is_main,
                });
                cur_head.clear();
            }
            continue;
        }
        if let Some(p) = trimmed.strip_prefix("worktree ") {
            cur_path = p.trim().to_string();
        } else if let Some(h) = trimmed.strip_prefix("HEAD ") {
            cur_head = h.trim().to_string();
        } else if let Some(b) = trimmed.strip_prefix("branch ") {
            cur_branch = b
                .trim()
                .strip_prefix("refs/heads/")
                .unwrap_or(b.trim())
                .to_string();
        }
    }
    items
}

pub fn create_worktree(workdir: &str, raw_slug: &str) -> Result<WorktreeItem, String> {
    let slug = sanitize_slug(raw_slug)
        .ok_or_else(|| "Worktree name must contain alphanumeric characters".to_string())?;
    let canon = sanitize_workdir(workdir)
        .ok_or_else(|| "Invalid or unauthorized repository path".to_string())?;
    let clean_dir = canon.to_string_lossy().to_string();

    // Find top-level git root
    let root_out = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("rev-parse")
        .arg("--show-toplevel")
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;
    if !root_out.status.success() {
        return Err("Directory is not a git repository".to_string());
    }
    let repo_root = Path::new(String::from_utf8_lossy(&root_out.stdout).trim()).to_path_buf();
    let repo_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .trim_end_matches("-wt");
    let parent_dir = repo_root
        .parent()
        .ok_or_else(|| "Cannot determine parent directory for worktree".to_string())?;

    let wt_base = parent_dir.join(format!("{}-wt", repo_name));
    let _ = fs::create_dir_all(&wt_base);
    let wt_path = wt_base.join(&slug);
    let branch_name = format!("feat/{}", slug);

    if wt_path.exists() {
        return Ok(WorktreeItem {
            path: wt_path.to_string_lossy().to_string(),
            branch: branch_name,
            head: "existing".to_string(),
            is_main: false,
        });
    }

    // Try creating with a new branch `feat/<slug>`, or attach to existing branch if it already exists
    let add_out = Command::new("git")
        .arg("-C")
        .arg(&repo_root)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch_name)
        .arg(&wt_path)
        .output()
        .map_err(|e| format!("Failed to spawn git worktree add: {}", e))?;

    if !add_out.status.success() {
        let fallback_out = Command::new("git")
            .arg("-C")
            .arg(&repo_root)
            .arg("worktree")
            .arg("add")
            .arg(&wt_path)
            .arg(&branch_name)
            .output()
            .map_err(|e| format!("Failed to spawn fallback git worktree add: {}", e))?;
        if !fallback_out.status.success() {
            let err_msg = String::from_utf8_lossy(&add_out.stderr).trim().to_string();
            return Err(format!("git worktree add failed: {}", err_msg));
        }
    }

    Ok(WorktreeItem {
        path: wt_path.to_string_lossy().to_string(),
        branch: branch_name,
        head: "HEAD".to_string(),
        is_main: false,
    })
}

pub fn remove_worktree(workdir: &str, worktree_path: &str) -> Result<(), String> {
    let canon_repo =
        sanitize_workdir(workdir).ok_or_else(|| "Unauthorized repository path".to_string())?;
    let canon_wt =
        sanitize_workdir(worktree_path).ok_or_else(|| "Unauthorized worktree path".to_string())?;
    let wt_str = canon_wt.to_string_lossy().to_string();
    if !wt_str.contains("-wt/") {
        return Err(
            "Refusing to remove main repository checkout (only *-wt/* worktrees can be removed)"
                .to_string(),
        );
    }

    let out = Command::new("git")
        .arg("-C")
        .arg(canon_repo)
        .arg("worktree")
        .arg("remove")
        .arg("--force")
        .arg(&canon_wt)
        .output()
        .map_err(|e| format!("Failed to run git worktree remove: {}", e))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("git worktree remove failed: {}", err));
    }
    Ok(())
}

pub fn inspect_pr_status(workdir: &str) -> PrStatusInfo {
    let Some(canon) = sanitize_workdir(workdir) else {
        return empty_pr_status();
    };
    if !canon.join(".git").exists() {
        return empty_pr_status();
    }
    let out = Command::new("gh")
        .current_dir(&canon)
        .arg("pr")
        .arg("view")
        .arg("--json")
        .arg("number,title,url,state,statusCheckRollup")
        .output();

    let Ok(output) = out else {
        return empty_pr_status();
    };
    if !output.status.success() {
        return empty_pr_status();
    }
    let json_text = String::from_utf8_lossy(&output.stdout);
    parse_pr_status_json(&json_text)
}

pub fn parse_pr_status_json(json_text: &str) -> PrStatusInfo {
    let Ok(val) = serde_json::from_str::<Value>(json_text) else {
        return empty_pr_status();
    };
    let number = val.get("number").and_then(|v| v.as_u64());
    if number.is_none() {
        return empty_pr_status();
    }
    let title = val
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let url = val
        .get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let state = val
        .get("state")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut total = 0usize;
    let mut passing = 0usize;
    let mut failing = 0usize;
    let mut pending = 0usize;

    if let Some(checks) = val.get("statusCheckRollup").and_then(|v| v.as_array()) {
        for c in checks {
            total += 1;
            let status = c
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_uppercase();
            let conclusion = c
                .get("conclusion")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_uppercase();
            let state_str = c
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_uppercase();

            if conclusion == "SUCCESS"
                || conclusion == "NEUTRAL"
                || conclusion == "SKIPPED"
                || state_str == "SUCCESS"
            {
                passing += 1;
            } else if conclusion == "FAILURE"
                || conclusion == "TIMED_OUT"
                || conclusion == "CANCELLED"
                || conclusion == "ACTION_REQUIRED"
                || state_str == "FAILURE"
                || state_str == "ERROR"
            {
                failing += 1;
            } else {
                let _ = status;
                pending += 1;
            }
        }
    }

    let (ci_state, checks_summary) = if total == 0 {
        ("none".to_string(), "No CI checks".to_string())
    } else if failing > 0 {
        (
            "failing".to_string(),
            format!("{}/{} failing", failing, total),
        )
    } else if pending > 0 {
        (
            "pending".to_string(),
            format!("{}/{} passing ({} running)", passing, total, pending),
        )
    } else {
        (
            "passing".to_string(),
            format!("{}/{} checks passing", passing, total),
        )
    };

    PrStatusInfo {
        has_pr: true,
        number,
        title,
        url,
        state,
        ci_state,
        checks_summary,
    }
}

fn empty_pr_status() -> PrStatusInfo {
    PrStatusInfo {
        has_pr: false,
        number: None,
        title: None,
        url: None,
        state: None,
        ci_state: "none".to_string(),
        checks_summary: "No open PR for current branch".to_string(),
    }
}

pub fn execute_pr_action(workdir: &str, action: &str) -> Result<String, String> {
    let canon = sanitize_workdir(workdir)
        .ok_or_else(|| "Invalid or unauthorized workspace path".to_string())?;
    match action {
        "merge" => {
            let out = Command::new("gh")
                .current_dir(&canon)
                .arg("pr")
                .arg("merge")
                .arg("--squash")
                .arg("--delete-branch")
                .output()
                .map_err(|e| format!("Failed to invoke gh pr merge: {}", e))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(format!("PR merge failed: {}", err));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        "create-pr" => {
            let _ = Command::new("git")
                .current_dir(&canon)
                .arg("push")
                .arg("-u")
                .arg("origin")
                .arg("HEAD")
                .output();
            let out = Command::new("gh")
                .current_dir(&canon)
                .arg("pr")
                .arg("create")
                .arg("--fill")
                .output()
                .map_err(|e| format!("Failed to invoke gh pr create: {}", e))?;
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                return Err(format!("PR creation failed: {}", err));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        "failed-logs" => {
            let list_out = Command::new("gh")
                .current_dir(&canon)
                .arg("run")
                .arg("list")
                .arg("--limit")
                .arg("1")
                .arg("--json")
                .arg("databaseId,conclusion,name")
                .output()
                .map_err(|e| format!("Failed to list workflow runs: {}", e))?;
            if !list_out.status.success() {
                return Err("Could not query GitHub Actions runs".to_string());
            }
            let arr: Value = serde_json::from_slice(&list_out.stdout)
                .map_err(|_| "Failed to parse gh run list JSON".to_string())?;
            let run_id = arr
                .as_array()
                .and_then(|a| a.first())
                .and_then(|r| r.get("databaseId"))
                .and_then(|v| v.as_u64())
                .ok_or_else(|| "No recent GitHub Actions run found".to_string())?;

            let log_out = Command::new("gh")
                .current_dir(&canon)
                .arg("run")
                .arg("view")
                .arg(run_id.to_string())
                .arg("--log-failed")
                .output()
                .map_err(|e| format!("Failed to fetch failed CI logs: {}", e))?;
            let raw_logs = String::from_utf8_lossy(&log_out.stdout).trim().to_string();
            if raw_logs.is_empty() {
                return Err("No failed step logs returned by GitHub Actions".to_string());
            }
            Ok(truncate_str(&raw_logs, 8000))
        }
        _ => Err(format!("Unsupported PR action: {}", action)),
    }
}

pub fn list_listening_ports() -> Vec<DevPortItem> {
    if let Ok(out) = Command::new("ss").arg("-tlnp").output() {
        if out.status.success() {
            let text = String::from_utf8_lossy(&out.stdout);
            let parsed = parse_ss_output(&text);
            if !parsed.is_empty() {
                return parsed;
            }
        }
    }
    Vec::new()
}

pub fn parse_ss_output(text: &str) -> Vec<DevPortItem> {
    let mut map: BTreeMap<u16, DevPortItem> = BTreeMap::new();
    // Skip internal infrastructure ports like 7770 (MoE Deck itself), 22 (SSH), 53 (DNS), 631 (CUPS)
    let ignored_ports = [22u16, 53, 631, 7770, 20128];

    for line in text.lines() {
        if !line.starts_with("LISTEN") {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 4 {
            continue;
        }
        let local_addr = cols[3];
        let Some(port_str) = local_addr.rsplit(':').next() else {
            continue;
        };
        let Ok(port) = port_str.parse::<u16>() else {
            continue;
        };
        if port < 1024 || ignored_ports.contains(&port) {
            continue;
        }

        let proc_name = line
            .split("users:((\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or("server")
            .to_string();

        let label = match port {
            3000..=3010 => format!("Web App ({})", proc_name),
            4173 | 5173..=5180 => format!("Vite Dev ({})", proc_name),
            8000..=8090 => format!("HTTP Server ({})", proc_name),
            8888 => format!("Notebook / Dev ({})", proc_name),
            _ => format!("{} (:{} )", proc_name, port),
        };

        map.entry(port).or_insert(DevPortItem {
            port,
            process: proc_name,
            label,
            page_title: None,
            http_status: None,
            preview_path: Some(format!("/preview/{}/", port)),
        });
    }

    map.into_values().collect()
}

pub fn merge_worktree_branch(workdir: &str, branch: &str) -> Result<String, String> {
    let Some(canon) = sanitize_workdir(workdir) else {
        return Err("Invalid workdir".to_string());
    };
    let clean_dir = canon.to_string_lossy().to_string();

    let out = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("merge")
        .arg("--no-ff")
        .arg(branch)
        .arg("-m")
        .arg(format!("Merge swarm branch `{}`", branch))
        .output()
        .map_err(|e| format!("Failed to invoke git merge: {}", e))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git merge failed: {}", err.trim()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.trim().to_string())
}

pub fn inspect_branch_diff(workdir: &str, branch: &str) -> Result<String, String> {
    let Some(canon) = sanitize_workdir(workdir) else {
        return Err("Invalid workdir".to_string());
    };
    let clean_dir = canon.to_string_lossy().to_string();

    let out = Command::new("git")
        .arg("-C")
        .arg(&clean_dir)
        .arg("diff")
        .arg(format!("HEAD...{}", branch))
        .output()
        .map_err(|e| format!("Failed to invoke git diff: {}", e))?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git diff failed: {}", err.trim()));
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_slug() {
        assert_eq!(
            sanitize_slug("Fix Water Shader!"),
            Some("fix-water-shader".to_string())
        );
        assert_eq!(sanitize_slug("../../../etc"), Some("etc".to_string()));
        assert_eq!(sanitize_slug("   "), None);
    }

    #[test]
    fn test_parse_worktree_porcelain() {
        let sample = "worktree /home/ada/projects/example\nHEAD 112233445566\nbranch refs/heads/main\n\nworktree /home/ada/projects/example-wt/water\nHEAD aabbccddeeff\nbranch refs/heads/feat/water\n\n";
        let list = parse_worktree_porcelain(sample);
        assert_eq!(list.len(), 2);
        assert!(list[0].is_main);
        assert_eq!(list[0].branch, "main");
        assert!(!list[1].is_main);
        assert_eq!(list[1].branch, "feat/water");
        assert_eq!(list[1].head, "aabbccd");
    }

    #[test]
    fn test_parse_pr_status_json() {
        let sample = r#"{
            "number": 5,
            "title": "fix(security): harden LAN auth",
            "url": "https://github.com/example/project/pull/5",
            "state": "OPEN",
            "statusCheckRollup": [
                {"status": "COMPLETED", "conclusion": "SUCCESS"}
            ]
        }"#;
        let info = parse_pr_status_json(sample);
        assert!(info.has_pr);
        assert_eq!(info.number, Some(5));
        assert_eq!(info.ci_state, "passing");
        assert_eq!(info.checks_summary, "1/1 checks passing");
    }

    #[test]
    fn test_parse_ss_output() {
        let sample = "LISTEN 0 4096 0.0.0.0:5173 0.0.0.0:* users:((\"node\",pid=1234,fd=19))\nLISTEN 0 4096 0.0.0.0:7770 0.0.0.0:* users:((\"helm\",pid=99,fd=9))\n";
        let ports = parse_ss_output(sample);
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 5173);
        assert_eq!(ports[0].process, "node");
    }
}
