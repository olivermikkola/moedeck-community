//! Outbound event webhooks.
//!
//! The gateway POSTs a small JSON body when a harness waits for approval, a
//! turn finishes, or a session is classified as stalled. Delivery runs on a
//! background task with a short timeout and a couple of retries. Nothing here
//! is consulted by the agent loop after the notify call returns.
//!
//! Targets come from `webhooks.json` in the config dir and from `MOEDECK_*`
//! (or legacy `HELM_*`) environment variables. With neither, webhooks stay off.

use chrono::SecondsFormat;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const SUMMARY_CHARS: usize = 300;
const TITLE_CHARS: usize = 120;
const SNIPPET_CHARS: usize = 8_000;
const ATTEMPTS: u32 = 3;
const DEFAULT_DEBOUNCE_SECS: u64 = 30;
const DEFAULT_TIMEOUT_SECS: u64 = 5;
const RETRY_BASE: Duration = Duration::from_millis(200);
/// How often a running gateway looks for sessions the status tracker calls stalled.
pub const STALL_POLL_SECS: u64 = 60;
const MAX_CONFIG_BYTES: u64 = 65_536;

type BoxFut = Pin<Box<dyn Future<Output = Result<u16, String>> + Send>>;
type Transport = Arc<dyn Fn(Delivery) -> BoxFut + Send + Sync>;

static GLOBAL: OnceLock<Arc<Dispatcher>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    ApprovalNeeded,
    TurnFinished,
    SessionStalled,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalNeeded => "approval_needed",
            Self::TurnFinished => "turn_finished",
            Self::SessionStalled => "session_stalled",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "approval_needed" | "approval-needed" => Some(Self::ApprovalNeeded),
            "turn_finished" | "turn-finished" => Some(Self::TurnFinished),
            "session_stalled" | "session-stalled" => Some(Self::SessionStalled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    debounce: Duration,
    timeout: Duration,
    origin: Option<String>,
    targets: Vec<Target>,
}

#[derive(Clone)]
struct Target {
    url: String,
    /// Empty means every event.
    events: Vec<EventKind>,
    secret_header: Option<String>,
    secret_value: Option<String>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("url", &public_target(&self.url))
            .field("events", &self.events)
            .field("secret_header", &self.secret_header)
            .field(
                "secret_value",
                &self.secret_value.as_ref().map(|_| "[redacted]"),
            )
            .finish()
    }
}

impl Target {
    fn accepts(&self, event: EventKind) -> bool {
        self.events.is_empty() || self.events.contains(&event)
    }
}

#[derive(Clone)]
struct Outbound {
    event: EventKind,
    conversation_id: String,
    harness: String,
    title: String,
    summary: String,
    outcome: Option<&'static str>,
}

#[derive(Serialize)]
struct Payload<'a> {
    event: &'a str,
    timestamp: String,
    harness: &'a str,
    conversation_id: &'a str,
    title: &'a str,
    summary: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    outcome: Option<&'a str>,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Clone)]
struct Delivery {
    url: String,
    headers: Vec<(String, String)>,
    body: String,
    timeout: Duration,
}

#[derive(Clone)]
pub struct TestResult {
    pub target: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
}

pub struct TestReport {
    pub event: EventKind,
    pub results: Vec<TestResult>,
}

impl TestReport {
    pub fn delivered(&self) -> usize {
        self.results.iter().filter(|r| r.ok).count()
    }

    pub fn failed(&self) -> usize {
        self.results.iter().filter(|r| !r.ok).count()
    }

    pub fn json(&self) -> Value {
        json_report(self)
    }
}

fn json_report(report: &TestReport) -> Value {
    let results: Vec<Value> = report
        .results
        .iter()
        .map(|r| {
            let mut item = serde_json::json!({
                "target": r.target,
                "ok": r.ok,
            });
            if let Some(status) = r.status {
                item["status"] = serde_json::json!(status);
            }
            if let Some(error) = &r.error {
                item["error"] = serde_json::json!(error);
            }
            item
        })
        .collect();
    serde_json::json!({
        "event": report.event.as_str(),
        "delivered": report.delivered(),
        "failed": report.failed(),
        "results": results,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StallNotice {
    pub conversation_id: String,
    pub harness: String,
    pub title: String,
    pub summary: String,
}

pub struct Dispatcher {
    targets: Vec<Target>,
    origin: Option<String>,
    debounce_ms: u64,
    timeout: Duration,
    retry_base: Duration,
    auto_flush: bool,
    include_ambient: bool,
    gate: Mutex<Gate>,
    transport: Transport,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
}

struct Gate {
    window_ms: u64,
    slots: HashMap<(EventKind, String), Slot>,
}

struct Slot {
    last_sent_ms: Option<u64>,
    pending: Option<Outbound>,
    armed: bool,
}

enum Step {
    Send(Outbound),
    /// A flush task should wake at this clock value.
    Arm {
        due_ms: u64,
    },
}

impl Gate {
    fn new(window_ms: u64) -> Self {
        Self {
            window_ms,
            slots: HashMap::new(),
        }
    }

    /// First event in a window is sent now. Later events for the same
    /// conversation and type replace a single pending send at the end of the
    /// window, so a chatty session cannot exceed one delivery per window.
    fn push(&mut self, now_ms: u64, item: Outbound) -> Option<Step> {
        if self.window_ms == 0 {
            return Some(Step::Send(item));
        }
        let key = (item.event, item.conversation_id.clone());
        let slot = self.slots.entry(key).or_insert(Slot {
            last_sent_ms: None,
            pending: None,
            armed: false,
        });
        let due = slot
            .last_sent_ms
            .map(|sent| sent.saturating_add(self.window_ms));
        if due.is_some_and(|due| now_ms < due) {
            slot.pending = Some(item);
            if slot.armed {
                return None;
            }
            slot.armed = true;
            return Some(Step::Arm {
                due_ms: due.unwrap_or(now_ms),
            });
        }
        slot.last_sent_ms = Some(now_ms);
        slot.pending = None;
        slot.armed = false;
        Some(Step::Send(item))
    }

    /// Pulls pending events whose window has elapsed. The second value is the
    /// next moment a still-waiting event is due, if the clock has not caught up.
    fn take_due(&mut self, now_ms: u64) -> (Vec<Outbound>, Option<u64>) {
        let mut ready = Vec::new();
        let mut next_due: Option<u64> = None;
        let window = self.window_ms;
        for slot in self.slots.values_mut() {
            if slot.pending.is_none() {
                slot.armed = false;
                continue;
            }
            let due = slot
                .last_sent_ms
                .map(|sent| sent.saturating_add(window))
                .unwrap_or(now_ms);
            if now_ms >= due {
                if let Some(item) = slot.pending.take() {
                    slot.last_sent_ms = Some(now_ms);
                    slot.armed = false;
                    ready.push(item);
                }
            } else {
                slot.armed = true;
                next_due = Some(next_due.map(|n| n.min(due)).unwrap_or(due));
            }
        }
        self.slots.retain(|_, slot| {
            slot.pending.is_some()
                || slot
                    .last_sent_ms
                    .is_some_and(|sent| now_ms.saturating_sub(sent) < window.saturating_mul(2))
        });
        (ready, next_due)
    }
}

pub fn install(origin_fallback: impl FnOnce() -> Option<String>) -> Arc<Dispatcher> {
    let dispatcher = Dispatcher::load(origin_fallback);
    let _ = GLOBAL.set(dispatcher.clone());
    if dispatcher.enabled() {
        let n = dispatcher.targets.len();
        eprintln!(
            "webhook: outbound hooks on ({n} target{}, debounce {}s)",
            if n == 1 { "" } else { "s" },
            dispatcher.debounce_ms / 1000
        );
        for target in &dispatcher.targets {
            eprintln!("webhook: {}", describe_target(target));
        }
    }
    dispatcher
}

fn global() -> Option<Arc<Dispatcher>> {
    GLOBAL.get().cloned()
}

/// Schedules an approval notification. Returns immediately.
pub fn notify_approval_needed(
    conversation_id: &str,
    harness: &str,
    tool: &str,
    target: &str,
    details: Option<&str>,
    risk: crate::models::RiskLevel,
) {
    let Some(dispatcher) = global() else {
        return;
    };
    if !dispatcher.wants(EventKind::ApprovalNeeded) || tokio_missing() {
        return;
    }
    let conversation_id = conversation_id.to_string();
    let harness = harness.to_string();
    let tool = tool.to_string();
    let target = target.to_string();
    let details = details.map(|d| first_chars(d, SNIPPET_CHARS));
    tokio::spawn(async move {
        let title = conversation_title(&conversation_id);
        let secrets = dispatcher.redaction_secrets();
        let summary = approval_summary(&tool, &target, details.as_deref(), risk, &secrets);
        dispatcher.submit(outbound(
            EventKind::ApprovalNeeded,
            &conversation_id,
            &harness,
            &title,
            summary,
            None,
            &secrets,
        ));
    });
}

/// Schedules a turn-finished notification. Returns immediately.
pub fn notify_turn_finished(conversation_id: &str, harness: &str, success: bool, text: &str) {
    let Some(dispatcher) = global() else {
        return;
    };
    if !dispatcher.wants(EventKind::TurnFinished) || tokio_missing() {
        return;
    }
    let conversation_id = conversation_id.to_string();
    let harness = harness.to_string();
    let text = first_chars(text, SNIPPET_CHARS);
    tokio::spawn(async move {
        let title = conversation_title(&conversation_id);
        let secrets = dispatcher.redaction_secrets();
        let summary = turn_summary(success, &text, &secrets);
        let outcome = if success { "success" } else { "error" };
        dispatcher.submit(outbound(
            EventKind::TurnFinished,
            &conversation_id,
            &harness,
            &title,
            summary,
            Some(outcome),
            &secrets,
        ));
    });
}

fn tokio_missing() -> bool {
    tokio::runtime::Handle::try_current().is_err()
}

fn outbound(
    event: EventKind,
    conversation_id: &str,
    harness: &str,
    title: &str,
    summary: String,
    outcome: Option<&'static str>,
    secrets: &[String],
) -> Outbound {
    Outbound {
        event,
        conversation_id: sanitize_id(conversation_id),
        harness: sanitize_harness_label(harness),
        title: sanitize_title(title, secrets),
        summary,
        outcome,
    }
}

pub async fn test_installed(event: EventKind) -> Result<TestReport, &'static str> {
    let Some(dispatcher) = global() else {
        return Err("Webhooks are not running in this process.");
    };
    if !dispatcher.enabled() {
        return Err("No webhook targets configured.");
    }
    let report = dispatcher.send_test(event).await;
    if report.results.is_empty() {
        return Err("No targets subscribe to this event.");
    }
    Ok(report)
}

pub async fn run_cli(args: &[String], origin_fallback: Option<String>) -> anyhow::Result<()> {
    let mut test = false;
    let mut event = EventKind::TurnFinished;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "test" => test = true,
            "--event" => {
                i += 1;
                let Some(name) = args.get(i) else {
                    eprintln!(
                        "usage: moedeck webhook test [--event approval_needed|turn_finished|session_stalled]"
                    );
                    std::process::exit(2);
                };
                let Some(parsed) = EventKind::parse(name) else {
                    eprintln!(
                        "unknown event `{name}` (use approval_needed, turn_finished, or session_stalled)"
                    );
                    std::process::exit(2);
                };
                event = parsed;
            }
            "-h" | "--help" => {
                print_webhook_help();
                return Ok(());
            }
            other => {
                eprintln!("unknown webhook argument `{other}`");
                print_webhook_help();
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let dispatcher = Dispatcher::load(|| origin_fallback);
    if !test {
        print_status(&dispatcher);
        return Ok(());
    }
    if !dispatcher.enabled() {
        eprintln!("No webhook targets configured.");
        eprintln!(
            "Add targets to {}/webhooks.json or set MOEDECK_WEBHOOK_URL.",
            crate::discovery::config_dir().display()
        );
        eprintln!("See docs/webhooks.md.");
        std::process::exit(2);
    }
    println!(
        "Sending {} to {} target{}...",
        event.as_str(),
        dispatcher.matching(event).len(),
        if dispatcher.matching(event).len() == 1 {
            ""
        } else {
            "s"
        }
    );
    let report = dispatcher.send_test(event).await;
    if report.results.is_empty() {
        eprintln!("No targets subscribe to `{}`.", event.as_str());
        std::process::exit(2);
    }
    print_report(&report);
    if report.failed() > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn print_webhook_help() {
    println!("usage: moedeck webhook [test] [--event EVENT]");
    println!();
    println!("  moedeck webhook");
    println!("      show configured targets (header values are never printed)");
    println!("  moedeck webhook test [--event EVENT]");
    println!("      POST one synthetic event and wait for each target");
    println!();
    println!("EVENT is approval_needed, turn_finished (default), or session_stalled.");
    println!("Configuration lives in $MOEDECK_CONFIG_DIR/webhooks.json and MOEDECK_WEBHOOK_*.");
    println!("See docs/webhooks.md. The older HELM_WEBHOOK_* names still work.");
}

fn print_status(dispatcher: &Dispatcher) {
    if !dispatcher.enabled() {
        println!("Outbound webhooks are off.");
        println!(
            "Add targets to {}/webhooks.json or set MOEDECK_WEBHOOK_URL.",
            crate::discovery::config_dir().display()
        );
        println!("See docs/webhooks.md.");
        return;
    }
    println!("Outbound webhooks are on.");
    println!("Debounce: {}s", dispatcher.debounce_ms / 1000);
    println!(
        "Timeout: {}s per attempt, {ATTEMPTS} attempts",
        dispatcher.timeout.as_secs()
    );
    match &dispatcher.origin {
        Some(origin) => println!("Conversation links: {origin}/?c=<id>"),
        None => println!("Conversation links: /?c=<id> on this gateway"),
    }
    println!("Targets:");
    for target in &dispatcher.targets {
        println!("  {}", describe_target(target));
    }
}

fn print_report(report: &TestReport) {
    for result in &report.results {
        if result.ok {
            println!("  ok   {}  {}", result.target, result.status.unwrap_or(0));
        } else {
            println!(
                "  fail {}  {}",
                result.target,
                result.error.as_deref().unwrap_or("delivery failed")
            );
        }
    }
    println!(
        "{} delivered, {} failed.",
        report.delivered(),
        report.failed()
    );
}

/// Where a pairing link should land after the session cookie is set.
/// Keeps `c` (the conversation deep link) and drops every other parameter,
/// including the login token.
pub fn location_after_login(query: &str) -> String {
    for pair in query.split('&') {
        let Some(raw) = pair.strip_prefix("c=") else {
            continue;
        };
        let id = url_decode(raw);
        if valid_conversation_ref(&id) {
            return conversation_path(&id);
        }
    }
    "/".to_string()
}

pub fn stall_notices(report: &crate::harness_status::StatusReport) -> Vec<StallNotice> {
    let overlays = crate::discovery::load_overlays();
    report
        .agents
        .iter()
        .filter(|agent| agent.state == crate::harness_status::AgentState::Stuck)
        .map(|agent| {
            let title = overlays
                .get(&agent.id)
                .and_then(|ov| ov.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| agent.label.clone());
            StallNotice {
                conversation_id: agent.id.clone(),
                harness: agent.harness.clone(),
                title,
                summary: stall_summary(agent.doing.as_deref(), agent.silent_secs),
            }
        })
        .collect()
}

pub fn newly_stalled(seen: &mut HashSet<String>, notices: Vec<StallNotice>) -> Vec<StallNotice> {
    let current: HashSet<String> = notices.iter().map(|n| n.conversation_id.clone()).collect();
    let fresh = notices
        .into_iter()
        .filter(|n| !seen.contains(&n.conversation_id))
        .collect();
    *seen = current;
    fresh
}

pub fn stall_summary(doing: Option<&str>, silent_secs: Option<u64>) -> String {
    let silence = match silent_secs {
        Some(secs) if secs >= 60 => format!(" Silent for {} min.", secs / 60),
        _ => String::new(),
    };
    match doing.map(str::trim).filter(|d| !d.is_empty()) {
        Some(doing) => format!("This session looks stalled during: {doing}.{silence}"),
        None => format!("This session looks stalled.{silence}"),
    }
}

impl Dispatcher {
    fn load(origin_fallback: impl FnOnce() -> Option<String>) -> Arc<Self> {
        let config = load_config();
        let enabled = !config.targets.is_empty();
        let origin = config
            .origin
            .clone()
            .or_else(|| if enabled { origin_fallback() } else { None });
        Arc::new(Self::from_config(config, origin, true))
    }

    fn from_config(config: Config, origin: Option<String>, include_ambient: bool) -> Self {
        let debounce_ms = duration_ms(config.debounce);
        let origin = config.origin.clone().or(origin);
        Self {
            targets: config.targets,
            origin,
            debounce_ms,
            timeout: config.timeout,
            retry_base: RETRY_BASE,
            auto_flush: true,
            include_ambient,
            gate: Mutex::new(Gate::new(debounce_ms)),
            transport: curl_transport(),
            clock: Arc::new(real_clock()),
        }
    }

    fn enabled(&self) -> bool {
        !self.targets.is_empty()
    }

    fn wants(&self, event: EventKind) -> bool {
        self.targets.iter().any(|t| t.accepts(event))
    }

    pub fn wants_session_stalled(&self) -> bool {
        self.wants(EventKind::SessionStalled)
    }

    fn matching(&self, event: EventKind) -> Vec<Target> {
        self.targets
            .iter()
            .filter(|t| t.accepts(event))
            .cloned()
            .collect()
    }

    fn submit(self: &Arc<Self>, item: Outbound) {
        if !self.wants(item.event) {
            return;
        }
        let now = (self.clock)();
        let step = self.lock_gate().push(now, item);
        match step {
            Some(Step::Send(item)) => self.spawn_send(item),
            Some(Step::Arm { due_ms }) if self.auto_flush => self.arm_flush(due_ms),
            _ => {}
        }
    }

    fn arm_flush(self: &Arc<Self>, mut due_ms: u64) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            for _ in 0..4 {
                let now = (this.clock)();
                let wait = due_ms.saturating_sub(now);
                if wait > 0 {
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                }
                let now = (this.clock)();
                let (ready, next) = this.lock_gate().take_due(now);
                for item in ready {
                    this.spawn_send(item);
                }
                match next {
                    Some(next) if next > (this.clock)() => due_ms = next,
                    _ => break,
                }
            }
        });
    }

    fn spawn_send(self: &Arc<Self>, item: Outbound) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let secrets = this.redaction_secrets();
            let Ok(body) = this.payload_body(&item) else {
                eprintln!("webhook: could not encode the payload");
                return;
            };
            for target in this.matching(item.event) {
                let delivery = this.delivery_for(&target, body.clone());
                if let Err((status, message)) =
                    deliver_with_retry(&this.transport, delivery, &secrets, this.retry_base).await
                {
                    eprintln!(
                        "{}",
                        failure_line(item.event, &target.url, status, &message)
                    );
                }
            }
        });
    }

    async fn send_test(&self, event: EventKind) -> TestReport {
        let item = test_outbound(event);
        let secrets = self.redaction_secrets();
        let body = self
            .payload_body(&item)
            .unwrap_or_else(|_| "{}".to_string());
        let mut results = Vec::new();
        for target in self.matching(event) {
            let delivery = self.delivery_for(&target, body.clone());
            let label = public_target(&target.url);
            match deliver_with_retry(&self.transport, delivery, &secrets, self.retry_base).await {
                Ok(status) => results.push(TestResult {
                    target: label,
                    ok: true,
                    status: Some(status),
                    error: None,
                }),
                Err((status, message)) => results.push(TestResult {
                    target: label,
                    ok: false,
                    status,
                    error: Some(message),
                }),
            }
        }
        TestReport { event, results }
    }

    pub fn notify_stalled(self: &Arc<Self>, notice: StallNotice) {
        if !self.wants(EventKind::SessionStalled) {
            return;
        }
        let secrets = self.redaction_secrets();
        let summary = summarize(&notice.summary, &secrets);
        self.submit(outbound(
            EventKind::SessionStalled,
            &notice.conversation_id,
            &notice.harness,
            &notice.title,
            summary,
            Some("stalled"),
            &secrets,
        ));
    }

    fn payload_body(&self, item: &Outbound) -> Result<String, String> {
        let payload = build_payload(item, self.origin.as_deref(), now_stamp());
        serde_json::to_string(&payload).map_err(|_| "could not encode the payload".to_string())
    }

    fn delivery_for(&self, target: &Target, body: String) -> Delivery {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "User-Agent".to_string(),
                format!("moedeck/{}", env!("CARGO_PKG_VERSION")),
            ),
        ];
        if let (Some(name), Some(value)) = (&target.secret_header, &target.secret_value) {
            if valid_header_name(name) && valid_header_value(value) {
                headers.push((name.clone(), value.clone()));
            }
        }
        Delivery {
            url: target.url.clone(),
            headers,
            body,
            timeout: self.timeout,
        }
    }

    fn redaction_secrets(&self) -> Vec<String> {
        let mut secrets = Vec::new();
        for target in &self.targets {
            if let Some(value) = &target.secret_value {
                if value.len() >= 6 {
                    secrets.push(value.clone());
                }
            }
            secrets.extend(sensitive_url_parts(&target.url));
        }
        if self.include_ambient {
            secrets.extend(ambient_secrets());
        }
        secrets
    }

    fn lock_gate(&self) -> std::sync::MutexGuard<'_, Gate> {
        self.gate.lock().unwrap_or_else(|err| err.into_inner())
    }
}

fn real_clock() -> impl Fn() -> u64 + Send + Sync {
    let start = std::time::Instant::now();
    move || start.elapsed().as_millis() as u64
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn test_outbound(event: EventKind) -> Outbound {
    let (summary, outcome) = match event {
        EventKind::ApprovalNeeded => (
            "MoE Deck sent this test event. A harness would be waiting for approval.",
            None,
        ),
        EventKind::TurnFinished => (
            "MoE Deck sent this test event. A turn would have finished successfully.",
            Some("success"),
        ),
        EventKind::SessionStalled => (
            "MoE Deck sent this test event. A session would look stalled.",
            Some("stalled"),
        ),
    };
    Outbound {
        event,
        conversation_id: "webhook-test".to_string(),
        harness: "claude".to_string(),
        title: "Webhook test".to_string(),
        summary: summary.to_string(),
        outcome,
    }
}

fn build_payload<'a>(item: &'a Outbound, origin: Option<&str>, timestamp: String) -> Payload<'a> {
    let path = conversation_path(&item.conversation_id);
    let url = origin.map(|base| format!("{}{path}", base.trim_end_matches('/')));
    Payload {
        event: item.event.as_str(),
        timestamp,
        harness: &item.harness,
        conversation_id: &item.conversation_id,
        title: &item.title,
        summary: &item.summary,
        outcome: item.outcome,
        path,
        url,
    }
}

fn conversation_path(id: &str) -> String {
    format!("/?c={}", url_encode(id))
}

fn conversation_title(id: &str) -> String {
    crate::discovery::load_overlays()
        .get(id)
        .and_then(|overlay| overlay.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Untitled".to_string())
}

pub fn approval_summary(
    tool: &str,
    target: &str,
    details: Option<&str>,
    risk: crate::models::RiskLevel,
    secrets: &[String],
) -> String {
    let mut text = format!("Approval needed ({}) for {}", risk_label(risk), tool.trim());
    let target = target.trim();
    if !target.is_empty() && target != tool.trim() {
        text.push_str(": ");
        text.push_str(target);
    }
    if let Some(details) = details.map(str::trim).filter(|d| !d.is_empty()) {
        text.push_str(". ");
        text.push_str(details);
    }
    summarize(&text, secrets)
}

pub fn turn_summary(success: bool, text: &str, secrets: &[String]) -> String {
    let plain = plain_language(text);
    if plain.is_empty() {
        let fallback = if success {
            "The turn finished."
        } else {
            "The turn failed."
        };
        return fallback.to_string();
    }
    summarize(&plain, secrets)
}

fn risk_label(risk: crate::models::RiskLevel) -> &'static str {
    match risk {
        crate::models::RiskLevel::Low => "low",
        crate::models::RiskLevel::Medium => "medium",
        crate::models::RiskLevel::High => "high",
        crate::models::RiskLevel::Critical => "critical",
    }
}

pub fn summarize(text: &str, secrets: &[String]) -> String {
    truncate_chars(&redact(&plain_language(text), secrets), SUMMARY_CHARS)
}

fn sanitize_title(title: &str, secrets: &[String]) -> String {
    let plain = redact(&plain_language(title), secrets);
    let plain = plain.trim();
    if plain.is_empty() {
        "Untitled".to_string()
    } else {
        truncate_chars(plain, TITLE_CHARS)
    }
}

fn sanitize_id(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).take(200).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

fn sanitize_harness_label(raw: &str) -> String {
    let label: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    let label = label.to_ascii_lowercase();
    if label.is_empty() {
        "agent".to_string()
    } else {
        label
    }
}

fn plain_language(text: &str) -> String {
    let text = crate::completion::strip_ansi(text);
    let text = text.replace(crate::completion::FAILURE_MARK, " ");
    let mut out = String::with_capacity(text.len().min(SNIPPET_CHARS));
    let mut space = false;
    for c in text.chars().take(SNIPPET_CHARS) {
        if c.is_control() {
            if !out.is_empty() {
                space = true;
            }
            continue;
        }
        if c.is_whitespace() {
            if !out.is_empty() {
                space = true;
            }
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out
}

fn first_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// Removes configured secrets and common credential shapes from text that
/// might be logged or placed in a summary. The replacement is the fixed
/// marker `[redacted]`.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut ordered: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|s| s.len() >= 6)
        .collect();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.len()));
    ordered.dedup();
    let mut out = text.to_string();
    for secret in ordered {
        if out.contains(secret) {
            out = out.replace(secret, "[redacted]");
        }
    }
    out = redact_bearer(&out);
    out = redact_known_prefixes(&out);
    out = redact_assignments(&out);
    out = redact_marked(&out, "helm_session=");
    out = redact_marked(&out, "token=");
    out
}

fn redact_bearer(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &lower[i..];
        if rest.len() >= 7 && rest.as_bytes()[..7].eq_ignore_ascii_case(b"bearer ") {
            let value_start = i + 7;
            let value_end = scan_token(&text.as_bytes()[value_start..]) + value_start;
            let value = &text[value_start..value_end];
            out.push_str(&text[i..value_start]);
            if value.chars().count() >= 8 {
                out.push_str("[redacted]");
            } else {
                out.push_str(value);
            }
            i = value_end;
            continue;
        }
        let len = utf8_len(text, i);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out
}

fn redact_known_prefixes(text: &str) -> String {
    type TokenCheck = fn(char) -> bool;
    let prefixes: &[(&str, usize, TokenCheck)] = &[
        ("sk-", 8, token_body),
        ("github_pat_", 16, token_body),
        ("ghp_", 16, token_body),
        ("xoxb-", 8, token_body),
        ("xoxa-", 8, token_body),
        ("xoxp-", 8, token_body),
        ("xoxr-", 8, token_body),
        ("xoxs-", 8, token_body),
        ("AKIA", 16, |c| c.is_ascii_uppercase() || c.is_ascii_digit()),
    ];
    let mut out = text.to_string();
    for (prefix, min_rest, ok) in prefixes {
        out = redact_prefix(&out, prefix, *min_rest, *ok);
    }
    out
}

fn token_body(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn redact_prefix(text: &str, prefix: &str, min_rest: usize, ok: fn(char) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with(prefix) {
            let rest = &text[i + prefix.len()..];
            let take = rest.chars().take_while(|c| ok(*c)).count();
            let rest_len: usize = rest.chars().take(take).map(char::len_utf8).sum();
            if take >= min_rest {
                out.push_str("[redacted]");
                i += prefix.len() + rest_len;
                continue;
            }
        }
        let len = utf8_len(text, i);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out
}

fn redact_marked(text: &str, marker: &str) -> String {
    let min_value = if marker == "token=" { 8 } else { 6 };
    let lower = text.to_ascii_lowercase();
    let marker_lower = marker.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if lower[i..].starts_with(&marker_lower) {
            let value_start = i + marker.len();
            let value_end = scan_token(&text.as_bytes()[value_start..]) + value_start;
            out.push_str(&text[i..value_start]);
            let value = &text[value_start..value_end];
            if value.chars().count() >= min_value {
                out.push_str("[redacted]");
            } else {
                out.push_str(value);
            }
            i = value_end;
            continue;
        }
        let len = utf8_len(text, i);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out
}

fn redact_assignments(text: &str) -> String {
    const KEYS: &[&str] = &[
        "session_token",
        "refresh_token",
        "access_token",
        "auth_token",
        "api_key",
        "api-key",
        "apikey",
        "passcode",
        "password",
        "secret",
    ];
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let boundary = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if boundary {
            if let Some((_key_len, value_start, value_end)) = assignment_at(&lower, i, KEYS) {
                out.push_str(&text[i..value_start]);
                let value = &text[value_start..value_end];
                if value.chars().count() >= 6 {
                    out.push_str("[redacted]");
                } else {
                    out.push_str(value);
                }
                i = value_end;
                continue;
            }
        }
        let len = utf8_len(text, i);
        out.push_str(&text[i..i + len]);
        i += len;
    }
    out
}

fn assignment_at(lower: &str, i: usize, keys: &[&str]) -> Option<(usize, usize, usize)> {
    let rest = &lower[i..];
    let mut matched: Option<usize> = None;
    for key in keys {
        if rest.starts_with(key) && matched.is_none_or(|len| key.len() > len) {
            matched = Some(key.len());
        }
    }
    let key_len = matched?;
    let after = i + key_len;
    let bytes = lower.as_bytes();
    let mut j = after;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j >= bytes.len() || (bytes[j] != b'=' && bytes[j] != b':') {
        return None;
    }
    j += 1;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
        j += 1;
    }
    let value_start = j;
    let value_end = scan_token(&bytes[value_start..]) + value_start;
    Some((key_len, value_start, value_end))
}

fn scan_token(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|b| {
            b.is_ascii_whitespace()
                || *b == b'"'
                || *b == b'\''
                || *b == b','
                || *b == b'}'
                || *b == b'&'
                || *b == b';'
        })
        .unwrap_or(bytes.len())
}

fn utf8_len(text: &str, i: usize) -> usize {
    text[i..].chars().next().map(char::len_utf8).unwrap_or(1)
}

fn ambient_secrets() -> Vec<String> {
    let cfg = crate::auth::load_auth_config();
    let mut out = Vec::new();
    if let Some(passcode) = crate::auth::effective_passcode(&cfg) {
        if passcode.len() >= 6 {
            out.push(passcode);
        }
    }
    if cfg.secret.len() >= 6 {
        out.push(cfg.secret);
    }
    out
}

pub fn failure_line(event: EventKind, url: &str, status: Option<u16>, message: &str) -> String {
    match status {
        Some(code) => format!(
            "webhook: delivery failed event={} target={} status={code} error={message}",
            event.as_str(),
            public_target(url)
        ),
        None => format!(
            "webhook: delivery failed event={} target={} error={message}",
            event.as_str(),
            public_target(url)
        ),
    }
}

fn describe_target(target: &Target) -> String {
    let events = if target.events.is_empty() {
        "approval_needed,turn_finished,session_stalled".to_string()
    } else {
        target
            .events
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(",")
    };
    let secret = match &target.secret_header {
        Some(name) if target.secret_value.is_some() => format!(" secret_header={name} (set)"),
        Some(name) => format!(" secret_header={name} (missing)"),
        None => String::new(),
    };
    let line = format!("{} events={events}{secret}", public_target(&target.url));
    let secrets: Vec<String> = target.secret_value.clone().into_iter().collect();
    redact(&line, &secrets)
}

/// Scheme, host, and path only. Userinfo and query strings stay out of logs.
pub fn public_target(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "invalid-url".to_string();
    };
    if scheme != "http" && scheme != "https" {
        return "invalid-url".to_string();
    }
    let rest = rest.split_once('@').map_or(rest, |(_, host)| host);
    let rest = rest.split_once('?').map_or(rest, |(path, _)| path);
    let rest = rest.split_once('#').map_or(rest, |(path, _)| path);
    if rest.is_empty() {
        "invalid-url".to_string()
    } else {
        format!("{scheme}://{rest}")
    }
}

fn sensitive_url_parts(url: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some((_, rest)) = url.split_once("://") else {
        return out;
    };
    if let Some((userinfo, _)) = rest.split_once('@') {
        if let Some((_, pass)) = userinfo.split_once(':') {
            if pass.len() >= 6 {
                out.push(pass.to_string());
            }
        }
        if userinfo.len() >= 6 {
            out.push(userinfo.to_string());
        }
    }
    if let Some((_, query)) = url.split_once('?') {
        let query = query.split_once('#').map_or(query, |(q, _)| q);
        if query.len() >= 6 {
            out.push(query.to_string());
        }
    }
    out
}

fn load_config() -> Config {
    let file = read_webhook_file();
    parse_config(file.as_deref(), real_env)
}

fn read_webhook_file() -> Option<String> {
    let path = crate::discovery::config_dir().join("webhooks.json");
    let meta = std::fs::metadata(&path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            eprintln!(
                "webhook: {} is readable by other users; tightening to mode 600",
                path.display()
            );
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
    if meta.len() > MAX_CONFIG_BYTES {
        eprintln!("webhook: webhooks.json is larger than 64KB; ignoring it");
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) => {
            eprintln!("webhook: could not read webhooks.json ({err})");
            None
        }
    }
}

fn real_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn setting_from(get: &impl Fn(&str) -> Option<String>, name: &str) -> Option<String> {
    get(&format!("MOEDECK_{name}"))
        .or_else(|| get(&format!("HELM_{name}")))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[derive(Deserialize)]
struct FileConfig {
    #[serde(default)]
    debounce_secs: Option<u64>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    public_base_url: Option<String>,
    #[serde(default)]
    targets: Vec<FileTarget>,
}

#[derive(Deserialize)]
struct FileTarget {
    url: String,
    #[serde(default)]
    events: EventList,
    #[serde(default)]
    secret_header: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    secret_env: Option<String>,
}

#[derive(Default)]
struct EventList(Vec<String>);

impl<'de> Deserialize<'de> for EventList {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::Null => Ok(Self(Vec::new())),
            Value::String(text) => Ok(Self(vec![text])),
            Value::Array(items) => {
                let mut names = Vec::new();
                for item in items {
                    match item {
                        Value::String(text) => names.push(text),
                        _ => return Err(serde::de::Error::custom("event names must be strings")),
                    }
                }
                Ok(Self(names))
            }
            _ => Err(serde::de::Error::custom(
                "events must be a string or a list of strings",
            )),
        }
    }
}

fn parse_config(file_text: Option<&str>, get: impl Fn(&str) -> Option<String>) -> Config {
    let mut debounce = DEFAULT_DEBOUNCE_SECS;
    let mut timeout = DEFAULT_TIMEOUT_SECS;
    let mut origin = None;
    let mut targets = Vec::new();

    if let Some(text) = file_text.map(str::trim).filter(|t| !t.is_empty()) {
        match serde_json::from_str::<FileConfig>(text) {
            Ok(file) => apply_file(
                &mut debounce,
                &mut timeout,
                &mut origin,
                &mut targets,
                file,
                &get,
            ),
            Err(err) => {
                eprintln!(
                    "webhook: webhooks.json could not be parsed at line {} column {}; file targets are off",
                    err.line(),
                    err.column()
                );
            }
        }
    }

    if let Some(raw) = setting_from(&get, "WEBHOOKS") {
        match parse_webhooks_env(&raw) {
            Ok(file) => {
                targets.clear();
                apply_file(
                    &mut debounce,
                    &mut timeout,
                    &mut origin,
                    &mut targets,
                    file,
                    &get,
                );
            }
            Err(()) => {
                eprintln!("webhook: MOEDECK_WEBHOOKS could not be parsed; that override is off");
            }
        }
    }

    if let Some(value) = setting_from(&get, "WEBHOOK_DEBOUNCE_SECS") {
        match value.parse::<u64>() {
            Ok(secs) => debounce = secs,
            Err(_) => eprintln!("webhook: ignoring invalid MOEDECK_WEBHOOK_DEBOUNCE_SECS"),
        }
    }
    if let Some(value) = setting_from(&get, "WEBHOOK_TIMEOUT_SECS") {
        match value.parse::<u64>() {
            Ok(secs) => timeout = secs,
            Err(_) => eprintln!("webhook: ignoring invalid MOEDECK_WEBHOOK_TIMEOUT_SECS"),
        }
    }
    if let Some(value) = setting_from(&get, "WEBHOOK_BASE_URL") {
        match normalize_origin(&value) {
            Some(next) => origin = Some(next),
            None => eprintln!(
                "webhook: ignoring MOEDECK_WEBHOOK_BASE_URL (use http://host:port with no token or userinfo)"
            ),
        }
    }
    if let Some(url) = setting_from(&get, "WEBHOOK_URL") {
        let events = setting_from(&get, "WEBHOOK_EVENTS").unwrap_or_default();
        let header = setting_from(&get, "WEBHOOK_SECRET_HEADER");
        let secret = setting_from(&get, "WEBHOOK_SECRET");
        if let Some(target) = make_target(&url, &[events], header, secret, None, &get) {
            targets.push(target);
        }
    }

    Config {
        debounce: Duration::from_secs(debounce.clamp(0, 3_600)),
        timeout: Duration::from_secs(timeout.clamp(1, 30)),
        origin,
        targets,
    }
}

fn apply_file(
    debounce: &mut u64,
    timeout: &mut u64,
    origin: &mut Option<String>,
    targets: &mut Vec<Target>,
    file: FileConfig,
    get: &impl Fn(&str) -> Option<String>,
) {
    if let Some(secs) = file.debounce_secs {
        *debounce = secs;
    }
    if let Some(secs) = file.timeout_secs {
        *timeout = secs;
    }
    if let Some(base) = file.public_base_url {
        match normalize_origin(&base) {
            Some(next) => *origin = Some(next),
            None => eprintln!(
                "webhook: ignoring public_base_url (use http://host:port with no token or userinfo)"
            ),
        }
    }
    for raw in file.targets {
        if let Some(target) = make_target(
            &raw.url,
            &raw.events.0,
            raw.secret_header,
            raw.secret,
            raw.secret_env,
            get,
        ) {
            targets.push(target);
        }
    }
}

fn parse_webhooks_env(raw: &str) -> Result<FileConfig, ()> {
    let value: Value = serde_json::from_str(raw).map_err(|_| ())?;
    if value.is_array() {
        let targets = serde_json::from_value(value).map_err(|_| ())?;
        return Ok(FileConfig {
            debounce_secs: None,
            timeout_secs: None,
            public_base_url: None,
            targets,
        });
    }
    serde_json::from_value(value).map_err(|_| ())
}

fn make_target(
    url: &str,
    events: &[String],
    secret_header: Option<String>,
    secret: Option<String>,
    secret_env: Option<String>,
    get: &impl Fn(&str) -> Option<String>,
) -> Option<Target> {
    let url = url.trim();
    if !valid_webhook_url(url) {
        eprintln!("webhook: skipped a target with an invalid URL");
        return None;
    }
    let (parsed, specified) = parse_event_list(events);
    if specified && parsed.is_empty() {
        eprintln!("webhook: skipped a target because none of its events are known");
        return None;
    }
    let header = secret_header
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty());
    let value = resolve_secret(secret, secret_env, get);
    if let Some(name) = &header {
        if !valid_header_name(name) {
            eprintln!("webhook: skipped a target because its secret header name is not valid");
            return None;
        }
        match &value {
            Some(secret) if !valid_header_value(secret) => {
                eprintln!(
                    "webhook: skipped the secret header on {} because the value contains a newline",
                    public_target(url)
                );
            }
            None => {
                eprintln!(
                    "webhook: {} has secret_header {name} but no secret value; the header will not be sent",
                    public_target(url)
                );
            }
            _ => {}
        }
    }
    let secret_value = value.filter(|v| valid_header_value(v));
    let secret_header = header.filter(|_| secret_value.is_some());
    Some(Target {
        url: url.to_string(),
        events: parsed,
        secret_header,
        secret_value,
    })
}

fn resolve_secret(
    secret: Option<String>,
    secret_env: Option<String>,
    get: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if let Some(name) = secret_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Some(value) = get(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            return Some(value);
        }
    }
    secret
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_event_list(raw: &[String]) -> (Vec<EventKind>, bool) {
    let mut events = Vec::new();
    let mut specified = false;
    for name in raw {
        for part in name.split(|c: char| c == ',' || c.is_whitespace()) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            specified = true;
            match EventKind::parse(part) {
                Some(event) if !events.contains(&event) => events.push(event),
                Some(_) => {}
                None => eprintln!("webhook: ignoring unknown event `{part}`"),
            }
        }
    }
    (events, specified)
}

fn normalize_origin(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.contains("token=") || raw.contains('@') || raw.contains('?') || raw.contains('#') {
        return None;
    }
    if !(raw.starts_with("http://") || raw.starts_with("https://")) {
        return None;
    }
    let host = raw.split_once("://")?.1;
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(raw.to_string())
}

fn valid_webhook_url(url: &str) -> bool {
    if url.len() > 2_000 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split_once('@').map_or(rest, |(_, host)| host);
    let host = host.split_once('/').map_or(host, |(host, _)| host);
    let host = host.split_once('?').map_or(host, |(host, _)| host);
    !host.is_empty()
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_header_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && !value.chars().any(|c| c == '\r' || c == '\n' || c == '\0')
}

fn valid_conversation_ref(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && !id.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '?' | '#' | '&')
        })
}

fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            match u8::from_str_radix(hex, 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                Err(_) => return String::new(),
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

async fn deliver_with_retry(
    transport: &Transport,
    delivery: Delivery,
    secrets: &[String],
    retry_base: Duration,
) -> Result<u16, (Option<u16>, String)> {
    let mut delay = retry_base;
    let mut last = (None, "delivery failed".to_string());
    for attempt in 1..=ATTEMPTS {
        match transport(delivery.clone()).await {
            Ok(code) if (200..300).contains(&code) => return Ok(code),
            Ok(code) => {
                last = (Some(code), format!("HTTP {code}"));
                if !retryable_status(code) || attempt == ATTEMPTS {
                    return Err((last.0, redact(&last.1, secrets)));
                }
            }
            Err(err) => {
                let message = truncate_chars(&redact(&err, secrets), SUMMARY_CHARS);
                last = (None, message);
                if !retryable_error(&last.1) || attempt == ATTEMPTS {
                    return Err(last);
                }
            }
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(3);
    }
    Err(last)
}

fn retryable_status(code: u16) -> bool {
    code == 408 || code == 429 || code >= 500
}

fn retryable_error(message: &str) -> bool {
    !message.contains("not installed") && !message.contains("invalid URL")
}

fn curl_transport() -> Transport {
    Arc::new(|delivery| {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || post_curl(&delivery))
                .await
                .unwrap_or_else(|_| Err("webhook delivery task stopped".to_string()))
        })
    })
}

fn post_curl(delivery: &Delivery) -> Result<u16, String> {
    let id = uuid::Uuid::new_v4();
    let dir = std::env::temp_dir();
    let body_path = dir.join(format!(".moedeck-wh-{id}.body"));
    let cfg_path = dir.join(format!(".moedeck-wh-{id}.cfg"));
    let _cleanup = TempFiles(vec![body_path.clone(), cfg_path.clone()]);
    write_private(&body_path, delivery.body.as_bytes())?;
    let cfg = render_curl_config(delivery, &body_path);
    write_private(&cfg_path, cfg.as_bytes())?;
    let output = Command::new("curl")
        .args(curl_args(&cfg_path))
        .output()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed or not on PATH".to_string()
            } else {
                format!("could not run curl: {err}")
            }
        })?;
    let code_raw = String::from_utf8_lossy(&output.stdout);
    let code = code_raw.trim().parse::<u16>().unwrap_or(0);
    if code == 0 {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim();
        if err.is_empty() {
            Err("connection failed".to_string())
        } else {
            Err(tail_chars(err, 500))
        }
    } else {
        Ok(code)
    }
}

fn curl_args(config_path: &Path) -> Vec<String> {
    vec!["--config".to_string(), config_path.display().to_string()]
}

fn render_curl_config(delivery: &Delivery, body_path: &Path) -> String {
    let secs = delivery.timeout.as_secs().clamp(1, 30);
    let mut text = format!(
        "silent\nshow-error\nproto = \"-all,http,https\"\nproto-redir = \"-all,http,https\"\nmax-time = {secs}\nconnect-timeout = {secs}\nrequest = \"POST\"\nurl = {}\ndata-binary = {}\noutput = \"/dev/null\"\nwrite-out = \"%{{http_code}}\"\n",
        curl_quote(&delivery.url),
        curl_quote(&format!("@{}", body_path.display())),
    );
    for (name, value) in &delivery.headers {
        text.push_str("header = ");
        text.push_str(&curl_quote(&format!("{name}: {value}")));
        text.push('\n');
    }
    text
}

fn curl_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        text.to_string()
    } else {
        text.chars().skip(count - max).collect()
    }
}

struct TempFiles(Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|err| format!("could not store the webhook body: {err}"))?;
    file.write_all(bytes)
        .map_err(|err| format!("could not store the webhook body: {err}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RiskLevel;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    fn sample_turn() -> Outbound {
        Outbound {
            event: EventKind::TurnFinished,
            conversation_id: "claude:abc".to_string(),
            harness: "claude".to_string(),
            title: "Fix login".to_string(),
            summary: "All checks passed.".to_string(),
            outcome: Some("success"),
        }
    }

    #[test]
    fn payload_shape_for_a_finished_turn() {
        let item = sample_turn();
        let payload = build_payload(
            &item,
            Some("http://100.64.0.1:7770"),
            "2026-11-10T16:25:00Z".to_string(),
        );
        let value = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "event": "turn_finished",
                "timestamp": "2026-11-10T16:25:00Z",
                "harness": "claude",
                "conversation_id": "claude:abc",
                "title": "Fix login",
                "summary": "All checks passed.",
                "outcome": "success",
                "path": "/?c=claude%3Aabc",
                "url": "http://100.64.0.1:7770/?c=claude%3Aabc"
            })
        );
        let url = value["url"].as_str().unwrap();
        assert!(!url.contains("token"));
        assert!(!url.contains("passcode"));
    }

    #[test]
    fn payload_omits_outcome_and_absolute_url_when_absent() {
        let item = Outbound {
            event: EventKind::ApprovalNeeded,
            conversation_id: "agy:agy-2:abc".to_string(),
            harness: "agy".to_string(),
            title: "Review the patch".to_string(),
            summary: "Approval needed (high) for Bash: cargo test".to_string(),
            outcome: None,
        };
        let value = serde_json::to_value(build_payload(&item, None, "2026-11-10T16:25:00Z".into()))
            .unwrap();
        let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "conversation_id",
                "event",
                "harness",
                "path",
                "summary",
                "timestamp",
                "title"
            ]
        );
        assert_eq!(value["event"], "approval_needed");
        assert_eq!(value["path"], "/?c=agy%3Aagy-2%3Aabc");
    }

    #[test]
    fn summary_is_short_plain_text_without_secrets_or_the_transcript_tail() {
        let mut text = "api_key=abcdEFGH12345678\n".to_string();
        text.push_str(&"a".repeat(400));
        text.push_str("UNIQUE_TAIL_MARKER");
        let summary = turn_summary(true, &text, &[]);
        assert!(summary.chars().count() <= SUMMARY_CHARS);
        assert!(!summary.contains("abcdEFGH12345678"));
        assert!(!summary.contains("UNIQUE_TAIL_MARKER"));
        assert!(summary.contains("[redacted]"));
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn approval_summary_redacts_the_request_and_states_the_ask() {
        let summary = approval_summary(
            "Bash",
            "cargo test",
            Some(r#"{"command":"curl","Authorization":"Bearer supersecretvalue"}"#),
            RiskLevel::High,
            &["supersecretvalue".to_string()],
        );
        assert!(summary.starts_with("Approval needed (high) for Bash: cargo test"));
        assert!(!summary.contains("supersecretvalue"));
        assert!(summary.contains("[redacted]"));
        assert!(summary.chars().count() <= SUMMARY_CHARS);
    }

    #[test]
    fn empty_turn_still_has_a_sentence() {
        assert_eq!(turn_summary(false, "  \n", &[]), "The turn failed.");
        assert_eq!(turn_summary(true, "", &[]), "The turn finished.");
    }

    #[test]
    fn filtering_keeps_each_target_on_its_events() {
        let file = r#"{
            "targets": [
                {"url": "https://hooks.example/approvals", "events": ["approval_needed"]},
                {"url": "https://hooks.example/turns", "events": "turn_finished"},
                {"url": "https://hooks.example/all"}
            ]
        }"#;
        let config = parse_config(Some(file), |_| None);
        let names = |event| {
            config
                .targets
                .iter()
                .filter(|t| t.accepts(event))
                .map(|t| public_target(&t.url))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(EventKind::ApprovalNeeded),
            vec![
                "https://hooks.example/approvals",
                "https://hooks.example/all"
            ]
        );
        assert_eq!(
            names(EventKind::TurnFinished),
            vec!["https://hooks.example/turns", "https://hooks.example/all"]
        );
        assert_eq!(
            names(EventKind::SessionStalled),
            vec!["https://hooks.example/all"]
        );
    }

    #[test]
    fn unknown_events_do_not_enable_a_target() {
        let file = r#"{"targets":[{"url":"https://hooks.example/a","events":["nope"]}]}"#;
        let config = parse_config(Some(file), |_| None);
        assert!(config.targets.is_empty());
    }

    #[test]
    fn env_replaces_file_targets_and_secret_env_wins() {
        let file = r#"{"debounce_secs":10,"targets":[{"url":"https://hooks.example/file","secret":"file-secret-value","secret_header":"X-File"}]}"#;
        let config = parse_config(Some(file), |key| {
            match key {
            "MOEDECK_WEBHOOKS" => Some(
                r#"[{"url":"https://hooks.example/env","events":["turn_finished"],"secret_header":"Authorization","secret_env":"GROK_WEBHOOK_SECRET","secret":"file-secret-value"}]"#
                    .to_string(),
            ),
            "GROK_WEBHOOK_SECRET" => Some("env-secret-value".to_string()),
            "MOEDECK_WEBHOOK_DEBOUNCE_SECS" => Some("45".to_string()),
            _ => None,
        }
        });
        assert_eq!(config.debounce, Duration::from_secs(45));
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.targets[0].url, "https://hooks.example/env");
        assert_eq!(
            config.targets[0].secret_value.as_deref(),
            Some("env-secret-value")
        );
        assert_eq!(config.targets[0].events, vec![EventKind::TurnFinished]);
        let rendered = format!("{:?}", config.targets[0]);
        assert!(!rendered.contains("env-secret-value"));
        assert!(!rendered.contains("file-secret-value"));
        assert!(rendered.contains("[redacted]"));
    }

    #[test]
    fn helm_url_and_missing_env_fall_back_to_the_file_secret() {
        let file = r#"{"targets":[{"url":"https://hooks.example/file","secret_header":"X-Auth","secret_env":"MISSING_SECRET","secret":"file-secret-value"}]}"#;
        let from_file = parse_config(Some(file), |_| None);
        assert_eq!(
            from_file.targets[0].secret_value.as_deref(),
            Some("file-secret-value")
        );
        let from_helm = parse_config(None, |key| {
            (key == "HELM_WEBHOOK_URL").then(|| "https://hooks.example/helm".to_string())
        });
        assert_eq!(from_helm.targets.len(), 1);
        assert_eq!(from_helm.targets[0].url, "https://hooks.example/helm");
        assert!(from_helm.targets[0].events.is_empty());
    }

    #[test]
    fn invalid_urls_and_token_bases_are_dropped() {
        let file = r#"{"public_base_url":"http://host/?token=abc","targets":[{"url":"file:///tmp/x"},{"url":"https://hooks.example/ok"}]}"#;
        let config = parse_config(Some(file), |_| None);
        assert!(config.origin.is_none());
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.targets[0].url, "https://hooks.example/ok");
    }

    #[test]
    fn off_when_nothing_is_configured() {
        let config = parse_config(None, |_| None);
        assert!(config.targets.is_empty());
        assert_eq!(config.debounce, Duration::from_secs(DEFAULT_DEBOUNCE_SECS));
    }

    #[test]
    fn debounce_sends_the_first_turn_and_coalesces_the_rest() {
        let mut gate = Gate::new(30_000);
        let mut item = sample_turn();
        item.summary = "first".into();
        assert!(matches!(gate.push(0, item.clone()), Some(Step::Send(_))));

        item.summary = "second".into();
        item.conversation_id = "claude:abc".into();
        assert!(matches!(
            gate.push(1_000, item.clone()),
            Some(Step::Arm { due_ms: 30_000 })
        ));
        item.summary = "latest".into();
        assert!(gate.push(2_000, item.clone()).is_none());

        let (early, _) = gate.take_due(29_999);
        assert!(early.is_empty());
        let (due, _) = gate.take_due(30_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].summary, "latest");
        let (again, _) = gate.take_due(30_000);
        assert!(again.is_empty());

        item.summary = "after window".into();
        assert!(matches!(gate.push(60_000, item), Some(Step::Send(_))));
    }

    #[test]
    fn debounce_is_per_conversation_and_per_event() {
        let mut gate = Gate::new(30_000);
        let mut item = sample_turn();
        assert!(matches!(gate.push(0, item.clone()), Some(Step::Send(_))));
        item.conversation_id = "other".into();
        assert!(matches!(gate.push(1, item.clone()), Some(Step::Send(_))));
        item.conversation_id = "claude:abc".into();
        item.event = EventKind::ApprovalNeeded;
        assert!(matches!(gate.push(2, item), Some(Step::Send(_))));
    }

    #[test]
    fn zero_debounce_sends_every_event() {
        let mut gate = Gate::new(0);
        let item = sample_turn();
        assert!(matches!(gate.push(0, item.clone()), Some(Step::Send(_))));
        assert!(matches!(gate.push(0, item), Some(Step::Send(_))));
    }

    #[test]
    fn stalled_edges_fire_once_until_the_session_recovers() {
        let notice = StallNotice {
            conversation_id: "claude:abc".into(),
            harness: "claude".into(),
            title: "Fix login".into(),
            summary: "This session looks stalled.".into(),
        };
        let other = StallNotice {
            conversation_id: "codex:z".into(),
            ..notice.clone()
        };
        let mut seen = HashSet::new();
        let first = newly_stalled(&mut seen, vec![notice.clone()]);
        assert_eq!(first.len(), 1);
        assert!(newly_stalled(&mut seen, vec![notice.clone()]).is_empty());
        let recovered = newly_stalled(&mut seen, vec![]);
        assert!(recovered.is_empty());
        let again = newly_stalled(&mut seen, vec![notice.clone(), other.clone()]);
        assert_eq!(again.len(), 2);
        let only_new = newly_stalled(&mut seen, vec![notice, other]);
        assert!(only_new.is_empty());
    }

    #[test]
    fn stall_summary_names_the_last_action() {
        let text = stall_summary(Some("Running cargo test"), Some(12 * 60));
        assert_eq!(
            text,
            "This session looks stalled during: Running cargo test. Silent for 12 min."
        );
    }

    #[test]
    fn redaction_strips_secrets_from_log_lines() {
        let secret = "supersecretvalue";
        let line = failure_line(
            EventKind::TurnFinished,
            &format!("https://user:{secret}@hooks.example/run?token={secret}"),
            Some(500),
            &redact(
                &format!("curl failed Authorization: Bearer {secret} sk-abcDEFghij123456"),
                &[secret.to_string()],
            ),
        );
        assert!(!line.contains(secret));
        assert!(!line.contains("user:"));
        assert!(!line.contains("token="));
        assert!(line.contains("https://hooks.example/run"));
        assert!(line.contains("[redacted]"));
        assert!(line.contains("turn_finished"));
        assert!(!line.contains("sk-abcDEFghij123456"));
    }

    #[test]
    fn redaction_leaves_ordinary_prose_alone() {
        let text = "The secret sauce passed the build and the token is fine.";
        assert_eq!(redact(text, &[]), text);
        assert_eq!(redact("token=short", &[]), "token=short");
    }

    #[test]
    fn assignment_and_prefix_patterns_redact_without_an_exact_secret() {
        let text = redact(
            "set api_key=abcdEFGH12345678 and ghp_abcdefghijklmnopqrst then AKIAABCDEFGHIJKLMNOP",
            &[],
        );
        assert!(!text.contains("abcdEFGH12345678"));
        assert!(!text.contains("ghp_abcdefghijklmnopqrst"));
        assert!(!text.contains("AKIAABCDEFGHIJKLMNOP"));
        assert_eq!(text.matches("[redacted]").count(), 3);
    }

    #[test]
    fn public_target_drops_userinfo_and_query() {
        assert_eq!(
            public_target("https://user:pass@hooks.example/run?token=abc"),
            "https://hooks.example/run"
        );
        assert_eq!(public_target("notaurl"), "invalid-url");
    }

    #[test]
    fn login_redirect_keeps_the_conversation_and_drops_the_token() {
        assert_eq!(
            location_after_login("token=sekret&c=claude%3Aabc"),
            "/?c=claude%3Aabc"
        );
        assert_eq!(
            location_after_login("c=claude%3Aabc&token=sekret"),
            "/?c=claude%3Aabc"
        );
        assert_eq!(location_after_login("token=sekret"), "/");
        assert!(!location_after_login("token=sekret&c=claude%3Aabc").contains("sekret"));
    }

    #[test]
    fn curl_argv_does_not_carry_the_secret() {
        let secret = "supersecretvalue";
        let delivery = Delivery {
            url: "https://hooks.example/run".into(),
            headers: vec![("X-Grok-Key".into(), secret.into())],
            body: "{\"event\":\"turn_finished\"}".into(),
            timeout: Duration::from_secs(5),
        };
        let path = std::env::temp_dir().join("moedeck-wh-test-body");
        let cfg = render_curl_config(&delivery, &path);
        assert!(cfg.contains(secret));
        let args = curl_args(Path::new("/tmp/.moedeck-wh-cfg"));
        let joined = args.join(" ");
        assert!(!joined.contains(secret));
        assert_eq!(args[0], "--config");
        let _ = std::fs::remove_file(&path);
    }

    struct Pair {
        dispatcher: Arc<Dispatcher>,
        now: Arc<AtomicU64>,
    }

    fn dispatcher_pair(targets: Vec<Target>, transport: Transport, debounce_ms: u64) -> Pair {
        let now = Arc::new(AtomicU64::new(0));
        let clock_read = now.clone();
        let dispatcher = Arc::new(Dispatcher {
            targets,
            origin: Some("http://127.0.0.1:7771".into()),
            debounce_ms,
            timeout: Duration::from_secs(5),
            retry_base: Duration::from_millis(1),
            auto_flush: false,
            include_ambient: false,
            gate: Mutex::new(Gate::new(debounce_ms)),
            transport,
            clock: Arc::new(move || clock_read.load(Ordering::SeqCst)),
        });
        Pair { dispatcher, now }
    }

    fn one_target(events: &[&str], secret: Option<&str>) -> Target {
        Target {
            url: "https://hooks.example/run?token=supersecretvalue".into(),
            events: events.iter().filter_map(|n| EventKind::parse(n)).collect(),
            secret_header: secret.map(|_| "X-Grok-Key".to_string()),
            secret_value: secret.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn dispatcher_filters_events_and_attaches_the_secret_header() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
        let transport: Transport = Arc::new(move |delivery| {
            let _ = tx.send(delivery);
            Box::pin(async { Ok(204u16) })
        });
        let pair = dispatcher_pair(
            vec![
                one_target(&["approval_needed"], Some("supersecretvalue")),
                Target {
                    url: "https://hooks.example/turns".into(),
                    events: vec![EventKind::TurnFinished],
                    secret_header: None,
                    secret_value: None,
                },
            ],
            transport,
            0,
        );
        pair.dispatcher.submit(sample_turn());
        let delivery = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.url, "https://hooks.example/turns");
        assert!(delivery
            .headers
            .iter()
            .all(|(name, _)| name != "X-Grok-Key"));
        let body: Value = serde_json::from_str(&delivery.body).unwrap();
        assert_eq!(body["event"], "turn_finished");
        assert_eq!(body["outcome"], "success");
        assert!(body["path"].as_str().unwrap().starts_with("/?c="));
        assert!(!delivery.body.contains("supersecretvalue"));

        let mut approval = sample_turn();
        approval.event = EventKind::ApprovalNeeded;
        approval.outcome = None;
        pair.dispatcher.submit(approval);
        let delivery = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            delivery.url,
            "https://hooks.example/run?token=supersecretvalue"
        );
        assert!(delivery
            .headers
            .iter()
            .any(|(name, value)| name == "X-Grok-Key" && value == "supersecretvalue"));
        assert!(!delivery.body.contains("supersecretvalue"));
    }

    #[tokio::test]
    async fn dispatcher_debounce_holds_the_latest_summary() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Delivery>();
        let transport: Transport = Arc::new(move |delivery| {
            let _ = tx.send(delivery);
            Box::pin(async { Ok(204u16) })
        });
        let pair = dispatcher_pair(
            vec![one_target(&["turn_finished"], None)],
            transport,
            30_000,
        );
        let mut item = sample_turn();
        item.summary = "first".into();
        pair.dispatcher.submit(item.clone());
        let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(first.body.contains("first"));

        item.summary = "latest".into();
        pair.dispatcher.submit(item);
        let none = tokio::time::timeout(Duration::from_millis(40), rx.recv()).await;
        assert!(none.is_err());

        pair.now.store(30_000, Ordering::SeqCst);
        let (ready, _) = pair.dispatcher.lock_gate().take_due(30_000);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].summary, "latest");
    }

    #[tokio::test]
    async fn retries_server_errors_and_stops_on_client_errors() {
        let hits = Arc::new(AtomicU32::new(0));
        let hits_for_transport = hits.clone();
        let transport: Transport = Arc::new(move |_delivery| {
            let n = hits_for_transport.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if n < 2 {
                    Err("connection reset supersecretvalue".into())
                } else {
                    Ok(204)
                }
            })
        });
        let pair = dispatcher_pair(
            vec![one_target(&[], Some("supersecretvalue"))],
            transport,
            0,
        );
        let report = pair.dispatcher.send_test(EventKind::TurnFinished).await;
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        assert_eq!(report.delivered(), 1);
        assert!(report.results[0].error.is_none());

        let hits = Arc::new(AtomicU32::new(0));
        let hits_for_transport = hits.clone();
        let transport: Transport = Arc::new(move |_delivery| {
            hits_for_transport.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(400u16) })
        });
        let pair = dispatcher_pair(
            vec![one_target(&[], Some("supersecretvalue"))],
            transport,
            0,
        );
        let report = pair.dispatcher.send_test(EventKind::ApprovalNeeded).await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(report.failed(), 1);
        let err = report.results[0].error.as_deref().unwrap();
        assert!(err.contains("HTTP 400"));
        assert!(!err.contains("supersecretvalue"));
        assert_eq!(report.results[0].target, "https://hooks.example/run");
    }
}
