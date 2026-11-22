//! Per-harness status: which agents are live and what they are doing.
//!
//! Plain arithmetic over signals the harnesses already expose. Runs MoE Deck
//! started report through their live event stream; agents started anywhere
//! else are read through one adapter per harness. Session files alone can't
//! be trusted (a crashed process leaves "busy" behind for good), so every
//! outside agent has to be backed by a live process first.

use crate::discovery::{self, truncate_str};
use crate::models::StoredThreadOverlay;
use crate::profiles;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;

/// Wrote something this recently: the agent is working.
pub const WORKING_WITHIN_SECS: u64 = 90;
/// Claims to be busy but has been silent this long: it may be stuck.
pub const STUCK_AFTER_SECS: u64 = 600;
/// A single tool call (say, a long build) is allowed to stay silent this long.
pub const TOOL_STUCK_AFTER_SECS: u64 = 1800;
/// An open but untouched session stops counting as an agent after this.
pub const LISTED_WITHIN_SECS: u64 = 1800;

const HARNESSES: [&str; 7] = [
    "claude", "codex", "agy", "opencode", "keel", "cursor", "grok",
];
const DOING_MAX_CHARS: usize = 44;
const LABEL_MAX_CHARS: usize = 24;
const TAIL_BYTES: u64 = 512 * 1024;
const MAX_COLOR_RULES: usize = 500;
const MAX_COLOR_KEY_LEN: usize = 200;
static COLORS_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Off,
    Waiting,
    Stuck,
    Working,
}

/// What one agent's process and session files say right now.
#[derive(Debug, Clone, Copy, Default)]
pub struct AgentSignal {
    pub process_alive: bool,
    pub claims_busy: bool,
    pub tool_running: bool,
    pub needs_you: bool,
    /// Seconds since the session last wrote anything, if known.
    pub silent_secs: Option<u64>,
}

pub fn classify(signal: AgentSignal) -> AgentState {
    if !signal.process_alive {
        return AgentState::Off;
    }
    if signal.needs_you {
        return AgentState::Waiting;
    }
    match signal.silent_secs {
        Some(secs)
            if secs <= WORKING_WITHIN_SECS && (signal.claims_busy || signal.tool_running) =>
        {
            AgentState::Working
        }
        Some(secs) if signal.tool_running => {
            if secs >= TOOL_STUCK_AFTER_SECS {
                AgentState::Stuck
            } else {
                AgentState::Working
            }
        }
        Some(secs) if signal.claims_busy && secs >= STUCK_AFTER_SECS => AgentState::Stuck,
        _ => AgentState::Waiting,
    }
}

/// Live, and either mid-task or touched recently enough to still matter.
fn is_listed(signal: AgentSignal) -> bool {
    signal.process_alive
        && (signal.claims_busy
            || signal.tool_running
            || signal.needs_you
            || signal.silent_secs.is_some_and(|s| s <= LISTED_WITHIN_SECS))
}

/// Turns a tool call into a short phrase for a pill. Commands keep only their
/// first two words, so a token further along the line can't reach the screen.
pub fn describe_action(tool: &str, target: &str) -> String {
    let low = tool.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| low.contains(w));
    let target = target.lines().next().unwrap_or("").trim().trim_matches('"');

    if has(&["bash", "shell", "exec", "command"]) {
        if target.is_empty() {
            return "Running a command".to_string();
        }
        // Descriptions read as sentences; a raw command starts in lowercase or
        // with a VAR=value prefix.
        let first_word = target.split_whitespace().next().unwrap_or("");
        if first_word.starts_with(char::is_uppercase) && first_word.chars().all(char::is_alphabetic)
        {
            return truncate_str(target, DOING_MAX_CHARS);
        }
        // "cd repo && pytest" is about pytest.
        let command = target
            .strip_prefix("cd ")
            .and_then(|rest| rest.split_once("&&"))
            .map_or(target, |(_, rest)| rest.trim());
        let words: Vec<&str> = command
            .split_whitespace()
            .filter(|w| !w.contains('='))
            .take(2)
            .collect();
        return truncate_str(&format!("Running {}", words.join(" ")), DOING_MAX_CHARS);
    }

    let verb = if has(&["edit", "write", "patch", "replace"]) {
        "Editing"
    } else if has(&["read", "view"]) {
        "Reading"
    } else if has(&["grep", "glob", "search", "find", "list"]) {
        "Searching"
    } else if has(&["web", "fetch", "browse"]) {
        "Browsing"
    } else if has(&["agent", "task"]) {
        "Delegating"
    } else {
        return truncate_str(&format!("Using {tool}"), DOING_MAX_CHARS);
    };
    let shown = if target.contains('/') && !target.contains(char::is_whitespace) {
        target.rsplit('/').find(|s| !s.is_empty()).unwrap_or(target)
    } else {
        target
    };
    if shown.is_empty() {
        return verb.to_string();
    }
    truncate_str(&format!("{verb} {shown}"), DOING_MAX_CHARS)
}

/// The repo name, or the chat title for agents opened in the home folder.
fn label_for(workdir: &str, title: &str, home: &Path) -> String {
    let path = Path::new(workdir);
    match path.file_name() {
        Some(name) if path != home => name.to_string_lossy().to_string(),
        _ if !title.trim().is_empty() => truncate_str(title.trim(), LABEL_MAX_CHARS),
        _ => "Home".to_string(),
    }
}

/// Latest state of a run MoE Deck itself started, from its event stream.
#[derive(Debug, Clone, Default)]
pub struct RunActivity {
    pub last_event_ms: i64,
    pub doing: Option<String>,
    pub tool_running: bool,
}

#[derive(Debug, Clone)]
struct Observed {
    id: String,
    harness: String,
    label: String,
    doing: Option<String>,
    started_by: &'static str,
    signal: AgentSignal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentEntry {
    /// Conversation id; open this to reach the agent's chat.
    pub id: String,
    pub harness: String,
    pub label: String,
    pub doing: Option<String>,
    /// The user's own colour, if a rule matches; otherwise the harness default.
    pub color: Option<String>,
    pub state: AgentState,
    pub needs_you: bool,
    pub silent_secs: Option<u64>,
    pub started_by: &'static str,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StateCounts {
    pub live: usize,
    pub working: usize,
    pub waiting: usize,
    pub stuck: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HarnessStatus {
    pub harness: String,
    /// The one state the chip shows.
    pub state: AgentState,
    pub agents: StateCounts,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct StatusReport {
    pub agents: Vec<AgentEntry>,
    pub harnesses: Vec<HarnessStatus>,
}

pub fn summarize(harness: &str, signals: &[AgentSignal]) -> HarnessStatus {
    let mut agents = StateCounts::default();
    for signal in signals {
        match classify(*signal) {
            AgentState::Off => continue,
            AgentState::Working => agents.working += 1,
            AgentState::Waiting => agents.waiting += 1,
            AgentState::Stuck => agents.stuck += 1,
        }
        agents.live += 1;
    }
    // Working outranks stuck so one quiet agent doesn't mark a busy harness;
    // the per-agent entries still carry the stuck one.
    let state = if agents.working > 0 {
        AgentState::Working
    } else if agents.stuck > 0 {
        AgentState::Stuck
    } else if agents.waiting > 0 {
        AgentState::Waiting
    } else {
        AgentState::Off
    };
    HarnessStatus {
        harness: harness.to_string(),
        state,
        agents,
    }
}

fn state_rank(state: AgentState) -> u8 {
    match state {
        AgentState::Working => 0,
        AgentState::Stuck => 1,
        AgentState::Waiting => 2,
        AgentState::Off => 3,
    }
}

fn build_report(observed: Vec<Observed>) -> StatusReport {
    let listed: Vec<Observed> = observed
        .into_iter()
        .filter(|o| is_listed(o.signal))
        .collect();
    let harnesses = HARNESSES
        .iter()
        .map(|harness| {
            let signals: Vec<AgentSignal> = listed
                .iter()
                .filter(|o| o.harness == *harness)
                .map(|o| o.signal)
                .collect();
            summarize(harness, &signals)
        })
        .collect();
    let mut agents: Vec<AgentEntry> = listed
        .into_iter()
        .map(|o| AgentEntry {
            state: classify(o.signal),
            needs_you: o.signal.needs_you,
            silent_secs: o.signal.silent_secs,
            id: o.id,
            harness: o.harness,
            label: o.label,
            doing: o.doing,
            color: None,
            started_by: o.started_by,
        })
        .collect();
    // Anything asking for you first, then by state, then most recent.
    agents.sort_by_key(|a| {
        (
            !a.needs_you,
            state_rank(a.state),
            a.silent_secs.unwrap_or(u64::MAX),
        )
    });
    StatusReport { agents, harnesses }
}

/// User-chosen colours. A task's own colour beats its repo's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentColors {
    #[serde(default)]
    pub tasks: BTreeMap<String, String>,
    #[serde(default)]
    pub repos: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct ColorChange {
    pub scope: String,
    pub key: String,
    /// `None` puts this agent or repo back on the default colour.
    pub color: Option<String>,
}

fn colors_path() -> PathBuf {
    discovery::config_dir().join("agent_colors.json")
}

fn load_colors() -> AgentColors {
    fs::read(colors_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn is_hex_color(color: &str) -> bool {
    color.len() == 7 && color.starts_with('#') && color[1..].chars().all(|c| c.is_ascii_hexdigit())
}

fn apply_color_change(colors: &mut AgentColors, change: ColorChange) -> Result<(), String> {
    if change.key.is_empty() || change.key.len() > MAX_COLOR_KEY_LEN {
        return Err("Invalid colour target".to_string());
    }
    let rules = match change.scope.as_str() {
        "task" => &mut colors.tasks,
        "repo" => &mut colors.repos,
        _ => return Err("Scope must be task or repo".to_string()),
    };
    match change.color {
        None => {
            rules.remove(&change.key);
        }
        Some(color) if is_hex_color(&color) => {
            if rules.len() >= MAX_COLOR_RULES && !rules.contains_key(&change.key) {
                return Err("Too many colour rules".to_string());
            }
            rules.insert(change.key, color);
        }
        Some(_) => return Err("Colour must look like #22C55E".to_string()),
    }
    Ok(())
}

pub fn set_color(change: ColorChange) -> Result<(), String> {
    let _guard = COLORS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut colors = load_colors();
    apply_color_change(&mut colors, change)?;
    let text = serde_json::to_string_pretty(&colors).map_err(|e| e.to_string())?;
    discovery::atomic_write(&colors_path(), &text).map_err(|e| e.to_string())
}

fn apply_colors(report: &mut StatusReport, colors: &AgentColors) {
    for agent in &mut report.agents {
        agent.color = colors
            .tasks
            .get(&agent.id)
            .or_else(|| colors.repos.get(&agent.label))
            .cloned();
    }
}

pub fn collect(
    active: &HashMap<String, String>,
    activity: &HashMap<String, RunActivity>,
    needs_you: &HashSet<String>,
) -> StatusReport {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let home = discovery::home_dir();
    let overlays = discovery::load_overlays();
    let mut observed = moedeck_runs(active, activity, needs_you, &overlays, now_ms, &home);

    // A run MoE Deck started is also a session on disk; count it only once.
    let mut own_native: HashSet<String> = HashSet::new();
    for id in active.keys() {
        own_native.insert(profiles::parse_conv_id(id).2);
        own_native.extend(discovery::overlay_native_session(id));
    }
    observed.extend(claude_agents(&own_native, &home));
    observed.extend(codex_agents(&own_native, &home));
    // Native OpenCode's shared server does not prove that any particular
    // session is alive. Only gateway-owned OpenCode runs have live signals.
    observed.extend(agy_agents(&own_native, active, &overlays, &home));
    observed.extend(cursor_agents(&own_native, &home));
    observed.extend(grok_agents(&own_native, &home));
    let mut report = build_report(observed);
    apply_colors(&mut report, &load_colors());
    report
}

fn moedeck_runs(
    active: &HashMap<String, String>,
    activity: &HashMap<String, RunActivity>,
    needs_you: &HashSet<String>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    now_ms: i64,
    home: &Path,
) -> Vec<Observed> {
    active
        .keys()
        .map(|id| {
            let summary = discovery::find_conversation_summary(id, active, overlays);
            let run = activity.get(id);
            let harness = summary
                .as_ref()
                .map(|s| s.harness.clone())
                .or_else(|| profiles::parse_conv_id(id).0)
                .unwrap_or_else(|| "claude".to_string());
            let label = summary
                .as_ref()
                .map(|s| label_for(&s.workdir, &s.title, home))
                .unwrap_or_else(|| "Home".to_string());
            Observed {
                id: id.clone(),
                harness,
                label,
                doing: Some(
                    run.and_then(|r| r.doing.clone())
                        .unwrap_or_else(|| "Starting".to_string()),
                ),
                started_by: "moedeck",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy: true,
                    tool_running: run.is_some_and(|r| r.tool_running),
                    needs_you: needs_you.contains(id),
                    silent_secs: run.map(|r| ((now_ms - r.last_event_ms).max(0) / 1000) as u64),
                },
            }
        })
        .collect()
}

fn silent_since(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(
        SystemTime::now()
            .duration_since(modified)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// The last `TAIL_BYTES` of a log, starting at a whole line.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Some(text);
    }
    text.split_once('\n').map(|(_, rest)| rest.to_string())
}

#[cfg(unix)]
fn pid_alive(pid: i32, name: &str) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    if pid <= 0 || unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    // Guards against the id having been reused by something else.
    match fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(cmdline) => String::from_utf8_lossy(&cmdline).contains(name),
        Err(_) => true,
    }
}

#[cfg(not(unix))]
fn pid_alive(_pid: i32, _name: &str) -> bool {
    false
}

fn pids_named(name: &str) -> Vec<i32> {
    Command::new("pgrep")
        .arg(name)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn last_claude_tool(tail: &str) -> Option<String> {
    for line in tail.lines().rev() {
        if !line.contains("\"tool_use\"") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let content = val.pointer("/message/content").and_then(Value::as_array);
        let tool = content
            .into_iter()
            .flatten()
            .rev()
            .find(|part| part["type"] == "tool_use");
        if let Some(tool) = tool {
            let name = tool["name"].as_str().unwrap_or("tool");
            let summary = crate::timeline::claude_tool_summary(name, tool.get("input"));
            return Some(describe_action(name, &summary));
        }
    }
    None
}

fn claude_agents(own_native: &HashSet<String>, default_home: &Path) -> Vec<Observed> {
    let mut agents = Vec::new();
    for (profile, home) in profiles::harness_homes("claude") {
        let Ok(entries) = fs::read_dir(home.join(".claude").join("sessions")) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(session) = fs::read_to_string(entry.path())
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            let pid = session["pid"].as_i64().unwrap_or(0) as i32;
            let Some(raw_id) = session["sessionId"].as_str() else {
                continue;
            };
            if own_native.contains(raw_id) || !pid_alive(pid, "claude") {
                continue;
            }
            let transcript = discovery::find_claude_jsonl(raw_id, &home);
            let silent_secs = transcript.as_deref().and_then(silent_since);
            let doing = transcript
                .as_deref()
                .and_then(read_tail)
                .and_then(|tail| last_claude_tool(&tail));
            agents.push(Observed {
                id: profiles::make_conv_id("claude", profile.as_deref(), raw_id),
                harness: "claude".to_string(),
                label: label_for(
                    session["cwd"].as_str().unwrap_or(""),
                    session["name"].as_str().unwrap_or(""),
                    default_home,
                ),
                doing,
                started_by: "outside",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy: session["status"] == "busy",
                    tool_running: false,
                    needs_you: false,
                    silent_secs,
                },
            });
        }
    }
    agents
}

/// Whether the newest turn marker says a turn is open, plus the last tool.
fn codex_turn_state(tail: &str) -> (bool, Option<String>) {
    let mut busy = false;
    let mut doing = None;
    for line in tail.lines() {
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = &val["payload"];
        match (val["type"].as_str(), payload["type"].as_str()) {
            (Some("event_msg"), Some("task_started")) => busy = true,
            (Some("event_msg"), Some("task_complete" | "turn_aborted")) => busy = false,
            (
                Some("response_item"),
                Some("function_call" | "custom_tool_call" | "local_shell_call"),
            ) => {
                doing = Some(describe_action(
                    payload["name"].as_str().unwrap_or("exec"),
                    "",
                ));
            }
            _ => {}
        }
    }
    (busy, doing)
}

fn codex_agents(own_native: &HashSet<String>, default_home: &Path) -> Vec<Observed> {
    let mut agents = Vec::new();
    for (profile, home) in profiles::harness_homes("codex") {
        for thread in discovery::codex_threads(&home, None, usize::MAX) {
            if own_native.contains(&thread.raw_id) {
                continue;
            }
            let Some(rollout) = discovery::find_codex_rollout(&thread.raw_id, &home) else {
                continue;
            };
            if !crate::native_status::codex_has_writer(&rollout) {
                continue;
            }
            let claims_busy = crate::native_status::codex_is_running(&rollout);
            let doing = if claims_busy {
                read_tail(&rollout).and_then(|tail| codex_turn_state(&tail).1)
            } else {
                None
            };
            agents.push(Observed {
                id: profiles::make_conv_id("codex", profile.as_deref(), &thread.raw_id),
                harness: "codex".into(),
                label: label_for(&thread.workdir, &thread.title, default_home),
                doing,
                started_by: "outside",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy,
                    silent_secs: silent_since(&rollout),
                    ..Default::default()
                },
            });
        }
    }
    agents
}

fn held_agy_locks(presence_dir: &Path) -> usize {
    let presence_dir =
        fs::canonicalize(presence_dir).unwrap_or_else(|_| presence_dir.to_path_buf());
    pids_named("agy")
        .into_iter()
        .map(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd"))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|fd| fs::read_link(fd.path()).ok())
                .filter(|target| target.starts_with(&presence_dir))
                .count()
        })
        .sum()
}

fn last_agy_tool(tail: &str) -> Option<String> {
    for line in tail.lines().rev() {
        if !line.contains("\"tool_calls\"") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(call) = val["tool_calls"].as_array().and_then(|calls| calls.last()) {
            let args = &call["args"];
            let target = args["toolSummary"]
                .as_str()
                .or_else(|| args["CommandLine"].as_str())
                .or_else(|| args["TargetFile"].as_str())
                .or_else(|| args["AbsolutePath"].as_str())
                .unwrap_or("");
            return Some(describe_action(
                call["name"].as_str().unwrap_or("tool"),
                target,
            ));
        }
    }
    None
}

fn agy_agents(
    own_native: &HashSet<String>,
    active: &HashMap<String, String>,
    overlays: &HashMap<String, StoredThreadOverlay>,
    default_home: &Path,
) -> Vec<Observed> {
    let mut agents = Vec::new();
    for (profile, home) in profiles::harness_homes("agy") {
        let root = home.join(".gemini").join("antigravity-cli");
        // The lock names a CLI window, not a chat, so N held locks are read
        // as the N most recently active conversations.
        let held = held_agy_locks(&root.join("presence"));
        if held == 0 {
            continue;
        }
        let mut recent: Vec<(u64, String, PathBuf)> = fs::read_dir(root.join("brain"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|dir| {
                let raw_id = dir.file_name().to_string_lossy().to_string();
                let transcript = dir.path().join(".system_generated/logs/transcript.jsonl");
                let database = root.join("conversations").join(format!("{raw_id}.db-wal"));
                let silent = [silent_since(&transcript), silent_since(&database)]
                    .into_iter()
                    .flatten()
                    .min()?;
                Some((silent, raw_id, transcript))
            })
            .collect();
        recent.sort();
        for (silent, raw_id, transcript) in recent.into_iter().take(held) {
            if own_native.contains(&raw_id) {
                continue;
            }
            let id = profiles::make_conv_id("agy", profile.as_deref(), &raw_id);
            let summary = discovery::find_conversation_summary(&id, active, overlays);
            agents.push(Observed {
                label: summary
                    .map(|s| label_for(&s.workdir, &s.title, default_home))
                    .unwrap_or_else(|| "Antigravity".to_string()),
                id,
                harness: "agy".to_string(),
                doing: read_tail(&transcript).and_then(|tail| last_agy_tool(&tail)),
                started_by: "outside",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy: false,
                    tool_running: false,
                    needs_you: false,
                    silent_secs: Some(silent),
                },
            });
        }
    }
    agents
}

fn cursor_cli_running() -> bool {
    if !pids_named("cursor-agent").is_empty() {
        return true;
    }
    // The installer also symlinks the binary as `agent`. Match that only when
    // the executable path itself is cursor-agent, not every process named agent.
    #[cfg(unix)]
    {
        let Ok(proc) = fs::read_dir("/proc") else {
            return false;
        };
        for entry in proc.flatten() {
            if entry.file_name().to_string_lossy().parse::<i32>().is_err() {
                continue;
            }
            let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
                continue;
            };
            if exe.to_string_lossy().contains("cursor-agent") {
                return true;
            }
        }
    }
    false
}

fn cursor_turn_open(tail: &str) -> bool {
    let mut open = false;
    for line in tail.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value["type"] == "turn_ended" {
            open = false;
        } else if value["role"] == "user" {
            open = true;
        }
    }
    open
}

fn cursor_agents(own_native: &HashSet<String>, default_home: &Path) -> Vec<Observed> {
    if !cursor_cli_running() {
        return Vec::new();
    }
    let mut agents = Vec::new();
    for (profile, home) in profiles::harness_homes("cursor") {
        for chat in discovery::cursor_listings(&home) {
            if own_native.contains(&chat.raw_id) {
                continue;
            }
            let Some(transcript) = chat.transcript.as_deref() else {
                continue;
            };
            let Some(silent) = silent_since(transcript) else {
                continue;
            };
            if silent > LISTED_WITHIN_SECS {
                continue;
            }
            let tail = read_tail(transcript);
            agents.push(Observed {
                id: profiles::make_conv_id("cursor", profile.as_deref(), &chat.raw_id),
                harness: "cursor".to_string(),
                label: label_for(&chat.workdir, &chat.title, default_home),
                doing: tail.as_deref().and_then(last_claude_tool),
                started_by: "outside",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy: tail.as_deref().is_some_and(cursor_turn_open),
                    tool_running: false,
                    needs_you: false,
                    silent_secs: Some(silent),
                },
            });
        }
    }
    agents
}

fn grok_agents(own_native: &HashSet<String>, default_home: &Path) -> Vec<Observed> {
    if pids_named("grok").is_empty() {
        return Vec::new();
    }
    let mut agents = Vec::new();
    for (profile, home) in profiles::harness_homes("grok") {
        for session in crate::grok::listings(&home) {
            if own_native.contains(&session.raw_id) {
                continue;
            }
            let Some(updates) = session.updates.as_deref() else {
                continue;
            };
            let Some(silent) = silent_since(updates) else {
                continue;
            };
            if silent > LISTED_WITHIN_SECS {
                continue;
            }
            let tail = read_tail(updates);
            let doing = tail
                .as_deref()
                .and_then(crate::grok::last_tool)
                .map(|(name, summary)| describe_action(&name, &summary));
            agents.push(Observed {
                id: profiles::make_conv_id("grok", profile.as_deref(), &session.raw_id),
                harness: "grok".to_string(),
                label: label_for(&session.workdir, &session.title, default_home),
                doing,
                started_by: "outside",
                signal: AgentSignal {
                    process_alive: true,
                    claims_busy: tail.as_deref().is_some_and(crate::grok::turn_open),
                    tool_running: false,
                    needs_you: false,
                    silent_secs: Some(silent),
                },
            });
        }
    }
    agents
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(process_alive: bool, claims_busy: bool, silent_secs: Option<u64>) -> AgentSignal {
        AgentSignal {
            process_alive,
            claims_busy,
            silent_secs,
            ..Default::default()
        }
    }

    fn observed(id: &str, harness: &str, signal: AgentSignal) -> Observed {
        Observed {
            id: id.to_string(),
            harness: harness.to_string(),
            label: id.to_string(),
            doing: None,
            started_by: "outside",
            signal,
        }
    }

    #[test]
    fn cursor_turn_stays_open_until_turn_ended() {
        let tail = "{\"role\":\"user\",\"message\":{\"content\":[]}}\n{\"type\":\"turn_ended\",\"status\":\"success\"}\n";
        assert!(!cursor_turn_open(tail));
        assert!(cursor_turn_open(
            "{\"role\":\"user\",\"message\":{\"content\":[]}}\n"
        ));
    }

    #[test]
    fn dead_process_is_off_even_when_its_file_says_busy() {
        assert_eq!(classify(agent(false, true, Some(1))), AgentState::Off);
    }

    #[test]
    fn state_changes_exactly_at_the_thresholds() {
        let at = |secs, busy| classify(agent(true, busy, Some(secs)));
        assert_eq!(at(WORKING_WITHIN_SECS, true), AgentState::Working);
        assert_eq!(at(WORKING_WITHIN_SECS + 1, true), AgentState::Waiting);
        assert_eq!(at(STUCK_AFTER_SECS - 1, true), AgentState::Waiting);
        assert_eq!(at(STUCK_AFTER_SECS, true), AgentState::Stuck);
    }

    #[test]
    fn long_silence_is_only_stuck_if_the_agent_claims_busy() {
        assert_eq!(
            classify(agent(true, false, Some(STUCK_AFTER_SECS * 10))),
            AgentState::Waiting
        );
    }

    #[test]
    fn unknown_activity_never_reads_as_working_or_stuck() {
        assert_eq!(classify(agent(true, true, None)), AgentState::Waiting);
    }

    #[test]
    fn a_long_build_is_working_until_the_tool_limit() {
        let build = |secs| AgentSignal {
            tool_running: true,
            ..agent(true, true, Some(secs))
        };
        assert_eq!(classify(build(STUCK_AFTER_SECS * 2)), AgentState::Working);
        assert_eq!(classify(build(TOOL_STUCK_AFTER_SECS)), AgentState::Stuck);
    }

    #[test]
    fn waiting_for_you_is_never_called_stuck() {
        let signal = AgentSignal {
            needs_you: true,
            ..agent(true, true, Some(STUCK_AFTER_SECS * 5))
        };
        assert_eq!(classify(signal), AgentState::Waiting);
    }

    #[test]
    fn summary_counts_live_agents_and_ignores_dead_ones() {
        let status = summarize(
            "claude",
            &[
                agent(true, true, Some(5)),
                agent(true, false, Some(300)),
                agent(true, true, Some(4000)),
                agent(false, true, Some(1)),
            ],
        );
        assert_eq!(status.state, AgentState::Working);
        assert_eq!(
            status.agents,
            StateCounts {
                live: 3,
                working: 1,
                waiting: 1,
                stuck: 1,
            }
        );
    }

    #[test]
    fn chip_falls_back_through_stuck_waiting_and_off() {
        let chip = |signals: &[AgentSignal]| summarize("agy", signals).state;
        assert_eq!(chip(&[agent(true, true, Some(900))]), AgentState::Stuck);
        assert_eq!(chip(&[agent(true, false, Some(200))]), AgentState::Waiting);
        assert_eq!(chip(&[agent(false, false, None)]), AgentState::Off);
        assert_eq!(chip(&[]), AgentState::Off);
    }

    #[test]
    fn report_hides_forgotten_sessions_and_puts_your_turn_first() {
        let asks = AgentSignal {
            needs_you: true,
            ..agent(true, false, Some(400))
        };
        let report = build_report(vec![
            observed("working", "claude", agent(true, true, Some(3))),
            observed("asks", "codex", asks),
            observed("forgotten", "claude", agent(true, false, Some(90_000))),
            observed("dead", "claude", agent(false, true, Some(1))),
        ]);
        let order: Vec<&str> = report.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(order, ["asks", "working"]);
        let claude = report.harnesses.iter().find(|h| h.harness == "claude");
        assert_eq!(claude.map(|h| h.agents.live), Some(1));
    }

    #[test]
    fn task_colour_beats_repo_colour_and_unmatched_stays_default() {
        let mut report = build_report(vec![
            observed("a", "claude", agent(true, true, Some(3))),
            observed("b", "codex", agent(true, true, Some(3))),
            observed("c", "agy", agent(true, true, Some(3))),
        ]);
        let colors = AgentColors {
            tasks: BTreeMap::from([("a".to_string(), "#111111".to_string())]),
            repos: BTreeMap::from([
                ("a".to_string(), "#222222".to_string()),
                ("b".to_string(), "#333333".to_string()),
            ]),
        };
        apply_colors(&mut report, &colors);
        let color_of = |id: &str| {
            let agent = report.agents.iter().find(|a| a.id == id);
            agent.and_then(|a| a.color.clone())
        };
        assert_eq!(color_of("a").as_deref(), Some("#111111"));
        assert_eq!(color_of("b").as_deref(), Some("#333333"));
        assert_eq!(color_of("c"), None);
    }

    #[test]
    fn only_plain_hex_colours_are_stored_and_none_clears() {
        let change = |color: Option<&str>| ColorChange {
            scope: "repo".to_string(),
            key: "MoE".to_string(),
            color: color.map(str::to_string),
        };
        let mut colors = AgentColors::default();
        assert!(apply_color_change(&mut colors, change(Some("#22C55E"))).is_ok());
        assert_eq!(colors.repos.get("MoE").map(String::as_str), Some("#22C55E"));
        for bad in ["red", "#12345", "#12345g", "#123456;x", "url(x)"] {
            assert!(
                apply_color_change(&mut colors, change(Some(bad))).is_err(),
                "{bad}"
            );
        }
        assert!(apply_color_change(&mut colors, change(None)).is_ok());
        assert!(colors.repos.is_empty());
    }

    #[test]
    fn actions_read_as_short_phrases() {
        assert_eq!(describe_action("Read", "/a/b/main.rs"), "Reading main.rs");
        assert_eq!(
            describe_action("replace_file_content", "/a/style.css"),
            "Editing style.css"
        );
        assert_eq!(
            describe_action("Bash", "cargo test --bin moedeck"),
            "Running cargo test"
        );
        assert_eq!(
            describe_action("Bash", "Run the status tests"),
            "Run the status tests"
        );
        assert_eq!(describe_action("exec", ""), "Running a command");
        assert_eq!(
            describe_action("shell", "cd ~/repo && pytest -q tests"),
            "Running pytest -q"
        );
    }

    #[test]
    fn commands_cannot_leak_values_onto_a_pill() {
        let phrase = describe_action("Bash", "API_KEY=abc123 curl -H secret-token https://x");
        assert_eq!(phrase, "Running curl -H");
    }

    #[test]
    fn labels_use_the_repo_or_the_title_when_in_home() {
        let home = Path::new("/home/me");
        assert_eq!(label_for("/home/me/Projects/MoE", "x", home), "MoE");
        assert_eq!(
            label_for("/home/me", "Social pipeline", home),
            "Social pipeline"
        );
        assert_eq!(label_for("/home/me", "", home), "Home");
    }

    #[test]
    fn codex_turn_state_follows_the_newest_marker() {
        let line = |t: &str, p: Value| json!({"type": t, "payload": p}).to_string();
        let tail = [
            line("event_msg", json!({"type": "task_complete"})),
            line("event_msg", json!({"type": "task_started"})),
            line(
                "response_item",
                json!({"type": "custom_tool_call", "name": "exec"}),
            ),
        ]
        .join("\n");
        assert_eq!(
            codex_turn_state(&tail),
            (true, Some("Running a command".to_string()))
        );
        let done = format!(
            "{tail}\n{}",
            line("event_msg", json!({"type": "task_complete"}))
        );
        assert!(!codex_turn_state(&done).0);
    }

    #[test]
    fn last_claude_tool_reads_the_newest_call() {
        let tail = [
            json!({"message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": "/a/old.rs"}}]}}).to_string(),
            json!({"message": {"content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": "/a/new.rs"}}]}}).to_string(),
        ]
        .join("\n");
        assert_eq!(last_claude_tool(&tail).as_deref(), Some("Editing new.rs"));
    }

    #[test]
    fn a_recently_completed_turn_is_waiting_not_working() {
        assert_eq!(classify(agent(true, false, Some(1))), AgentState::Waiting);
    }

    #[test]
    fn last_agy_tool_prefers_its_own_summary() {
        let tail = json!({"tool_calls": [{"name": "run_command",
            "args": {"toolSummary": "Build the app", "CommandLine": "npm run build"}}]})
        .to_string();
        assert_eq!(last_agy_tool(&tail).as_deref(), Some("Build the app"));
    }
}
