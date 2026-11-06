//! `moedeck install` / `moedeck uninstall`: keep MoE Deck running in the background as
//! the current user, with this user's own paths. systemd user service on
//! Linux, launchd agent on macOS. Nothing here needs root.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const UNIT: &str = "moedeck.service";
const LABEL: &str = "moedeck";

/// Everything the service file needs, gathered from the installing shell.
pub struct Plan {
    pub exe: PathBuf,
    pub home: PathBuf,
    /// The installing shell's PATH plus the usual per-user bin dirs, so the
    /// service finds the same `claude`, `codex`, `agy`… the user can run.
    pub path: String,
    /// Only what changes behaviour; never the passcode (it stays in auth.json).
    pub env: Vec<(String, String)>,
}

pub fn plan(exe: PathBuf, home: PathBuf, shell_path: &str) -> Plan {
    let h = home.display();
    let mut dirs: Vec<String> = vec![
        format!("{h}/.local/bin"),
        format!("{h}/.cargo/bin"),
        format!("{h}/.npm-global/bin"),
        format!("{h}/.bun/bin"),
        format!("{h}/.gemini/antigravity-cli/bin"),
    ];
    dirs.extend(shell_path.split(':').map(str::to_string));
    dirs.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"]
            .iter()
            .map(|d| d.to_string()),
    );
    let mut path: Vec<String> = Vec::new();
    for dir in dirs {
        if !dir.is_empty() && !path.contains(&dir) {
            path.push(dir);
        }
    }
    let env = ["PORT", "CONFIG_DIR", "SOURCE_DIR"]
        .iter()
        .filter_map(|k| crate::discovery::setting(k).map(|v| (format!("MOEDECK_{k}"), v)))
        .collect();
    Plan {
        exe,
        home,
        path: path.join(":"),
        env,
    }
}

pub fn systemd_unit(p: &Plan) -> String {
    let quote = |v: &str| format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
    let mut env = format!("Environment={}\n", quote(&format!("PATH={}", p.path)));
    for (k, v) in &p.env {
        env.push_str(&format!("Environment={}\n", quote(&format!("{k}={v}"))));
    }
    format!(
        "[Unit]
Description=MoE Deck: run coding agents from your phone
After=network-online.target

[Service]
Type=simple
WorkingDirectory={home}
ExecStart={exe}
Restart=always
RestartSec=2
KillMode=process
{env}
[Install]
WantedBy=default.target
",
        home = p.home.display(),
        exe = quote(&p.exe.display().to_string()),
    )
}

pub fn launchd_plist(p: &Plan) -> String {
    let x = |v: &str| {
        v.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    let mut env = format!("    <key>PATH</key><string>{}</string>\n", x(&p.path));
    for (k, v) in &p.env {
        env.push_str(&format!(
            "    <key>{}</key><string>{}</string>\n",
            x(k),
            x(v)
        ));
    }
    let log = p.home.join("Library/Logs/moedeck.log");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>WorkingDirectory</key><string>{home}</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
</dict>
</plist>
"#,
        exe = x(&p.exe.display().to_string()),
        home = x(&p.home.display().to_string()),
        log = x(&log.display().to_string()),
    )
}

fn service_file(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join(format!("Library/LaunchAgents/{LABEL}.plist"))
    } else {
        home.join(".config/systemd/user").join(UNIT)
    }
}

fn run(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(cmd).args(args).status()?;
    anyhow::ensure!(status.success(), "`{cmd} {}` failed", args.join(" "));
    Ok(())
}

/// Where the service should run MoE Deck from: a stable per-user bin dir, since a
/// service pointing into `target/` or `Downloads/` breaks when that is cleaned.
fn stable_exe(home: &Path, exe: &Path) -> PathBuf {
    let bin = home.join(".local/bin");
    if exe.parent() == Some(bin.as_path()) {
        exe.to_path_buf()
    } else {
        bin.join("moedeck")
    }
}

/// Copy the running binary into place, renaming over any running copy.
fn place(exe: &Path, target: &Path) -> anyhow::Result<()> {
    if exe == target {
        return Ok(());
    }
    let bin = target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(bin)?;
    let staged = bin.join("moedeck.new");
    fs::copy(exe, &staged)?;
    fs::rename(&staged, target)?;
    println!("Copied MoE Deck to {}", target.display());
    Ok(())
}

pub fn run_install(dry_run: bool) -> anyhow::Result<()> {
    let home = crate::discovery::home_dir();
    let current = std::env::current_exe()?.canonicalize()?;
    let exe = stable_exe(&home, &current);
    let plan = plan(
        exe.clone(),
        home.clone(),
        &std::env::var("PATH").unwrap_or_default(),
    );
    let file = service_file(&home);
    let content = if cfg!(target_os = "macos") {
        launchd_plist(&plan)
    } else {
        systemd_unit(&plan)
    };
    if dry_run {
        if exe != current {
            println!("# would copy {} to {}", current.display(), exe.display());
        }
        println!("# would write {}\n{content}", file.display());
        return Ok(());
    }
    place(&current, &exe)?;
    fs::create_dir_all(file.parent().unwrap_or(&home))?;
    if fs::read_to_string(&file).is_ok_and(|old| old != content) {
        let backup = file.with_extension("bak");
        fs::copy(&file, &backup)?;
        println!("Kept your previous service file as {}", backup.display());
    }
    fs::write(&file, &content)?;
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let plist = file.display().to_string();
        let _ = run("launchctl", &["bootout", &domain, &plist]);
        run("launchctl", &["bootstrap", &domain, &plist])?;
    } else {
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", UNIT])?;
        run("systemctl", &["--user", "restart", UNIT])?;
        println!(
            "To keep MoE Deck running after you log out: loginctl enable-linger {}",
            std::env::var("USER").unwrap_or_else(|_| "$USER".into())
        );
    }
    println!("MoE Deck is installed and running ({}).\n", file.display());
    Ok(())
}

pub fn run_uninstall() -> anyhow::Result<()> {
    let home = crate::discovery::home_dir();
    let file = service_file(&home);
    if cfg!(target_os = "macos") {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let _ = run(
            "launchctl",
            &["bootout", &domain, &file.display().to_string()],
        );
    } else {
        let _ = run("systemctl", &["--user", "disable", "--now", UNIT]);
    }
    if file.exists() {
        fs::remove_file(&file)?;
    }
    if !cfg!(target_os = "macos") {
        let _ = run("systemctl", &["--user", "daemon-reload"]);
    }
    println!(
        "MoE Deck's background service is removed. Your settings and chats in {} are kept.",
        crate::discovery::config_dir().display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Plan {
        Plan {
            exe: "/home/ada/.local/bin/moedeck".into(),
            home: "/home/ada".into(),
            path: "/home/ada/.local/bin:/usr/bin".into(),
            env: vec![("MOEDECK_PORT".into(), "7780".into())],
        }
    }

    #[test]
    fn plan_keeps_the_shell_path_and_user_bins_once_each() {
        let p = plan(
            "/x/moedeck".into(),
            "/home/ada".into(),
            "/home/ada/.nvm/versions/node/v24/bin:/usr/bin:/home/ada/.local/bin",
        );
        let dirs: Vec<&str> = p.path.split(':').collect();
        assert_eq!(dirs[0], "/home/ada/.local/bin");
        assert!(dirs.contains(&"/home/ada/.nvm/versions/node/v24/bin"));
        assert_eq!(dirs.iter().filter(|d| **d == "/usr/bin").count(), 1);
        assert!(!p.env.iter().any(|(k, _)| k.ends_with("PASSCODE")));
    }

    #[test]
    fn unit_and_plist_use_this_users_paths_only() {
        let unit = systemd_unit(&sample());
        assert!(unit.contains("ExecStart=\"/home/ada/.local/bin/moedeck\""));
        assert!(unit.contains("WorkingDirectory=/home/ada\n"));
        assert!(unit.contains("Environment=\"MOEDECK_PORT=7780\""));
        let plist = launchd_plist(&sample());
        assert!(plist.contains("<string>/home/ada/.local/bin/moedeck</string>"));
        assert!(plist.contains("<key>MOEDECK_PORT</key><string>7780</string>"));
    }
}
