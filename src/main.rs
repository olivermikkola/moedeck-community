mod approval;
mod auth;
mod bounce;
mod claude_rc;
mod codex;
mod completion;
mod discovery;
mod install;
mod models;
mod ops;
mod profiles;
mod proxy;
mod runner;
mod settings;
mod sprint;
mod timeline;
mod tui;
mod ui;
mod update;
mod voice;

use crate::auth::{
    is_allowed_host_header, is_safe_mutation_request, is_safe_preview_mutation, load_auth_config,
    verify_session_token,
};
use crate::bounce::SwarmManager;
use crate::discovery::{
    config_dir, get_conversation_detail, get_conversation_detail_tail, home_dir, home_str,
    inspect_git_diff, inspect_workspace, list_all_conversations, list_projects,
    list_workspace_files, load_overlays, sanitize_workdir, save_overlay,
};
use crate::models::{
    default_harnesses, CreateConversationRequest, CreateSprintRequest, CreateWorktreeRequest,
    PrActionRequest, RemoveWorktreeRequest, SecondOpinionRequest, SendMessageRequest,
    StoredThreadOverlay, SwarmBounceRequest, UpdateConversationRequest, VoiceIntent,
    VoiceTranscribeRequest, VoiceTranscribeResponse,
};
use crate::ops::{
    create_worktree, execute_pr_action, inspect_pr_status, list_listening_ports, remove_worktree,
};
use crate::proxy::{extract_preview_target, forward_to_port};
use crate::runner::{dispatch_turn, sanitize_harness, RunnerState};
use crate::sprint::{parse_markdown_checklist_to_steps, SprintManager};
use crate::voice::process_voice_transcription;
use base64::{engine::general_purpose::STANDARD as BASE64_STD, Engine};
use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full, Limited};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::json;

static LOGIN_STATE: std::sync::Mutex<(u32, i64)> = std::sync::Mutex::new((0, 0));
use std::convert::Infallible;
use std::fs;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use uuid::Uuid;

const LOGIN_FAIL_LIMIT: u32 = 20;
const LOGIN_LOCKOUT_SECS: i64 = 300;

#[derive(Deserialize)]
struct UploadPayload {
    filename: String,
    data_base64: String,
}

/// Local session token for `moedeck tui`. It is the same token the pairing URL
/// carries, but kept in a user-only file so the terminal cockpit can attach to
/// a daemon whose passcode came from the service environment.
fn write_tui_token(token: &str) {
    let path = config_dir().join("tui.token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
        {
            use std::io::Write as _;
            let _ = writeln!(file, "{token}");
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(&path, format!("{token}\n"));
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "MoE Deck — Multi-Harness Agent Gateway (v{})",
            env!("CARGO_PKG_VERSION")
        );
        println!();
        println!("usage: moedeck [options] [command]");
        println!();
        println!("commands:");
        println!("  install          run MoE Deck in the background as you (systemd/launchd), then show pairing");
        println!("  install --dry-run  print the service file without installing anything");
        println!("  uninstall        stop and remove the background service (keeps your chats)");
        println!("  tui              open the terminal cockpit (starts the daemon if needed)");
        println!("  doctor           inspect local CLI harness installations & security readiness");
        println!("  pair             display mobile pairing URL & active cockpit passcode");
        println!("  passcode [code]  view or set cockpit authentication passcode");
        println!("  login <id>       log an extra account in (e.g. `moedeck login agy-2`)");
        println!();
        println!("options:");
        println!("  -h, --help       show this help");
        println!("  -v, --version    show version");
        println!("  --passcode CODE  passcode for `moedeck tui` when no local token is available");
        println!();
        println!("environment:");
        println!("  MOEDECK_PORT        HTTP port to bind (default: 7770)");
        println!("  MOEDECK_PASSCODE    cockpit passcode (overrides auth.json)");
        println!(
            "  MOEDECK_CONFIG_DIR  where chats and settings live (default: ~/.config/moedeck)"
        );
        println!("  (the older HELM_* names still work)");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!(
            "moedeck {} ({})",
            env!("CARGO_PKG_VERSION"),
            update::BUILD_SHA.get(..7).unwrap_or("unknown commit")
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "doctor") {
        return run_doctor();
    }
    let port: u16 = discovery::setting("PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7770);
    if args.iter().any(|a| a == "tui") {
        let passcode = args
            .iter()
            .position(|a| a == "--passcode")
            .and_then(|idx| args.get(idx + 1))
            .cloned();
        return tui::run(port, passcode).await;
    }
    match args.get(1).map(String::as_str) {
        Some("install") => {
            let dry_run = args.iter().any(|a| a == "--dry-run");
            install::run_install(dry_run)?;
            return if dry_run { Ok(()) } else { run_pair(port) };
        }
        Some("uninstall") => return install::run_uninstall(),
        _ => {}
    }
    if args.iter().any(|a| a == "pair") {
        return run_pair(port);
    }
    if let Some(idx) = args.iter().position(|a| a == "passcode") {
        let new_code = args.get(idx + 1).map(|s| s.as_str());
        return run_passcode(new_code);
    }
    // `moedeck login <profile-id>` — interactive OAuth into one account profile.
    if let Some(idx) = args.iter().position(|a| a == "login") {
        let Some(pid) = args.get(idx + 1) else {
            eprintln!("usage: moedeck login <profile-id>   (e.g. moedeck login agy-2)");
            std::process::exit(2);
        };
        return profiles::exec_profile_login(pid);
    }

    // Terminal-first default: an interactive `moedeck` opens the cockpit and
    // starts the daemon behind it. Service managers (no TTY) keep the old
    // daemon-only behavior.
    if std::io::stdout().is_terminal() {
        return tui::run(port, None).await;
    }

    profiles::prune_invalid_registry();
    tokio::spawn(async {
        let _ = settings::refresh().await;
    });

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    let runner = RunnerState::new();
    let swarm_mgr = SwarmManager::new();
    let sprint_mgr = SprintManager::new();

    // Re-arm queued/running sprints left behind by a previous daemon run.
    let resumed = sprint_mgr.clone().resume_incomplete(runner.clone()).await;
    if resumed > 0 {
        println!("  Sprint engine resumed {} incomplete sprint(s)", resumed);
    }

    let auth_cfg = auth::load_auth_config();
    let startup_token = auth::issue_session_token(&auth_cfg);
    write_tui_token(&startup_token);
    println!("====================================================");
    println!(
        "  MoE Deck — Multi-Harness Agent Gateway (v{})",
        env!("CARGO_PKG_VERSION")
    );
    println!("  Local Cockpit:  http://127.0.0.1:{}", port);
    println!("  Terminal TUI:   moedeck tui");
    // This output ends up in service logs, so no passcode or login link here.
    println!("  Mobile Pairing: run `moedeck pair` for the link and passcode");
    let installed: Vec<String> = default_harnesses()
        .into_iter()
        .filter(|h| h.available)
        .map(|h| h.name)
        .collect();
    println!(
        "  Harnesses:      {}",
        if installed.is_empty() {
            "none installed yet (claude, codex, agy, opencode or keel)".to_string()
        } else {
            installed.join(" | ")
        }
    );
    let account_summary = profiles::registry_summary();
    if account_summary != "none" {
        println!("  Extra accounts: {}", account_summary);
    }
    println!("====================================================");

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        let (stream, _) = tokio::select! {
            _ = &mut shutdown => break,
            connection = listener.accept() => connection?,
        };
        let io = TokioIo::new(stream);
        let runner_ref = runner.clone();
        let swarm_ref = swarm_mgr.clone();
        let sprint_ref = sprint_mgr.clone();

        tokio::spawn(async move {
            let svc = service_fn(move |req| {
                handle_request(
                    req,
                    port,
                    runner_ref.clone(),
                    swarm_ref.clone(),
                    sprint_ref.clone(),
                )
            });
            let _ = http1::Builder::new().serve_connection(io, svc).await;
        });
    }
    runner.remote_controls.stop_all();
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn handle_request(
    req: Request<hyper::body::Incoming>,
    helm_port: u16,
    runner: Arc<RunnerState>,
    swarm_mgr: Arc<SwarmManager>,
    sprint_mgr: Arc<SprintManager>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let host_hdr: Option<String> = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    if !is_allowed_host_header(host_hdr.as_deref()) {
        return Ok(json_response(
            StatusCode::FORBIDDEN,
            &json!({ "error": "Forbidden Host header (DNS rebinding protection)" }),
        ));
    }

    // Paths that must load before any session exists: the PWA shell, its
    // assets, health, and the login endpoint itself.
    let raw_path = req.uri().path().to_string();
    let public_path = matches!(
        raw_path.as_str(),
        "/" | "/sw.js"
            | "/manifest.json"
            | "/icon.svg"
            | "/icon-180.png"
            | "/icon-192.png"
            | "/icon-512.png"
            | "/icon-512-maskable.png"
            | "/api/health"
            | "/api/auth/login"
    );

    let auth_cfg = load_auth_config();

    // Magic URL login: If a valid ?token=<session_token> is present, set the session cookie and redirect to clean path.
    if raw_path == "/" && req.method() == Method::GET {
        if let Some(q) = req.uri().query() {
            let token_opt = q.split('&').find_map(|pair| pair.strip_prefix("token="));
            if let Some(tok) = token_opt {
                if auth::verify_session_token(&auth_cfg, tok) {
                    let secure = host_hdr.as_deref().unwrap_or("").ends_with(".ts.net");
                    let cookie = format!(
                        "helm_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
                        tok,
                        auth::SESSION_TTL_SECS,
                        if secure { "; Secure" } else { "" }
                    );
                    return Ok(Response::builder()
                        .status(StatusCode::FOUND)
                        .header("Location", "/")
                        .header("Set-Cookie", cookie)
                        .body(Full::from(Bytes::new()))
                        .unwrap());
                }
            }
        }
    }

    let authorized = if public_path {
        true
    } else {
        // Every non-public request needs a session cookie, whatever network
        // it comes from. `load_auth_config` guarantees a passcode exists.
        check_cookie(&req, &auth_cfg)
    };

    if !authorized {
        return Ok(json_response(
            StatusCode::UNAUTHORIZED,
            &json!({ "error": "Unauthorized" }),
        ));
    }

    let referer = req.headers().get("referer").and_then(|h| h.to_str().ok());
    let query_str = req.uri().query().map(|q| q.to_string());

    let orig_method = req.method().clone();
    let is_mutation = orig_method == Method::POST
        || orig_method == Method::PUT
        || orig_method == Method::PATCH
        || orig_method == Method::DELETE;

    // The preview proxy forwards to arbitrary loopback ports, so mutation
    // requests must be gated *before* forwarding: otherwise a cross-origin
    // page could relay form/no-cors POSTs into local services (Docker API,
    // Jupyter, …) through MoE Deck. Browsers always attach an honest Origin to
    // POSTs, and the referer fallback only accepts same-origin referers.
    if let Some((target_port, subpath)) =
        extract_preview_target(&raw_path, referer, host_hdr.as_deref(), helm_port)
    {
        if is_mutation {
            let orig = req.headers().get("origin").and_then(|h| h.to_str().ok());
            let sfs = req
                .headers()
                .get("sec-fetch-site")
                .and_then(|h| h.to_str().ok());
            if !is_safe_preview_mutation(orig, sfs, host_hdr.as_deref()) {
                return Ok(json_response(
                    StatusCode::FORBIDDEN,
                    &json!({ "error": "Preview mutations require a same-origin browser context" }),
                ));
            }
        }
        match forward_to_port(req, target_port, &subpath, query_str.as_deref()).await {
            Ok(resp) => return Ok(resp),
            Err((status, err_msg)) => {
                return Ok(json_response(
                    status,
                    &json!({ "error": err_msg, "port": target_port }),
                ));
            }
        }
    }

    // Serve HEAD requests with the matching GET route's headers; hyper
    // suppresses the body for HEAD responses at the connection level.
    let method = if orig_method == Method::HEAD {
        Method::GET
    } else {
        orig_method
    };
    if is_mutation {
        let ct = req
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok());
        let orig = req.headers().get("origin").and_then(|h| h.to_str().ok());
        let sfs = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|h| h.to_str().ok());
        if !is_safe_mutation_request(ct, orig, sfs, host_hdr.as_deref()) {
            return Ok(json_response(
                StatusCode::FORBIDDEN,
                &json!({ "error": "CSRF check failed: JSON Content-Type and same-origin required" }),
            ));
        }
    }

    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();

    match (&method, path.as_str()) {
        (&Method::GET, "/") | (&Method::GET, "/index.html") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Cache-Control", "no-cache")
            .body(Full::from(Bytes::from_static(ui::INDEX_HTML.as_bytes())))
            .unwrap()),

        (&Method::GET, "/manifest.json") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/manifest+json")
            .body(Full::from(Bytes::from_static(ui::MANIFEST_JSON.as_bytes())))
            .unwrap()),

        (&Method::GET, "/icon.svg") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "image/svg+xml")
            .body(Full::from(Bytes::from_static(ui::ICON_SVG.as_bytes())))
            .unwrap()),

        (&Method::GET, "/icon-180.png") => Ok(png_response(ui::ICON_180_PNG)),
        (&Method::GET, "/icon-192.png") => Ok(png_response(ui::ICON_192_PNG)),
        (&Method::GET, "/icon-512.png") => Ok(png_response(ui::ICON_512_PNG)),
        (&Method::GET, "/icon-512-maskable.png") => Ok(png_response(ui::ICON_512_MASKABLE_PNG)),

        (&Method::GET, "/sw.js") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/javascript")
            .header("Cache-Control", "no-cache")
            .body(Full::from(Bytes::from(ui::sw_js().into_bytes())))
            .unwrap()),

        (&Method::GET, "/api/health") => Ok(json_response(
            StatusCode::OK,
            &json!({ "status": "ok", "service": "moedeck" }),
        )),

        (&Method::POST, "/api/auth/login") => {
            let body_bytes = read_body(req).await;
            let cfg = load_auth_config();
            let Some(expected) = auth::effective_passcode(&cfg) else {
                return Ok(json_response(
                    StatusCode::CONFLICT,
                    &json!({ "error": "No passcode configured" }),
                ));
            };
            // Global lockout: repeated failures freeze login for a few minutes,
            // parallelized guessing included (a per-request sleep alone is not
            // a defense against concurrent attempts).
            {
                let st = LOGIN_STATE.lock().unwrap_or_else(|e| e.into_inner());
                let now = Utc::now().timestamp();
                if now < st.1 {
                    return Ok(json_response(
                        StatusCode::TOO_MANY_REQUESTS,
                        &json!({
                            "error": "Too many failed attempts — login locked briefly",
                            "retry_after_secs": st.1 - now,
                        }),
                    ));
                }
            }
            let provided = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                .ok()
                .and_then(|v| {
                    v.get("passcode")
                        .and_then(|p| p.as_str())
                        .map(str::to_string)
                });
            let valid = provided
                .as_deref()
                .map(|p| auth::passcode_matches(&expected, p))
                .unwrap_or(false);

            if valid {
                {
                    let mut st = LOGIN_STATE.lock().unwrap_or_else(|e| e.into_inner());
                    *st = (0, 0);
                }
                let token = auth::issue_session_token(&cfg);
                let secure = host_hdr.as_deref().unwrap_or("").ends_with(".ts.net");
                let cookie = format!(
                    "helm_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}{}",
                    token,
                    auth::SESSION_TTL_SECS,
                    if secure { "; Secure" } else { "" }
                );
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .header("Set-Cookie", cookie)
                    .body(Full::from(Bytes::from_static(br#"{"ok":true}"#)))
                    .unwrap())
            } else {
                // Damping for single-threaded guessers + hard lockout beyond
                // LOGIN_FAIL_LIMIT consecutive failures.
                {
                    let mut st = LOGIN_STATE.lock().unwrap_or_else(|e| e.into_inner());
                    let now = Utc::now().timestamp();
                    if now >= st.1 {
                        st.0 += 1;
                        if st.0 >= LOGIN_FAIL_LIMIT {
                            st.1 = now + LOGIN_LOCKOUT_SECS;
                            st.0 = 0;
                            eprintln!(
                                "auth: {} consecutive failed logins — locked out for {}s",
                                LOGIN_FAIL_LIMIT, LOGIN_LOCKOUT_SECS
                            );
                        }
                    }
                }
                let fails = LOGIN_STATE.lock().unwrap_or_else(|e| e.into_inner()).0;
                let delay_ms = 250u64.saturating_mul(u64::from((fails + 1).min(12)));
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                Ok(json_response(
                    StatusCode::UNAUTHORIZED,
                    &json!({ "error": "Wrong passcode" }),
                ))
            }
        }

        (&Method::POST, "/api/auth/logout") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .header("Set-Cookie", "helm_session=; Path=/; HttpOnly; Max-Age=0")
            .body(Full::from(Bytes::from_static(br#"{"ok":true}"#)))
            .unwrap()),

        (&Method::GET, "/api/auth/status") => {
            let cfg = load_auth_config();
            Ok(json_response(
                StatusCode::OK,
                &json!({
                    "passcode_required": auth::effective_passcode(&cfg).is_some(),
                    "session_valid": check_cookie(&req, &cfg),
                    "network_trusted": false,
                }),
            ))
        }

        (&Method::POST, "/api/auth/passcode") => {
            let body_bytes = read_body(req).await;
            let new_pc = serde_json::from_slice::<serde_json::Value>(&body_bytes)
                .ok()
                .and_then(|v| {
                    v.get("passcode")
                        .and_then(|p| p.as_str())
                        .map(str::trim)
                        .map(String::from)
                })
                .unwrap_or_default();
            if new_pc.chars().count() < 4 {
                return Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": "Passcode must be at least 4 characters" }),
                ));
            }
            let mut cfg = load_auth_config();
            cfg.passcode = Some(new_pc);
            // Rotating the passcode revokes every previously issued session
            // cookie: the claim in the response below is now actually true.
            auth::revoke_all_sessions(&mut cfg);
            auth::save_auth_config(&cfg);
            Ok(json_response(
                StatusCode::OK,
                &json!({ "ok": true, "note": "Passcode active — all clients must log in now" }),
            ))
        }

        // MoE Deck runs shell commands for whoever is logged in, so it always
        // needs a passcode. "Turning it off" used to save an empty one, which
        // the next request replaced with a random code nobody was shown.
        (&Method::DELETE, "/api/auth/passcode") => Ok(json_response(
            StatusCode::BAD_REQUEST,
            &json!({ "error": "MoE Deck always needs a passcode. Change it instead (POST /api/auth/passcode or `moedeck passcode <new>`)." }),
        )),

        (&Method::GET, "/api/settings") => Ok(json_response(
            StatusCode::OK,
            &json!({
                "settings": settings::load(), "harnesses": default_harnesses(), "catalogs": settings::catalogs()
            }),
        )),
        (&Method::PUT, "/api/settings") => {
            let body = read_body(req).await;
            match serde_json::from_slice::<settings::Settings>(&body) {
                Ok(config) => match settings::save(&config) {
                    Ok(()) => Ok(json_response(StatusCode::OK, &json!({"ok":true}))),
                    Err(error) => Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({"error":error}),
                    )),
                },
                Err(_) => Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"error":"Invalid settings"}),
                )),
            }
        }
        (&Method::POST, "/api/catalog/refresh") => match settings::refresh().await {
            Ok(()) => Ok(json_response(
                StatusCode::OK,
                &json!({"ok":true, "catalogs":settings::catalogs()}),
            )),
            Err(error) => Ok(json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"error":error}),
            )),
        },

        (&Method::GET, "/api/update") => match update::check().await {
            Ok(check) => Ok(json_response(StatusCode::OK, &json!(check))),
            Err(error) => Ok(json_response(
                StatusCode::BAD_GATEWAY,
                &json!({"error": error, "status": update::status()}),
            )),
        },
        (&Method::GET, "/api/update/status") => {
            Ok(json_response(StatusCode::OK, &json!(update::status())))
        }
        (&Method::POST, "/api/update") => {
            let body: serde_json::Value =
                serde_json::from_slice(&read_body(req).await).unwrap_or_default();
            let force = body["force"].as_bool().unwrap_or(false);
            match update::start(runner.clone(), force) {
                Ok(()) => Ok(json_response(
                    StatusCode::ACCEPTED,
                    &json!(update::status()),
                )),
                Err(error) => Ok(json_response(
                    StatusCode::CONFLICT,
                    &json!({"error": error}),
                )),
            }
        }

        (&Method::GET, "/api/profiles") => Ok(json_response(
            StatusCode::OK,
            &json!({ "profiles": profiles::profile_items() }),
        )),

        (&Method::POST, "/api/profiles") => {
            let body_bytes = read_body(req).await;
            #[derive(Deserialize)]
            struct CreateProfileRequest {
                harness: String,
            }
            let harness = serde_json::from_slice::<CreateProfileRequest>(&body_bytes)
                .map(|p| p.harness)
                .unwrap_or_default();
            match profiles::add_profile(&runner::sanitize_harness(&harness)) {
                Ok(p) => {
                    let home = profiles::profile_home(&p.id);
                    let login_cmd = profiles::login_command(&p.harness, &p.id);
                    println!("  + New account profile [{}] for {}", p.id, p.harness);
                    Ok(json_response(
                        StatusCode::OK,
                        &json!({
                            "profile": {
                                "id": p.id,
                                "harness": p.harness,
                                "label": p.label,
                                "home": home.to_string_lossy(),
                                "logged_in": profiles::home_looks_logged_in(&p.harness, &home),
                            },
                            "login_command": login_cmd,
                        }),
                    ))
                }
                Err(e) => Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": e }),
                )),
            }
        }

        (&Method::GET, "/api/claude-rc") => Ok(json_response(
            StatusCode::OK,
            &json!({ "sessions": runner.remote_controls.list() }),
        )),

        (&Method::POST, "/api/claude-rc") => {
            #[derive(Deserialize)]
            struct Start {
                workdir: String,
                profile: Option<String>,
            }
            let payload: Start = match serde_json::from_slice(&read_body(req).await) {
                Ok(payload) => payload,
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": "Choose a project folder for Claude Remote Control." }),
                    ))
                }
            };
            let controls = runner.remote_controls.clone();
            let permission = if runner.approvals.get_safety_mode().await == models::SafetyMode::Safe
            {
                "plan"
            } else {
                "default"
            };
            let result = match blocking("Claude Remote Control startup", move || {
                controls.start(&payload.workdir, payload.profile, permission)
            })
            .await
            {
                Ok(result) => result,
                Err(response) => return Ok(response),
            };
            match result {
                Ok(view) => Ok(json_response(StatusCode::OK, &view)),
                Err(error) => Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": error }),
                )),
            }
        }

        (&Method::GET, "/api/bootstrap") => {
            let active = runner.active_snapshot().await;
            let config = settings::load();
            let harnesses: Vec<_> = default_harnesses()
                .into_iter()
                .filter(|h| {
                    config
                        .harnesses
                        .get(&h.id)
                        .map(|c| c.enabled)
                        // Unconfigured: offer it only if its CLI is installed.
                        .unwrap_or(h.available)
                })
                .collect();
            let mut conversations = match blocking("bootstrap discovery", move || {
                list_all_conversations(&active)
            })
            .await
            {
                Ok(c) => c,
                Err(resp) => return Ok(resp),
            };
            for c in &mut conversations {
                c.pending_approval = runner.approvals.get_pending_for_conversation(&c.id).await;
            }
            let pending_approvals = runner.approvals.list_pending().await;
            let safety_mode = runner.approvals.get_safety_mode().await;
            let swarm_sessions = swarm_mgr.list_sessions().await;
            let sprints = sprint_mgr.list_sprints().await;
            Ok(json_response(
                StatusCode::OK,
                &json!({
                    "harnesses": harnesses,
                    "conversations": conversations,
                    "archived": discovery::archived_ids(),
                    "pending_approvals": pending_approvals,
                    "safety_mode": safety_mode,
                    "swarm_sessions": swarm_sessions,
                    "sprints": sprints,
                    "profiles": profiles::profile_items(),
                    "home": home_str(),
                }),
            ))
        }

        (&Method::GET, "/api/sprints") => {
            let list = sprint_mgr.list_sprints().await;
            Ok(json_response(StatusCode::OK, &json!({ "sprints": list })))
        }

        (&Method::POST, "/api/sprints") => {
            let body_bytes = read_body(req).await;
            if let Ok(create_req) = serde_json::from_slice::<CreateSprintRequest>(&body_bytes) {
                match sprint_mgr.create_sprint(create_req, runner.clone()).await {
                    Ok(sprint) => Ok(json_response(StatusCode::OK, &sprint)),
                    Err(e) => Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": e }),
                    )),
                }
            } else {
                Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": "Invalid sprint creation request body" }),
                ))
            }
        }

        (&Method::POST, "/api/sprints/parse") => {
            let body_bytes = read_body(req).await;
            #[derive(serde::Deserialize)]
            struct ParsePayload {
                markdown: String,
            }
            if let Ok(p) = serde_json::from_slice::<ParsePayload>(&body_bytes) {
                let steps = parse_markdown_checklist_to_steps(&p.markdown);
                Ok(json_response(StatusCode::OK, &json!({ "steps": steps })))
            } else {
                Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({ "error": "Invalid markdown payload" }),
                ))
            }
        }

        (&Method::GET, "/api/swarm") => {
            let sessions = swarm_mgr.list_sessions().await;
            Ok(json_response(
                StatusCode::OK,
                &json!({ "sessions": sessions }),
            ))
        }

        (&Method::GET, "/api/approvals") => {
            let pending = runner.approvals.list_pending().await;
            let safety_mode = runner.approvals.get_safety_mode().await;
            Ok(json_response(
                StatusCode::OK,
                &json!({
                    "safety_mode": safety_mode,
                    "pending": pending,
                }),
            ))
        }

        (&Method::POST, "/api/approvals/safety-mode") => {
            let body_bytes = read_body(req).await;
            if let Ok(payload) =
                serde_json::from_slice::<crate::models::SafetyModeUpdateRequest>(&body_bytes)
            {
                let mode = match payload.mode.to_lowercase().as_str() {
                    "safe" => crate::models::SafetyMode::Safe,
                    "yolo" => crate::models::SafetyMode::Yolo,
                    _ => crate::models::SafetyMode::Standard,
                };
                runner.approvals.set_safety_mode(mode).await;
                return Ok(json_response(
                    StatusCode::OK,
                    &json!({ "ok": true, "safety_mode": mode }),
                ));
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid safety mode payload" }),
            ))
        }

        (&Method::GET, "/api/projects") => {
            let projects = match blocking("project list", list_projects).await {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(
                StatusCode::OK,
                &json!({ "projects": projects }),
            ))
        }

        (&Method::GET, "/api/ports") => {
            let ports = match blocking("ports scan", list_listening_ports).await {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &json!({ "ports": ports })))
        }

        (&Method::GET, "/api/workspace-info") => {
            let raw_workdir = extract_query_param(&query, "workdir")
                .map(|w| url_decode(&w))
                .unwrap_or_else(home_str);
            let info =
                match blocking("workspace info", move || inspect_workspace(&raw_workdir)).await {
                    Ok(i) => i,
                    Err(resp) => return Ok(resp),
                };
            Ok(json_response(StatusCode::OK, &info))
        }

        (&Method::GET, "/api/pr-status") => {
            let raw_workdir = extract_query_param(&query, "workdir")
                .map(|w| url_decode(&w))
                .unwrap_or_else(home_str);
            let pr = match blocking("PR status", move || inspect_pr_status(&raw_workdir)).await {
                Ok(p) => p,
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &pr))
        }

        (&Method::POST, "/api/pr-action") => {
            let body_bytes = read_body(req).await;
            if let Ok(payload) = serde_json::from_slice::<PrActionRequest>(&body_bytes) {
                if payload.action == "fix-ci" {
                    let wd = payload.workdir.clone();
                    match blocking("failed CI logs", move || {
                        execute_pr_action(&wd, "failed-logs")
                    })
                    .await
                    {
                        Ok(Ok(failed_logs)) => {
                            if let Some(conv_id) = payload.conversation_id {
                                let prompt = format!(
                                    "GitHub Actions CI failed on this branch. Diagnose and fix the failure, verify locally, and push:\n\n```\n{}\n```",
                                    failed_logs
                                );
                                let send_req = SendMessageRequest {
                                    text: prompt,
                                    harness: None,
                                    model: None,
                                    effort: None,
                                    workdir: Some(payload.workdir.clone()),
                                    profile: None,
                                    images: Vec::new(),
                                };
                                match dispatch_turn(runner, conv_id, send_req).await {
                                    Ok(msg) => {
                                        return Ok(json_response(
                                            StatusCode::OK,
                                            &json!({ "ok": true, "dispatched": true, "message": msg }),
                                        ))
                                    }
                                    Err(e) => {
                                        return Ok(json_response(
                                            StatusCode::BAD_REQUEST,
                                            &json!({ "error": e }),
                                        ))
                                    }
                                }
                            }
                            return Ok(json_response(
                                StatusCode::OK,
                                &json!({ "ok": true, "logs": failed_logs }),
                            ));
                        }
                        Ok(Err(e)) => {
                            return Ok(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "error": e }),
                            ))
                        }
                        Err(resp) => return Ok(resp),
                    }
                }

                let wd = payload.workdir.clone();
                let act = payload.action.clone();
                match blocking("PR action", move || execute_pr_action(&wd, &act)).await {
                    Ok(Ok(output)) => {
                        return Ok(json_response(
                            StatusCode::OK,
                            &json!({ "ok": true, "output": output }),
                        ))
                    }
                    Ok(Err(e)) => {
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        ))
                    }
                    Err(resp) => return Ok(resp),
                }
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid PR action request" }),
            ))
        }

        (&Method::POST, "/api/worktrees") => {
            let body_bytes = read_body(req).await;
            if let Ok(payload) = serde_json::from_slice::<CreateWorktreeRequest>(&body_bytes) {
                let wd = payload.workdir.clone();
                let slug = payload.slug.clone();
                match blocking("create worktree", move || create_worktree(&wd, &slug)).await {
                    Ok(Ok(wt)) => {
                        if let Some(conv_id) = payload.conversation_id {
                            let mut overlays = load_overlays();
                            let now = Utc::now().timestamp_millis();
                            let mut ov = overlays.remove(&conv_id).unwrap_or(StoredThreadOverlay {
                                id: conv_id,
                                title: None,
                                pinned: false,
                                harness: None,
                                model: None,
                                effort: None,
                                workdir: Some(wt.path.clone()),
                                profile: None,
                                created_at: now,
                                updated_at: now,
                                appended_messages: Vec::new(),
                                native_session: None,
                            });
                            ov.workdir = Some(wt.path.clone());
                            ov.updated_at = now;
                            save_overlay(&ov);
                        }
                        return Ok(json_response(StatusCode::OK, &wt));
                    }
                    Ok(Err(e)) => {
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        ))
                    }
                    Err(resp) => return Ok(resp),
                }
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid worktree payload" }),
            ))
        }

        (&Method::DELETE, "/api/worktrees") => {
            let body_bytes = read_body(req).await;
            if let Ok(payload) = serde_json::from_slice::<RemoveWorktreeRequest>(&body_bytes) {
                let wd = payload.workdir.clone();
                let wt = payload.worktree_path.clone();
                match blocking("remove worktree", move || remove_worktree(&wd, &wt)).await {
                    Ok(Ok(())) => return Ok(json_response(StatusCode::OK, &json!({ "ok": true }))),
                    Ok(Err(e)) => {
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        ))
                    }
                    Err(resp) => return Ok(resp),
                }
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid worktree removal payload" }),
            ))
        }

        (&Method::GET, "/api/git-diff") => {
            let raw_workdir = extract_query_param(&query, "workdir")
                .map(|w| url_decode(&w))
                .unwrap_or_else(home_str);
            let diff = match blocking("git diff", move || inspect_git_diff(&raw_workdir)).await {
                Ok(d) => d,
                Err(resp) => return Ok(resp),
            };
            Ok(json_response(StatusCode::OK, &json!({ "diff": diff })))
        }

        (&Method::GET, "/api/files") => {
            let raw_workdir = extract_query_param(&query, "workdir")
                .map(|w| url_decode(&w))
                .unwrap_or_else(home_str);
            let q = extract_query_param(&query, "q")
                .map(|s| url_decode(&s))
                .unwrap_or_default();
            let files =
                match blocking("file list", move || list_workspace_files(&raw_workdir, &q)).await {
                    Ok(f) => f,
                    Err(resp) => return Ok(resp),
                };
            Ok(json_response(StatusCode::OK, &json!({ "files": files })))
        }

        (&Method::POST, "/api/conversations") => {
            let body_bytes = read_body(req).await;
            let mut payload: CreateConversationRequest = match serde_json::from_slice(&body_bytes) {
                Ok(payload) => payload,
                Err(_) => {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": "Invalid conversation request" }),
                    ))
                }
            };
            if let Some(ref workdir) = payload.workdir {
                let Some(path) = sanitize_workdir(workdir) else {
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": "Choose an existing project folder on this computer." }),
                    ));
                };
                payload.workdir = Some(path.to_string_lossy().into_owned());
            }
            let now = Utc::now().timestamp_millis();
            let id = format!("helm:{}", Uuid::new_v4());
            // Only bind a thread to an account that actually exists and runs
            // the same harness as the thread.
            let th = sanitize_harness(&payload.harness);
            let profile = payload
                .profile
                .as_deref()
                .and_then(profiles::sanitize_profile_id)
                .filter(|pid| {
                    profiles::get_profile(pid)
                        .map(|p| p.harness == th)
                        .unwrap_or(false)
                });
            let overlay = StoredThreadOverlay {
                id: id.clone(),
                title: payload.title.filter(|t| !t.trim().is_empty()),
                // New threads start unpinned; pinning is a deliberate user act.
                pinned: false,
                harness: Some(payload.harness),
                model: payload.model,
                effort: payload.effort,
                workdir: payload.workdir,
                profile,
                created_at: now,
                updated_at: now,
                appended_messages: Vec::new(),
                native_session: None,
            };
            save_overlay(&overlay);
            Ok(json_response(StatusCode::OK, &json!({ "id": id })))
        }

        (&Method::POST, "/api/upload") => {
            let body_bytes = read_body(req).await;
            if let Ok(payload) = serde_json::from_slice::<UploadPayload>(&body_bytes) {
                if let Ok(decoded) = BASE64_STD.decode(payload.data_base64.trim()) {
                    let ext = Path::new(&payload.filename)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("jpg");
                    let safe_ext: String = ext
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .take(5)
                        .collect();
                    let fname = format!(
                        "upload_{}_{}.{}",
                        Utc::now().timestamp_millis(),
                        &Uuid::new_v4().to_string()[..8],
                        if safe_ext.is_empty() {
                            "jpg"
                        } else {
                            &safe_ext
                        }
                    );
                    let dest = config_dir().join("uploads").join(&fname);
                    if fs::write(&dest, decoded).is_ok() {
                        return Ok(json_response(
                            StatusCode::OK,
                            &json!({
                                "url": format!("/api/uploads/{}", fname),
                                "path": dest.to_string_lossy()
                            }),
                        ));
                    }
                }
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid upload payload" }),
            ))
        }

        (&Method::POST, "/api/voice/transcribe") => {
            let body_bytes = read_body(req).await;
            if let Ok(transcribe_req) =
                serde_json::from_slice::<VoiceTranscribeRequest>(&body_bytes)
            {
                // Whisper subprocesses are blocking; keep them off the async
                // reactor workers entirely.
                let resp = tokio::task::spawn_blocking(move || {
                    process_voice_transcription(&transcribe_req)
                })
                .await
                .unwrap_or_else(|_| VoiceTranscribeResponse {
                    ok: false,
                    transcript: String::new(),
                    intent: VoiceIntent::ChatPrompt(String::new()),
                    engine: "error".to_string(),
                });
                return Ok(json_response(StatusCode::OK, &resp));
            }
            Ok(json_response(
                StatusCode::BAD_REQUEST,
                &json!({ "error": "Invalid voice transcribe payload" }),
            ))
        }

        (&Method::GET, "/api/local-image") => {
            if let Some(raw_path) = extract_query_param(&query, "path") {
                let decoded = url_decode(&raw_path);
                // Serve the *canonicalized* path that was validated, not the
                // raw user string (closes the check-then-open symlink swap).
                if let Some(canon) = resolve_allowed_image(&decoded) {
                    if let Ok(bytes) = fs::read(&canon) {
                        let mime = guess_image_mime(&canon.to_string_lossy());
                        return Ok(Response::builder()
                            .status(StatusCode::OK)
                            .header("Content-Type", mime)
                            .header("Cache-Control", "public, max-age=60")
                            .body(Full::from(Bytes::from(bytes)))
                            .unwrap());
                    }
                }
            }
            Ok(json_response(
                StatusCode::NOT_FOUND,
                &json!({ "error": "Image not found" }),
            ))
        }

        _ => {
            if let Some(rest) = path.strip_prefix("/api/claude-rc/") {
                let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
                let id = url_decode(id);
                if runner.remote_controls.get(&id).is_none() {
                    return Ok(json_response(
                        StatusCode::NOT_FOUND,
                        &json!({ "error": "Remote Control not found." }),
                    ));
                }
                if method == Method::GET && action.is_empty() {
                    return Ok(json_response(
                        StatusCode::OK,
                        &runner.remote_controls.get(&id),
                    ));
                }
                if method == Method::POST && (action == "input" || action == "stop") {
                    let result = if action == "stop" {
                        runner.remote_controls.stop(&id)
                    } else {
                        #[derive(Deserialize)]
                        struct Input {
                            #[serde(default)]
                            text: String,
                            key: Option<String>,
                        }
                        let input: Input = match serde_json::from_slice(&read_body(req).await) {
                            Ok(input) => input,
                            Err(_) => {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": "Invalid terminal input." }),
                                ))
                            }
                        };
                        runner
                            .remote_controls
                            .input(&id, &input.text, input.key.as_deref())
                    };
                    return Ok(match result {
                        Ok(()) => json_response(StatusCode::OK, &runner.remote_controls.get(&id)),
                        Err(error) => {
                            json_response(StatusCode::BAD_REQUEST, &json!({ "error": error }))
                        }
                    });
                }
                return Ok(json_response(
                    StatusCode::NOT_FOUND,
                    &json!({ "error": "Unknown Remote Control action." }),
                ));
            }
            if let Some(fname) = path.strip_prefix("/api/uploads/") {
                let safe: String = fname
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '_')
                    .collect();
                let fpath = config_dir().join("uploads").join(&safe);
                if let Ok(bytes) = fs::read(&fpath) {
                    let mime = guess_image_mime(&safe);
                    return Ok(Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", mime)
                        .body(Full::from(Bytes::from(bytes)))
                        .unwrap());
                }
                return Ok(json_response(
                    StatusCode::NOT_FOUND,
                    &json!({ "error": "Not found" }),
                ));
            }

            if let Some(rest) = path.strip_prefix("/api/profiles/") {
                if method == Method::DELETE {
                    let pid = url_decode(rest);
                    let purge = extract_query_param(&query, "purge")
                        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                        .unwrap_or(false);
                    return match profiles::remove_profile(&pid, purge) {
                        Ok(()) => Ok(json_response(
                            StatusCode::OK,
                            &json!({ "ok": true, "purged_data": purge }),
                        )),
                        Err(e) => Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": e }),
                        )),
                    };
                }
            }

            if let Some(rest) = path.strip_prefix("/api/approvals/") {
                if let Some(id_enc) = rest.strip_suffix("/decision") {
                    if method == Method::POST {
                        let appr_id = url_decode(id_enc);
                        let body_bytes = read_body(req).await;
                        if let Ok(payload) = serde_json::from_slice::<
                            crate::models::ApprovalDecisionRequest,
                        >(&body_bytes)
                        {
                            let kind = match payload.decision.to_lowercase().as_str() {
                                "allow_always" => crate::models::ApprovalDecisionKind::AllowAlways,
                                "deny" => crate::models::ApprovalDecisionKind::Deny,
                                _ => crate::models::ApprovalDecisionKind::AllowOnce,
                            };
                            match runner.approvals.decide(&appr_id, kind).await {
                                Ok(item) => {
                                    return Ok(json_response(
                                        StatusCode::OK,
                                        &json!({ "ok": true, "item": item }),
                                    ));
                                }
                                Err(e) => {
                                    return Ok(json_response(
                                        StatusCode::BAD_REQUEST,
                                        &json!({ "error": e }),
                                    ));
                                }
                            }
                        }
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &json!({ "error": "Invalid approval decision body" }),
                        ));
                    }
                }
            }

            if let Some(rest) = path.strip_prefix("/api/swarm/") {
                if let Some(id_enc) = rest.strip_suffix("/merge") {
                    if method == Method::POST {
                        let swarm_id = url_decode(id_enc);
                        match swarm_mgr.merge_session(&swarm_id).await {
                            Ok(msg) => {
                                return Ok(json_response(
                                    StatusCode::OK,
                                    &json!({ "ok": true, "message": msg }),
                                ));
                            }
                            Err(e) => {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": e }),
                                ));
                            }
                        }
                    }
                } else if let Some(id_enc) = rest.strip_suffix("/discard") {
                    if method == Method::POST {
                        let swarm_id = url_decode(id_enc);
                        match swarm_mgr.discard_session(&swarm_id).await {
                            Ok(()) => {
                                return Ok(json_response(StatusCode::OK, &json!({ "ok": true })));
                            }
                            Err(e) => {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": e }),
                                ));
                            }
                        }
                    }
                } else if let Some(id_enc) = rest.strip_suffix("/diff") {
                    if method == Method::GET {
                        let swarm_id = url_decode(id_enc);
                        match swarm_mgr.diff_session(&swarm_id).await {
                            Ok(diff) => {
                                return Ok(json_response(StatusCode::OK, &json!({ "diff": diff })));
                            }
                            Err(e) => {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": e }),
                                ));
                            }
                        }
                    }
                } else if method == Method::GET {
                    let swarm_id = url_decode(rest);
                    if let Some(session) = swarm_mgr.get_session(&swarm_id).await {
                        return Ok(json_response(
                            StatusCode::OK,
                            &json!({ "session": session }),
                        ));
                    } else {
                        return Ok(json_response(
                            StatusCode::NOT_FOUND,
                            &json!({ "error": "Swarm session not found" }),
                        ));
                    }
                }
            }

            if let Some(rest) = path.strip_prefix("/api/sprints/") {
                if let Some(id_enc) = rest.strip_suffix("/cancel") {
                    if method == Method::POST {
                        let sprint_id = url_decode(id_enc);
                        match sprint_mgr.cancel_sprint(&sprint_id).await {
                            Ok(()) => {
                                return Ok(json_response(StatusCode::OK, &json!({ "ok": true })))
                            }
                            Err(e) => {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": e }),
                                ))
                            }
                        }
                    }
                } else if method == Method::DELETE {
                    let sprint_id = url_decode(rest);
                    match sprint_mgr.delete_sprint(&sprint_id).await {
                        Ok(()) => return Ok(json_response(StatusCode::OK, &json!({ "ok": true }))),
                        Err(e) => {
                            return Ok(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "error": e }),
                            ))
                        }
                    }
                } else if method == Method::GET {
                    let sprint_id = url_decode(rest);
                    if let Some(sprint) = sprint_mgr.get_sprint(&sprint_id).await {
                        return Ok(json_response(StatusCode::OK, &sprint));
                    } else {
                        return Ok(json_response(
                            StatusCode::NOT_FOUND,
                            &json!({ "error": "Sprint not found" }),
                        ));
                    }
                }
            }

            if let Some(rest) = path.strip_prefix("/api/conversations/") {
                if let Some(id_enc) = rest.strip_suffix("/bounce") {
                    if method == Method::POST {
                        let conv_id = url_decode(id_enc);
                        let body_bytes = read_body(req).await;
                        if let Ok(bounce_req) =
                            serde_json::from_slice::<SwarmBounceRequest>(&body_bytes)
                        {
                            match swarm_mgr
                                .execute_bounce(&conv_id, bounce_req, runner.clone())
                                .await
                            {
                                Ok(session) => {
                                    return Ok(json_response(
                                        StatusCode::OK,
                                        &json!({ "ok": true, "session": session }),
                                    ));
                                }
                                Err(e) => {
                                    return Ok(json_response(
                                        StatusCode::BAD_REQUEST,
                                        &json!({ "error": e }),
                                    ));
                                }
                            }
                        } else {
                            return Ok(json_response(
                                StatusCode::BAD_REQUEST,
                                &json!({ "error": "Invalid bounce request payload" }),
                            ));
                        }
                    }
                } else if let Some(id_enc) = rest.strip_suffix("/messages") {
                    if method == Method::POST {
                        let conv_id = url_decode(id_enc);
                        // Idempotency for offline-queue replays: a retried
                        // delivery must never start a *new* harness turn.
                        // (Read before `read_body` consumes the request.)
                        let idem_key = req
                            .headers()
                            .get("x-idempotency-key")
                            .and_then(|v| v.to_str().ok())
                            .map(|v| v.trim().to_string())
                            .filter(|k| !k.is_empty() && k.len() <= 128);
                        let body_bytes = read_body(req).await;
                        if let Ok(send_req) =
                            serde_json::from_slice::<SendMessageRequest>(&body_bytes)
                        {
                            if let Some(idem_key) = idem_key {
                                if !runner.check_idempotency(&idem_key).await {
                                    return Ok(json_response(
                                        StatusCode::OK,
                                        &json!({ "status": "duplicate" }),
                                    ));
                                }
                            }
                            match dispatch_turn(runner, conv_id, send_req).await {
                                Ok(msg) => return Ok(json_response(StatusCode::OK, &msg)),
                                Err(e) => {
                                    return Ok(json_response(
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        &json!({ "error": e }),
                                    ))
                                }
                            }
                        }
                    }
                } else if let Some(id_enc) = rest.strip_suffix("/stop") {
                    if method == Method::POST {
                        let conv_id = url_decode(id_enc);
                        let stopped = runner.stop_conversation(&conv_id).await;
                        return Ok(json_response(
                            StatusCode::OK,
                            &json!({ "stopped": stopped }),
                        ));
                    }
                } else if let Some(id_enc) = rest.strip_suffix("/second-opinion") {
                    if method == Method::POST {
                        let conv_id = url_decode(id_enc);
                        let body_bytes = read_body(req).await;
                        if let Ok(so_req) =
                            serde_json::from_slice::<SecondOpinionRequest>(&body_bytes)
                        {
                            let active = runner.active_snapshot().await;
                            let cid2 = conv_id.clone();
                            let detail = match blocking("second-opinion detail", move || {
                                get_conversation_detail(&cid2, &active)
                            })
                            .await
                            {
                                Ok(d) => d,
                                Err(resp) => return Ok(resp),
                            };
                            let workdir = detail
                                .as_ref()
                                .map(|d| d.summary.workdir.clone())
                                .unwrap_or_else(home_str);
                            let mode = so_req
                                .mode
                                .clone()
                                .unwrap_or_else(|| "review-diff".to_string());

                            if mode == "arena" {
                                let base_prompt = so_req
                                    .prompt
                                    .clone()
                                    .filter(|p| !p.trim().is_empty())
                                    .or_else(|| {
                                        detail.as_ref().and_then(|d| {
                                            d.messages
                                                .iter()
                                                .rev()
                                                .find(|m| m.role == "user")
                                                .map(|m| m.text.clone())
                                        })
                                    })
                                    .unwrap_or_else(|| {
                                        "Review the current repository architecture and suggest improvements."
                                            .to_string()
                                    });

                                let now = Utc::now().timestamp_millis();
                                let new_id = format!("helm:{}", Uuid::new_v4());
                                let title = format!(
                                    "Arena ({}) · {}",
                                    so_req.harness.to_uppercase(),
                                    detail
                                        .as_ref()
                                        .map(|d| d.summary.title.as_str())
                                        .unwrap_or("Run")
                                );
                                let overlay = StoredThreadOverlay {
                                    id: new_id.clone(),
                                    title: Some(title),
                                    pinned: true,
                                    harness: Some(so_req.harness.clone()),
                                    model: so_req.model.clone(),
                                    effort: Some("high".to_string()),
                                    workdir: Some(workdir.clone()),
                                    profile: None,
                                    created_at: now,
                                    updated_at: now,
                                    appended_messages: Vec::new(),
                                    native_session: None,
                                };
                                save_overlay(&overlay);

                                let send_req = SendMessageRequest {
                                    text: base_prompt,
                                    harness: Some(so_req.harness),
                                    model: so_req.model,
                                    effort: Some("high".to_string()),
                                    workdir: Some(workdir),
                                    profile: None,
                                    images: Vec::new(),
                                };
                                let _ = dispatch_turn(runner, new_id.clone(), send_req).await;
                                return Ok(json_response(
                                    StatusCode::OK,
                                    &json!({ "ok": true, "mode": "arena", "conversation_id": new_id }),
                                ));
                            } else {
                                let wd = workdir.clone();
                                let diff = match blocking("second-opinion diff", move || {
                                    inspect_git_diff(&wd)
                                })
                                .await
                                {
                                    Ok(d) => d,
                                    Err(resp) => return Ok(resp),
                                };
                                let review_prompt = format!(
                                    "[Second-Opinion Audit via {}]\nReview the current git diff in `{}` for bugs, edge cases, regressions, or security issues. Be concise and actionable:\n\n```diff\n{}\n```",
                                    so_req.harness.to_uppercase(),
                                    workdir,
                                    diff
                                );
                                let send_req = SendMessageRequest {
                                    text: review_prompt,
                                    harness: Some(so_req.harness),
                                    model: so_req.model,
                                    effort: Some("high".to_string()),
                                    workdir: Some(workdir),
                                    profile: None,
                                    images: Vec::new(),
                                };
                                match dispatch_turn(runner, conv_id.clone(), send_req).await {
                                    Ok(msg) => {
                                        return Ok(json_response(
                                            StatusCode::OK,
                                            &json!({ "ok": true, "mode": "review-diff", "conversation_id": conv_id, "message": msg }),
                                        ))
                                    }
                                    Err(e) => {
                                        return Ok(json_response(
                                            StatusCode::BAD_REQUEST,
                                            &json!({ "error": e }),
                                        ))
                                    }
                                }
                            }
                        }
                    }
                } else if method == Method::PATCH {
                    let conv_id = url_decode(rest);
                    let body_bytes = read_body(req).await;
                    if let Ok(patch) =
                        serde_json::from_slice::<UpdateConversationRequest>(&body_bytes)
                    {
                        if let Some(archived) = patch.archived {
                            if let Err(e) = discovery::set_archived(&conv_id, archived) {
                                return Ok(json_response(
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    &json!({ "error": format!("Could not save: {e}") }),
                                ));
                            }
                            // Archiving alone must not create an overlay for a native session.
                            if patch.title.is_none()
                                && patch.pinned.is_none()
                                && patch.workdir.is_none()
                                && patch.harness.is_none()
                                && patch.model.is_none()
                                && patch.effort.is_none()
                                && patch.profile.is_none()
                            {
                                return Ok(json_response(StatusCode::OK, &json!({ "ok": true })));
                            }
                        }
                        let mut overlays = load_overlays();
                        let now = Utc::now().timestamp_millis();
                        let mut ov = overlays.remove(&conv_id).unwrap_or(StoredThreadOverlay {
                            id: conv_id.clone(),
                            title: None,
                            pinned: false,
                            harness: None,
                            model: None,
                            effort: None,
                            workdir: None,
                            profile: None,
                            created_at: now,
                            updated_at: now,
                            appended_messages: Vec::new(),
                            native_session: None,
                        });
                        if let Some(t) = patch.title {
                            ov.title = Some(t);
                        }
                        if let Some(p) = patch.pinned {
                            ov.pinned = p;
                        }
                        if let Some(w) = patch.workdir {
                            let Some(path) = sanitize_workdir(&w) else {
                                return Ok(json_response(
                                    StatusCode::BAD_REQUEST,
                                    &json!({ "error": "Choose an existing project folder on this computer." }),
                                ));
                            };
                            ov.workdir = Some(path.to_string_lossy().into_owned());
                        }
                        if let Some(h) = patch.harness {
                            ov.harness = Some(h);
                        }
                        if let Some(m) = patch.model {
                            ov.model = Some(m);
                        }
                        if let Some(ef) = patch.effort {
                            ov.effort = Some(ef);
                        }
                        if let Some(pr) = patch.profile {
                            // `"default"` (or "") un-binds the account.
                            ov.profile = profiles::sanitize_profile_id(&pr)
                                .filter(|pid| pid != "default")
                                .filter(|pid| {
                                    ov.harness
                                        .as_deref()
                                        .map(|h| {
                                            profiles::get_profile(pid)
                                                .map(|p| p.harness == h)
                                                .unwrap_or(false)
                                        })
                                        .unwrap_or(false)
                                });
                        }
                        ov.updated_at = now;
                        save_overlay(&ov);
                        return Ok(json_response(StatusCode::OK, &ov));
                    }
                    return Ok(json_response(
                        StatusCode::BAD_REQUEST,
                        &json!({ "error": "Invalid patch body" }),
                    ));
                } else if method == Method::GET {
                    let conv_id = url_decode(rest);
                    // `?tail=N` keeps phone polls small: only the last N
                    // messages, with totals reported for the UI.
                    let tail = extract_query_param(&query, "tail")
                        .and_then(|v| v.parse::<usize>().ok())
                        .filter(|n| *n > 0)
                        .map(|n| n.min(2000));
                    let active = runner.active_snapshot().await;
                    let cid = conv_id.clone();
                    let found = match blocking("conversation detail", move || {
                        get_conversation_detail_tail(&cid, &active, tail)
                    })
                    .await
                    {
                        Ok(d) => d,
                        Err(resp) => return Ok(resp),
                    };
                    if let Some(mut detail) = found {
                        detail.summary.pending_approval = runner
                            .approvals
                            .get_pending_for_conversation(&conv_id)
                            .await;
                        return Ok(json_response(StatusCode::OK, &detail));
                    } else {
                        return Ok(json_response(
                            StatusCode::NOT_FOUND,
                            &json!({ "error": "Conversation not found" }),
                        ));
                    }
                }
            }

            Ok(json_response(
                StatusCode::NOT_FOUND,
                &json!({ "error": "Route not found" }),
            ))
        }
    }
}

const MAX_BODY_BYTES: usize = 15 * 1024 * 1024; // 15 MB cap to prevent OOM DoS

/// Run a blocking helper (git/gh/ss subprocesses, transcript crawls) on the
/// blocking pool instead of an async worker, and map a join failure to a clean
/// 500 instead of a hung event loop. Several routes below funnel through this:
/// without it a handful of concurrent network-bound `gh`/`git` calls could
/// stall every other request, including agent streams.
#[allow(clippy::result_large_err)]
async fn blocking<T, F>(label: &'static str, f: F) -> Result<T, Response<Full<Bytes>>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|_| {
        json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({ "error": format!("Internal task failed: {label}") }),
        )
    })
}

async fn read_body(req: Request<hyper::body::Incoming>) -> Bytes {
    // A dribbled body must not pin the task forever either: 30 s ceiling.
    let collect = Limited::new(req.into_body(), MAX_BODY_BYTES).collect();
    match tokio::time::timeout(Duration::from_secs(30), collect).await {
        Ok(Ok(collected)) => collected.to_bytes(),
        // Over-limit / transport errors and timeouts surface as an empty body
        // → callers answer 400; the size limit itself answers 413 upstream via
        // hyper where the content-length is declared.
        _ => Bytes::new(),
    }
}

fn json_response<T: serde::Serialize>(status: StatusCode, data: &T) -> Response<Full<Bytes>> {
    let body = serde_json::to_vec(data).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .body(Full::from(Bytes::from(body)))
        .unwrap()
}

fn png_response(bytes: &'static [u8]) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "image/png")
        .header("Cache-Control", "public, max-age=86400")
        .body(Full::from(Bytes::from_static(bytes)))
        .unwrap()
}

fn check_cookie(req: &Request<hyper::body::Incoming>, cfg: &auth::AuthConfig) -> bool {
    if let Some(cookie_hdr) = req.headers().get("cookie").and_then(|h| h.to_str().ok()) {
        for part in cookie_hdr.split(';') {
            let trimmed = part.trim();
            if let Some(val) = trimmed.strip_prefix("helm_session=") {
                if verify_session_token(cfg, val) {
                    return true;
                }
            }
        }
    }
    if let Some(auth_hdr) = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
    {
        if let Some(val) = auth_hdr.strip_prefix("Bearer ") {
            if verify_session_token(cfg, val.trim()) {
                return true;
            }
        }
    }
    false
}

/// Resolves and validates a `/api/local-image` target. The extension whitelist
/// is applied to the **canonical** path (so a `x.png` symlink cannot point at
/// `<config dir>/auth.json` and leak the HMAC secret), the resolved target
/// must be a regular file with a bounded size, and the final location must sit
/// inside an allow-listed prefix.
const MAX_SERVED_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

fn resolve_allowed_image(path: &str) -> Option<std::path::PathBuf> {
    if path.contains("..") {
        return None;
    }
    let canon = fs::canonicalize(path).ok()?;
    let canon_lower = canon.to_string_lossy().to_lowercase();
    let valid_ext = [".png", ".jpg", ".jpeg", ".webp", ".gif"]
        .iter()
        .any(|e| canon_lower.ends_with(e));
    if !valid_ext {
        return None;
    }
    let md = fs::metadata(&canon).ok()?;
    if !md.is_file() || md.len() > MAX_SERVED_IMAGE_BYTES {
        return None;
    }
    let home = fs::canonicalize(home_dir())
        .unwrap_or_else(|_| home_dir())
        .to_string_lossy()
        .to_string();
    let canon_str = canon.to_string_lossy();
    if canon_str.starts_with("/tmp/")
        || canon_str.starts_with(&format!("{}/Projects/", home))
        || canon_str.starts_with(&format!("{}/.gemini/", home))
        || canon.starts_with(
            fs::canonicalize(crate::discovery::config_dir())
                .unwrap_or_else(|_| crate::discovery::config_dir()),
        )
    {
        Some(canon)
    } else {
        None
    }
}

fn guess_image_mime(path: &str) -> &'static str {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else {
        "image/jpeg"
    }
}

fn extract_query_param(query: &str, key: &str) -> Option<String> {
    let prefix = format!("{}=", key);
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix(&prefix) {
            return Some(v.to_string());
        }
    }
    None
}

fn url_decode(input: &str) -> String {
    let mut bytes = Vec::with_capacity(input.len());
    let raw = input.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(hex_str) = std::str::from_utf8(&raw[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex_str, 16) {
                    bytes.push(b);
                    i += 3;
                    continue;
                }
            }
        } else if raw[i] == b'+' {
            bytes.push(b' ');
            i += 1;
            continue;
        }
        bytes.push(raw[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

fn which_binary(bin: &str) -> Option<std::path::PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(meta) = candidate.metadata() {
                        if meta.permissions().mode() & 0o111 != 0 {
                            return Some(candidate);
                        }
                    }
                }
                #[cfg(not(unix))]
                return Some(candidate);
            }
        }
    }
    None
}

fn detect_lan_ip() -> Option<String> {
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(local) = socket.local_addr() {
                let ip = local.ip();
                if !ip.is_loopback() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    if let Ok(out) = std::process::Command::new("hostname").arg("-I").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            for part in s.split_whitespace() {
                if let Ok(ip) = part.parse::<std::net::IpAddr>() {
                    if !ip.is_loopback() {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }
    None
}

fn detect_tailscale_host() -> Option<String> {
    if let Ok(out) = std::process::Command::new("tailscale")
        .arg("status")
        .arg("--json")
        .output()
    {
        if out.status.success() {
            if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(dns_name) = val
                    .get("Self")
                    .and_then(|s| s.get("DNSName"))
                    .and_then(|d| d.as_str())
                {
                    let trimmed = dns_name.trim_end_matches('.');
                    if !trimmed.is_empty() {
                        return Some(trimmed.to_string());
                    }
                }
            }
        }
    }
    if let Ok(out) = std::process::Command::new("tailscale")
        .arg("ip")
        .arg("-4")
        .output()
    {
        if out.status.success() {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip.is_empty() {
                return Some(ip);
            }
        }
    }
    None
}

fn run_doctor() -> anyhow::Result<()> {
    println!("MoE Deck Doctor — Multi-Harness Agent Gateway Preflight");
    println!("===================================================");

    let tools = [
        ("claude", "Claude Code CLI"),
        ("opencode", "OpenCode CLI"),
        ("agy", "Antigravity CLI"),
        ("codex", "Codex CLI"),
        ("keel", "Keel Agent CLI"),
        ("git", "Git version control"),
        ("tailscale", "Tailscale mesh VPN"),
    ];

    println!("Agents & Harnesses:");
    let mut found_any = false;
    for (bin, desc) in &tools {
        match which_binary(bin) {
            Some(path) => {
                println!("  ✓ {:<10} {:<24} [{}]", bin, desc, path.display());
                if *bin != "git" && *bin != "tailscale" {
                    found_any = true;
                }
            }
            None => {
                println!("  ○ {:<10} {:<24} (not found in PATH)", bin, desc);
            }
        }
    }

    if !found_any {
        println!();
        println!("  ⚠️ No agent harness CLIs found in PATH!");
        println!("     Install at least one of: claude, opencode, agy, codex, or keel.");
    }

    println!();
    println!("Security & Connectivity:");
    let auth_cfg = auth::load_auth_config();
    if let Some(pc) = auth::effective_passcode(&auth_cfg) {
        println!("  ✓ Passcode:  configured ({} chars)", pc.len());
    } else {
        println!(
            "  ⚠️ Passcode:  none configured (set MOEDECK_PASSCODE or run `moedeck passcode`)"
        );
    }

    if let Some(ts) = detect_tailscale_host() {
        println!("  ✓ Tailscale: active ({})", ts);
    } else {
        println!("  ○ Tailscale: not detected / daemon not running");
    }

    if let Some(lan) = detect_lan_ip() {
        println!("  ✓ LAN IP:    {}", lan);
    }

    let port: u16 = discovery::setting("PORT")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7770);
    println!("  ✓ Port:      {}", port);

    println!("===================================================");
    Ok(())
}

fn run_pair(port: u16) -> anyhow::Result<()> {
    let auth_cfg = auth::load_auth_config();
    let token = auth::issue_session_token(&auth_cfg);
    let effective_pc = auth::effective_passcode(&auth_cfg).unwrap_or_else(|| "none".to_string());
    let host = detect_tailscale_host()
        .or_else(detect_lan_ip)
        .unwrap_or_else(|| "127.0.0.1".to_string());

    let pair_url = format!("http://{}:{}/?token={}", host, port, token);
    let local_url = format!("http://127.0.0.1:{}", port);

    println!("MoE Deck Mobile Pairing");
    println!("===================================================");
    println!("  Pairing URL (tap or scan on phone):");
    println!("  {}", pair_url);
    println!();
    println!("  Local Cockpit: {}", local_url);
    println!("  Passcode:      {}", effective_pc);
    println!("===================================================");
    Ok(())
}

fn run_passcode(new_code: Option<&str>) -> anyhow::Result<()> {
    let mut cfg = auth::load_auth_config();
    if let Some(code) = new_code {
        let trimmed = code.trim().to_string();
        if trimmed.is_empty() {
            eprintln!("Passcode cannot be empty.");
            std::process::exit(1);
        }
        cfg.passcode = Some(trimmed.clone());
        auth::save_auth_config(&cfg);
        println!("Passcode updated to: {}", trimmed);
    } else {
        match auth::effective_passcode(&cfg) {
            Some(pc) => println!("Current passcode: {}", pc),
            None => println!("No passcode currently configured."),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_which_binary() {
        assert!(which_binary("sh").is_some() || which_binary("bash").is_some());
        assert!(which_binary("definitely_non_existent_binary_xyz_99").is_none());
    }

    #[test]
    fn test_extract_query_param() {
        assert_eq!(
            extract_query_param("foo=1&token=s.1234.abcd&bar=2", "token"),
            Some("s.1234.abcd".to_string())
        );
        assert_eq!(
            extract_query_param("token=mytoken", "token"),
            Some("mytoken".to_string())
        );
        assert_eq!(extract_query_param("other=123", "token"), None);
    }

    #[test]
    fn test_url_decode() {
        assert_eq!(url_decode("hello%20world"), "hello world");
        assert_eq!(url_decode("hello+world"), "hello world");
        assert_eq!(url_decode("simple"), "simple");
    }
}
