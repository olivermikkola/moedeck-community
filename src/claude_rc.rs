//! User-started native Claude Remote Control processes. No print-mode prompts,
//! shell interpolation, permission bypass, or auto-answering trust dialogs.
use crate::{discovery, models, profiles};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OUTPUT_LIMIT: usize = 24 * 1024;

#[derive(Clone, Serialize)]
pub struct View {
    pub id: String,
    pub workdir: String,
    pub profile: Option<String>,
    pub name: String,
    pub permission_mode: String,
    pub status: String,
    pub url: Option<String>,
    pub output: String,
    pub exit_code: Option<i32>,
}
impl View {
    fn running(&self) -> bool {
        matches!(self.status.as_str(), "starting" | "ready" | "stopping")
    }
}

struct Session {
    view: Arc<Mutex<View>>,
    input: Mutex<File>,
    pid: u32,
}
#[derive(Default)]
pub struct RemoteControls {
    sessions: Mutex<HashMap<String, Session>>,
}
impl RemoteControls {
    pub fn list(&self) -> Vec<View> {
        let sessions = self.sessions.lock().unwrap();
        let mut views: Vec<_> = sessions
            .values()
            .map(|s| s.view.lock().unwrap().clone())
            .collect();
        views.sort_by(|a, b| a.id.cmp(&b.id));
        views
    }
    pub fn get(&self, id: &str) -> Option<View> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.view.lock().unwrap().clone())
    }
    pub fn start(
        &self,
        workdir: &str,
        profile: Option<String>,
        permission: &str,
    ) -> Result<View, String> {
        let dir = discovery::sanitize_workdir(workdir)
            .ok_or("Choose an existing project folder on this computer.")?;
        let home =
            std::fs::canonicalize(discovery::home_dir()).unwrap_or_else(|_| discovery::home_dir());
        if dir == home || dir == std::path::Path::new("/") {
            return Err("Choose a project folder, rather than the entire home directory.".into());
        }
        let workdir = dir.to_string_lossy().into_owned();
        let profile = profile.filter(|p| p != "default" && !p.is_empty());
        if let Some(id) = &profile {
            let account = profiles::get_profile(id).ok_or("Claude account not found.")?;
            if account.harness != "claude" {
                return Err("Choose a Claude account.".into());
            }
            profiles::ensure_shadow_home("claude", id)?;
        }
        let binary = models::resolve_bin_path("claude")
            .ok_or("Claude CLI is not installed on this computer.")?;
        let mut sessions = self.sessions.lock().unwrap();
        for session in sessions.values() {
            let view = session.view.lock().unwrap();
            if view.workdir == workdir && view.profile == profile && view.running() {
                return Ok(view.clone());
            }
        }
        if sessions
            .values()
            .filter(|s| s.view.lock().unwrap().running())
            .count()
            >= 8
        {
            return Err("Stop an existing Remote Control before starting another.".into());
        }
        sessions.retain(|_, s| s.view.lock().unwrap().running());
        let permission = match permission {
            "plan" => "plan",
            _ => "default",
        };
        let name = format!(
            "MoE Deck · {}",
            dir.file_name().unwrap_or_default().to_string_lossy()
        );
        let (mut master, slave) =
            terminal().map_err(|e| format!("Could not open Claude terminal: {e}"))?;
        let input = master.try_clone().map_err(|e| e.to_string())?;
        let stdin = slave.try_clone().map_err(|e| e.to_string())?;
        let stdout = slave.try_clone().map_err(|e| e.to_string())?;
        let mut command = Command::new(binary);
        command
            .args([
                "remote-control",
                "--spawn",
                "session",
                "--name",
                &name,
                "--permission-mode",
                permission,
            ])
            .current_dir(&dir)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(slave));
        if let Some(id) = &profile {
            command.envs(profiles::child_env_overrides("claude", id));
            for key in [
                "CLAUDE_CODE_OAUTH_TOKEN",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
            ] {
                command.env_remove(key);
            }
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Could not start Claude Remote Control: {e}"))?;
        let pid = child.id();
        let view = View {
            id: uuid::Uuid::new_v4().to_string(),
            workdir,
            profile,
            name,
            permission_mode: permission.into(),
            status: "starting".into(),
            url: None,
            output: String::new(),
            exit_code: None,
        };
        let shared = Arc::new(Mutex::new(view.clone()));
        sessions.insert(
            view.id.clone(),
            Session {
                view: shared.clone(),
                input: Mutex::new(input),
                pid,
            },
        );
        tokio::spawn(async move {
            let mut process = Process { child, live: true };
            let mut decoder = TerminalText::default();
            let mut stopping_since = None;
            loop {
                if shared.lock().unwrap().status == "stopping" {
                    let since = stopping_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() > Duration::from_secs(3) {
                        unsafe {
                            libc::kill(-(process.child.id() as i32), libc::SIGKILL);
                        }
                    }
                }
                // Observe exit before draining so the last error/link bytes
                // are still read when a short-lived CLI exits between ticks.
                let exit = process.child.try_wait();
                let mut bytes = [0u8; 8192];
                for _ in 0..8 {
                    match master.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            let text = decoder.feed(&bytes[..n]);
                            let mut state = shared.lock().unwrap();
                            state.output.push_str(&text);
                            if let Some(url) = connection_url(&state.output) {
                                state.url = Some(url);
                            }
                            trim_output(&mut state.output);
                            if state.status == "starting" && state.url.is_some() {
                                state.status = "ready".into();
                            }
                        }
                        Err(_) => break, // nonblocking PTY: EAGAIN while idle, EIO at EOF
                    }
                }
                match exit {
                    Ok(Some(exit)) => {
                        process.live = false;
                        // This server owns its session group. A worker must
                        // not remain running after the server has exited.
                        unsafe {
                            libc::kill(-(process.child.id() as i32), libc::SIGKILL);
                        }
                        let mut state = shared.lock().unwrap();
                        state.exit_code = exit.code();
                        state.status = if state.status == "stopping" {
                            "stopped"
                        } else if exit.success() {
                            "exited"
                        } else {
                            "error"
                        }
                        .into();
                        break;
                    }
                    Ok(None) => tokio::time::sleep(Duration::from_millis(100)).await,
                    Err(e) => {
                        let mut state = shared.lock().unwrap();
                        state
                            .output
                            .push_str(&format!("\nCould not observe Claude process: {e}"));
                        state.status = "error".into();
                        break;
                    }
                }
            }
        });
        Ok(view)
    }
    pub fn input(&self, id: &str, text: &str, key: Option<&str>) -> Result<(), String> {
        if text.len() > 4096 || text.contains('\0') {
            return Err("Terminal input is too long or invalid.".into());
        }
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.get(id).ok_or("Remote Control not found.")?;
        if !session.view.lock().unwrap().running() {
            return Err("This Remote Control has stopped. Start it again.".into());
        }
        let suffix = match key.unwrap_or("enter") {
            "enter" => "\r",
            "escape" => "\x1b",
            "up" => "\x1b[A",
            "down" => "\x1b[B",
            _ => return Err("Unknown terminal key.".into()),
        };
        let mut input = session.input.lock().unwrap();
        input
            .write_all(format!("{text}{suffix}").as_bytes())
            .map_err(|e| format!("Could not send terminal input: {e}"))
    }
    pub fn stop(&self, id: &str) -> Result<(), String> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions.get(id).ok_or("Remote Control not found.")?;
        stop(session);
        Ok(())
    }
    pub fn stop_all(&self) {
        for session in self.sessions.lock().unwrap().values() {
            stop(session);
        }
    }
}
impl Drop for RemoteControls {
    fn drop(&mut self) {
        self.stop_all();
    }
}
fn stop(session: &Session) {
    let mut state = session.view.lock().unwrap();
    if state.running() {
        state.status = "stopping".into();
        unsafe {
            libc::kill(-(session.pid as i32), libc::SIGTERM);
        }
    }
}
struct Process {
    child: Child,
    live: bool,
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.live {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}
fn terminal() -> std::io::Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    // macOS declares termios/winsize as `*mut`, Linux as `*const`; a `*mut`
    // satisfies both.
    let mut size = libc::winsize {
        ws_row: 28,
        ws_col: 90,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        if libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        ) == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        let master = File::from_raw_fd(master);
        let slave = File::from_raw_fd(slave);
        for fd in [&master, &slave] {
            use std::os::fd::AsRawFd;
            if libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        use std::os::fd::AsRawFd;
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        if flags == -1
            || libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok((master, slave))
    }
}
fn trim_output(text: &mut String) {
    if text.len() > OUTPUT_LIMIT {
        let mut start = text.len() - OUTPUT_LIMIT;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text.drain(..start);
    }
}
fn connection_url(text: &str) -> Option<String> {
    // Wait for a delimiter: the final token can be a URL split across PTY reads.
    let complete = text.rfind(char::is_whitespace)?;
    text[..=complete].split_whitespace().find_map(|word| {
        let url = word.trim_matches(|c: char| matches!(c, '(' | ')' | '<' | '>' | '"' | '\''));
        (url.starts_with("https://claude.ai/code/")
            && url.len() > "https://claude.ai/code/".len()
            && url.len() < 2048
            && !url.contains(['<', '>', '"', '\'']))
        .then(|| url.to_string())
    })
}
#[derive(Default)]
struct TerminalText {
    escape: u8,
}
impl TerminalText {
    fn feed(&mut self, bytes: &[u8]) -> String {
        let mut plain = Vec::new();
        for &byte in bytes {
            match self.escape {
                0 if byte == 27 => self.escape = 1,
                0 if byte == b'\r' => {}
                0 if byte >= 32 || byte == b'\n' || byte == b'\t' => plain.push(byte),
                1 => {
                    self.escape = match byte {
                        b'[' => 2,
                        b']' => 3,
                        _ => 0,
                    }
                }
                2 if (0x40..=0x7e).contains(&byte) => self.escape = 0,
                3 if byte == 7 => self.escape = 0,
                3 if byte == 27 => self.escape = 4,
                4 => self.escape = if byte == b'\\' { 0 } else { 3 },
                _ => {}
            }
        }
        String::from_utf8_lossy(&plain).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_terminal_sequences_split_across_reads_do_not_hide_the_link() {
        let mut decoder = TerminalText::default();
        let mut text = decoder.feed(b"\x1b[3");
        text += &decoder.feed(b"2mConnect:\x1b[0m \x1b]8;;https://claude.ai/code/session_test\x07");
        text += &decoder.feed(b"https://claude.ai/code/session_test?env=local\x1b]8;;\x1b\\\r\n");
        assert_eq!(
            connection_url(&text).as_deref(),
            Some("https://claude.ai/code/session_test?env=local")
        );
        assert!(connection_url(
            "https://claude.ai.evil/code/a https://evil.test/code/b https://claude.ai/code"
        )
        .is_none());
        assert!(connection_url("https://claude.ai/code/session_partial").is_none());
        let mut large = "å".repeat(OUTPUT_LIMIT);
        trim_output(&mut large);
        assert!(large.len() <= OUTPUT_LIMIT);
        assert!(large.ends_with('å'));
    }
}
