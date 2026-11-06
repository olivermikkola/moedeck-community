use crate::discovery::config_dir;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::IpAddr;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Session cookies stay valid for at most this long; passcode changes or
/// explicit revocation (see `revoke_all_sessions`) invalidate earlier ones
/// immediately via `revoked_before`.
pub const SESSION_TTL_SECS: i64 = 90 * 24 * 3600;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub secret: String,
    pub passcode: Option<String>,
    /// All session cookies issued at or before this timestamp are rejected.
    #[serde(default)]
    pub revoked_before: i64,
}

pub fn generate_random_passcode() -> String {
    let raw = Uuid::new_v4().simple().to_string();
    raw[..8].to_string()
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: Uuid::new_v4().to_string(),
            passcode: Some(generate_random_passcode()),
            revoked_before: 0,
        }
    }
}

pub fn load_auth_config() -> AuthConfig {
    let path = config_dir().join("auth.json");
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(mut cfg) = serde_json::from_str::<AuthConfig>(&content) {
            let empty_passcode = cfg
                .passcode
                .as_deref()
                .map(str::trim)
                .unwrap_or("")
                .is_empty();
            if empty_passcode {
                cfg.passcode = Some(generate_random_passcode());
                save_auth_config(&cfg);
            }
            return cfg;
        }
    }
    let cfg = AuthConfig::default();
    save_auth_config(&cfg);
    cfg
}

pub fn save_auth_config(cfg: &AuthConfig) {
    let path = config_dir().join("auth.json");
    let Ok(json) = serde_json::to_string_pretty(cfg) else {
        return;
    };
    // Create the temp file with 0600 *before* the secret/passcode lands in it,
    // then rename: no window where the world/group can read the new config.
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let tmp_path = dir.join(format!(".tmp.auth.{}", Uuid::new_v4()));
    let write_result = (|| -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        opts.mode(0o600);
        use std::io::Write;
        let mut f = opts.open(&tmp_path)?;
        f.write_all(json.as_bytes())?;
        f.flush()?;
        fs::rename(&tmp_path, &path)?;
        #[cfg(unix)]
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = fs::remove_file(&tmp_path);
        eprintln!("Failed to persist auth config: {}", e);
    }
}

/// Invalidate every previously issued session cookie (passcode rotation,
/// manual "log out everything").
pub fn revoke_all_sessions(cfg: &mut AuthConfig) {
    cfg.revoked_before = now_unix();
}

pub fn is_tailscale_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_loopback() {
                return true;
            }
            let octets = v4.octets();
            // Tailscale CGNAT 100.64.0.0/10 (100.64.0.0 - 100.127.255.255)
            octets[0] == 100 && (octets[1] & 0b1100_0000) == 64
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return true;
            }
            if let Some(mapped_v4) = v6.to_ipv4_mapped() {
                return is_tailscale_or_local_ip(IpAddr::V4(mapped_v4));
            }
            let segs = v6.segments();
            // Tailscale ULA fd7a:115c:a1e0::/48
            segs[0] == 0xfd7a && segs[1] == 0x115c && segs[2] == 0xa1e0
        }
    }
}

/// Protects against DNS rebinding attacks by ensuring the Host header
/// is localhost, a loopback IP, a Tailscale MagicDNS host (`.ts.net`),
/// or a Tailscale IP address. A missing Host header fails closed — HTTP/1.1
/// clients (and hyper itself) always send one; only raw HTTP/1.0 probes lack it.
pub fn is_allowed_host_header(host_hdr: Option<&str>) -> bool {
    let Some(raw) = host_hdr else {
        return false;
    };
    let Some(host) = normalize_host(raw) else {
        return false;
    };

    if host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".ts.net") {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_tailscale_or_local_ip(ip);
    }
    // Single-label local MagicDNS hostname (no dots, alphanumeric/hyphens)
    !host.is_empty()
        && !host.contains('.')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Lowercase hostname with scheme and port stripped: `http://a.b:7770/x` → `a.b`.
pub(crate) fn normalize_host(raw: &str) -> Option<String> {
    let mut h = raw.trim().to_lowercase();
    for scheme in ["http://", "https://"] {
        if let Some(rest) = h.strip_prefix(scheme) {
            h = rest.to_string();
        }
    }
    h = h.trim_end_matches('/').to_string();
    if let Some(rest) = h.strip_prefix('[') {
        // Bracketed IPv6: keep the address, drop ]:port
        h = rest.split(']').next().unwrap_or("").to_string();
    } else if let Some((host, maybe_port)) = h.rsplit_once(':') {
        if maybe_port.chars().all(|c| c.is_ascii_digit()) && !maybe_port.is_empty() {
            h = host.to_string();
        }
    }
    if h.is_empty() {
        None
    } else {
        Some(h)
    }
}

/// Protects state-mutating endpoints (POST/PATCH/DELETE) against Cross-Site
/// Request Forgery (CSRF). When a browser sends an `Origin`, its host must
/// match the request `Host` exactly — an arbitrary allow-listed label like
/// `null` or a dotless attacker hostname no longer passes. Non-browser clients
/// (raw API use) send no Origin and are judged on Content-Type alone.
pub fn is_safe_mutation_request(
    content_type: Option<&str>,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
    request_host: Option<&str>,
) -> bool {
    if let Some(sfs) = sec_fetch_site {
        if sfs.eq_ignore_ascii_case("cross-site") {
            return false;
        }
    }
    if let Some(orig) = origin {
        let Some(req_host) = request_host.and_then(normalize_host) else {
            return false;
        };
        let Some(orig_host) = normalize_host(orig) else {
            return false;
        };
        // Empty/`null` origins (sandboxed frames, data: pages) and any origin
        // that is not literally the MoE Deck host get rejected.
        if orig_host != req_host {
            return false;
        }
    }
    // Require JSON content-type on state-changing requests so HTML forms / no-cors simple POSTs are blocked
    if let Some(ct) = content_type {
        ct.to_lowercase().contains("application/json")
    } else {
        false
    }
}

/// Same-origin gate for reverse-proxied preview traffic, which browsers may
/// hit with form content-types the JSON rule above cannot allow. Requires an
/// explicit, matching Origin (or non-cross-site Sec-Fetch-Site) — a raw
/// no-cors `fetch(..., mode:'no-cors')` from evil.com always carries an
/// honest cross-origin Origin that fails here.
pub fn is_safe_preview_mutation(
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
    request_host: Option<&str>,
) -> bool {
    if let Some(sfs) = sec_fetch_site {
        if sfs.eq_ignore_ascii_case("cross-site") {
            return false;
        }
    }
    match (origin, request_host) {
        (Some(orig), Some(host)) => {
            let (Some(orig_host), Some(req_host)) = (normalize_host(orig), normalize_host(host))
            else {
                return false;
            };
            orig_host == req_host
        }
        // A cross-origin *browser* POST always sends Origin. Only bare
        // non-browser clients may omit it, and then Sec-Fetch-Site must be
        // absent or same-origin (never cross-site, handled above).
        (Some(_), None) => false,
        (None, _) => {
            sec_fetch_site.is_none()
                || matches!(sec_fetch_site, Some(s) if s.eq_ignore_ascii_case("same-origin"))
        }
    }
}

/// Issue a fresh, per-login session token. The payload carries an issue
/// timestamp and nonce, so every cookie is unique, expires, and can be
/// mass-revoked through `AuthConfig::revoked_before`.
pub fn issue_session_token(cfg: &AuthConfig) -> String {
    let payload = format!(
        "s.{}.{}",
        now_unix(),
        &Uuid::new_v4().simple().to_string()[..12]
    );
    let Some(sig) = sign_token(cfg, &payload) else {
        return String::new();
    };
    format!("{}.{}", payload, sig)
}

fn sign_token(cfg: &AuthConfig, payload: &str) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(cfg.secret.as_bytes()).ok()?;
    mac.update(payload.as_bytes());
    Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
}

/// The passcode actually enforced: `MOEDECK_PASSCODE` (or the older
/// `HELM_PASSCODE`) wins over the stored config so ops can rotate/lock without
/// touching auth.json.
pub fn effective_passcode(cfg: &AuthConfig) -> Option<String> {
    if let Some(env_pc) = crate::discovery::setting("PASSCODE") {
        return Some(env_pc.trim().to_string());
    }
    cfg.passcode
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Constant-time passcode comparison over SHA-256 digests so neither content
/// nor length of either string leaks through timing.
pub fn passcode_matches(expected: &str, provided: &str) -> bool {
    let he = Sha256::digest(expected.as_bytes());
    let hp = Sha256::digest(provided.as_bytes());
    constant_time_eq(&he, &hp)
}

pub fn verify_session_token(cfg: &AuthConfig, token: &str) -> bool {
    let Some((payload, sig)) = token.rsplit_once('.') else {
        return false;
    };
    // payload = s.<issued_unix>.<nonce>
    let mut parts = payload.split('.');
    if parts.next() != Some("s") {
        return false;
    }
    let Some(issued) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
        return false;
    };
    let Some(nonce) = parts.next() else {
        return false;
    };
    if nonce.is_empty() || parts.next().is_some() {
        return false;
    }
    let now = now_unix();
    // Reject future-dated tokens (small skew), expired tokens, and anything
    // issued before a revoke-all event.
    if issued > now + 60
        || now.saturating_sub(issued) > SESSION_TTL_SECS
        || issued <= cfg.revoked_before
    {
        return false;
    }
    match sign_token(cfg, payload) {
        Some(expected) => constant_time_eq(expected.as_bytes(), sig.as_bytes()),
        None => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tailscale_and_loopback_ip_only() {
        // Loopback allowed
        assert!(is_tailscale_or_local_ip("127.0.0.1".parse().unwrap()));
        assert!(is_tailscale_or_local_ip("::1".parse().unwrap()));
        // Tailscale CGNAT 100.64.0.0/10 allowed
        assert!(is_tailscale_or_local_ip("100.100.50.25".parse().unwrap()));
        assert!(is_tailscale_or_local_ip("100.64.0.1".parse().unwrap()));
        assert!(is_tailscale_or_local_ip("100.127.255.254".parse().unwrap()));
        // Non-Tailscale 100.x rejected
        assert!(!is_tailscale_or_local_ip("100.63.255.255".parse().unwrap()));
        assert!(!is_tailscale_or_local_ip("100.128.0.1".parse().unwrap()));
        // Untrusted RFC1918 LAN / Wi-Fi / Docker bridge IPs rejected
        assert!(!is_tailscale_or_local_ip("192.168.1.50".parse().unwrap()));
        assert!(!is_tailscale_or_local_ip("10.0.0.5".parse().unwrap()));
        assert!(!is_tailscale_or_local_ip("172.17.0.2".parse().unwrap()));
    }

    #[test]
    fn test_dns_rebinding_and_csrf_guards() {
        // Missing Host now fails closed (HTTP/1.0 style probes).
        assert!(!is_allowed_host_header(None));
        assert!(is_allowed_host_header(Some(
            "example.tail123456.ts.net:7770"
        )));
        assert!(is_allowed_host_header(Some("127.0.0.1:7770")));
        assert!(is_allowed_host_header(Some("100.90.1.2:7770")));
        assert!(!is_allowed_host_header(Some("evil-rebind.com:7770")));
        assert!(!is_allowed_host_header(Some("192.168.1.10:7770")));

        // Origin must match the request Host exactly — allow-listed-looking
        // origins ("null", dotless labels) no longer pass.
        assert!(is_safe_mutation_request(
            Some("application/json"),
            Some("http://example.tail123456.ts.net:7770"),
            Some("same-origin"),
            Some("example.tail123456.ts.net:7770"),
        ));
        assert!(!is_safe_mutation_request(
            Some("application/json"),
            Some("https://evil.example.com"),
            Some("cross-site"),
            Some("127.0.0.1:7770"),
        ));
        assert!(!is_safe_mutation_request(
            Some("application/json"),
            Some("null"),
            Some("cross-site"),
            Some("127.0.0.1:7770"),
        ));
        assert!(!is_safe_mutation_request(
            Some("application/json"),
            Some("http://evil"),
            Some("same-origin"), // forged-ish value: origin mismatch still wins
            Some("127.0.0.1:7770"),
        ));
        // Reject simple form / text/plain POST without application/json
        assert!(!is_safe_mutation_request(
            Some("text/plain"),
            Some("http://127.0.0.1:7770"),
            None,
            Some("127.0.0.1:7770"),
        ));
        // Raw API client: no Origin at all → judged on JSON content-type only.
        assert!(is_safe_mutation_request(
            Some("application/json"),
            None,
            None,
            Some("127.0.0.1:7770"),
        ));
    }

    #[test]
    fn test_preview_mutation_same_origin_gate() {
        // Browser form post from a dev-server preview *inside* MoE Deck: same
        // origin, urlencoded — allowed.
        assert!(is_safe_preview_mutation(
            Some("http://127.0.0.1:7770"),
            Some("same-origin"),
            Some("127.0.0.1:7770"),
        ));
        // Cross-site no-cors POST from evil.com with form-urlencoded body.
        assert!(!is_safe_preview_mutation(
            Some("https://evil.com"),
            Some("cross-site"),
            Some("127.0.0.1:7770"),
        ));
        // Sandbox iframe with Origin: null.
        assert!(!is_safe_preview_mutation(
            Some("null"),
            Some("cross-site"),
            Some("127.0.0.1:7770"),
        ));
        assert!(!is_safe_preview_mutation(
            Some("null"),
            None,
            Some("127.0.0.1:7770"),
        ));
        // Missing host header fails closed.
        assert!(!is_safe_preview_mutation(
            Some("http://127.0.0.1:7770"),
            None,
            None
        ));
    }

    #[test]
    fn test_normalize_host() {
        assert_eq!(
            normalize_host("HTTP://Example.TS.NET:7770").as_deref(),
            Some("example.ts.net")
        );
        assert_eq!(normalize_host("http://[::1]:7770").as_deref(), Some("::1"));
        assert_eq!(
            normalize_host("127.0.0.1:7770").as_deref(),
            Some("127.0.0.1")
        );
        assert_eq!(normalize_host("evil/").as_deref(), Some("evil"));
        assert_eq!(normalize_host("").as_deref(), None);
    }

    #[test]
    fn test_passcode_matches() {
        assert!(passcode_matches("123456", "123456"));
        assert!(!passcode_matches("123456", "123457"));
        assert!(!passcode_matches("123456", "12345"));
        assert!(!passcode_matches("123456", ""));
        assert!(passcode_matches("pepper-onion-42", "pepper-onion-42"));
    }

    #[test]
    fn test_session_token_roundtrip() {
        let cfg = AuthConfig::default();
        let token = issue_session_token(&cfg);
        assert!(verify_session_token(&cfg, &token));
        // Every login gets a unique token (nonce + issue time).
        let token2 = issue_session_token(&cfg);
        assert_ne!(token, token2);
        assert!(verify_session_token(&cfg, &token2));
        // A different secret invalidates the token
        let other = AuthConfig::default();
        assert!(!verify_session_token(&other, &token));
        assert!(!verify_session_token(&cfg, "tampered.token"));
        assert!(!verify_session_token(
            &cfg,
            "helm-authorized-captain.deadbeef"
        ));
        assert!(!verify_session_token(&cfg, ""));
    }

    #[test]
    fn test_session_expiry_and_revocation() {
        let mut cfg = AuthConfig::default();
        // A token issued long before the TTL window must not verify even with
        // a perfect signature.
        let stale_payload = format!("s.{}.abcdefghijkl", now_unix() - SESSION_TTL_SECS - 10);
        let stale_sig = sign_token(&cfg, &stale_payload).unwrap();
        assert!(!verify_session_token(
            &cfg,
            &format!("{}.{}", stale_payload, stale_sig)
        ));

        let token = issue_session_token(&cfg);
        assert!(verify_session_token(&cfg, &token));
        // Mass revocation kills previously issued cookies…
        revoke_all_sessions(&mut cfg);
        assert!(!verify_session_token(&cfg, &token));
        // …but logins after the revocation timestamp work again.
        cfg.revoked_before -= 5;
        assert!(verify_session_token(&cfg, &issue_session_token(&cfg)));
    }

    #[test]
    fn test_auto_generated_passcode() {
        let pc = generate_random_passcode();
        assert_eq!(pc.len(), 8);
        assert!(pc.chars().all(|c| c.is_ascii_hexdigit()));

        let cfg = AuthConfig::default();
        assert!(cfg.passcode.is_some());
        assert_eq!(cfg.passcode.as_ref().unwrap().len(), 8);
    }
}
