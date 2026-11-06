use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{CONTENT_TYPE, LOCATION};
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Extracts target port and rewritten path from an explicit `/preview/<port>/...` URI
/// or from a Referer header when a subresource is requested with an absolute path.
/// `request_host` is the Host header of the incoming request — the Referer
/// fallback only fires when the referer is same-origin with MoE Deck itself, so
/// attacker pages cannot smuggle `/preview/NNNN/` into their own URLs and get
/// arbitrary loopback ports proxied.
pub fn extract_preview_target(
    path: &str,
    referer: Option<&str>,
    request_host: Option<&str>,
    helm_port: u16,
) -> Option<(u16, String)> {
    if let Some(rest) = path.strip_prefix("/preview/") {
        let (port_str, subpath) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };
        if let Ok(port) = port_str.parse::<u16>() {
            if (1024..=65535).contains(&port) && port != helm_port {
                let clean_subpath = if subpath.is_empty() { "/" } else { subpath };
                return Some((port, clean_subpath.to_string()));
            }
        }
    }

    // Fallback: If request is for an asset (e.g. /@vite/client or /src/main.tsx)
    // and the Referer indicates it came from /preview/<port>/... on *this*
    // MoE Deck origin.
    if !path.starts_with("/api/")
        && !path.starts_with("/preview/")
        && !path.starts_with("/static/")
        && path != "/"
        && path != "/manifest.json"
        && path != "/icon.svg"
        && path != "/sw.js"
    {
        if let (Some(ref_str), Some(host)) = (referer, request_host) {
            if let Some((port, _)) = preview_port_from_referer(ref_str, host, helm_port) {
                return Some((port, path.to_string()));
            }
        }
    }

    None
}

/// Parse a referer URL of the form `http(s)://<helm-host>/preview/<port>/…`
/// and return the port, but only when the referer's authority is exactly the
/// host being talked to (same-origin). Rejects `evil.com/preview/9200/` style
/// referers outright.
pub fn preview_port_from_referer(
    referer: &str,
    request_host: &str,
    helm_port: u16,
) -> Option<(u16, String)> {
    let after_scheme = referer
        .strip_prefix("http://")
        .or_else(|| referer.strip_prefix("https://"))?;
    // NB: split_once drops the separator, so use an index slice to keep the
    // leading '/' of the path (strip_prefix("/preview/") below relies on it).
    let slash_idx = after_scheme.find('/')?;
    let authority = &after_scheme[..slash_idx];
    let ref_path = &after_scheme[slash_idx..];
    // Strict origin equality including the port: MoE Deck always lives on an
    // explicit non-default port, and a referer from another port on the same
    // host is not this cockpit's preview page.
    let auth_lc = authority.trim().to_lowercase();
    let host_lc = request_host.trim().to_lowercase();
    if auth_lc.is_empty() || auth_lc != host_lc {
        return None;
    }
    let rest = ref_path.strip_prefix("/preview/")?;
    let port_str = match rest.find('/') {
        Some(slash_idx) => &rest[..slash_idx],
        None => rest,
    };
    let port = port_str.parse::<u16>().ok()?;
    if (1024..=65535).contains(&port) && port != helm_port {
        Some((port, ref_path.to_string()))
    } else {
        None
    }
}

/// Byte-safe, ASCII-case-insensitive tag search that also requires a tag
/// boundary (`>`, whitespace, `/`) after the match — so `<head` never matches
/// inside `<header`, and indices always refer to the *original* string (no
/// `to_lowercase` offset panics on multibyte HTML).
fn find_tag_ci(html: &str, tag: &str) -> Option<usize> {
    let bytes = html.as_bytes();
    let pat = tag.as_bytes();
    if pat.is_empty() || bytes.len() < pat.len() {
        return None;
    }
    let mut i = 0usize;
    while i + pat.len() <= bytes.len() {
        if bytes[i..i + pat.len()].eq_ignore_ascii_case(pat) {
            let next = bytes.get(i + pat.len());
            let boundary = matches!(
                next,
                None | Some(b'>')
                    | Some(b' ')
                    | Some(b'\t')
                    | Some(b'\n')
                    | Some(b'\r')
                    | Some(b'/')
            );
            if boundary {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Injects `<base href="/preview/<port>/">` and the Cockpit telemetry bridge into HTML responses.
pub fn rewrite_html_for_preview(html: &str, port: u16) -> String {
    let base_tag = format!(r#"<base href="/preview/{}/">"#, port);
    let console_bridge = r#"<script>
(function(){
  function sendLog(type, args) {
    try {
      window.parent.postMessage({
        type: 'MOEDECK_PREVIEW_CONSOLE',
        level: type,
        messages: Array.from(args).map(function(a){
          try { return typeof a === 'object' ? JSON.stringify(a) : String(a); } catch(_) { return String(a); }
        })
      }, window.location.origin);
    } catch(_) {}
  }
  var origLog = console.log, origWarn = console.warn, origErr = console.error;
  console.log = function() { sendLog('log', arguments); origLog.apply(console, arguments); };
  console.warn = function() { sendLog('warn', arguments); origWarn.apply(console, arguments); };
  console.error = function() { sendLog('error', arguments); origErr.apply(console, arguments); };
  window.addEventListener('error', function(e) {
    sendLog('error', [e.message + ' at ' + e.filename + ':' + e.lineno]);
  });
})();
</script>"#;

    let injection = format!("{}\n{}", base_tag, console_bridge);

    if let Some(pos) = find_tag_ci(html, "<head") {
        let insert_idx = html[pos..]
            .find('>')
            .map(|i| pos + i + 1)
            .unwrap_or(pos + "<head".len());
        format!(
            "{}\n{}{}",
            &html[..insert_idx],
            injection,
            &html[insert_idx..]
        )
    } else if let Some(pos) = find_tag_ci(html, "<html") {
        let insert_idx = html[pos..]
            .find('>')
            .map(|i| pos + i + 1)
            .unwrap_or(pos + "<html".len());
        format!(
            "{}\n<head>{}</head>{}",
            &html[..insert_idx],
            injection,
            &html[insert_idx..]
        )
    } else {
        format!("{}\n{}", injection, html)
    }
}

/// Rewrites Location redirect header so the browser stays within /preview/<port>/
pub fn rewrite_location(location: &str, port: u16) -> String {
    let prefix = format!("/preview/{}/", port);
    if location.starts_with('/') {
        format!("/preview/{}{}", port, location)
    } else if let Some(stripped) = location
        .strip_prefix(&format!("http://127.0.0.1:{}/", port))
        .or_else(|| location.strip_prefix(&format!("http://localhost:{}/", port)))
    {
        format!("{}{}", prefix, stripped)
    } else if location == format!("http://127.0.0.1:{}", port)
        || location == format!("http://localhost:{}", port)
    {
        format!("/preview/{}/", port)
    } else {
        location.to_string()
    }
}

/// Maximum bytes buffered in either direction for a proxied preview request.
/// Dev-server assets are big (sourcemaps), but this is still a hard ceiling —
/// before this cap the route would buffer unbounded attacker/upstream bodies
/// into RAM.
const MAX_PREVIEW_BODY_BYTES: usize = 32 * 1024 * 1024;
const PREVIEW_BODY_TIMEOUT_SECS: u64 = 60;

/// Proxies an HTTP request directly to the local dev server on 127.0.0.1:<port>
pub async fn forward_to_port(
    req: Request<Incoming>,
    port: u16,
    subpath: &str,
    query: Option<&str>,
) -> Result<Response<Full<Bytes>>, (StatusCode, String)> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let tcp_stream = match timeout(Duration::from_millis(2000), TcpStream::connect(addr)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            return Err((
                StatusCode::BAD_GATEWAY,
                format!("Failed to connect to dev server on port {}: {}", port, e),
            ));
        }
        Err(_) => {
            return Err((
                StatusCode::GATEWAY_TIMEOUT,
                format!("Connection to port {} timed out", port),
            ));
        }
    };

    let io = TokioIo::new(tcp_stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("HTTP handshake failed: {}", e),
            )
        })?;

    tokio::spawn(async move {
        let _ = conn.await;
    });

    let uri_str = if let Some(q) = query {
        format!("{}?{}", subpath, q)
    } else {
        subpath.to_string()
    };
    let uri: Uri = uri_str
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid URI".to_string()))?;

    let (parts, incoming_body) = req.into_parts();
    let body_bytes = timeout(
        Duration::from_secs(PREVIEW_BODY_TIMEOUT_SECS),
        Limited::new(incoming_body, MAX_PREVIEW_BODY_BYTES).collect(),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "Timed out reading request body".to_string(),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Request body rejected (missing, truncated or too large): {}",
                e
            ),
        )
    })?
    .to_bytes();

    let mut out_req = Request::builder().method(parts.method).uri(uri);

    for (k, v) in parts.headers.iter() {
        if k != "host"
            && k != "connection"
            && k != "upgrade"
            && k != "transfer-encoding"
            // Never leak browser credentials into an arbitrary local dev
            // server just because it happens to live on an open port.
            && k.as_str() != "cookie"
            && k.as_str() != "authorization"
        {
            out_req = out_req.header(k, v);
        }
    }
    out_req = out_req.header("host", format!("127.0.0.1:{}", port));

    let fwd_req = out_req.body(Full::new(body_bytes)).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to construct request: {}", e),
        )
    })?;

    // Waiting for response *headers* must also be bounded, or a local service
    // that accepts the TCP connection and stalls pins this task + connection
    // forever (slowloris via preview).
    let upstream_resp = timeout(Duration::from_secs(20), sender.send_request(fwd_req))
        .await
        .map_err(|_| {
            (
                StatusCode::GATEWAY_TIMEOUT,
                format!(
                    "Dev server on port {} did not respond to headers in time",
                    port
                ),
            )
        })?
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("Dev server request failed: {}", e),
            )
        })?;

    let (resp_parts, resp_body) = upstream_resp.into_parts();
    let resp_bytes = timeout(
        Duration::from_secs(PREVIEW_BODY_TIMEOUT_SECS),
        Limited::new(resp_body, MAX_PREVIEW_BODY_BYTES).collect(),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "Dev server response timed out".to_string(),
        )
    })?
    .map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            format!("Failed to read dev server response: {}", e),
        )
    })?
    .to_bytes();

    let is_html = resp_parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/html"))
        .unwrap_or(false);

    let (final_bytes, is_modified) = if is_html {
        if let Ok(html_str) = std::str::from_utf8(&resp_bytes) {
            let modified = rewrite_html_for_preview(html_str, port);
            (Bytes::from(modified), true)
        } else {
            (resp_bytes, false)
        }
    } else {
        (resp_bytes, false)
    };

    let mut client_resp = Response::builder().status(resp_parts.status);

    for (k, v) in resp_parts.headers.iter() {
        if k == LOCATION {
            if let Ok(loc_str) = v.to_str() {
                let rewritten = rewrite_location(loc_str, port);
                if let Ok(new_val) = hyper::header::HeaderValue::from_str(&rewritten) {
                    client_resp = client_resp.header(LOCATION, new_val);
                    continue;
                }
            }
        }

        if is_modified && k == "content-length" {
            continue;
        }

        if k.as_str() == "set-cookie" {
            continue;
        }

        // Previews are same-origin by design (base-href rewriting): letting an
        // upstream `Access-Control-Allow-Origin` through would make internal
        // responses readable by *any* page that embeds the preview through
        // MoE Deck, so CORS widening from dev servers is stripped.
        if k.as_str().starts_with("access-control-allow-") {
            continue;
        }

        if k != "connection" && k != "transfer-encoding" {
            client_resp = client_resp.header(k, v);
        }
    }

    if is_modified {
        client_resp = client_resp.header("content-length", final_bytes.len().to_string());
    }

    client_resp.body(Full::new(final_bytes)).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to assemble response: {}", e),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preview_target() {
        let host = Some("100.80.20.10:7770");
        assert_eq!(
            extract_preview_target("/preview/3000", None, None, 7770),
            Some((3000, "/".to_string()))
        );
        assert_eq!(
            extract_preview_target("/preview/5173/assets/index.js", None, None, 7770),
            Some((5173, "/assets/index.js".to_string()))
        );
        assert_eq!(
            extract_preview_target("/preview/7770/foo", None, None, 7770),
            None
        ); // MoE Deck port ignored
        assert_eq!(
            extract_preview_target("/preview/80/foo", None, None, 7770),
            None
        ); // Privileged port ignored
           // The excluded port is the *configured* one, not a hardcoded 7770.
        assert_eq!(
            extract_preview_target("/preview/8888/x", None, None, 8888),
            None
        );
        assert_eq!(
            extract_preview_target("/preview/8888/x", None, None, 7770),
            Some((8888, "/x".to_string()))
        );

        // Referer fallback: only when the referer is same-origin with MoE Deck.
        let ref_url = "http://100.80.20.10:7770/preview/4173/index.html";
        assert_eq!(
            extract_preview_target("/@vite/client", Some(ref_url), host, 7770),
            Some((4173, "/@vite/client".to_string()))
        );
        assert_eq!(
            extract_preview_target("/api/conversations", Some(ref_url), host, 7770),
            None
        );
        // Same asset path but the referer lives on the attacker's site → out.
        let evil_ref = "https://evil.com/preview/9200/steal";
        assert_eq!(
            extract_preview_target("/_search", Some(evil_ref), host, 7770),
            None
        );
        // Referer on the MoE Deck host but not under /preview/ → no match.
        assert_eq!(
            extract_preview_target(
                "/x",
                Some("http://100.80.20.10:7770/other/4173/"),
                host,
                7770
            ),
            None
        );
        // Referer host with a port different from the request → rejected.
        assert_eq!(
            extract_preview_target(
                "/x",
                Some("http://100.80.20.10:9999/preview/4173/"),
                host,
                7770
            ),
            None
        );
    }

    #[test]
    fn test_rewrite_html_for_preview_multibyte_safe() {
        let html = "<html><head><title>App</title></head><body><h1>Hello</h1></body></html>";
        let out = rewrite_html_for_preview(html, 5173);
        assert!(out.contains(r#"<base href="/preview/5173/">"#));
        assert!(out.contains("MOEDECK_PREVIEW_CONSOLE"));
        assert!(out.contains("<h1>Hello</h1>"));

        // The old to_lowercase-offset slicing panicked on strings like this:
        // multibyte chars before <head> shift byte positions between the
        // lowercased copy and the original. <header> must also not be
        // mistaken for <head>.
        let tricky = "<html><body><header>🧠🚀 Café naïve</header></body></html>";
        let out2 = rewrite_html_for_preview(tricky, 3000);
        assert!(out2.contains(r#"<base href="/preview/3000/">"#));
        assert!(out2.contains("<header>"));
        let with_head_attrs =
            "<!doctype html><html lang=\"fi\"><head id=\"h\"><title>x</title></head></html>";
        let out3 = rewrite_html_for_preview(with_head_attrs, 3000);
        assert!(out3.contains(r#"<head id="h">"#));
        // injection lands *after* the opening head tag, not inside its name
        assert!(out3.find(r#"<head id="h">"#).unwrap() < out3.find(r#"<base href="#).unwrap());
    }

    #[test]
    fn test_rewrite_location() {
        assert_eq!(
            rewrite_location("/dashboard", 3000),
            "/preview/3000/dashboard"
        );
        assert_eq!(
            rewrite_location("http://127.0.0.1:3000/login", 3000),
            "/preview/3000/login"
        );
        assert_eq!(
            rewrite_location("http://localhost:3000", 3000),
            "/preview/3000/"
        );
        assert_eq!(
            rewrite_location("https://github.com/login", 3000),
            "https://github.com/login"
        );
    }

    #[test]
    fn test_rewrite_html_for_preview() {
        let html = "<html><head><title>App</title></head><body><h1>Hello</h1></body></html>";
        let out = rewrite_html_for_preview(html, 5173);
        assert!(out.contains(r#"<base href="/preview/5173/">"#));
        assert!(out.contains("MOEDECK_PREVIEW_CONSOLE"));
        assert!(out.contains("<h1>Hello</h1>"));
    }
}
