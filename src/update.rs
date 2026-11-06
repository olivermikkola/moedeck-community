//! Update MoE Deck from the phone: build the source checkout's `origin/main` in a
//! private worktree, swap the binary in place, and restart by re-exec.
//! The user's own checkout and branch are never touched.
use crate::runner::RunnerState;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// The commit this binary was built from (empty outside a git checkout).
pub const BUILD_SHA: &str = env!("MOEDECK_GIT_SHA");

#[derive(Clone, Serialize)]
pub struct Status {
    /// "idle" | "building" | "waiting" | "restarting" | "failed"
    pub phase: &'static str,
    pub message: String,
    /// Tail of the build output when something failed.
    pub log: String,
    pub current: &'static str,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    phase: "idle",
    message: String::new(),
    log: String::new(),
    current: BUILD_SHA,
});
/// "Restart now" while waiting for chats to finish.
static FORCE: AtomicBool = AtomicBool::new(false);

pub fn status() -> Status {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set(phase: &'static str, message: impl Into<String>, log: String) {
    let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    s.phase = phase;
    s.message = message.into();
    s.log = log;
}

fn source_dir() -> PathBuf {
    crate::discovery::setting("SOURCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

/// Downloaded release binaries point at the CI machine's checkout, which does
/// not exist here; say where new versions come from instead of a git error.
fn require_checkout(src: &Path) -> Result<(), String> {
    if src.join(".git").exists() {
        Ok(())
    } else {
        Err(format!(
            "This copy was installed from a download, so it can't update itself yet. \
             Get the newest version at {}/releases",
            env!("CARGO_PKG_REPOSITORY")
        ))
    }
}

fn work_dir() -> PathBuf {
    crate::discovery::config_dir().join("update")
}

async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("Could not run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[derive(Serialize)]
pub struct Check {
    pub current: &'static str,
    pub latest: String,
    pub up_to_date: bool,
    /// `abc1234 subject` lines, newest first.
    pub commits: Vec<String>,
    pub status: Status,
}

pub async fn check() -> Result<Check, String> {
    let src = source_dir();
    require_checkout(&src)?;
    git(&src, &["fetch", "--quiet", "origin", "main"]).await?;
    let latest = git(&src, &["rev-parse", "origin/main"]).await?;
    // Same content counts as current, so a squash merge of the running
    // branch does not show up as an update.
    let up_to_date = !BUILD_SHA.is_empty()
        && git(&src, &["diff", "--quiet", BUILD_SHA, &latest, "--", "."])
            .await
            .is_ok();
    let commits = if up_to_date {
        Vec::new()
    } else {
        let range = format!("{BUILD_SHA}..{latest}");
        let log = if BUILD_SHA.is_empty() {
            git(&src, &["log", "--format=%h %s", "-n", "5", &latest]).await
        } else {
            git(&src, &["log", "--format=%h %s", "-n", "20", &range]).await
        };
        log.unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    Ok(Check {
        current: BUILD_SHA,
        latest,
        up_to_date,
        commits,
        status: status(),
    })
}

/// Start an update in the background. A second request while one is waiting
/// for chats to finish means "restart now".
pub fn start(runner: Arc<RunnerState>, force: bool) -> Result<(), String> {
    require_checkout(&source_dir())?;
    {
        let mut s = STATUS.lock().unwrap_or_else(|e| e.into_inner());
        match s.phase {
            "waiting" if force => {
                FORCE.store(true, Ordering::SeqCst);
                return Ok(());
            }
            "building" | "waiting" | "restarting" => {
                return Err("An update is already running.".into())
            }
            _ => {}
        }
        s.phase = "building";
        s.message = "Fetching main…".into();
        s.log.clear();
    }
    FORCE.store(force, Ordering::SeqCst);
    tokio::spawn(async move {
        if let Err((message, log)) = run(runner).await {
            set("failed", message, log);
        }
    });
    Ok(())
}

type Failure = (String, String);

async fn run(runner: Arc<RunnerState>) -> Result<(), Failure> {
    let plain = |e: String| (e, String::new());
    let src = source_dir();
    let work = work_dir();
    let tree = work.join("src");
    git(&src, &["fetch", "--quiet", "origin", "main"])
        .await
        .map_err(plain)?;
    if tree.join(".git").exists() {
        git(
            &tree,
            &["checkout", "--quiet", "--force", "--detach", "origin/main"],
        )
        .await
        .map_err(plain)?;
    } else {
        std::fs::create_dir_all(&work).map_err(|e| plain(e.to_string()))?;
        let _ = git(&src, &["worktree", "prune"]).await;
        let tree_arg = tree.to_string_lossy().to_string();
        git(
            &src,
            &[
                "worktree",
                "add",
                "--force",
                "--detach",
                &tree_arg,
                "origin/main",
            ],
        )
        .await
        .map_err(plain)?;
    }

    set(
        "building",
        "Building main (the first update takes a few minutes)…",
        String::new(),
    );
    let cargo = [
        crate::discovery::home_dir().join(".cargo/bin/cargo"),
        PathBuf::from("cargo"),
    ]
    .into_iter()
    .find(|p| p.is_absolute() && p.exists())
    .or_else(|| crate::models::resolve_bin_path("cargo"))
    .unwrap_or_else(|| PathBuf::from("cargo"));
    // Low priority and two jobs: the computer stays usable and quiet.
    let out = Command::new("nice")
        .args(["-n", "19"])
        .arg(&cargo)
        .args(["build", "--release", "--locked"])
        .current_dir(&tree)
        .env("CARGO_TARGET_DIR", work.join("target"))
        .env("CARGO_BUILD_JOBS", "2")
        .output()
        .await
        .map_err(|e| plain(format!("Could not start cargo: {e}")))?;
    if !out.status.success() {
        return Err(("The build failed.".into(), tail(&out.stderr)));
    }
    let built = work.join("target/release/moedeck");
    let smoke = Command::new(&built)
        .arg("--version")
        .output()
        .await
        .map_err(|e| plain(format!("The new build does not start: {e}")))?;
    if !smoke.status.success() {
        return Err(("The new build does not start.".into(), tail(&smoke.stderr)));
    }

    loop {
        let busy = runner.active_snapshot().await.len();
        if busy == 0 || FORCE.load(Ordering::SeqCst) {
            break;
        }
        let chats = if busy == 1 { "chat" } else { "chats" };
        set(
            "waiting",
            format!("Built. Restarting when {busy} working {chats} finish…"),
            String::new(),
        );
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }

    let exe = running_exe().map_err(plain)?;
    install(&built, &exe).map_err(plain)?;
    set("restarting", "Restarting MoE Deck…", String::new());
    tokio::spawn(async move {
        // Let the status poll see "restarting" first.
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        restart(&exe);
    });
    Ok(())
}

/// Keep the running binary as `<name>.prev`, then rename the new one over it.
/// A rename works while the old file is still executing; a copy does not.
fn install(built: &Path, exe: &Path) -> Result<(), String> {
    let name = exe
        .file_name()
        .ok_or("Unknown binary name")?
        .to_string_lossy()
        .to_string();
    let prev = exe.with_file_name(format!("{name}.prev"));
    let staged = exe.with_file_name(format!("{name}.new"));
    std::fs::copy(exe, &prev).map_err(|e| format!("Could not back up MoE Deck: {e}"))?;
    std::fs::copy(built, &staged).map_err(|e| format!("Could not stage the update: {e}"))?;
    std::fs::rename(&staged, exe).map_err(|e| format!("Could not install the update: {e}"))?;
    Ok(())
}

/// Linux reports a replaced executable as `/path/moedeck (deleted)`.
fn running_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let text = exe.to_string_lossy();
    Ok(PathBuf::from(
        text.strip_suffix(" (deleted)").unwrap_or(&text).to_string(),
    ))
}

/// Re-exec in place: same PID, args and environment, so it works under
/// systemd and when started by hand. Sockets are close-on-exec.
fn restart(exe: &Path) {
    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .exec();
    set(
        "failed",
        format!("Installed, but could not restart: {error}. Restart MoE Deck by hand."),
        String::new(),
    );
}

fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_keeps_the_previous_binary_and_replaces_by_rename() {
        let dir = std::env::temp_dir().join(format!("helm-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let built = dir.join("built");
        std::fs::write(&built, "new").unwrap();
        let exe = dir.join("moedeck");
        std::fs::write(&exe, "old").unwrap();
        install(&built, &exe).unwrap();
        let (prev, staged) = (dir.join("moedeck.prev"), dir.join("moedeck.new"));
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&prev).unwrap(), "old");
        assert!(!staged.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_downloaded_binary_points_to_releases() {
        let dir = std::env::temp_dir().join(format!("moedeck-nosrc-{}", uuid::Uuid::new_v4()));
        let err = require_checkout(&dir).unwrap_err();
        assert!(err.contains("installed from a download"));
        assert!(err.ends_with("/releases"));
        assert!(require_checkout(Path::new(env!("CARGO_MANIFEST_DIR"))).is_ok());
    }

    #[test]
    fn build_tail_keeps_the_last_lines() {
        let log: String = (0..50).map(|i| format!("line {i}\n")).collect();
        let t = tail(log.as_bytes());
        assert!(t.starts_with("line 20"));
        assert!(t.ends_with("line 49"));
    }
}
