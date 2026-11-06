//! Terminal cockpit for the MoE Deck daemon.
//!
//! `moedeck tui` attaches to a running daemon (or starts one) and renders the
//! same threads, harnesses, models, approvals and safety modes as the PWA.
//! The daemon remains the single source of truth; the TUI is just another
//! local client over the HTTP API.

use crate::models::{
    ApprovalRequestItem, ChatMessage, ConversationDetail, ConversationSummary, HarnessInfo,
    ModelOption, SafetyMode,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

const THREAD_WIDTH: u16 = 34;
const POLL_BOOTSTRAP: Duration = Duration::from_secs(2);
const POLL_DETAIL: Duration = Duration::from_millis(700);
const POLL_ANIMATION: Duration = Duration::from_millis(180);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn harness_glyph(id: &str) -> &'static str {
    match id {
        "claude" => "✳",
        "agy" => "◈",
        "codex" => "⌘",
        "opencode" => "⬡",
        "keel" => "⚓",
        _ => "•",
    }
}

fn relative_time(updated_at_ms: i64) -> String {
    let delta = chrono::Utc::now().timestamp_millis() - updated_at_ms;
    if delta < 0 {
        return "now".to_string();
    }
    let seconds = delta / 1000;
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

#[derive(Debug)]
enum ApiError {
    Unauthorized,
    Http(String),
    Network(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::Http(message) => write!(f, "{message}"),
            Self::Network(message) => write!(f, "{message}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Bootstrap {
    harnesses: Vec<HarnessInfo>,
    conversations: Vec<ConversationSummary>,
    pending_approvals: Vec<ApprovalRequestItem>,
    safety_mode: SafetyMode,
}

struct Api {
    client: Client<HttpConnector, Full<Bytes>>,
    base: String,
    cookie: Option<String>,
}

impl Api {
    fn new(port: u16) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        Self {
            client: Client::builder(TokioExecutor::new()).build(connector),
            base: format!("http://127.0.0.1:{port}"),
            cookie: None,
        }
    }

    async fn call(
        &mut self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), ApiError> {
        let uri = format!("{}{}", self.base, path);
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(&uri)
            .header("Accept", "application/json");
        if let Some(cookie) = &self.cookie {
            builder = builder.header("Cookie", cookie);
        }
        let payload = match body {
            Some(value) => {
                builder = builder
                    .header("Content-Type", "application/json")
                    .header("Origin", &self.base);
                Bytes::from(
                    serde_json::to_vec(&value)
                        .map_err(|e| ApiError::Network(format!("serialize request: {e}")))?,
                )
            }
            None => Bytes::new(),
        };
        let request = builder
            .body(Full::new(payload))
            .map_err(|e| ApiError::Network(format!("build request: {e}")))?;
        let response =
            self.client.request(request).await.map_err(|e| {
                ApiError::Network(format!("connect to MoE Deck on {}: {e}", self.base))
            })?;

        for value in response.headers().get_all("set-cookie") {
            if let Ok(raw) = value.to_str() {
                if let Some(pair) = raw.split(';').next() {
                    if pair.starts_with("helm_session=") {
                        self.cookie = Some(pair.to_string());
                    }
                }
            }
        }

        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| ApiError::Network(format!("read response: {e}")))?
            .to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        Ok((status, value))
    }

    async fn get(&mut self, path: &str) -> Result<(StatusCode, Value), ApiError> {
        self.call(Method::GET, path, None).await
    }

    async fn post(&mut self, path: &str, body: Value) -> Result<(StatusCode, Value), ApiError> {
        self.call(Method::POST, path, Some(body)).await
    }

    fn set_session_token(&mut self, token: &str) {
        self.cookie = Some(format!("helm_session={token}"));
    }

    async fn health(&mut self) -> bool {
        self.get("/api/health")
            .await
            .map(|(status, _)| status.is_success())
            .unwrap_or(false)
    }

    async fn login(&mut self, passcode: &str) -> Result<(), ApiError> {
        let (status, value) = self
            .post("/api/auth/login", json!({ "passcode": passcode }))
            .await?;
        if status.is_success() {
            Ok(())
        } else {
            Err(ApiError::Http(error_message(&value)))
        }
    }

    async fn bootstrap(&mut self) -> Result<Bootstrap, ApiError> {
        let (status, value) = self.get("/api/bootstrap").await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ApiError::Http(error_message(&value)));
        }
        serde_json::from_value(value)
            .map_err(|e| ApiError::Http(format!("invalid bootstrap response: {e}")))
    }

    async fn detail(&mut self, id: &str) -> Result<ConversationDetail, ApiError> {
        let (status, value) = self
            .get(&format!(
                "/api/conversations/{}?tail=200",
                encode_path_segment(id)
            ))
            .await?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        if !status.is_success() {
            return Err(ApiError::Http(error_message(&value)));
        }
        serde_json::from_value(value)
            .map_err(|e| ApiError::Http(format!("invalid conversation response: {e}")))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Threads,
    Composer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PaletteAction {
    NewThread,
    SearchThreads,
    FilterHarness,
    ModelPicker,
    Approvals,
    CycleSafety,
    Refresh,
    StopTurn,
    Help,
    Quit,
}

impl PaletteAction {
    const ALL: [PaletteAction; 10] = [
        PaletteAction::NewThread,
        PaletteAction::SearchThreads,
        PaletteAction::FilterHarness,
        PaletteAction::ModelPicker,
        PaletteAction::Approvals,
        PaletteAction::CycleSafety,
        PaletteAction::Refresh,
        PaletteAction::StopTurn,
        PaletteAction::Help,
        PaletteAction::Quit,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::NewThread => "New thread",
            Self::SearchThreads => "Search threads",
            Self::FilterHarness => "Filter by harness",
            Self::ModelPicker => "Choose harness & model",
            Self::Approvals => "Pending approvals",
            Self::CycleSafety => "Cycle safety mode",
            Self::Refresh => "Refresh now",
            Self::StopTurn => "Stop current turn",
            Self::Help => "Keyboard help",
            Self::Quit => "Quit MoE Deck",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::NewThread => "start a thread with the selected harness/model",
            Self::SearchThreads => "find a thread by title, preview, harness or model",
            Self::FilterHarness => "cycle All / Claude / AGY / Codex / OpenCode / Keel",
            Self::ModelPicker => "pick a harness and a model/provider",
            Self::Approvals => "review and decide pending tool approvals",
            Self::CycleSafety => "safe → standard → yolo",
            Self::Refresh => "reload threads and the open conversation",
            Self::StopTurn => "ask the daemon to stop the active turn",
            Self::Help => "show every key binding",
            Self::Quit => "leave the TUI; the daemon keeps running",
        }
    }
}

struct PaletteState {
    query: String,
    selected: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerFocus {
    Harness,
    Model,
}

struct ModelPickerState {
    harness: usize,
    model: usize,
    focus: PickerFocus,
}

struct App {
    api: Api,
    harnesses: Vec<HarnessInfo>,
    conversations: Vec<ConversationSummary>,
    pending: Vec<ApprovalRequestItem>,
    safety: SafetyMode,
    thread_idx: usize,
    thread_state: ListState,
    active_id: Option<String>,
    detail: Option<ConversationDetail>,
    harness_idx: usize,
    model_idx: usize,
    input: Vec<char>,
    cursor: usize,
    focus: Focus,
    message_scroll: usize,
    auto_scroll: bool,
    status: String,
    error: Option<String>,
    need_auth: bool,
    auth_input: Vec<char>,
    show_help: bool,
    show_approvals: bool,
    approval_idx: usize,
    quit: bool,
    workdir: String,
    animation: u64,
    harness_filter: Option<String>,
    search_active: bool,
    search_query: String,
    palette: Option<PaletteState>,
    picker: Option<ModelPickerState>,
}

impl App {
    fn new(api: Api) -> Self {
        Self {
            api,
            harnesses: Vec::new(),
            conversations: Vec::new(),
            pending: Vec::new(),
            safety: SafetyMode::Standard,
            thread_idx: 0,
            thread_state: ListState::default(),
            active_id: None,
            detail: None,
            harness_idx: 0,
            model_idx: 0,
            input: Vec::new(),
            cursor: 0,
            focus: Focus::Threads,
            message_scroll: 0,
            auto_scroll: true,
            status: "Connecting to MoE Deck…".to_string(),
            error: None,
            need_auth: false,
            auth_input: Vec::new(),
            show_help: false,
            show_approvals: false,
            approval_idx: 0,
            quit: false,
            workdir: crate::discovery::home_str(),
            animation: 0,
            harness_filter: None,
            search_active: false,
            search_query: String::new(),
            palette: None,
            picker: None,
        }
    }

    fn current_harness(&self) -> Option<&HarnessInfo> {
        self.harnesses.get(self.harness_idx)
    }

    fn current_model(&self) -> Option<&ModelOption> {
        self.current_harness()
            .and_then(|h| h.models.get(self.model_idx))
    }

    fn harness_color(&self, id: &str) -> Color {
        self.harnesses
            .iter()
            .find(|h| h.id == id)
            .map(|h| parse_hex_color(&h.accent))
            .unwrap_or(Color::Cyan)
    }

    fn visible_indices(&self) -> Vec<usize> {
        let query = self.search_query.trim().to_lowercase();
        self.conversations
            .iter()
            .enumerate()
            .filter(|(_, conversation)| {
                self.harness_filter
                    .as_deref()
                    .map(|harness| conversation.harness == harness)
                    .unwrap_or(true)
            })
            .filter(|(_, conversation)| {
                query.is_empty()
                    || conversation.title.to_lowercase().contains(&query)
                    || conversation.subtitle.to_lowercase().contains(&query)
                    || conversation.last_preview.to_lowercase().contains(&query)
                    || conversation.harness.to_lowercase().contains(&query)
                    || conversation.model_label.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn ensure_visible_selection(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        if !visible.contains(&self.thread_idx) {
            self.thread_idx = visible[0];
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let current = visible
            .iter()
            .position(|index| *index == self.thread_idx)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(visible.len() as isize) as usize;
        self.thread_idx = visible[next];
    }

    fn cycle_harness_filter(&mut self) {
        // Offered harnesses plus any that already have threads.
        let mut filters: Vec<Option<&str>> = vec![None];
        for id in ["claude", "agy", "codex", "opencode", "keel"] {
            if self.harnesses.iter().any(|h| h.id == id)
                || self.conversations.iter().any(|c| c.harness == id)
            {
                filters.push(Some(id));
            }
        }
        let current = filters
            .iter()
            .position(|filter| *filter == self.harness_filter.as_deref())
            .unwrap_or(0);
        self.harness_filter = filters[(current + 1) % filters.len()].map(str::to_string);
        self.ensure_visible_selection();
        self.status = match &self.harness_filter {
            Some(harness) => format!("Filter: {harness}"),
            None => "Filter: all harnesses".to_string(),
        };
    }

    fn filtered_palette_actions(&self) -> Vec<PaletteAction> {
        let query = self
            .palette
            .as_ref()
            .map(|palette| palette.query.trim().to_lowercase())
            .unwrap_or_default();
        PaletteAction::ALL
            .into_iter()
            .filter(|action| {
                query.is_empty()
                    || action.label().to_lowercase().contains(&query)
                    || action.hint().to_lowercase().contains(&query)
            })
            .collect()
    }

    fn open_palette(&mut self) {
        self.palette = Some(PaletteState {
            query: String::new(),
            selected: 0,
        });
    }

    fn open_model_picker(&mut self) {
        self.picker = Some(ModelPickerState {
            harness: self.harness_idx,
            model: self.model_idx,
            focus: PickerFocus::Harness,
        });
    }

    async fn execute_palette_action(&mut self, action: PaletteAction) {
        match action {
            PaletteAction::NewThread => {
                self.create_thread().await;
            }
            PaletteAction::SearchThreads => {
                self.focus = Focus::Threads;
                self.search_active = true;
                self.search_query.clear();
            }
            PaletteAction::FilterHarness => self.cycle_harness_filter(),
            PaletteAction::ModelPicker => self.open_model_picker(),
            PaletteAction::Approvals => {
                self.approval_idx = 0;
                self.show_approvals = true;
            }
            PaletteAction::CycleSafety => self.cycle_safety().await,
            PaletteAction::Refresh => {
                self.refresh_bootstrap().await;
                self.refresh_detail().await;
            }
            PaletteAction::StopTurn => self.stop_turn().await,
            PaletteAction::Help => self.show_help = true,
            PaletteAction::Quit => self.quit = true,
        }
    }

    async fn handle_palette_key(&mut self, key: KeyEvent) {
        if self.palette.is_none() {
            return;
        }
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Enter => {
                let selected = self
                    .palette
                    .as_ref()
                    .map(|palette| palette.selected)
                    .unwrap_or(0);
                let action = self.filtered_palette_actions().get(selected).copied();
                self.palette = None;
                if let Some(action) = action {
                    self.execute_palette_action(action).await;
                }
            }
            KeyCode::Up => {
                let count = self.filtered_palette_actions().len();
                if let Some(palette) = self.palette.as_mut() {
                    if count > 0 {
                        palette.selected = palette.selected.checked_sub(1).unwrap_or(count - 1);
                    }
                }
            }
            KeyCode::Down => {
                let count = self.filtered_palette_actions().len();
                if let Some(palette) = self.palette.as_mut() {
                    if count > 0 {
                        palette.selected = (palette.selected + 1) % count;
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.query.pop();
                    palette.selected = 0;
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(palette) = self.palette.as_mut() {
                    palette.query.clear();
                    palette.selected = 0;
                }
            }
            KeyCode::Char(character)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(palette) = self.palette.as_mut() {
                    palette.query.push(character);
                    palette.selected = 0;
                }
            }
            _ => {}
        }
    }

    fn handle_picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                picker.focus = match picker.focus {
                    PickerFocus::Harness => PickerFocus::Model,
                    PickerFocus::Model => PickerFocus::Harness,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => match picker.focus {
                PickerFocus::Harness => {
                    if !self.harnesses.is_empty() {
                        picker.harness = picker
                            .harness
                            .checked_sub(1)
                            .unwrap_or(self.harnesses.len() - 1);
                        picker.model = 0;
                    }
                }
                PickerFocus::Model => {
                    let count = self
                        .harnesses
                        .get(picker.harness)
                        .map(|harness| harness.models.len())
                        .unwrap_or(0);
                    if count > 0 {
                        picker.model = picker.model.checked_sub(1).unwrap_or(count - 1);
                    }
                }
            },
            KeyCode::Down | KeyCode::Char('j') => match picker.focus {
                PickerFocus::Harness => {
                    if !self.harnesses.is_empty() {
                        picker.harness = (picker.harness + 1) % self.harnesses.len();
                        picker.model = 0;
                    }
                }
                PickerFocus::Model => {
                    let count = self
                        .harnesses
                        .get(picker.harness)
                        .map(|harness| harness.models.len())
                        .unwrap_or(0);
                    if count > 0 {
                        picker.model = (picker.model + 1) % count;
                    }
                }
            },
            KeyCode::Enter => match picker.focus {
                PickerFocus::Harness => picker.focus = PickerFocus::Model,
                PickerFocus::Model => {
                    self.harness_idx = picker.harness;
                    self.model_idx = picker.model;
                    let harness = self
                        .current_harness()
                        .map(|h| h.short_name.clone())
                        .unwrap_or_else(|| "harness".to_string());
                    let model = self
                        .current_model()
                        .map(|m| m.label.clone())
                        .unwrap_or_else(|| "default".to_string());
                    self.status = format!("Selected {harness} / {model}");
                    self.picker = None;
                }
            },
            _ => {}
        }
    }

    async fn refresh_bootstrap(&mut self) {
        match self.api.bootstrap().await {
            Ok(bootstrap) => {
                self.harnesses = bootstrap.harnesses;
                self.conversations = bootstrap.conversations;
                self.pending = bootstrap.pending_approvals;
                self.safety = bootstrap.safety_mode;
                self.need_auth = false;
                self.error = None;

                if self.harnesses.is_empty() || self.harness_idx >= self.harnesses.len() {
                    self.harness_idx = 0;
                    self.model_idx = 0;
                } else {
                    let model_count = self.harnesses[self.harness_idx].models.len();
                    if self.model_idx >= model_count {
                        self.model_idx = 0;
                    }
                }

                if self.conversations.is_empty() {
                    self.active_id = None;
                    self.detail = None;
                    self.thread_idx = 0;
                    self.thread_state.select(None);
                    self.status = "No threads yet. Press n to start one.".to_string();
                    return;
                }

                if let Some(active) = &self.active_id {
                    if let Some(index) = self
                        .conversations
                        .iter()
                        .position(|conversation| &conversation.id == active)
                    {
                        self.thread_idx = index;
                    } else {
                        self.active_id = None;
                        self.detail = None;
                    }
                }

                if self.active_id.is_none() {
                    self.thread_idx = 0;
                    self.active_id = Some(self.conversations[0].id.clone());
                    self.workdir = self.conversations[0].workdir.clone();
                    self.apply_thread_harness_model(0);
                    self.detail = None;
                    self.auto_scroll = true;
                    self.refresh_detail().await;
                }

                self.thread_idx = self.thread_idx.min(self.conversations.len() - 1);
                self.ensure_visible_selection();
                self.status = format!(
                    "{} thread{} · {} pending approval{}",
                    self.conversations.len(),
                    if self.conversations.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    self.pending.len(),
                    if self.pending.len() == 1 { "" } else { "s" }
                );
            }
            Err(ApiError::Unauthorized) => {
                self.need_auth = true;
                self.status = "Passcode required".to_string();
            }
            Err(error) => {
                self.error = Some(error.to_string());
                self.status = "MoE Deck is not reachable".to_string();
            }
        }
    }

    async fn refresh_detail(&mut self) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        match self.api.detail(&id).await {
            Ok(detail) => {
                self.workdir = detail.summary.workdir.clone();
                self.detail = Some(detail);
                self.error = None;
            }
            Err(ApiError::Unauthorized) => {
                self.need_auth = true;
            }
            Err(ApiError::Http(message)) if message.contains("not found") => {
                self.active_id = None;
                self.detail = None;
            }
            Err(error) => {
                self.error = Some(error.to_string());
            }
        }
    }

    fn apply_thread_harness_model(&mut self, index: usize) {
        let Some(conversation) = self.conversations.get(index) else {
            return;
        };
        if let Some(harness_index) = self
            .harnesses
            .iter()
            .position(|harness| harness.id == conversation.harness)
        {
            self.harness_idx = harness_index;
        }
        if let Some(harness) = self.harnesses.get(self.harness_idx) {
            self.model_idx = harness
                .models
                .iter()
                .position(|model| model.id == conversation.model)
                .unwrap_or(0);
        }
    }

    async fn open_thread(&mut self, index: usize) {
        let Some(conversation) = self.conversations.get(index).cloned() else {
            return;
        };
        self.thread_idx = index;
        self.thread_state.select(Some(index));
        self.active_id = Some(conversation.id.clone());
        self.workdir = conversation.workdir.clone();
        self.apply_thread_harness_model(index);
        self.detail = None;
        self.message_scroll = 0;
        self.auto_scroll = true;
        self.focus = Focus::Composer;
        self.refresh_detail().await;
    }

    async fn create_thread(&mut self) -> bool {
        let harness = self
            .current_harness()
            .map(|h| h.id.clone())
            .unwrap_or_else(|| "claude".to_string());
        let model = self
            .current_model()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| "default".to_string());
        let body = json!({
            "harness": harness,
            "model": model,
            "effort": "default",
            "workdir": self.workdir,
        });
        match self.api.post("/api/conversations", body).await {
            Ok((status, value)) if status.is_success() => {
                let Some(id) = value.get("id").and_then(Value::as_str) else {
                    self.error = Some("MoE Deck did not return a thread id".to_string());
                    return false;
                };
                self.active_id = Some(id.to_string());
                self.detail = None;
                self.message_scroll = 0;
                self.auto_scroll = true;
                self.focus = Focus::Composer;
                self.status = "New thread".to_string();
                self.refresh_bootstrap().await;
                true
            }
            Ok((_, value)) => {
                self.error = Some(error_message(&value));
                false
            }
            Err(error) => {
                self.error = Some(error.to_string());
                false
            }
        }
    }

    async fn send_message(&mut self) {
        let text: String = self.input.iter().collect();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if self.active_id.is_none() && !self.create_thread().await {
            return;
        }
        let Some(id) = self.active_id.clone() else {
            return;
        };
        let harness = self
            .current_harness()
            .map(|h| h.id.clone())
            .unwrap_or_else(|| "claude".to_string());
        let model = self
            .current_model()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| "default".to_string());
        let body = json!({
            "text": text,
            "harness": harness,
            "model": model,
            "effort": "default",
        });
        match self
            .api
            .post(
                &format!("/api/conversations/{}/messages", encode_path_segment(&id)),
                body,
            )
            .await
        {
            Ok((status, _value)) if status.is_success() => {
                self.input.clear();
                self.cursor = 0;
                self.auto_scroll = true;
                self.status = format!("Sent to {} · {}", harness, model);
                self.refresh_detail().await;
            }
            Ok((_, value)) => {
                self.error = Some(error_message(&value));
            }
            Err(ApiError::Unauthorized) => {
                self.need_auth = true;
            }
            Err(error) => {
                self.error = Some(error.to_string());
            }
        }
    }

    async fn stop_turn(&mut self) {
        let Some(id) = self.active_id.clone() else {
            return;
        };
        match self
            .api
            .post(
                &format!("/api/conversations/{}/stop", encode_path_segment(&id)),
                json!({}),
            )
            .await
        {
            Ok((status, _)) if status.is_success() => {
                self.status = "Stop requested".to_string();
                self.refresh_detail().await;
            }
            Ok((_, value)) => self.error = Some(error_message(&value)),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    async fn decide_approval(&mut self, decision: &str) {
        let Some(approval) = self.pending.get(self.approval_idx).cloned() else {
            return;
        };
        match self
            .api
            .post(
                &format!(
                    "/api/approvals/{}/decision",
                    encode_path_segment(&approval.id)
                ),
                json!({ "decision": decision }),
            )
            .await
        {
            Ok((status, _value)) if status.is_success() => {
                self.status = format!("{}: {}", decision, approval.tool_name);
                self.refresh_bootstrap().await;
                self.refresh_detail().await;
            }
            Ok((_, value)) => self.error = Some(error_message(&value)),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    async fn cycle_safety(&mut self) {
        let next = match self.safety {
            SafetyMode::Safe => SafetyMode::Standard,
            SafetyMode::Standard => SafetyMode::Yolo,
            SafetyMode::Yolo => SafetyMode::Safe,
        };
        let mode = format!("{next:?}").to_lowercase();
        match self
            .api
            .post("/api/approvals/safety-mode", json!({ "mode": mode }))
            .await
        {
            Ok((status, _value)) if status.is_success() => {
                self.safety = next;
                self.status = format!("Safety mode: {mode}");
            }
            Ok((_, value)) => self.error = Some(error_message(&value)),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn cycle_harness(&mut self) {
        if self.harnesses.is_empty() {
            return;
        }
        self.harness_idx = (self.harness_idx + 1) % self.harnesses.len();
        self.model_idx = 0;
        self.status = format!("Harness: {}", self.harnesses[self.harness_idx].short_name);
    }

    fn cycle_model(&mut self) {
        let Some(harness) = self.current_harness() else {
            return;
        };
        if harness.models.is_empty() {
            return;
        }
        let next = (self.model_idx + 1) % harness.models.len();
        let label = harness.models[next].label.clone();
        self.model_idx = next;
        self.status = format!("Model: {label}");
    }

    async fn login(&mut self) {
        let passcode: String = self.auth_input.iter().collect();
        if passcode.is_empty() {
            return;
        }
        match self.api.login(&passcode).await {
            Ok(()) => {
                self.auth_input.clear();
                self.need_auth = false;
                self.refresh_bootstrap().await;
            }
            Err(error) => {
                self.error = Some(format!("Login failed: {error}"));
            }
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }

        if self.need_auth {
            match key.code {
                KeyCode::Esc => self.quit = true,
                KeyCode::Enter => self.login().await,
                KeyCode::Backspace => {
                    self.auth_input.pop();
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.auth_input.push(character);
                }
                _ => {}
            }
            return;
        }

        if self.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')
            ) {
                self.show_help = false;
            }
            return;
        }

        if self.show_approvals {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.show_approvals = false,
                KeyCode::Char('j') | KeyCode::Down => {
                    if !self.pending.is_empty() {
                        self.approval_idx = (self.approval_idx + 1) % self.pending.len();
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !self.pending.is_empty() {
                        self.approval_idx = self
                            .approval_idx
                            .checked_sub(1)
                            .unwrap_or(self.pending.len() - 1);
                    }
                }
                KeyCode::Char('y') => self.decide_approval("allow_once").await,
                KeyCode::Char('!') => self.decide_approval("allow_always").await,
                KeyCode::Char('n') => self.decide_approval("deny").await,
                _ => {}
            }
            return;
        }

        if self.palette.is_some() {
            self.handle_palette_key(key).await;
            return;
        }
        if self.picker.is_some() {
            self.handle_picker_key(key);
            return;
        }
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_palette();
            return;
        }

        if self.search_active {
            match key.code {
                KeyCode::Esc => {
                    self.search_active = false;
                    self.search_query.clear();
                    self.ensure_visible_selection();
                }
                KeyCode::Enter => {
                    self.search_active = false;
                    self.open_thread(self.thread_idx).await;
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.ensure_visible_selection();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.search_query.clear();
                    self.ensure_visible_selection();
                }
                KeyCode::Down => self.move_selection(1),
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.search_query.push(character);
                    self.ensure_visible_selection();
                }
                _ => {}
            }
            return;
        }

        match self.focus {
            Focus::Threads => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                KeyCode::Char('g') => {
                    if let Some(first) = self.visible_indices().first().copied() {
                        self.thread_idx = first;
                    }
                }
                KeyCode::Char('G') => {
                    if let Some(last) = self.visible_indices().last().copied() {
                        self.thread_idx = last;
                    }
                }
                KeyCode::Enter => self.open_thread(self.thread_idx).await,
                KeyCode::Char('i') => self.focus = Focus::Composer,
                KeyCode::Char('n') => {
                    self.create_thread().await;
                }
                KeyCode::Char('h') => self.cycle_harness(),
                KeyCode::Char('m') => self.cycle_model(),
                KeyCode::Char('M') => self.open_model_picker(),
                KeyCode::Char(':') => self.open_palette(),
                KeyCode::Char('f') => self.cycle_harness_filter(),
                KeyCode::Char('/') => {
                    self.search_active = true;
                    self.search_query.clear();
                }
                KeyCode::Char('a') => {
                    self.approval_idx = 0;
                    self.show_approvals = true;
                }
                KeyCode::Char('s') => self.cycle_safety().await,
                KeyCode::Char('r') => {
                    self.refresh_bootstrap().await;
                    self.refresh_detail().await;
                }
                KeyCode::Char('?') => self.show_help = true,
                KeyCode::Char('x') => self.stop_turn().await,
                KeyCode::PageUp => {
                    self.auto_scroll = false;
                    self.message_scroll = self.message_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.message_scroll = self.message_scroll.saturating_add(10);
                }
                _ => {}
            },
            Focus::Composer => match key.code {
                KeyCode::Esc => self.focus = Focus::Threads,
                KeyCode::Tab => self.focus = Focus::Threads,
                KeyCode::Enter => self.send_message().await,
                KeyCode::Backspace => {
                    if self.cursor > 0 {
                        self.cursor -= 1;
                        self.input.remove(self.cursor);
                    }
                }
                KeyCode::Delete => {
                    if self.cursor < self.input.len() {
                        self.input.remove(self.cursor);
                    }
                }
                KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
                KeyCode::Right => {
                    if self.cursor < self.input.len() {
                        self.cursor += 1;
                    }
                }
                KeyCode::Home => self.cursor = 0,
                KeyCode::End => self.cursor = self.input.len(),
                KeyCode::PageUp => {
                    self.auto_scroll = false;
                    self.message_scroll = self.message_scroll.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    self.message_scroll = self.message_scroll.saturating_add(10);
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.input.clear();
                    self.cursor = 0;
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    while self.cursor > 0 && self.input[self.cursor - 1].is_whitespace() {
                        self.cursor -= 1;
                        self.input.remove(self.cursor);
                    }
                    while self.cursor > 0 && !self.input[self.cursor - 1].is_whitespace() {
                        self.cursor -= 1;
                        self.input.remove(self.cursor);
                    }
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT) =>
                {
                    self.input.insert(self.cursor, character);
                    self.cursor += 1;
                }
                _ => {}
            },
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        if self.need_auth {
            self.render_auth(frame, area);
            return;
        }
        if self.show_help {
            self.render_help(frame, area);
            return;
        }
        if self.show_approvals {
            self.render_approvals(frame, area);
            return;
        }
        if self.palette.is_some() {
            self.render_palette(frame, area);
            return;
        }
        if self.picker.is_some() {
            self.render_model_picker(frame, area);
            return;
        }

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(4),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_header(frame, vertical[0]);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(THREAD_WIDTH), Constraint::Min(24)])
            .split(vertical[1]);
        self.render_threads(frame, body[0]);
        self.render_conversation(frame, body[1]);
        self.render_footer(frame, vertical[2]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let harness_id = self
            .current_harness()
            .map(|h| h.id.clone())
            .unwrap_or_default();
        let harness = self
            .current_harness()
            .map(|h| h.short_name.clone())
            .unwrap_or_else(|| "no harness".to_string());
        let harness_color = self.harness_color(&harness_id);
        let model = self
            .current_model()
            .map(|m| m.label.clone())
            .unwrap_or_else(|| "no model".to_string());
        let safety = match self.safety {
            SafetyMode::Safe => Span::styled(
                "SAFE",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            SafetyMode::Standard => Span::styled(
                "STANDARD",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            SafetyMode::Yolo => Span::styled(
                "YOLO",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        };
        let pending = if self.pending.is_empty() {
            Span::styled("no approvals", Style::default().fg(Color::DarkGray))
        } else {
            Span::styled(
                format!("⚠ {} approval(s)", self.pending.len()),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )
        };
        let line = Line::from(vec![
            Span::styled(
                " MoE Deck ",
                Style::default()
                    .bg(parse_hex_color("#06B6D4"))
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            safety,
            Span::raw("  "),
            Span::styled(
                format!("{} {harness}", harness_glyph(&harness_id)),
                Style::default()
                    .fg(harness_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled(model, Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(
                truncate(&self.workdir, 48),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            pending,
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_threads(&mut self, frame: &mut Frame, area: Rect) {
        let visible = self.visible_indices();
        let items: Vec<ListItem> = visible
            .iter()
            .map(|index| {
                let conversation = &self.conversations[*index];
                let color = self.harness_color(&conversation.harness);
                let dot = if conversation.status == "busy" {
                    SPINNER[(self.animation as usize) % SPINNER.len()]
                } else {
                    "○"
                };
                let approval = if conversation.pending_approval.is_some() {
                    Span::styled(" !", Style::default().fg(Color::Red))
                } else {
                    Span::raw("")
                };
                let first = Line::from(vec![
                    Span::styled(
                        format!("{dot} "),
                        Style::default().fg(if conversation.status == "busy" {
                            Color::Green
                        } else {
                            color
                        }),
                    ),
                    Span::styled(
                        format!("{} ", harness_glyph(&conversation.harness)),
                        Style::default().fg(color),
                    ),
                    Span::styled(
                        truncate(&conversation.title, 23),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    approval,
                ]);
                let harness = self
                    .harnesses
                    .iter()
                    .find(|h| h.id == conversation.harness)
                    .map(|h| h.short_name.as_str())
                    .unwrap_or(conversation.harness.as_str());
                let second = Line::from(vec![
                    Span::styled(truncate(harness, 10), Style::default().fg(color)),
                    Span::styled(
                        truncate(
                            &format!(
                                " · {} · {}",
                                conversation.model_label,
                                relative_time(conversation.updated_at)
                            ),
                            21,
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]);
                ListItem::new(vec![first, second])
            })
            .collect();

        let title = if self.search_active {
            format!(" Search: {} ", truncate(&self.search_query, 20))
        } else if let Some(filter) = &self.harness_filter {
            format!(" {} · {} ", harness_glyph(filter), filter)
        } else {
            format!(" Threads · {} ", visible.len())
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        if items.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "No matching threads.\nPress / to search or f to change the harness filter.",
                    Style::default().fg(Color::DarkGray),
                )))
                .block(block)
                .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        let selected = visible
            .iter()
            .position(|index| *index == self.thread_idx)
            .unwrap_or(0);
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(36, 36, 42))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        self.thread_state.select(Some(selected));
        frame.render_stateful_widget(list, area, &mut self.thread_state);
    }

    fn render_conversation(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(3)])
            .split(area);

        let Some(detail) = self.detail.as_ref() else {
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title(" Conversation ");
            let paragraph = Paragraph::new(Line::from(Span::styled(
                "Select a thread with Enter, or press n for a new one.",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block)
            .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, chunks[0]);
            self.render_composer(frame, chunks[1]);
            return;
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        for message in &detail.messages {
            append_message_lines(&mut lines, message, &self.harnesses);
        }
        let title = format!(
            " {} · {} · {} ",
            truncate(&detail.summary.title, 42),
            detail.summary.harness,
            detail.summary.model_label
        );
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ));
        let content_height = wrapped_line_count(&lines, chunks[0].width.saturating_sub(2));
        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false });
        let max_scroll = content_height.saturating_sub(chunks[0].height.saturating_sub(2) as usize);
        if self.auto_scroll {
            self.message_scroll = max_scroll;
        } else {
            self.message_scroll = self.message_scroll.min(max_scroll);
        }
        let paragraph = paragraph.scroll((self.message_scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(paragraph, chunks[0]);
        self.render_composer(frame, chunks[1]);
    }

    fn render_composer(&self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Composer;
        let input: String = self.input.iter().collect();
        let accent = if focused {
            parse_hex_color("#06B6D4")
        } else {
            Color::DarkGray
        };
        let text = if input.is_empty() {
            Line::from(vec![
                Span::styled("› ", Style::default().fg(accent)),
                Span::styled(
                    "Ask the selected harness…",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled("› ", Style::default().fg(accent)),
                Span::raw(input),
            ])
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(accent))
            .title(Span::styled(
                if focused {
                    " Message · Enter send · Esc threads "
                } else {
                    " Message · i to type "
                },
                Style::default().fg(accent),
            ));
        frame.render_widget(Paragraph::new(text).block(block), area);
        if focused {
            let x = area
                .x
                .saturating_add(3)
                .saturating_add(self.cursor as u16)
                .min(area.right().saturating_sub(2));
            let y = area.y.saturating_add(1);
            frame.set_cursor_position((x, y));
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let hints = match self.focus {
            Focus::Threads => {
                "j/k move · / search · f filter · Ctrl-P palette · M models · Enter open · i compose · n new · a approvals · s safety · ? help · q quit"
            }
            Focus::Composer => {
                "Enter send · Esc threads · PgUp/PgDn scroll · Ctrl-U clear · Ctrl-C quit"
            }
        };
        let line = if self.search_active {
            Line::from(vec![
                Span::styled(" / ", Style::default().fg(parse_hex_color("#06B6D4"))),
                Span::styled(self.search_query.clone(), Style::default().fg(Color::White)),
                Span::raw("  "),
                Span::styled(
                    "Enter open · Esc clear · ↑/↓ move",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else if let Some(error) = &self.error {
            Line::from(vec![
                Span::styled(" ! ", Style::default().fg(Color::Red)),
                Span::styled(truncate(error, 110), Style::default().fg(Color::Red)),
                Span::raw("  "),
                Span::styled("[r] retry", Style::default().fg(Color::DarkGray)),
            ])
        } else {
            Line::from(vec![
                Span::styled(
                    truncate(&self.status, 56),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("  "),
                Span::styled(hints, Style::default().fg(Color::DarkGray)),
            ])
        };
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_palette(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(20.min(area.height)),
                Constraint::Fill(1),
            ])
            .split(area);
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(78.min(area.width)),
                Constraint::Fill(1),
            ])
            .split(vertical[1]);
        let box_area = horizontal[1];
        let Some(palette) = &self.palette else {
            return;
        };
        let actions = self.filtered_palette_actions();
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::styled(" › ", Style::default().fg(parse_hex_color("#06B6D4"))),
            Span::styled(palette.query.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));
        if actions.is_empty() {
            lines.push(Line::from(Span::styled(
                "No matching actions.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, action) in actions.iter().enumerate() {
                let selected = index == palette.selected;
                let style = if selected {
                    Style::default().bg(Color::Rgb(36, 36, 42)).fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { " ▸ " } else { "   " },
                        Style::default().fg(parse_hex_color("#06B6D4")),
                    ),
                    Span::styled(action.label(), style.add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(
                        truncate(action.hint(), 44),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Enter run · ↑/↓ move · Esc close",
            Style::default().fg(Color::DarkGray),
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(parse_hex_color("#06B6D4")))
            .title(" Command palette ");
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .wrap(Wrap { trim: false }),
            box_area,
        );
    }

    fn render_model_picker(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(24.min(area.height)),
                Constraint::Fill(1),
            ])
            .split(area);
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(94.min(area.width)),
                Constraint::Fill(1),
            ])
            .split(vertical[1]);
        let box_area = horizontal[1];
        let Some(picker) = &self.picker else {
            return;
        };
        let accent = parse_hex_color("#06B6D4");
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(40)])
            .split(box_area);
        let harness_block = Block::default()
            .borders(Borders::ALL)
            .border_style(
                Style::default().fg(if picker.focus == PickerFocus::Harness {
                    accent
                } else {
                    Color::DarkGray
                }),
            )
            .title(" Harness ");
        let mut harness_lines: Vec<Line<'static>> = Vec::new();
        for (index, harness) in self.harnesses.iter().enumerate() {
            let selected = index == picker.harness;
            let color = parse_hex_color(&harness.accent);
            let style = if selected {
                Style::default().bg(Color::Rgb(36, 36, 42)).fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            harness_lines.push(Line::from(vec![
                Span::styled(
                    if selected { "▸ " } else { "  " },
                    Style::default().fg(color),
                ),
                Span::styled(
                    format!("{} ", harness_glyph(&harness.id)),
                    Style::default().fg(color),
                ),
                Span::styled(
                    harness.short_name.clone(),
                    style.add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if harness.available { "" } else { "  (missing)" },
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        frame.render_widget(
            Paragraph::new(Text::from(harness_lines)).block(harness_block),
            columns[0],
        );

        let model_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if picker.focus == PickerFocus::Model {
                accent
            } else {
                Color::DarkGray
            }))
            .title(" Model / provider ");
        let mut model_lines: Vec<Line<'static>> = Vec::new();
        if let Some(harness) = self.harnesses.get(picker.harness) {
            for (index, model) in harness.models.iter().enumerate() {
                let selected = index == picker.model;
                let style = if selected {
                    Style::default().bg(Color::Rgb(36, 36, 42)).fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };
                model_lines.push(Line::from(vec![
                    Span::styled(
                        if selected { "▸ " } else { "  " },
                        Style::default().fg(parse_hex_color(&harness.accent)),
                    ),
                    Span::styled(model.label.clone(), style.add_modifier(Modifier::BOLD)),
                ]));
                let detail = match &model.base_url {
                    Some(base_url) => format!("   {} · {base_url}", model.id),
                    None => format!("   {}", model.id),
                };
                model_lines.push(Line::from(Span::styled(
                    truncate(&detail, 58),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
        model_lines.push(Line::from(""));
        model_lines.push(Line::from(Span::styled(
            "Tab switch · j/k move · Enter select · Esc close",
            Style::default().fg(Color::DarkGray),
        )));
        frame.render_widget(
            Paragraph::new(Text::from(model_lines)).block(model_block),
            columns[1],
        );
    }

    fn render_auth(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(9),
                Constraint::Fill(1),
            ])
            .split(area);
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(56.min(area.width)),
                Constraint::Fill(1),
            ])
            .split(vertical[1]);
        let box_area = horizontal[1];
        let masked = "*".repeat(self.auth_input.len());
        let mut lines = vec![
            Line::from(Span::styled(
                "MoE Deck terminal cockpit",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Passcode: ", Style::default().fg(Color::DarkGray)),
                Span::raw(masked),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "Run `moedeck pair` in another terminal to see the passcode.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        if let Some(error) = &self.error {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                truncate(error, 52),
                Style::default().fg(Color::Red),
            )));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(parse_hex_color("#06B6D4")))
            .title(" Login ");
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .wrap(Wrap { trim: false }),
            box_area,
        );
        let x = box_area
            .x
            .saturating_add(11)
            .saturating_add(self.auth_input.len() as u16)
            .min(box_area.right().saturating_sub(2));
        let y = box_area.y.saturating_add(3);
        frame.set_cursor_position((x, y));
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(20.min(area.height)),
                Constraint::Fill(1),
            ])
            .split(area);
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(72.min(area.width)),
                Constraint::Fill(1),
            ])
            .split(vertical[1]);
        let text = Text::from(vec![
            Line::from(Span::styled(
                "Threads",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  j / k, ↑ / ↓     move selection"),
            Line::from("  g / G            first / last"),
            Line::from("  Enter            open thread and focus composer"),
            Line::from("  i                focus composer"),
            Line::from("  n                new thread with the selected harness/model"),
            Line::from("  /                search threads (↑/↓ move, Enter open)"),
            Line::from("  f                cycle harness filter"),
            Line::from("  Ctrl-P or :      command palette"),
            Line::from(""),
            Line::from(Span::styled(
                "Harness & model",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  h                cycle harness"),
            Line::from("  m                cycle model for the current harness"),
            Line::from("  M                harness & model picker"),
            Line::from("  s                cycle safety mode (safe/standard/yolo)"),
            Line::from("  a                approvals (y allow, ! always, n deny)"),
            Line::from("  x                stop the current turn"),
            Line::from("  r                refresh threads and conversation"),
            Line::from(""),
            Line::from(Span::styled(
                "Composer",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from("  Enter send · Esc threads · PgUp/PgDn scroll"),
            Line::from("  Ctrl-U clear · Ctrl-W delete word · Ctrl-C quit"),
            Line::from(""),
            Line::from(Span::styled(
                "Press Esc or ? to close this help.",
                Style::default().fg(Color::DarkGray),
            )),
        ]);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(parse_hex_color("#06B6D4")))
            .title(" Keys ");
        frame.render_widget(
            Paragraph::new(text).block(block).wrap(Wrap { trim: false }),
            horizontal[1],
        );
    }

    fn render_approvals(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(22.min(area.height)),
                Constraint::Fill(1),
            ])
            .split(area);
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Fill(1),
                Constraint::Length(86.min(area.width)),
                Constraint::Fill(1),
            ])
            .split(vertical[1]);
        let box_area = horizontal[1];
        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.pending.is_empty() {
            lines.push(Line::from(Span::styled(
                "No pending approvals.",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, approval) in self.pending.iter().enumerate() {
                let selected = index == self.approval_idx;
                let marker = if selected { "▸ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(
                        marker,
                        Style::default().fg(if selected {
                            Color::Red
                        } else {
                            Color::DarkGray
                        }),
                    ),
                    Span::styled(
                        format!("{} ", approval.harness.to_uppercase()),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        approval.tool_name.clone(),
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  [{:?}]", approval.risk_level),
                        Style::default().fg(Color::Red),
                    ),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("    {}", truncate(&approval.target, 78)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            if let Some(approval) = self.pending.get(self.approval_idx) {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Raw input:",
                    Style::default().add_modifier(Modifier::BOLD),
                )));
                for line in approval
                    .details
                    .as_deref()
                    .unwrap_or("(none)")
                    .lines()
                    .take(4)
                {
                    lines.push(Line::from(Span::styled(
                        truncate(line, 82),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "y allow once · ! allow always · n deny · j/k select · Esc close",
            Style::default().fg(Color::DarkGray),
        )));
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red))
            .title(" Approvals ");
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(block)
                .wrap(Wrap { trim: false }),
            box_area,
        );
    }
}

fn append_message_lines(
    lines: &mut Vec<Line<'static>>,
    message: &ChatMessage,
    harnesses: &[HarnessInfo],
) {
    let (label, color) = match message.role.as_str() {
        "user" => ("You", Color::Cyan),
        "assistant" => (
            "Assistant",
            harnesses
                .iter()
                .find(|h| h.id == message.harness)
                .map(|h| parse_hex_color(&h.accent))
                .unwrap_or(Color::Green),
        ),
        _ => ("System", Color::Yellow),
    };
    let mut head = vec![Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )];
    if let Some(model) = &message.model {
        head.push(Span::styled(
            format!(" · {model}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(head));
    if message.text.is_empty() {
        lines.push(Line::from(""));
    } else {
        for text_line in message.text.lines() {
            lines.push(Line::from(text_line.to_string()));
        }
    }
    for tool in &message.tools {
        lines.push(Line::from(Span::styled(
            format!("  ⚡ {} · {}", tool.name, truncate(&tool.summary, 90)),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if let Some(thinking) = &message.thinking {
        if !thinking.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  💭 {}", truncate(thinking, 160)),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }
    if !message.images.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  [{} image(s)]", message.images.len()),
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.push(Line::from(""));
}

fn wrapped_line_count(lines: &[Line], width: u16) -> usize {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|line| {
            let line_width = line.width();
            if line_width == 0 {
                1
            } else {
                line_width.div_ceil(width)
            }
        })
        .sum()
}

fn error_message(value: &Value) -> String {
    value
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if value.is_null() {
                "request failed".to_string()
            } else {
                truncate(&value.to_string(), 160)
            }
        })
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                encoded.push(byte as char)
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn parse_hex_color(value: &str) -> Color {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Color::Cyan;
    }
    let Ok(red) = u8::from_str_radix(&hex[0..2], 16) else {
        return Color::Cyan;
    };
    let Ok(green) = u8::from_str_radix(&hex[2..4], 16) else {
        return Color::Cyan;
    };
    let Ok(blue) = u8::from_str_radix(&hex[4..6], 16) else {
        return Color::Cyan;
    };
    Color::Rgb(red, green, blue)
}

fn daemon_log_path() -> PathBuf {
    crate::discovery::config_dir().join("daemon.log")
}

/// Start a daemon only when the port is not already serving MoE Deck.
async fn ensure_daemon(port: u16) -> Result<bool, String> {
    let mut api = Api::new(port);
    if api.health().await {
        return Ok(false);
    }
    if crate::discovery::setting("TUI_NO_SPAWN").is_some() {
        return Err(format!(
            "MoE Deck is not running on 127.0.0.1:{port} (MOEDECK_TUI_NO_SPAWN is set)"
        ));
    }
    let executable =
        std::env::current_exe().map_err(|e| format!("cannot locate moedeck binary: {e}"))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(daemon_log_path())
        .map_err(|e| format!("cannot open daemon log: {e}"))?;
    let stderr = log
        .try_clone()
        .map_err(|e| format!("cannot clone daemon log handle: {e}"))?;
    let mut command = std::process::Command::new(executable);
    command
        .env("MOEDECK_PORT", port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(stderr);
    command
        .spawn()
        .map_err(|e| format!("cannot start moedeck daemon: {e}"))?;

    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if api.health().await {
            return Ok(true);
        }
    }
    Err(format!(
        "MoE Deck daemon did not become healthy on 127.0.0.1:{port}; see {}",
        daemon_log_path().display()
    ))
}

fn local_session_token() -> Option<String> {
    std::fs::read_to_string(crate::discovery::config_dir().join("tui.token"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn local_passcode(explicit: Option<String>) -> Option<String> {
    explicit
        .filter(|value| !value.trim().is_empty())
        .or_else(|| crate::discovery::setting("PASSCODE"))
        .or_else(|| {
            crate::auth::load_auth_config()
                .passcode
                .filter(|value| !value.trim().is_empty())
        })
}

/// Run the terminal cockpit. Returns after the terminal is restored.
pub async fn run(port: u16, passcode: Option<String>) -> anyhow::Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("moedeck tui needs an interactive terminal (stdout is not a TTY)");
    }

    let spawned = ensure_daemon(port).await.map_err(anyhow::Error::msg)?;
    let mut app = App::new(Api::new(port));
    if let Some(token) = local_session_token() {
        app.api.set_session_token(&token);
    }
    app.refresh_bootstrap().await;
    if app.need_auth {
        if let Some(passcode) = local_passcode(passcode) {
            if app.api.login(&passcode).await.is_ok() {
                app.need_auth = false;
                app.refresh_bootstrap().await;
            }
        }
    }

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    std::thread::spawn(move || loop {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                Ok(ev) => {
                    if event_tx.send(ev).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            Ok(false) => {}
            Err(_) => break,
        }
    });

    let mut terminal = ratatui::init();
    let mut bootstrap_tick = tokio::time::interval(POLL_BOOTSTRAP);
    let mut detail_tick = tokio::time::interval(POLL_DETAIL);
    let mut animation_tick = tokio::time::interval(POLL_ANIMATION);
    bootstrap_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    detail_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    animation_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let result: anyhow::Result<()> = async {
        loop {
            terminal.draw(|frame| app.render(frame))?;
            tokio::select! {
                maybe_event = event_rx.recv() => {
                    match maybe_event {
                        Some(Event::Key(key)) => app.handle_key(key).await,
                        Some(Event::Paste(text)) => {
                            if app.focus == Focus::Composer && !app.need_auth {
                                for character in text.chars() {
                                    app.input.insert(app.cursor, character);
                                    app.cursor += 1;
                                }
                            }
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = bootstrap_tick.tick() => {
                    if !app.need_auth {
                        app.refresh_bootstrap().await;
                    }
                }
                _ = detail_tick.tick() => {
                    if !app.need_auth && app.active_id.is_some() {
                        app.refresh_detail().await;
                    }
                }
                _ = animation_tick.tick() => {
                    app.animation = app.animation.wrapping_add(1);
                }
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    ratatui::restore();
    if spawned {
        println!(
            "MoE Deck daemon left running on http://127.0.0.1:{port} (PWA for phone). Log: {}",
            daemon_log_path().display()
        );
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("ääkkösiä", 4), "ääk…");
    }

    #[test]
    fn palette_filters_by_label_and_hint() {
        let mut app = App::new(Api::new(1));
        app.open_palette();
        app.palette.as_mut().unwrap().query = "model".to_string();
        let actions = app.filtered_palette_actions();
        assert!(actions.contains(&PaletteAction::ModelPicker));
        app.palette.as_mut().unwrap().query = "zzzz".to_string();
        assert!(app.filtered_palette_actions().is_empty());
    }

    #[test]
    fn relative_time_uses_compact_buckets() {
        let now = chrono::Utc::now().timestamp_millis();
        assert_eq!(relative_time(now), "now");
        assert_eq!(relative_time(now - 120_000), "2m");
        assert_eq!(relative_time(now - 7_200_000), "2h");
        assert_eq!(relative_time(now - 172_800_000), "2d");
    }

    #[test]
    fn every_harness_has_a_glyph() {
        for harness in ["claude", "agy", "codex", "opencode", "keel"] {
            assert_ne!(harness_glyph(harness), "•");
        }
        assert_eq!(harness_glyph("unknown"), "•");
    }

    #[test]
    fn parses_harness_hex_colors() {
        assert_eq!(parse_hex_color("#06B6D4"), Color::Rgb(6, 182, 212));
        assert_eq!(parse_hex_color("10A37F"), Color::Rgb(16, 163, 127));
        assert_eq!(parse_hex_color("nope"), Color::Cyan);
    }

    #[test]
    fn path_segments_keep_safe_colons_and_encode_the_rest() {
        assert_eq!(encode_path_segment("helm:123"), "helm:123");
        assert_eq!(encode_path_segment("a b/c"), "a%20b%2Fc");
    }
}
