pub const MANIFEST_JSON: &str = r##"{
  "name": "MoE Deck — Multi-Harness Agent Gateway",
  "short_name": "MoE Deck",
  "description": "Mobile-first multi-harness command UI for Claude Code, Antigravity, Codex, and OpenCode",
  "id": "/",
  "scope": "/",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#0d0d0d",
  "theme_color": "#0d0d0d",
  "orientation": "any",
  "icons": [
    { "src": "/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any" },
    { "src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any" },
    { "src": "/icon-512-maskable.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" },
    { "src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any" }
  ]
}"##;

pub const ICON_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <rect width="256" height="256" rx="60" fill="#141413"/>
  <circle cx="128" cy="128" r="76" fill="none" stroke="#D97757" stroke-width="14" stroke-dasharray="120 360" stroke-linecap="round"/>
  <circle cx="128" cy="128" r="76" fill="none" stroke="#4F8FF7" stroke-width="14" stroke-dasharray="120 360" stroke-dashoffset="-120" stroke-linecap="round"/>
  <circle cx="128" cy="128" r="76" fill="none" stroke="#10A37F" stroke-width="14" stroke-dasharray="120 360" stroke-dashoffset="-240" stroke-linecap="round"/>
  <circle cx="128" cy="128" r="26" fill="#F4F4F6"/>
</svg>"##;

// Pre-rendered PNG variants of the logo (see assets/icons/make_icons.py).
// Apple home-screen icons must be PNG; SVGs are ignored by iOS.
pub const ICON_180_PNG: &[u8] = include_bytes!("../assets/icons/icon-180.png");
pub const ICON_192_PNG: &[u8] = include_bytes!("../assets/icons/icon-192.png");
pub const ICON_512_PNG: &[u8] = include_bytes!("../assets/icons/icon-512.png");
pub const ICON_512_MASKABLE_PNG: &[u8] = include_bytes!("../assets/icons/icon-512-maskable.png");

pub const SW_JS: &str = r##"
const CACHE = 'helm-shell-__VER__';
const SHELL = [
  '/',
  '/manifest.json',
  '/icon.svg',
  '/icon-180.png',
  '/icon-192.png',
  '/icon-512.png',
  '/icon-512-maskable.png',
];

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches.open(CACHE).then((c) => c.addAll(SHELL)).catch(() => {})
  );
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith('helm-shell-') && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim())
  );
});

self.addEventListener('fetch', (event) => {
  const req = event.request;
  if (req.method !== 'GET') return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  // Live data always goes to the network.
  if (url.pathname.startsWith('/api/')) return;

  if (req.mode === 'navigate') {
    event.respondWith(
      fetch(req)
        .then((res) => {
          const copy = res.clone();
          if (res.ok) caches.open(CACHE).then((c) => c.put(req.mode === 'navigate' ? '/' : req, copy)).catch(() => {});
          return res;
        })
        .catch(() => caches.match(req).then((r) => r || caches.match('/')))
    );
    return;
  }

  // Static shell assets: serve from cache, refresh in the background.
  event.respondWith(
    caches.match(req).then((cached) => {
      const network = fetch(req)
        .then((res) => {
          const copy = res.clone();
          if (res.ok) caches.open(CACHE).then((c) => c.put(req.mode === 'navigate' ? '/' : req, copy)).catch(() => {});
          return res;
        })
        .catch(() => cached);
      return cached || network;
    })
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const convId = event.notification.data && event.notification.data.conversationId;
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((clientList) => {
      for (const client of clientList) {
        if ('focus' in client) {
          client.postMessage({ type: 'OPEN_CONVERSATION', conversationId: convId });
          return client.focus();
        }
      }
      if (self.clients.openWindow) {
        return self.clients.openWindow('/');
      }
    })
  );
});
"##;

/// Service worker script with the shell-cache name versioned by a hash of the
/// embedded UI. When the UI changes in a build, the hash changes and the
/// browser installs a fresh SW, which purges the old cache on `activate` —
/// instead of every client keeping a stale shell cached forever.
pub fn sw_js() -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for b in INDEX_HTML.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    SW_JS.replace("__VER__", &format!("v{hash}"))
}

pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="en" class="dark">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover, interactive-widget=resizes-content" />
  <meta name="theme-color" content="#0d0d0d" />
  <meta name="apple-mobile-web-app-capable" content="yes" />
  <meta name="apple-mobile-web-app-status-bar-style" content="black-translucent" />
  <meta name="apple-mobile-web-app-title" content="MoE Deck" />
  <link rel="manifest" href="/manifest.json" />
  <link rel="icon" type="image/svg+xml" href="/icon.svg" />
  <link rel="apple-touch-icon" sizes="180x180" href="/icon-180.png" />
  <title>MoE Deck</title>
  <style>
    :root {
      --bg: #0d0d0d;
      --surface: #1c1c1e;
      --surface-2: #29292b;
      --border: rgba(255, 255, 255, 0.08);
      --border-strong: rgba(255, 255, 255, 0.13);
      --text: #ececeb;
      --text-muted: #aaa9ae;
      --text-dim: #919096;
      --accent: #D97757;
      --accent-soft: rgba(217, 119, 87, 0.16);
      --safe-top: env(safe-area-inset-top, 0px);
      --safe-bottom: env(safe-area-inset-bottom, 0px);
    }

    * {
      box-sizing: border-box;
      -webkit-tap-highlight-color: transparent;
    }

    html, body {
      margin: 0;
      padding: 0;
      width: 100%;
      height: 100%;
      background: var(--bg);
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "Inter", system-ui, sans-serif;
      font-size: 14.5px;
      line-height: 1.48;
      overflow: hidden;
      overscroll-behavior: none;
      -webkit-font-smoothing: antialiased;
    }

    .app-shell {
      display: flex;
      flex-direction: column;
      height: var(--vvh, 100dvh);
      max-width: 720px;
      margin: 0 auto;
      position: relative;
      /* iOS scrolls the page when the keyboard opens; follow the visible area */
      top: var(--vv-top, 0px);
      background: var(--bg);
    }

    /* Top Header matching reference screenshot */
    .top-header {
      padding-top: calc(var(--safe-top) + 12px);
      padding-left: 14px;
      padding-right: 14px;
      padding-bottom: 12px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
      background: rgba(13,13,13,0.88);
      backdrop-filter: blur(16px);
      -webkit-backdrop-filter: blur(16px);
      z-index: 20;
    }

    .circle-btn {
      width: 44px;
      height: 44px;
      border-radius: 999px;
      background: linear-gradient(155deg, #262628, #19191b);
      border: 1px solid var(--border);
      color: var(--text);
      display: inline-flex;
      align-items: center;
      justify-content: center;
      cursor: pointer;
      flex-shrink: 0;
      transition: transform 0.12s ease;
    }
    .circle-btn:active {
      transform: scale(0.93);
    }

    .header-title-wrap {
      border: 0;
      padding: 0;
      background: transparent;
      color: inherit;
      flex: 1;
      min-width: 0;
      text-align: center;
      cursor: pointer;
      user-select: none;
      padding: 0 4px;
    }
    .header-title {
      font-size: 16px;
      font-weight: 640;
      color: var(--text);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
      letter-spacing: -0.01em;
      line-height: 1.25;
    }
    .header-subtitle {
      font-size: 11.5px;
      color: var(--text-muted);
      display: flex;
      align-items: center;
      justify-content: center;
      gap: 5px;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
      margin-top: 1px;
    }

    .header-right-pill {
      height: 44px;
      padding: 0 12px;
      border-radius: 999px;
      background: var(--surface);
      border: 1px solid var(--border);
      display: inline-flex;
      align-items: center;
      gap: 8px;
      color: var(--text-muted);
      font-size: 12px;
      font-weight: 600;
      cursor: pointer;
      flex-shrink: 0;
      transition: transform 0.12s ease;
    }
    .header-right-pill:active {
      transform: scale(0.95);
    }

    /* Subtle Quick-Bounce Strip */
    .bounce-strip {
      display: flex;
      align-items: center;
      gap: 6px;
      padding: 2px 14px 8px 14px;
      overflow-x: auto;
      scrollbar-width: none;
      border-bottom: 1px solid rgba(255,255,255,0.05);
      background: rgba(20, 20, 19, 0.92);
      z-index: 19;
    }
    .bounce-strip::-webkit-scrollbar {
      display: none;
    }

    .bounce-tab {
      display: inline-flex;
      align-items: center;
      gap: 5px;
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 12px;
      font-weight: 540;
      background: rgba(255,255,255,0.03);
      border: 1px solid rgba(255,255,255,0.06);
      color: var(--text-muted);
      white-space: nowrap;
      cursor: pointer;
      flex-shrink: 0;
    }
    .bounce-tab.active {
      background: var(--accent-soft);
      border-color: var(--accent);
      color: var(--text);
    }
    .bounce-dot {
      width: 6.5px;
      height: 6.5px;
      border-radius: 50%;
      flex-shrink: 0;
    }
    .bounce-dot.busy {
      animation: pulseDot 1.3s infinite ease-in-out;
    }
    .harness-tag-mini {
      font-size: 9.5px;
      font-weight: 700;
      text-transform: uppercase;
      letter-spacing: 0.03em;
    }

    @keyframes pulseDot {
      0%, 100% { transform: scale(1); opacity: 1; }
      50% { transform: scale(1.45); opacity: 0.45; }
    }

    /* Chat Canvas */
    .chat-scroll {
      flex: 1;
      overflow-y: auto;
      padding: 24px 20px 28px;
      scroll-behavior: smooth;
      -webkit-overflow-scrolling: touch;
    }

    .msg-block {
      margin-bottom: 28px;
    }

    .msg-user {
      display: flex;
      flex-direction: column;
      align-items: flex-end;
    }
    .msg-user-bubble {
      max-width: 90%;
      background: #242426;
      border: none;
      color: var(--text);
      padding: 12px 16px;
      border-radius: 22px;
      font-size: 16px;
      line-height: 1.5;
      white-space: pre-wrap;
      word-break: break-word;
    }

    .msg-assistant {
      color: var(--text);
    }

    /* Tool calls collapsible pill */
    .tool-pill {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 8px 0;
      border-radius: 8px;
      background: transparent;
      border: none;
      color: var(--text-muted);
      font-size: 13px;
      margin-bottom: 8px;
      cursor: pointer;
      user-select: none;
      min-height: 44px;
    }
    .tool-list {
      margin-top: 4px;
      margin-bottom: 10px;
      padding: 8px 11px;
      border-radius: 10px;
      background: #191918;
      border: 1px solid rgba(255,255,255,0.06);
      font-family: "SF Mono", "JetBrains Mono", ui-monospace, monospace;
      font-size: 11.5px;
      color: var(--text-muted);
      display: none;
    }
    .tool-list.open {
      display: block;
    }
    .tool-item-row {
      padding: 2.5px 0;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    /* Thinking / Thought pattern accordion */
    .thinking-pill {
      display: inline-flex;
      align-items: center;
      gap: 6px;
      padding: 3px 9px;
      margin-bottom: 8px;
      font-size: 11.5px;
      font-weight: 520;
      color: var(--text-dim);
      background: rgba(255,255,255,0.035);
      border: 1px solid rgba(255,255,255,0.06);
      border-radius: 12px;
      cursor: pointer;
      user-select: none;
      transition: background 0.15s, color 0.15s;
    }
    .thinking-pill:hover {
      color: var(--text);
      background: rgba(255,255,255,0.06);
    }
    .thinking-chevron {
      font-size: 9.5px;
      transition: transform 0.18s ease;
      display: inline-block;
    }
    .thinking-pill.open .thinking-chevron {
      transform: rotate(180deg);
    }
    .thinking-drawer {
      display: none;
      margin-bottom: 10px;
      padding: 8px 12px;
      background: rgba(0, 0, 0, 0.28);
      border-left: 2.5px solid var(--border-strong);
      border-radius: 0 8px 8px 0;
      font-size: 12px;
      line-height: 1.5;
      color: var(--text-muted);
      max-height: 280px;
      overflow-y: auto;
    }
    .thinking-drawer.open {
      display: block;
    }

    /* Editorial Prose matching reference screenshot */
    .prose {
      font-size: 16px;
      line-height: 1.65;
      color: #e9e9eb;
      word-break: break-word;
    }
    .prose p {
      margin: 0 0 16px;
    }
    .prose p:last-child {
      margin-bottom: 0;
    }
    .prose h2, .prose h3 {
      font-size: 18px;
      font-weight: 650;
      color: #ffffff;
      margin: 22px 0 10px;
      letter-spacing: -0.01em;
    }
    .prose h2:first-child, .prose h3:first-child {
      margin-top: 0;
    }
    .prose ul, .prose ol {
      margin: 4px 0 12px 0;
      padding-left: 18px;
    }
    .prose li {
      margin-bottom: 8px;
      line-height: 1.6;
    }
    .prose strong {
      color: #ffffff;
      font-weight: 640;
    }
    .prose code {
      font-family: "SF Mono", "JetBrains Mono", ui-monospace, monospace;
      font-size: 12.5px;
      background: rgba(255,255,255,0.075);
      color: #f2f2f0;
      padding: 1px 5px;
      border-radius: 4px;
      word-break: break-word;
    }
    .prose pre {
      background: #1a1a18;
      border: 1px solid var(--border);
      border-radius: 10px;
      padding: 10px 12px;
      overflow-x: auto;
      margin: 8px 0 12px 0;
    }
    .prose pre code {
      background: none;
      padding: 0;
      font-size: 12px;
    }
    .prose a {
      color: var(--accent);
      text-decoration: underline;
      text-underline-offset: 2px;
    }

    /* Terminal pane: a turn drawn in the order it happened, like a CLI agent */
    :root { --mono: "JetBrains Mono", "SF Mono", ui-monospace, Menlo, Consolas, monospace; --t-ok: #4ade80; --t-err: #f87171; --t-run: #facc15; --t-dim: #8b8b8b; }
    .chat-scroll { font-family: var(--mono); font-size: 13.5px; line-height: 1.55; padding: 16px 14px 20px; }
    .chat-scroll .prose { font-size: 13.5px; line-height: 1.55; color: #e6e6e6; }
    .chat-scroll .prose p { margin: 0 0 8px; }
    .chat-scroll .prose h2, .chat-scroll .prose h3 { font-size: 13.5px; font-weight: 700; margin: 12px 0 6px; color: #fff; }
    .chat-scroll .prose h2::before { content: "\0023\0023  "; color: var(--t-dim); }
    .chat-scroll .prose h3::before { content: "\0023\0023\0023  "; color: var(--t-dim); }
    .chat-scroll .prose ul, .chat-scroll .prose ol { margin: 2px 0 8px; padding-left: 20px; }
    .chat-scroll .prose li { margin-bottom: 2px; line-height: 1.55; }
    .chat-scroll .prose code { font-size: inherit; background: none; color: #93c5fd; padding: 0; }
    .chat-scroll .prose pre { background: #111; border-radius: 4px; padding: 8px 10px; }
    .chat-scroll .prose pre code { color: #e6e6e6; }
    .t-turn { margin-bottom: 18px; }
    .t-user { display: flex; gap: 8px; padding: 6px 10px; margin: 0 0 14px; background: #1d1d1d; border-left: 2px solid var(--t-dim); white-space: pre-wrap; word-break: break-word; color: #f2f2f2; }
    .t-user .t-glyph { color: var(--t-dim); }
    .t-line { display: flex; gap: 8px; margin: 0 0 6px; min-width: 0; }
    .t-glyph { flex: 0 0 auto; width: 1ch; color: #e6e6e6; user-select: none; }
    .t-body { flex: 1; min-width: 0; overflow-wrap: anywhere; }
    .t-tool, .t-think { cursor: pointer; }
    .t-tool .t-glyph { color: var(--t-ok); }
    .t-tool.error .t-glyph { color: var(--t-err); }
    .t-tool.running .t-glyph, .t-tool.waiting_approval .t-glyph { color: var(--t-run); animation: pulseDot 1.2s infinite; }
    .t-tool.denied .t-glyph { color: var(--t-dim); }
    .t-tool-name { font-weight: 700; color: #f2f2f2; }
    .t-tool-arg { color: var(--t-dim); }
    .t-tool.waiting_approval .t-tool-arg::after { content: "  awaiting approval"; color: var(--t-run); }
    .t-tool.denied .t-tool-arg::after { content: "  denied"; color: var(--t-err); }
    .t-peek { color: var(--t-dim); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
    .t-peek::before { content: "⎿  "; }
    .t-detail { display: none; margin-top: 4px; }
    .t-line.open .t-detail { display: block; }
    .t-line.open .t-peek { display: none; }
    .t-detail pre { margin: 0 0 6px; padding: 6px 8px; max-height: 320px; overflow: auto; background: #111; border-left: 2px solid #333; color: #cfcfcf; font: inherit; font-size: 12px; white-space: pre-wrap; word-break: break-word; }
    .t-detail .t-label { color: var(--t-dim); font-size: 11.5px; }
    .t-tool.error .t-detail pre.t-out { border-left-color: var(--t-err); }
    .t-think .t-glyph, .t-think .t-body { color: var(--t-dim); font-style: italic; }
    .t-think .t-detail { white-space: pre-wrap; font-size: 12.5px; }
    .t-working { color: var(--t-dim); }
    .t-working .t-glyph { color: var(--accent); animation: pulseDot 1.2s infinite; }
    .t-meta { display: flex; align-items: center; gap: 10px; color: var(--t-dim); font-size: 11.5px; margin-top: 2px; }
    .t-meta button { background: none; border: 0; padding: 4px 0; color: var(--t-dim); font: inherit; cursor: pointer; }
    .t-meta button:hover { color: #e6e6e6; }

    /* Multiplex panes (tmux/btop look). The focused pane receives the composer. */
    .app-shell { max-width: none; }
    .bounce-strip { display: none; }
    .pane-grid { touch-action: pan-y; flex: 1; min-height: 0; display: grid; gap: 14px 10px; padding: 12px 10px 4px; grid-template: 1fr / 1fr; }
    .pane-grid[data-count="2"] { grid-template: 1fr / 1fr 1fr; }
    .pane-grid[data-count="3"] { grid-template: 1fr 1fr / 1fr 1fr; }
    .pane-grid[data-count="3"] > .pane:first-child { grid-row: span 2; }
    .pane-grid[data-count="4"] { grid-template: 1fr 1fr / 1fr 1fr; }
    .pane-grid[data-count="5"], .pane-grid[data-count="6"] { grid-template: 1fr 1fr / 1fr 1fr 1fr; }
    .pane { position: relative; display: flex; flex-direction: column; min-width: 0; min-height: 0; padding-top: 0.6em; border: 1px solid #3a3a3a; border-radius: 6px; background: var(--bg); }
    .pane.focused { border-color: var(--t-ok); }
    .pane.busy:not(.focused) { border-color: #6b5d1f; }
    .pane-head { position: absolute; top: -0.8em; left: 8px; right: 8px; display: flex; align-items: center; gap: 6px; font: 12px/1.4 var(--mono); color: var(--t-dim); pointer-events: none; z-index: 1; }
    .pane-head > * { background: var(--bg); padding: 0 4px; pointer-events: auto; }
    .pane-label { display: flex; gap: 6px; min-width: 0; overflow: hidden; white-space: nowrap; cursor: grab; border: 0; color: inherit; font: inherit; }
    .pane-label b { font-weight: 700; }
    .pane-label .pane-name { color: #e6e6e6; overflow: hidden; text-overflow: ellipsis; }
    .pane.focused .pane-idx { color: var(--t-ok); }
    .pane-spacer { flex: 1; background: none; pointer-events: none; }
    .pane-btn { border: 0; color: var(--t-dim); font: inherit; cursor: pointer; min-width: 26px; }
    .pane-btn:hover { color: #fff; }
    .pane .chat-scroll, .pane-scroll { flex: 1; min-height: 0; overflow-y: auto; padding: 14px 12px 12px; -webkit-mask-image: linear-gradient(transparent, #000 14px); mask-image: linear-gradient(transparent, #000 14px); font-family: var(--mono); font-size: var(--term-font, 13.5px); line-height: 1.55; }
    .pane-scroll { cursor: pointer; }
    .pane-grid pre, .pane-grid .msg-images { touch-action: pan-x pan-y; }
    .pane .chat-scroll .prose, .pane-scroll .prose { font-size: var(--term-font, 13.5px); }
    .pane.drop-target { border-style: dashed; border-color: var(--t-run); }
    .tmux-bar { display: flex; align-items: center; gap: 2px; margin: 4px 10px 6px; background: #4caf50; color: #0b0b0b; font: 12.5px/1.6 var(--mono); white-space: nowrap; overflow-x: auto; scrollbar-width: none; border-radius: 2px; }
    .tmux-bar::-webkit-scrollbar { display: none; }
    .tmux-bar button { border: 0; background: none; color: inherit; font: inherit; padding: 4px 7px; min-height: 32px; cursor: pointer; }
    .tmux-bar .tmux-win.active { background: #0b0b0b; color: #4caf50; font-weight: 700; }
    .tmux-bar .tmux-win.busy::after { content: " ●"; }
    .tmux-bar .tmux-win.approval { background: #ef4444; color: #fff; }
    .tmux-bar .tmux-fill { flex: 1; }
    .tmux-bar .tmux-clock { padding: 0 8px; }
    /* Minimal look (default): no window bar, and a lone pane drops its frame. */
    .tmux-bar[hidden] { display: none; }
    .app-shell.minimal .pane-grid[data-count="1"] > .pane { border-color: transparent; }
    .app-shell.minimal .pane-grid[data-count="1"] > .pane > .pane-head { display: none; }
    .bottom-dock { width: 100%; max-width: 980px; margin: 0 auto; }
    .pane-grid.swipe-next > .pane { animation: paneIn 0.18s ease-out; }
    .pane-grid.swipe-prev > .pane { animation: paneIn 0.18s ease-out reverse; }
    @keyframes paneIn { from { transform: translateX(24px); opacity: 0.4; } to { transform: none; opacity: 1; } }
    .quick-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 12px 2px; border-bottom: 1px solid var(--border); }
    .quick-row:last-child { border-bottom: 0; }
    .quick-choices { display: flex; gap: 6px; }
    .quick-choices .filter-chip { min-height: 40px; padding: 4px 12px; }
    @media (max-width: 759px) {
      .pane-grid { padding: 12px 6px 2px; }
      .pane-grid[data-count] { grid-template: 1fr / 1fr; }
      .pane-grid > .pane:first-child { grid-row: auto; }
      .tmux-bar { margin: 2px 6px 4px; }
      .tmux-bar .tmux-clock, .tmux-bar .tmux-mode { display: none; }
    }

    /* Image attachments & inspected renders */
    .msg-images {
      display: flex;
      gap: 8px;
      overflow-x: auto;
      margin: 8px 0;
      padding-bottom: 2px;
    }
    .msg-img-card {
      max-height: 190px;
      border-radius: 12px;
      border: 1px solid var(--border-strong);
      cursor: pointer;
      object-fit: cover;
    }

    /* Message action icons (Copy + Play + Harness badge) */
    .msg-actions {
      display: flex;
      align-items: center;
      gap: 12px;
      margin-top: 8px;
      color: var(--text-muted);
    }
    .msg-action-btn {
      background: none;
      border: none;
      color: var(--text-muted);
      padding: 3px;
      cursor: pointer;
      display: inline-flex;
      align-items: center;
    }
    .msg-meta-badge {
      font-size: 11px;
      color: var(--text-dim);
      margin-left: auto;
      display: inline-flex;
      align-items: center;
      gap: 4px;
    }

    /* Bottom Dock: Running Task line + Composer Card (tightened mobile spacing) */
    .bottom-dock {
      padding: 4px 12px max(var(--safe-bottom), 12px);
      background: linear-gradient(to top, var(--bg) 75%, transparent);
      z-index: 20;
    }

    /* Interactive Remote Push Approvals (HITL) */
    .approval-dock {
      background: #231212;
      border: 1.5px solid #ef4444;
      border-radius: 16px;
      padding: 10px 12px;
      margin-bottom: 8px;
      box-shadow: 0 0 24px rgba(239, 68, 68, 0.28);
      animation: pulseApproval 2.4s infinite ease-in-out;
    }
    .approval-dock.hidden {
      display: none;
    }
    .approval-dock.risk-critical {
      background: #241111;
      border-color: #ef4444;
      box-shadow: 0 0 24px rgba(239, 68, 68, 0.35);
    }
    .approval-dock.risk-high {
      background: #231711;
      border-color: #f97316;
      box-shadow: 0 0 20px rgba(249, 115, 22, 0.25);
    }
    .approval-dock.risk-medium {
      background: #201e14;
      border-color: #eab308;
      box-shadow: 0 0 18px rgba(234, 179, 8, 0.2);
    }
    @keyframes pulseApproval {
      0%, 100% { box-shadow: 0 0 16px rgba(239,68,68,0.2); }
      50% { box-shadow: 0 0 28px rgba(239,68,68,0.45); }
    }

    .running-task-bar {
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 2px 6px 6px 6px;
      font-size: 12px;
      font-weight: 540;
      color: #5899ff;
      cursor: pointer;
      user-select: none;
    }
    .running-task-bar.hidden {
      display: none;
    }

    .composer-card {
      background: linear-gradient(155deg, #252527, #1b1b1d);
      border: 1px solid var(--border-strong);
      border-radius: 28px;
      padding: 12px 10px 8px;
      box-shadow: 0 8px 32px rgba(0,0,0,.24);
      position: relative;
    }

    .upload-preview-strip {
      display: flex;
      gap: 8px;
      margin-bottom: 8px;
      overflow-x: auto;
    }
    .upload-preview-strip.hidden {
      display: none;
    }
    .upload-thumb-wrap {
      position: relative;
      width: 52px;
      height: 52px;
      border-radius: 9px;
      overflow: hidden;
      border: 1px solid var(--border-strong);
      flex-shrink: 0;
    }
    .upload-thumb-wrap img {
      width: 100%;
      height: 100%;
      object-fit: cover;
    }
    .upload-remove-btn {
      position: absolute;
      top: 2px;
      right: 2px;
      width: 17px;
      height: 17px;
      border-radius: 50%;
      background: rgba(0,0,0,0.75);
      color: #fff;
      border: none;
      font-size: 11px;
      cursor: pointer;
    }

    .composer-input {
      width: 100%;
      background: transparent;
      border: none;
      outline: none;
      color: var(--text);
      font-family: inherit;
      font-size: 16px;
      line-height: 1.5;
      resize: none;
      max-height: 120px;
      min-height: 24px;
      padding: 3px 8px;
      margin-bottom: 8px;
    }
    .composer-input::placeholder {
      color: var(--text-dim);
    }

    /* 2-tier ergonomic mobile composer layout */
    .composer-meta-row {
      display: flex;
      align-items: center;
      gap: 4px;
      margin-bottom: 0;
      padding-bottom: 0;
      border-bottom: none;
      overflow-x: auto;
      scrollbar-width: none;
      -webkit-overflow-scrolling: touch;
      flex: 1;
      min-width: 0;
    }
    .composer-meta-row::-webkit-scrollbar {
      display: none;
    }

    .composer-controls {
      display: flex;
      align-items: center;
      gap: 4px;
      width: 100%;
      min-width: 0;
      margin-top: 0;
    }

    .composer-icon-btn {
      width: 40px;
      height: 44px;
      border-radius: 50%;
      background: transparent;
      border: none;
      color: #d0cfd4;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      cursor: pointer;
      flex-shrink: 0;
      touch-action: manipulation;
      transition: transform 0.12s ease, background 0.15s ease;
    }
    .composer-icon-btn:active {
      color: var(--text);
      transform: scale(0.92);
      background: rgba(255, 255, 255, 0.12);
    }

    .selector-pill {
      height: 44px;
      padding: 0 7px;
      border-radius: 999px;
      background: transparent;
      border: none;
      color: var(--text);
      font-size: 11.5px;
      font-weight: 560;
      display: inline-flex;
      align-items: center;
      gap: 5px;
      cursor: pointer;
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
      max-width: 142px;
      touch-action: manipulation;
      min-width: 0;
    }
    .selector-pill.harness-pill {
      border-color: transparent;
      background: transparent;
      flex-shrink: 0;
    }

    .composer-spacer {
      flex: 1;
      min-width: 4px;
      display: none;
    }

    .send-btn {
      width: 36px;
      height: 36px;
      border-radius: 50%;
      background: var(--accent);
      border: none;
      color: #ffffff;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      cursor: pointer;
      flex-shrink: 0;
      touch-action: manipulation;
      transition: transform 0.12s ease, filter 0.15s ease;
    }
    .send-btn:active {
      transform: scale(0.92);
      filter: brightness(1.15);
    }
    button:disabled { opacity: .5; cursor: wait; }
    .project-form { display: grid; gap: 16px; }
    .project-form[hidden] { display: none; }
    .project-form label { display: grid; gap: 8px; font-size: 14px; }
    .project-form input, .project-form select { width: 100%; min-width: 0; min-height: 48px; padding: 12px; border: 1px solid var(--border-strong); border-radius: 12px; background: #242426; color: var(--text); font: inherit; font-size: 16px; }
    .project-form [role=alert] { color: #fca5a5; font-size: 14px; }
    .rc-console { background: #0d0d0d; color: #e8e8e8; border-radius: 12px; padding: 12px; max-height: 240px; overflow: auto; white-space: pre-wrap; overflow-wrap: anywhere; font: 12px/1.5 ui-monospace, monospace; }
    .rc-actions { display: flex; gap: 8px; flex-wrap: wrap; margin-top: 12px; }
    .rc-link { display: block; padding: 14px; border-radius: 12px; background: #d97757; color: #fff; text-align: center; text-decoration: none; font-weight: 650; margin: 12px 0; }
    .rc-link[hidden] { display: none; }

    /* Slash Command Popup */
    .slash-menu {
      position: absolute;
      left: 8px;
      right: 8px;
      bottom: calc(100% + 8px);
      background: #20201e;
      border: 1px solid var(--border-strong);
      border-radius: 16px;
      padding: 6px;
      box-shadow: 0 12px 32px rgba(0,0,0,0.65);
      display: none;
      z-index: 40;
    }
    .slash-menu.open {
      display: block;
    }
    .slash-item {
      padding: 8px 11px;
      border-radius: 10px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      cursor: pointer;
      font-size: 13px;
    }
    .slash-item:active {
      background: rgba(255,255,255,0.08);
    }
    .slash-cmd {
      font-weight: 650;
      color: var(--text);
    }
    .slash-desc {
      color: var(--text-muted);
      font-size: 11.5px;
    }

    /* Slide-over Drawer & Bottom Sheets */
    .backdrop {
      position: fixed;
      inset: 0;
      background: rgba(0,0,0,0.6);
      backdrop-filter: blur(3px);
      -webkit-backdrop-filter: blur(3px);
      z-index: 50;
      opacity: 0;
      pointer-events: none;
      transition: opacity 0.2s ease;
    }
    .backdrop.open {
      opacity: 1;
      pointer-events: auto;
    }

    .drawer {
      position: fixed;
      top: 0;
      left: 0;
      bottom: 0;
      width: min(88vw, 360px);
      background: #101011;
      border-right: none;
      z-index: 60;
      transform: translateX(-102%);
      transition: transform 0.24s cubic-bezier(0.22, 1, 0.36, 1);
      display: flex;
      flex-direction: column;
      padding-top: calc(var(--safe-top) + 12px);
      padding-bottom: calc(var(--safe-bottom) + 12px);
      border-radius: 0 28px 28px 0;
    }
    .drawer.open {
      transform: translateX(0);
    }

    .drawer-header {
      padding: 8px 20px 20px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      border-bottom: none;
    }
    .drawer-brand {
      font-size: 24px;
      font-weight: 700;
      display: flex;
      align-items: center;
      gap: 8px;
      letter-spacing: -.04em;
    }
    .new-thread-btn {
      background: var(--text);
      color: #141413;
      border: none;
      border-radius: 999px;
      padding: 6px 12px;
      font-size: 12.5px;
      font-weight: 650;
      cursor: pointer;
    }

    .drawer-filters {
      display: flex;
      gap: 6px;
      padding: 10px 14px;
      overflow-x: auto;
    }
    .filter-chip {
      padding: 4px 10px;
      border-radius: 999px;
      font-size: 12px;
      background: var(--surface);
      border: 1px solid var(--border);
      color: var(--text-muted);
      cursor: pointer;
    }
    .filter-chip.active {
      background: var(--text);
      color: #141413;
      font-weight: 600;
    }

    .conv-list {
      flex: 1;
      overflow-y: auto;
      padding: 4px 10px;
    }
    .conv-item { -webkit-touch-callout:none; -webkit-user-select:none; user-select:none; }
    .drawer-archived-toggle { display:block; width:100%; margin:6px 0 10px; padding:12px 14px; background:none; border:1px solid var(--border); border-radius:12px; color:var(--text-muted); font-size:13px; text-align:left; }
    .conv-item {
      padding: 14px 12px;
      border-radius: 12px;
      margin-bottom: 4px;
      cursor: pointer;
      display: flex;
      flex-direction: column;
      gap: 6px;
      border: none;
    }
    .conv-item.active {
      background: var(--surface);
      border-color: var(--border-strong);
    }
    .conv-item-top {
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 8px;
    }
    .conv-item-title {
      font-size: 15px;
      font-weight: 520;
      color: var(--text);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }
    .conv-item-preview {
      font-size: 12px;
      color: var(--text-muted);
      white-space: nowrap;
      overflow: hidden;
      text-overflow: ellipsis;
    }

    /* Bottom Sheet */
    .bottom-sheet {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      max-width: 720px;
      margin: 0 auto;
      background: #171719;
      border-top: 1px solid var(--border-strong);
      border-radius: 30px 30px 0 0;
      padding: 14px 20px calc(var(--safe-bottom) + 24px);
      z-index: 60;
      transform: translateY(105%);
      transition: transform 0.22s cubic-bezier(0.22, 1, 0.36, 1);
      max-height: 85dvh;
      overflow-y: auto;
    }
    .bottom-sheet.open {
      transform: translateY(0);
    }
    .sheet-handle {
      width: 36px;
      height: 4px;
      border-radius: 99px;
      background: rgba(255,255,255,0.2);
      margin: 0 auto 12px auto;
    }
    .sheet-title {
      font-size: 19px;
      font-weight: 650;
      margin-bottom: 8px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 14px 40px 16px 2px;
      min-height: 56px;
    }
    .sheet-option {
      padding: 18px 16px;
      border-radius: 16px;
      background: #242426;
      border: 1px solid transparent;
      margin-bottom: 4px;
      display: flex;
      align-items: center;
      justify-content: space-between;
      cursor: pointer;
      min-height: 64px;
      font-size: 15px;
      gap: 12px;
    }
    .sheet-option.selected {
      border-color: transparent;
      background: #2b2b2e;
    }
    .effort-grid {
      display: grid;
      grid-template-columns: repeat(5, 1fr);
      gap: 6px;
      margin-top: 8px;
    }
    .effort-btn {
      padding: 12px 4px;
      border-radius: 12px;
      background: #252522;
      border: 1px solid var(--border);
      color: var(--text-muted);
      font-size: 12px;
      font-weight: 600;
      text-align: center;
      cursor: pointer;
      text-transform: capitalize;
      min-height: 44px;
    }
    .effort-btn.selected {
      background: var(--accent);
      color: #fff;
      border-color: var(--accent);
    }

    /* Fullscreen Lightbox */
    .lightbox {
      position: fixed;
      inset: 0;
      background: rgba(0,0,0,0.92);
      z-index: 90;
      display: none;
      align-items: center;
      justify-content: center;
      padding: 14px;
    }
    .lightbox.open {
      display: flex;
    }
    .lightbox img {
      max-width: 100%;
      max-height: 90dvh;
      border-radius: 10px;
    }

    .sheet-option { width: 100%; color: var(--text); text-align: left; font-family: inherit; }
    .conv-list { min-height: 0; }
    .sheet-close { position: absolute; right: 14px; top: 26px; font-size: 27px; font-weight: 300; }
    .drawer-voice-setting { padding: 0 16px 8px; }
    .drawer-voice-setting .filter-chip { min-height: 40px; }
    #modelPillBtn { flex: 1; }
    #pillModelName { overflow: hidden; text-overflow: ellipsis; }
    .empty-mark { font-size: 50px; color: var(--accent); margin-bottom: 8px; }
    button { font-family: inherit; }
    button:focus-visible, [tabindex]:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
    /* The home indicator hides behind the keyboard, so its inset isn't needed. */
    .keyboard-open .bottom-dock, .short-viewport .bottom-dock { padding-bottom: 6px; }
    @media (max-height: 500px) {
      .top-header { padding-top: max(var(--safe-top), 4px); padding-bottom: 4px; }
      .quick-bounce-strip { display: none; }
      .bottom-dock { padding-bottom: max(var(--safe-bottom), 4px); }
      .chat-scroll { padding-top: 12px; }
    }
    @media (prefers-reduced-motion: reduce) {
      *, *::before, *::after { animation: none !important; transition: none !important; scroll-behavior: auto !important; }
    }

    .settings-entry { display:flex; align-items:center; gap:12px; margin:8px 14px; padding:14px; border:0; border-radius:14px; background:var(--surface); color:var(--text); font:inherit; text-align:left; cursor:pointer; }
    .settings-entry span:last-child { margin-left:auto; }
    .bottom-sheet.settings-sheet { height:var(--vvh,100dvh); max-height:100dvh; border-radius:0; padding:calc(var(--safe-top) + 12px) 20px max(var(--safe-bottom),20px); overflow:hidden; }
    .settings-sheet .sheet-handle { display:none; }
    .settings-sheet #sheetContent { height:100%; overflow:auto; overscroll-behavior:contain; }
    .settings-sheet .sheet-title { position:sticky; top:0; background:#171719; z-index:1; justify-content:flex-start; gap:12px; }
    .settings-sheet .sheet-close { top:calc(var(--safe-top) + 22px); z-index:2; }
    .settings-section { margin:24px 0 30px; }
    .settings-section h2 { font-size:15px; margin:0 0 12px; font-weight:650; }
    .settings-section h3 { font-size:14px; margin-top:24px; }
    .settings-section-heading { display:flex; align-items:center; justify-content:space-between; gap:10px; }
    .settings-help { color:var(--text-muted); font-size:13px; line-height:1.6; }
    .settings-group { background:#242426; border-radius:18px; overflow:hidden; }
    .settings-row { width:100%; display:flex; align-items:center; gap:12px; padding:16px; min-height:60px; background:transparent; border:0; border-bottom:1px solid var(--border); color:var(--text); font:inherit; text-align:left; }
    .settings-row:last-child { border-bottom:0; }
    .settings-row-label { flex:1; min-width:0; }
    .settings-row small, .settings-model small { display:block; margin-top:5px; color:var(--text-muted); font-size:12px; overflow-wrap:anywhere; }
    .settings-row input[type=checkbox] { width:22px; height:22px; accent-color:var(--accent); }
    .settings-secondary, .settings-save, .settings-back { min-height:44px; padding:10px 14px; border:1px solid var(--border); border-radius:12px; background:var(--surface-2); color:var(--text); font:inherit; cursor:pointer; }
    .settings-save { width:100%; background:var(--accent); color:#fff; margin:12px 0 20px; }
    .settings-models { max-height:240px; overflow:auto; border-radius:14px; background:var(--surface); }
    .settings-model { padding:14px; border-bottom:1px solid var(--border); font-size:14px; }
    .settings-model-editor { padding:14px; margin:10px 0; background:var(--surface); border-radius:14px; }
    .settings-model-editor label { display:block; color:var(--text-muted); font-size:12px; margin-bottom:12px; }
    .settings-model-editor input, .model-search { display:block; width:100%; padding:12px; margin-top:6px; background:#29292b; color:var(--text); border:1px solid var(--border); border-radius:12px; font-size:16px; }
    .model-search { margin:0 0 14px; }
    .model-choice[hidden] { display:none; }
    button:disabled { opacity:.55; cursor:default; }
  </style>
</head>
<body>
  <div class="app-shell" id="appShell">
    <!-- Top Header -->
    <header class="top-header" id="topHeader">
      <button class="circle-btn" id="openDrawerBtn" aria-label="Menu" title="Open thread drawer">
        <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><line x1="4" y1="7" x2="20" y2="7"/><line x1="4" y1="12" x2="16" y2="12"/><line x1="4" y1="17" x2="20" y2="17"/></svg>
      </button>

      <button class="header-title-wrap" id="headerTitleWrap" type="button" aria-label="Choose project">
        <div class="header-title" id="convTitle">MoE Deck</div>
        <div class="header-subtitle">
          <span class="bounce-dot" id="headerHarnessDot" style="background: var(--accent)"></span>
          <span id="convSubtitle"></span>
        </div>
      </button>

      <div style="display:flex;align-items:center;gap:6px">
        <button class="circle-btn" id="headerNewBtn" onclick="document.getElementById('newThreadBtn').click()" title="Start a fresh conversation" aria-label="New thread">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.3" stroke-linecap="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>
        </button>
        <button class="header-right-pill" id="quickFlipBtn" title="Tap to flip to previous active conversation">
          <span class="bounce-dot" id="flipDot" style="width:9px;height:9px;background:var(--accent)"></span>
          <span id="flipLabel">Flip</span>
        </button>
      </div>
    </header>

    <!-- Compact Quick-Bounce Strip -->
    <nav class="bounce-strip" id="bounceStrip"></nav>

    <!-- Messages Scroll Area -->
    <!-- Multiplex: one pane per conversation; the focused pane holds #chatScroll -->
    <div class="pane-grid" id="paneGrid" data-count="1"><main class="chat-scroll" id="chatScroll"></main></div>
    <nav class="tmux-bar" id="tmuxBar" aria-label="Windows"></nav>

    <!-- Bottom Dock -->
    <footer class="bottom-dock">
      <!-- Interactive Remote Push Approvals (HITL) -->
      <div class="approval-dock hidden" id="approvalDock"></div>

      <!-- Offline Intent Queue Indicator -->
      <div id="offlineIndicator" style="display:none;align-items:center;justify-content:space-between;padding:6px 12px;background:#f59e0b;color:#18181b;font-size:11.5px;font-weight:700;border-radius:10px;margin-bottom:6px">
        <span>⚠️ Offline Mode: Turns are queued and will auto-send on reconnect</span>
        <span id="offlineQueueCountBadge" style="background:rgba(0,0,0,0.18);padding:1px 6px;border-radius:8px">0 queued</span>
      </div>

      <div class="running-task-bar hidden" id="runningTaskBar">
        <span id="runningTaskText">1 running task</span>
        <button id="runningStopBtn" style="display:none;background:#ef4444;color:#fff;border:none;border-radius:7px;padding:4px 11px;font-size:11.5px;font-weight:700;cursor:pointer" onclick="event.stopPropagation(); if (state.activeId) stopConvTask(state.activeId)">■ Stop</button>
        <span style="font-size:11.5px;color:var(--text-muted)">Details ▾</span>
      </div>

      <div class="composer-card">
        <div class="slash-menu" id="slashMenu">
          <div class="slash-item" data-cmd="/new">
            <span class="slash-cmd">/new</span>
            <span class="slash-desc">Start a fresh conversation</span>
          </div>
          <div class="slash-item" data-cmd="/handoff">
            <span class="slash-cmd">/handoff</span>
            <span class="slash-desc">Switch harness & carry context</span>
          </div>
          <div class="slash-item" data-cmd="/rc">
            <span class="slash-cmd">/rc</span>
            <span class="slash-desc">Claude app · native Remote Control</span>
          </div>
          <div class="slash-item" data-cmd="/workdir ">
            <span class="slash-cmd">/workdir</span>
            <span class="slash-desc">Switch project, worktrees &amp; PR actions</span>
          </div>
          <div class="slash-item" data-cmd="/review">
            <span class="slash-cmd">/review</span>
            <span class="slash-desc">Second-opinion diff audit or Arena run</span>
          </div>
          <div class="slash-item" data-cmd="/bounce">
            <span class="slash-cmd">/bounce</span>
            <span class="slash-desc">⚡ Swarm Bounce: Distill context &amp; fork to peer harness in worktree</span>
          </div>
          <div class="slash-item" data-cmd="/worktree">
            <span class="slash-cmd">/worktree</span>
            <span class="slash-desc">Fork into isolated git worktree sandbox</span>
          </div>
          <div class="slash-item" data-cmd="/preview">
            <span class="slash-cmd">/preview</span>
            <span class="slash-desc">Live local dev server preview</span>
          </div>
          <div class="slash-item" data-cmd="/sprint">
            <span class="slash-cmd">/sprint</span>
            <span class="slash-desc">🏃 Autonomous Multi-Step Sprint Pipeline</span>
          </div>
          <div class="slash-item" data-cmd="/diff">
            <span class="slash-cmd">/diff</span>
            <span class="slash-desc">Inspect live git diff for workspace</span>
          </div>
          <div class="slash-item" data-cmd="/stop">
            <span class="slash-cmd">/stop</span>
            <span class="slash-desc">Stop running task</span>
          </div>
        </div>

        <div class="slash-menu" id="atMenu" style="max-height:220px;overflow-y:auto"></div>

        <div class="upload-preview-strip hidden" id="uploadPreviewStrip"></div>

        <!-- Live Voice Waveform Bar (active during Walkie-Talkie / dictation) -->
        <div id="voiceWaveBar" style="display:none;align-items:center;justify-content:space-between;padding:4px 10px;margin-bottom:6px;background:rgba(239,68,68,0.12);border:1px solid rgba(239,68,68,0.3);border-radius:8px">
          <div style="display:flex;align-items:center;gap:6px">
            <span class="bounce-dot busy" style="background:#ef4444"></span>
            <span style="font-size:11px;font-weight:700;color:#ef4444" id="voiceWaveStatus">🎙️ Listening…</span>
          </div>
          <div style="display:flex;align-items:center;gap:6px">
            <div id="voiceLevelMeter" style="width:60px;height:6px;background:#333;border-radius:3px;overflow:hidden">
              <div id="voiceLevelFill" style="width:30%;height:100%;background:#ef4444;transition:width 0.1s ease"></div>
            </div>
            <button class="filter-chip" onclick="cancelVoiceRecording()" style="padding:1px 6px;font-size:9.5px;color:#f87171">Cancel</button>
          </div>
        </div>


        <textarea
          class="composer-input"
          id="composerInput"
          rows="1"
          placeholder="Ask anything, or describe a task…"
        ></textarea>

        <div class="composer-controls">
          <input type="file" id="fileInput" accept="image/*" multiple style="display:none" />
          <button class="composer-icon-btn" id="attachBtn" title="Attach screenshot or image">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round"><line x1="12" y1="5" x2="12" y2="19"/><line x1="5" y1="12" x2="19" y2="12"/></svg>
          </button>

        <div class="composer-meta-row">
          <!-- Harness Selector Pill -->
          <button class="selector-pill harness-pill" id="harnessPillBtn" title="Switch agent harness">
            <span class="bounce-dot" id="pillHarnessDot" style="background: var(--accent)"></span>
            <span id="pillHarnessName">Choose harness</span>
          </button>

          <!-- Account switch: only shown when the harness has more than one account -->
          <button class="selector-pill" id="accountPillBtn" title="Switch account" onclick="cycleAccount()" hidden>
            <span id="pillAccountName">#1</span>
          </button>

          <!-- Model + Effort Selector Pill -->
          <button class="selector-pill" id="modelPillBtn" title="Switch model">
            <span id="pillModelName">Configured model</span>
          </button>


        </div>
          <div class="composer-spacer"></div>

          <!-- Voice Dictation Button -->
          <button class="composer-icon-btn" id="micBtn" title="Voice push-to-talk">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><rect x="9" y="2" width="6" height="11" rx="3"/><path d="M5 10a7 7 0 0 0 14 0"/><line x1="12" y1="19" x2="12" y2="22"/></svg>
          </button>

          <!-- Send Button -->
          <button class="send-btn" id="sendBtn" aria-label="Send">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><line x1="12" y1="19" x2="12" y2="5"/><polyline points="5 12 12 5 19 12"/></svg>
          </button>
        </div>
      </div>
    </footer>
  </div>

  <!-- Backdrop -->
  <div class="backdrop" id="backdrop"></div>

  <!-- Left Drawer -->
  <aside class="drawer" id="drawer">
    <div class="drawer-header">
      <div class="drawer-brand">
        <span style="width:9px;height:9px;border-radius:50%;background:var(--accent)"></span>
        MoE Deck
      </div>
      <div style="display:flex;align-items:center;gap:6px">
        <button class="filter-chip" id="notifyBtn" onclick="enableNotifications()" title="Enable mobile notifications when tasks finish">🔔 Alerts</button>
        <button class="new-thread-btn" id="newThreadBtn">+ New</button>
      </div>
    </div>
    <div style="padding:10px 12px 2px 12px">
      <input
        id="drawerSearchInput"
        type="search"
        placeholder="Search threads or projects…"
        style="width:100%;background:#232320;border:1px solid var(--border-strong);border-radius:10px;padding:7px 11px;color:var(--text);font-size:12.5px;outline:none"
      />
    </div>
    <div class="drawer-filters" id="drawerFilters">
      <button class="filter-chip active" data-filter="all">All</button>
      <button class="filter-chip" data-filter="claude">Claude</button>
      <button class="filter-chip" data-filter="agy">AGY</button>
      <button class="filter-chip" data-filter="codex">Codex</button>
      <button class="filter-chip" data-filter="opencode">OpenCode</button>
      <button class="filter-chip" data-filter="keel">Keel</button>
    </div>
    <div id="convListHeading" style="padding:12px 22px 8px;color:var(--text-muted);font-size:13px">Conversations</div>
    <div class="conv-list" id="convList"></div>
    <button class="settings-entry" id="quickSettingsBtn" onclick="openQuickSettings()"><span aria-hidden="true">◐</span> Quick settings <span aria-hidden="true">›</span></button>
    <button class="settings-entry" id="settingsBtn" onclick="openSettings()"><span aria-hidden="true">⚙</span> Settings <span aria-hidden="true">›</span></button>    <div hidden>    <div class="drawer-voice-setting">          <!-- Walkie-Talkie Mode Toggle -->
          <button class="filter-chip" id="walkieTalkieToggleBtn" onclick="toggleWalkieTalkieMode()" title="Walkie-Talkie Mode: Auto-send on mic release" style="padding:2px 7px;font-size:10.5px">
            <span id="walkieTalkieLabel">🎙️ Walkie</span>
          </button>
    </div>
    <!-- HITL Safety Mode Bar -->
    <div style="margin:6px 12px 8px 12px;padding:8px 10px;border-radius:10px;background:#20201d;border:1px solid var(--border)">
      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:6px">
        <span style="font-size:11.5px;font-weight:650">🛡️ Safety Mode</span>
        <span id="safetyModeBadge" style="font-size:10.5px;color:var(--accent);font-weight:700;letter-spacing:0.4px">STANDARD</span>
      </div>
      <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:5px">
        <button class="filter-chip" id="safetyModeSafe" onclick="setSafetyMode('safe')" style="padding:4px 0;text-align:center;font-size:11px">🛡️ Safe</button>
        <button class="filter-chip active" id="safetyModeStandard" onclick="setSafetyMode('standard')" style="padding:4px 0;text-align:center;font-size:11px">⚡ Standard</button>
        <button class="filter-chip" id="safetyModeYolo" onclick="setSafetyMode('yolo')" style="padding:4px 0;text-align:center;font-size:11px">🚀 Yolo</button>
      </div>
    </div>

</div>
  </aside>

  <!-- Bottom Sheet -->
  <div class="bottom-sheet" id="bottomSheet" role="dialog" aria-label="Conversation controls" aria-modal="true" inert>
    <div class="sheet-handle"></div>
    <button class="sheet-close circle-btn" type="button" aria-label="Close panel" onclick="closeOverlays()">×</button>
    <div id="sheetContent"></div>
  </div>

  <!-- Fullscreen Lightbox -->
  <div class="lightbox" id="lightbox">
    <img id="lightboxImg" src="" alt="Preview" />
  </div>

  <!-- In-Cockpit Mobile Sandbox Preview Modal -->
  <div class="sandbox-modal" id="sandboxModal" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,0.85);backdrop-filter:blur(12px);z-index:900;flex-direction:column;align-items:center;justify-content:center;padding:12px">
    <div style="background:#191917;border:1px solid var(--border-strong);border-radius:18px;display:flex;flex-direction:column;width:100%;max-width:960px;height:94vh;overflow:hidden;box-shadow:0 24px 60px rgba(0,0,0,0.7)">

      <!-- Modal Top Bar -->
      <div style="display:flex;align-items:center;justify-content:space-between;padding:10px 14px;border-bottom:1px solid var(--border);background:#1f1f1c;gap:8px;flex-wrap:wrap">
        <div style="display:flex;align-items:center;gap:8px">
          <span style="font-size:13px;font-weight:700">📱 Mobile Sandbox Preview</span>
          <div style="display:flex;gap:4px">
            <button class="filter-chip active" id="deviceIphoneBtn" onclick="setSandboxDevice('iphone')">iPhone 16</button>
            <button class="filter-chip" id="deviceIpadBtn" onclick="setSandboxDevice('ipad')">iPad</button>
            <button class="filter-chip" id="deviceFullBtn" onclick="setSandboxDevice('full')">Full</button>
          </div>
          <button class="filter-chip" id="deviceOrientBtn" onclick="toggleSandboxOrientation()" title="Rotate Orientation">🔄</button>
        </div>

        <!-- URL and Port Bar -->
        <div style="display:flex;align-items:center;gap:6px;flex:1;max-width:440px;min-width:200px">
          <button class="filter-chip" onclick="refreshSandboxFrame()" style="padding:4px 8px">↻</button>
          <input id="sandboxUrlInput" value="/preview/3000/" style="flex:1;background:#141412;border:1px solid var(--border-strong);border-radius:8px;padding:4px 8px;font-family:ui-monospace,monospace;font-size:11.5px;color:var(--text)" onkeydown="if(event.key==='Enter')navigateSandboxUrl()" />
          <button class="filter-chip" onclick="openSandboxExternal()" title="Open in new browser tab">↗</button>
          <button class="filter-chip" onclick="toggleSandboxQr()" title="Scan QR with physical phone">📷 QR</button>
        </div>

        <div style="display:flex;align-items:center;gap:6px">
          <button class="filter-chip" id="sandboxConsoleToggleBtn" onclick="toggleSandboxConsole()">📟 Logs (<span id="sandboxLogCount">0</span>)</button>
          <button class="filter-chip" onclick="closeSandboxModal()" style="font-weight:700;padding:4px 10px">✕</button>
        </div>
      </div>

      <!-- QR Code Popover (hidden by default) -->
      <div id="sandboxQrWrap" style="display:none;background:#252522;border-bottom:1px solid var(--border);padding:12px;text-align:center">
        <div style="font-size:12px;color:var(--text-muted);margin-bottom:6px">Scan with your phone's camera on LAN / Tailscale:</div>
        <div id="sandboxQrContainer" style="display:inline-block;background:#fff;padding:8px;border-radius:10px"></div>
        <div id="sandboxQrUrlText" style="font-family:ui-monospace,monospace;font-size:11px;color:var(--text-dim);margin-top:4px"></div>
      </div>

      <!-- Simulator Viewport Area -->
      <div style="flex:1;display:flex;align-items:center;justify-content:center;background:#0d0d0c;overflow:auto;padding:16px;position:relative">
        <div id="sandboxDeviceFrame" style="width:393px;height:852px;background:#000;border:12px solid #282826;border-radius:46px;box-shadow:0 12px 40px rgba(0,0,0,0.8);position:relative;display:flex;flex-direction:column;overflow:hidden;transition:all 0.25s ease">
          <!-- Dynamic Island Notch Skin -->
          <div id="sandboxNotch" style="height:26px;width:108px;background:#000;border-radius:14px;position:absolute;top:9px;left:50%;transform:translateX(-50%);z-index:10;pointer-events:none"></div>
          <iframe id="sandboxFrame" src="about:blank" style="width:100%;height:100%;border:none;background:#fff"></iframe>
        </div>
      </div>

      <!-- Console Drawer (hidden by default) -->
      <div id="sandboxConsoleDrawer" style="display:none;height:140px;background:#141412;border-top:1px solid var(--border);flex-direction:column">
        <div style="display:flex;align-items:center;justify-content:space-between;padding:4px 10px;background:#1c1c19;font-size:11px;font-weight:600;color:var(--text-muted)">
          <span>Client Console Telemetry</span>
          <button class="filter-chip" onclick="clearSandboxLogs()" style="padding:1px 6px;font-size:10px">Clear</button>
        </div>
        <div id="sandboxConsoleLogs" style="flex:1;overflow-y:auto;padding:6px 10px;font-family:ui-monospace,monospace;font-size:10.5px;line-height:1.4"></div>
      </div>

    </div>
  </div>

  <script>
    const state = {
      harnesses: [],
      conversations: [],
      archived: new Set(),
      showArchived: false,
      activeId: localStorage.getItem('helm_active_id') || null,
      previousId: localStorage.getItem('helm_prev_id') || null,
      detail: null,
      selectedHarness: null,
      selectedModel: 'default',
      selectedModelLabel: 'Default',
      selectedEffort: 'default',
      selectedWorkdir: '',
      selectedProfile: null,
      profiles: [],
      drawerFilter: 'all',
      drawerSearch: '',
      queuedImages: [],
      drafts: new Map(),
      expandedTools: new Set(),
      expandedThinking: new Set(),
      prevBusyIds: new Set(),
      bootstrappedOnce: false,
      lastMessageCount: 0,
      lastTailText: '',
      lastTruncated: null,
      fullHistoryId: null,
      pendingApprovals: [],
      safetyMode: 'standard',
      seenApprovalIds: new Set(),
      swarmSessions: [],
      swarmTargetHarness: null,
      walkieTalkie: localStorage.getItem('helm_walkie') === 'true',
      audioChimes: localStorage.getItem('helm_chimes') !== 'false',
      offlineQueue: JSON.parse(localStorage.getItem('helm_offline_queue') || '[]'),
      reconnecting: false,
      sprints: [],
      panes: JSON.parse(localStorage.getItem('helm_panes') || '[]'),
      // Panes you opened yourself stay until closed; the rest follow each harness's newest chat.
      pinnedPanes: new Set(JSON.parse(localStorage.getItem('helm_pinned_panes') || '[]')),
      dismissedPanes: new Set(JSON.parse(localStorage.getItem('helm_dismissed_panes') || '[]')),
      tmuxBar: localStorage.getItem('helm_tmux_bar') === 'on',
      zoomed: false,
      paneCache: new Map()
    };

    // A poll must not remove a touch target between pointerdown and click.
    const pressedPointers = new Set();
    const deferredRenders = new Set();
    document.addEventListener('pointerdown', e => pressedPointers.add(e.pointerId), true);
    function releasePointer(e) {
      pressedPointers.delete(e.pointerId);
      setTimeout(() => {
        if (pressedPointers.size) return;
        const renders = [...deferredRenders];
        deferredRenders.clear();
        renders.forEach(render => render());
      }, 0);
    }
    document.addEventListener('pointerup', releasePointer, true);
    document.addEventListener('pointercancel', releasePointer, true);
    function deferRender(render) {
      if (!pressedPointers.size) return false;
      deferredRenders.add(render);
      return true;
    }
    function replaceChangedHtml(el, html) {
      if (el._renderedHtml === html) return;
      const scroll = el.scrollTop;
      el.innerHTML = html;
      el._renderedHtml = html;
      el.scrollTop = scroll;
    }

    async function apiRequest(url, options) {
      const res = await fetch(url, options);
      const data = await res.json().catch(() => ({}));
      if (res.status === 401) showLoginOverlay();
      if (!res.ok) throw new Error(data.error || (res.status === 401
        ? 'Unlock MoE Deck to continue.' : `Request failed (${res.status}).`));
      return data;
    }
    const jsonRequest = (method, body) => ({ method,
      headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) });

    // Dynamic visual viewport handling for mobile virtual keyboards
    function syncVisualViewport() {
      if (window.visualViewport) {
        const vvh = window.visualViewport.height;
        // iOS 26.1 home-screen apps can't draw in a strip at the bottom of the
        // screen (WebKit bug 301994), so the home indicator already sits outside
        // the page. Stay inside the page, and skip its inset when the strip is there.
        const portrait = window.innerWidth <= window.innerHeight;
        const screenH = portrait ? Math.max(screen.width, screen.height) : Math.min(screen.width, screen.height);
        const missing = screenH - window.innerHeight;
        const keyboard = window.innerHeight - vvh > 120;
        document.documentElement.classList.toggle('keyboard-open', keyboard);
        document.documentElement.classList.toggle('short-viewport', navigator.standalone === true && missing > 0 && missing <= 80);
        document.documentElement.style.setProperty('--vvh', `${vvh}px`);
        document.documentElement.style.setProperty('--vv-top', `${Math.max(0, window.visualViewport.offsetTop)}px`);
        const active = document.activeElement;
        if (active && (active.id === 'composerInput' || active.tagName === 'TEXTAREA')) {
          requestAnimationFrame(() => {
            const scroller = document.getElementById('chatScroll');
            if (scroller) scroller.scrollTop = scroller.scrollHeight;
          });
        }
      }
    }
    if (window.visualViewport) {
      window.visualViewport.addEventListener('resize', syncVisualViewport);
      window.visualViewport.addEventListener('scroll', syncVisualViewport);
      syncVisualViewport();
    }

    // Edge-swipe gesture support for mobile drawer
    let touchStartX = 0;
    let touchStartY = 0;
    let swipeTracked = false;

    window.addEventListener('touchstart', (e) => {
      if (e.touches.length !== 1) return;
      touchStartX = e.touches[0].clientX;
      touchStartY = e.touches[0].clientY;
      const drawer = document.getElementById('drawer');
      const isDrawerOpen = drawer && drawer.classList.contains('open');
      swipeTracked = touchStartX < 36 || isDrawerOpen;
    }, { passive: true });

    window.addEventListener('touchend', (e) => {
      if (!swipeTracked || e.changedTouches.length !== 1) return;
      const dx = e.changedTouches[0].clientX - touchStartX;
      const dy = e.changedTouches[0].clientY - touchStartY;
      swipeTracked = false;
      if (Math.abs(dx) > Math.abs(dy) * 1.4 && Math.abs(dx) > 46) {
        const drawer = document.getElementById('drawer');
        if (dx > 0 && touchStartX < 40) {
          if (drawer && !drawer.classList.contains('open')) {
            drawer.classList.add('open');
            document.getElementById('backdrop').classList.add('open');
          }
        } else if (dx < 0) {
          if (drawer && drawer.classList.contains('open')) {
            closeOverlays();
          }
        }
      }
    }, { passive: true });

    if ('serviceWorker' in navigator) {
      navigator.serviceWorker.register('/sw.js').catch(() => {});
      navigator.serviceWorker.addEventListener('message', (event) => {
        if (event.data && event.data.type === 'OPEN_CONVERSATION' && event.data.conversationId) {
          loadConversation(event.data.conversationId, true);
        }
      });
    }

    window.enableNotifications = async function() {
      if (!('Notification' in window)) {
        alert('Add MoE Deck to your Home Screen first to enable iOS/Android notifications.');
        return;
      }
      const perm = await Notification.requestPermission();
      const btn = document.getElementById('notifyBtn');
      if (perm === 'granted' && btn) {
        btn.classList.add('active');
        btn.textContent = '🔔 Alerts On';
      }
    };

    function fireTurnCompletedNotification(conv) {
      playAudioChime('success');
      // No banner for the thread the user is already looking at.
      if (document.visibilityState === 'visible' && conv.id === state.activeId) return;
      if (!('Notification' in window) || Notification.permission !== 'granted') return;
      const hc = HARNESS_COLORS[conv.harness] || HARNESS_COLORS.claude;
      const title = `⚡ ${hc.short} finished · ${conv.title}`;
      const body = conv.last_preview || `Ready in ${conv.subtitle}`;
      if (navigator.serviceWorker && navigator.serviceWorker.ready) {
        navigator.serviceWorker.ready.then(reg => {
          reg.showNotification(title, {
            body,
            icon: '/icon.svg',
            data: { conversationId: conv.id }
          });
        }).catch(() => {});
      } else {
        new Notification(title, { body, icon: '/icon.svg' });
      }
    }

    const HARNESS_COLORS = {
      claude: { accent: '#D97757', soft: 'rgba(217, 119, 87, 0.16)', short: 'Claude' },
      agy: { accent: '#4F8FF7', soft: 'rgba(79, 143, 247, 0.16)', short: 'AGY' },
      codex: { accent: '#10A37F', soft: 'rgba(16, 163, 127, 0.16)', short: 'Codex' },
      opencode: { accent: '#A855F7', soft: 'rgba(168, 85, 247, 0.16)', short: 'OpenCode' },
      keel: { accent: '#06B6D4', soft: 'rgba(6, 182, 212, 0.16)', short: 'Keel' }
    };
    const HARNESS_ORDER = ['claude', 'codex', 'agy', 'opencode', 'keel'];

    // Harnesses whose CLI is installed on the MoE Deck machine, so pickers
    // never offer one that can only fail. Before the first bootstrap: all.
    function installedHarnessIds() {
      if (!state.harnesses.length) return HARNESS_ORDER;
      const ids = state.harnesses.filter(h => h.available).map(h => h.id);
      return HARNESS_ORDER.filter(id => ids.includes(id));
    }

    function swarmTarget() {
      const ids = installedHarnessIds();
      if (ids.includes(state.swarmTargetHarness)) return state.swarmTargetHarness;
      return ids.find(id => id !== state.selectedHarness) || ids[0] || null;
    }

    // Drawer chips: installed harnesses plus any that already have chats.
    function updateDrawerFilterChips() {
      const ids = new Set(installedHarnessIds());
      state.conversations.forEach(c => ids.add(c.harness));
      document.querySelectorAll('#drawerFilters .filter-chip[data-filter]').forEach(btn => {
        const id = btn.dataset.filter;
        btn.hidden = id !== 'all' && !ids.has(id);
      });
      if (state.drawerFilter !== 'all' && !ids.has(state.drawerFilter)) {
        state.drawerFilter = 'all';
        document.querySelectorAll('#drawerFilters .filter-chip[data-filter]').forEach(b => b.classList.toggle('active', b.dataset.filter === 'all'));
      }
    }

    function shortEffort(ef) {
      if (ef === 'medium') return 'Med';
      return ef.charAt(0).toUpperCase() + ef.slice(1);
    }

    function compactModelLabel(lbl) {
      if (lbl === 'Codex configured default') return 'Default';
      return String(lbl || '').replace('Gemini 4 ', '').replace('Gemini 3.8 ', '').replace('Gemini 3.5 ', '');
    }

    function accountTag(profileId) {
      const p = (state.profiles || []).find(x => x.id === profileId);
      if (!p) return '';
      const m = String(p.label || '').match(/(\d+)/);
      return m ? ` · #${m[1]}` : ` · ${p.label}`;
    }

    function applyHarnessTheme(harnessId) {
      const theme = HARNESS_COLORS[harnessId] || HARNESS_COLORS.claude;
      document.documentElement.style.setProperty('--accent', theme.accent);
      document.documentElement.style.setProperty('--accent-soft', theme.soft);
      const accounts = switchableAccounts(harnessId);
      const accountPill = document.getElementById('accountPillBtn');
      accountPill.hidden = accounts.length < 2;
      document.getElementById('pillAccountName').textContent =
        state.selectedProfile ? accountTag(state.selectedProfile).replace(/^ · /, '') : '#1';
      document.getElementById('pillHarnessName').textContent =
        theme.short + (accounts.length < 2 ? accountTag(state.selectedProfile) : '');
      document.getElementById('pillModelName').textContent = compactModelLabel(state.selectedModelLabel)
        + (state.selectedEffort === 'default' ? '' : ` · ${shortEffort(state.selectedEffort)}`);
    }

    // Passcode login overlay — shown when /api/* returns 401.
    function showLoginOverlay() {
      const existing = document.getElementById('loginOverlay');
      if (existing) {
        existing.style.display = 'flex';
        const inp = document.getElementById('loginPass');
        if (inp && document.activeElement !== inp && !('ontouchstart' in window)) inp.focus();
        return;
      }
      const ov = document.createElement('div');
      ov.id = 'loginOverlay';
      ov.style.cssText = 'position:fixed;inset:0;z-index:10000;background:#0d0d0c;display:flex;align-items:center;justify-content:center;flex-direction:column;gap:14px;padding:30px';
      ov.innerHTML = `
        <img src="/icon.svg" alt="" width="56" height="56" style="border-radius:14px">
        <div style="font-weight:780;font-size:17px;color:var(--text)">MoE Deck · Locked</div>
        <div style="font-size:12.5px;color:var(--text-muted)">Enter the cockpit passcode</div>
        <form id="loginForm" style="display:flex;gap:8px;width:100%;max-width:280px">
          <input id="loginPass" type="password" autocomplete="current-password" placeholder="Passcode" style="flex:1;background:#191917;border:1px solid var(--border-strong);border-radius:10px;padding:11px 12px;color:var(--text);font-size:15px;outline:none" />
          <button class="new-thread-btn" style="padding:8px 16px;font-size:15px" type="submit">→</button>
        </form>
        <div id="loginErr" style="font-size:12px;color:#ef4444;min-height:16px"></div>`;
      document.body.appendChild(ov);
      document.getElementById('loginForm').addEventListener('submit', async (e) => {
        e.preventDefault();
        const pc = document.getElementById('loginPass').value;
        const errEl = document.getElementById('loginErr');
        try {
          const res = await fetch('/api/auth/login', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ passcode: pc })
          });
          if (res.status === 401) {
            errEl.textContent = 'Wrong passcode';
            document.getElementById('loginPass').value = '';
            return;
          }
          if (!res.ok) {
            errEl.textContent = res.status === 409
              ? 'No passcode configured on server'
              : 'Login failed (' + res.status + ')';
            return;
          }
          ov.remove();
          await fetchState();
        } catch (_) {
          errEl.textContent = 'Network error';
        }
      });
    }

    // Poll health tracking: distinguishes "browser offline" from the more
    // insidious "browser thinks it is online but the server is unreachable".
    let pollFailures = 0;    function notePollSuccess() {
      if (pollFailures > 0) {
        pollFailures = 0;
        state.reconnecting = false;
        updateNetworkUI();
      }
    }
    function notePollFailure() {
      pollFailures += 1;
      state.reconnecting = true;
      updateNetworkUI();
    }

    let bootstrapRequest = 0;
    async function fetchState() {
      const request = ++bootstrapRequest;
      const activeAtStart = state.activeId;
      try {
        const res = await fetch('/api/bootstrap');
        if (res.status === 401) {
          showLoginOverlay();
          return;
        }
        if (!res.ok) { notePollFailure(); return; }
        notePollSuccess();
        const data = await res.json();
        if (request !== bootstrapRequest) return;
        state.home = data.home;
        state.harnesses = data.harnesses || [];
        state.profiles = data.profiles || [];
        const selectedModel = state.harnesses.find(h=>h.id===state.selectedHarness)?.models.find(m=>m.id===state.selectedModel);
        if(selectedModel) state.selectedModelLabel=selectedModel.label;
        if (!state.selectedWorkdir && data.home) {
          // Prefer the last workdir the user actually sent from, so a new
          // thread doesn't silently land in the home directory again.
          state.selectedWorkdir = localStorage.getItem('helm_last_workdir') || data.home;
          // Cold-start header: show the home workspace until a thread loads.
          const homeLabel = String(data.home).split('/').filter(Boolean).pop() || 'MoE Deck';
          const sub = document.getElementById('convSubtitle');
          if (sub && !state.detail) {
            sub.textContent = homeLabel;
          }
        }
        const newConvs = data.conversations || [];
        state.pendingApprovals = data.pending_approvals || [];
        state.safetyMode = data.safety_mode || 'standard';
        state.swarmSessions = data.swarm_sessions || [];
        state.sprints = data.sprints || [];

        updateSafetyModeUI();
        updateWalkieTalkieUI();
        checkNewApprovals(state.pendingApprovals);
        renderApprovalDock();

        if (state.bootstrappedOnce) {
          for (const c of newConvs) {
            if (state.prevBusyIds.has(c.id) && c.status !== 'busy') {
              fireTurnCompletedNotification(c);
            }
          }
        }
        state.prevBusyIds = new Set(newConvs.filter(c => c.status === 'busy').map(c => c.id));
        state.bootstrappedOnce = true;
        state.conversations = newConvs;
        state.archived = new Set(data.archived || []);
        syncPanes(newConvs.filter(c => !state.archived.has(c.id)));
        if (!localStorage.getItem('helm_last_workdir') && data.home) {
          const project = newConvs.find(c => c.workdir && c.workdir !== data.home);
          if (project) {
            state.selectedWorkdir = project.workdir;
            const sub = document.getElementById('convSubtitle');
            if (sub && !state.detail) {
              sub.textContent = String(project.workdir).split('/').filter(Boolean).pop() || 'MoE Deck';
            }
          }
        }

        if ('Notification' in window && Notification.permission === 'granted') {
          const btn = document.getElementById('notifyBtn');
          if (btn) {
            btn.classList.add('active');
            btn.textContent = '🔔 Alerts On';
          }
        }

        if (!state.activeId && state.conversations.length > 0) {
          state.activeId = state.conversations[0].id;
          const other = state.conversations.find(c => c.id !== state.activeId);
          if (other) state.previousId = other.id;
        } else if (state.activeId === activeAtStart && !state.detail && !creatingThread && state.conversations.length > 0 && !state.conversations.some(c => c.id === state.activeId)) {
          state.activeId = state.conversations[0].id;
        }
        renderBounceStrip();
        renderDrawerList();
        renderPanes();
        if (state.activeId) {
          await loadConversation(state.activeId, false);
          refreshBackgroundPanes();
        } else {
          const harness = state.harnesses.find(h => h.available);
          if (!state.selectedHarness && harness) pickHarness(harness.id);
          document.getElementById('convSubtitle').textContent = 'Choose a project to begin';
          renderMessages([], false);
        }
      } catch (e) {
        notePollFailure();
        console.error('Bootstrap error:', e);
      }
    }

    let detailRequest = 0;
    async function loadConversation(id, switchExplicit = true) {
      const request = ++detailRequest;
      const changing = state.detail?.summary.id !== id;
      if (state.activeId !== id) {
        state.drafts.set(state.activeId, { text: composerInput.value, images: [...state.queuedImages] });
        const draft = state.drafts.get(id) || (!state.activeId ? state.drafts.get(null) : null);
        composerInput.value = draft?.text || '';
        composerInput.style.height = 'auto';
        state.queuedImages = draft?.images || [];
        renderQueuedImages();
      }
      if (switchExplicit && state.activeId && state.activeId !== id) {
        state.previousId = state.activeId;
        localStorage.setItem('helm_prev_id', state.previousId);
      }
      if (changing && state.detail) {
        // The pane we leave keeps showing what it had until its next refresh.
        state.paneCache.set(state.detail.summary.id, { stamp: '', messages: state.detail.messages });
      }
      state.activeId = id;
      localStorage.setItem('helm_active_id', id);
      if (switchExplicit && !state.panes.includes(id)) state.pinnedPanes.add(id);
      addPane(id);
      renderPanes();
      if (changing) {
        state.detail = null;
        composerInput.disabled = true;
        document.getElementById('sendBtn').disabled = true;
        document.getElementById('chatScroll').textContent = 'Loading conversation…';
        const summary = state.conversations.find(c => c.id === id);
        if (summary) document.getElementById('convTitle').textContent = summary.title;
      }

      try {
        // Phone polls only need the recent tail. `loadFullHistory` opts a
        // single thread into the complete transcript on demand.
        const fullHistory = state.fullHistoryId === id;
        const url = `/api/conversations/${encodeURIComponent(id)}${fullHistory ? '' : '?tail=100'}`;
        const res = await fetch(url);
        if (!res.ok) throw new Error(`Could not load conversation (${res.status}).`);
        const detail = await res.json();
        if (request !== detailRequest || state.activeId !== id) return;
        state.detail = detail;

        if (switchExplicit || changing || !state.selectedHarness) {
          state.selectedHarness = detail.summary.harness;
          state.selectedModel = detail.summary.model;
          state.selectedModelLabel = detail.summary.model_label;
          state.selectedEffort = detail.summary.effort || 'medium';
          state.selectedWorkdir = detail.summary.workdir;
          state.selectedProfile = detail.summary.profile || null;
        }
        applyHarnessTheme(state.selectedHarness);
        composerInput.disabled = false;
        document.getElementById('sendBtn').disabled = sendingTurn;

        document.getElementById('convTitle').textContent = detail.summary.title;
        const summaryTheme = HARNESS_COLORS[detail.summary.harness] || HARNESS_COLORS.claude;
        document.getElementById('headerHarnessDot').style.background = summaryTheme.accent;
        document.getElementById('convSubtitle').textContent =
          `${summaryTheme.short}${accountTag(detail.summary.profile)} · ${String(state.selectedWorkdir).split('/').filter(Boolean).pop() || state.selectedWorkdir}`;

        // Update running task bar
        const rBar = document.getElementById('runningTaskBar');
        const rText = document.getElementById('runningTaskText');
        const totalBusy = state.conversations.filter(c => c.status === 'busy').length;
        const activeSprint = (state.sprints || []).find(s => s.status === 'running');
        if (activeSprint) {
          rBar.classList.remove('hidden');
          const passed = activeSprint.steps.filter(s => s.status === 'passed').length;
          rText.textContent = `🏃 Sprint: ${activeSprint.title} (${passed}/${activeSprint.steps.length})`;
        } else if (detail.summary.status === 'busy' || totalBusy > 0) {
          rBar.classList.remove('hidden');
          if (detail.summary.status === 'busy') {
            rText.textContent = `1 running task`;
          } else {
            rText.textContent = `${totalBusy} running task${totalBusy > 1 ? 's' : ''} in background`;
          }
        } else {
          rBar.classList.add('hidden');
        }
        const stopBtn = document.getElementById('runningStopBtn');
        if (stopBtn) stopBtn.style.display = detail.summary.status === 'busy' ? 'inline-block' : 'none';

        // Update top-right 1-Tap Quick-Flip pill
        const flipDot = document.getElementById('flipDot');
        const flipLabel = document.getElementById('flipLabel');
        const prevConv = state.conversations.find(c => c.id === state.previousId && c.id !== state.activeId)
          || state.conversations.find(c => c.id !== state.activeId);
        if (prevConv) {
          const prevTheme = HARNESS_COLORS[prevConv.harness] || HARNESS_COLORS.claude;
          flipDot.style.background = prevTheme.accent;
          flipLabel.textContent = `↔ ${prevTheme.short}`;
          document.getElementById('quickFlipBtn').setAttribute('aria-label', `Return to ${prevConv.title}`);
        } else {
          flipLabel.textContent = 'Previous';
        }
        document.getElementById('quickFlipBtn').disabled = !prevConv;

        renderBounceStrip();
        renderPanes();
        renderApprovalDock();
        renderMessages(detail.messages, switchExplicit);
      } catch (e) {
        if (request !== detailRequest || state.activeId !== id) return;
        showToast(e.message || 'Could not load conversation.');
        if (changing) document.getElementById('chatScroll').textContent = 'Conversation could not load. Open the menu to retry or choose another thread.';
      }
    }

    // Header Title / Workspace Inspector Sheet
    async function openThreadInspector() {
      if (!state.detail) return;
      const sum = state.detail.summary;
      const curWorkdir = state.selectedWorkdir || sum.workdir;
      const content = document.getElementById('sheetContent');
      content.innerHTML = `<div class="sheet-title"><span>Loading workspace &amp; CI…</span></div>`;
      const version = openSheet();

      try {
        const [wsRes, projRes, prRes, portsRes] = await Promise.all([
          fetch(`/api/workspace-info?workdir=${encodeURIComponent(curWorkdir)}`),
          fetch('/api/projects'),
          fetch(`/api/pr-status?workdir=${encodeURIComponent(curWorkdir)}`),
          fetch('/api/ports')
        ]);
        const ws = wsRes.ok ? await wsRes.json() : { workdir: curWorkdir, is_git: false, modified_files: [], recent_commits: [], worktrees: [] };
        const projData = projRes.ok ? await projRes.json() : { projects: [] };
        const prInfo = prRes.ok ? await prRes.json() : { has_pr: false, ci_state: 'none', checks_summary: '' };
        const portsData = portsRes.ok ? await portsRes.json() : { ports: [] };
        if (version !== sheetVersion || state.detail?.summary.id !== sum.id) return;
        const projects = projData.projects || [];
        const ports = portsData.ports || [];
        const worktrees = ws.worktrees || [];
        const prLinks = sum.pr_links || [];
        const curSwarmSessions = (state.swarmSessions || []).filter(s => s.base_workdir === curWorkdir || s.source_conversation_id === sum.id || s.target_conversation_id === sum.id);

        const ciBadgeColor = prInfo.ci_state === 'passing' ? '#4ade80'
          : prInfo.ci_state === 'failing' ? '#f87171'
          : '#fbbf24';

        content.innerHTML = `
          <div class="sheet-title">
            <span>Thread, Git &amp; DevOps</span>
            <span style="display:flex;gap:6px">
              <button class="filter-chip ${sum.pinned ? 'active' : ''}" onclick="togglePinConv('${escapeAttr(sum.id)}', ${!sum.pinned})">
                ${sum.pinned ? '📌 Pinned' : 'Pin'}
              </button>
              <button class="filter-chip" onclick="setConvArchived('${escapeAttr(sum.id)}', ${!state.archived.has(sum.id)})">${state.archived.has(sum.id) ? 'Unarchive' : 'Archive'}</button>
            </span>
          </div>
          <div style="display:flex;gap:6px;margin-bottom:12px">
            <input id="renameTitleInput" value="${escapeAttr(sum.title)}" style="flex:1;background:#252522;border:1px solid var(--border-strong);border-radius:10px;padding:8px 10px;color:var(--text);font-size:13.5px" />
            <button class="new-thread-btn" onclick="saveConvTitle('${escapeAttr(sum.id)}')">Save</button>
          </div>

          ${ws.is_git ? `
            <div style="background:#232320;border:1px solid var(--border);border-radius:12px;padding:10px 12px;margin-bottom:12px">
              <div style="display:flex;align-items:center;justify-content:space-between;font-size:12.5px;font-weight:650">
                <span>🌿 Branch: <code>${escapeHtml(ws.branch || 'HEAD')}</code></span>
                <div style="display:flex;align-items:center;gap:8px">
                  <span style="color:var(--text-muted)">${ws.modified_files.length} changed</span>
                  <button class="filter-chip active" style="padding:3px 9px;font-size:11px" onclick="viewGitDiff('${escapeAttr(ws.workdir)}')">View Diff</button>
                </div>
              </div>
              ${ws.modified_files.length > 0 ? `
                <div style="margin-top:8px;max-height:100px;overflow-y:auto;font-family:ui-monospace,monospace;font-size:11.5px;color:var(--text-muted)">
                  ${ws.modified_files.slice(0, 12).map(f => `<div><strong style="color:var(--accent)">${escapeHtml(f.status)}</strong> ${escapeHtml(f.path)}</div>`).join('')}
                </div>
              ` : `<div style="margin-top:5px;font-size:11.5px;color:var(--text-muted)">Working tree clean ✓ (Tap View Diff for latest commit)</div>`}
              <div id="gitDiffContainer" style="display:none;margin-top:10px;padding:9px;border-radius:9px;background:#131312;border:1px solid var(--border-strong);max-height:240px;overflow:auto;font-family:ui-monospace,monospace;font-size:11px;line-height:1.42;white-space:pre"></div>

              <!-- PR & CI Actions Row -->
              <div style="margin-top:10px;padding-top:9px;border-top:1px solid var(--border);display:flex;align-items:center;justify-content:space-between;gap:8px;flex-wrap:wrap">
                ${prInfo.has_pr ? `
                  <div style="font-size:12px">
                    <a href="${escapeAttr(safeUrl(prInfo.url || ''))}" target="_blank" style="color:var(--accent);font-weight:650;text-decoration:underline">PR #${prInfo.number}</a>
                    <span style="color:${ciBadgeColor};margin-left:6px;font-weight:600">● ${escapeHtml(prInfo.checks_summary)}</span>
                  </div>
                  <div style="display:flex;gap:6px">
                    ${prInfo.ci_state === 'failing' ? `
                      <button class="new-thread-btn" style="background:#ef4444;color:#fff;padding:4px 9px;font-size:11.5px" onclick="runPrAction('${escapeAttr(ws.workdir)}', 'fix-ci')">🔧 Fix CI</button>
                    ` : ''}
                    <button class="new-thread-btn" style="padding:4px 10px;font-size:11.5px" onclick="runPrAction('${escapeAttr(ws.workdir)}', 'merge')">Squash &amp; Merge</button>
                  </div>
                ` : `
                  <span style="font-size:11.5px;color:var(--text-muted)">${ws.branch && ws.branch !== 'main' && ws.branch !== 'master' ? `Branch <code>${escapeHtml(ws.branch)}</code> ready for PR` : 'On default branch'}</span>
                  ${ws.branch && ws.branch !== 'main' && ws.branch !== 'master' ? `
                    <button class="filter-chip active" style="padding:4px 10px;font-size:11.5px" onclick="runPrAction('${escapeAttr(ws.workdir)}', 'create-pr')">+ Create PR</button>
                  ` : ''}
                `}
              </div>
            </div>
          ` : ''}

          <!-- Multi-Harness Second-Opinion & Arena Mode -->
          <div style="background:#232320;border:1px solid var(--border);border-radius:12px;padding:10px 12px;margin-bottom:12px">
            <div style="font-size:12px;font-weight:650;margin-bottom:7px;display:flex;align-items:center;justify-content:space-between">
              <span>🔍 Second-Opinion Diff Audit &amp; ⚔️ Arena</span>
              <span style="font-size:11px;color:var(--text-muted)">Cross-check across models</span>
            </div>
            <div style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:7px">
              ${installedHarnessIds().map(h => `<button class="filter-chip" onclick="triggerSecondOpinion('${h}', 'review-diff')">🔍 ${HARNESS_COLORS[h].short} Audit Diff</button>`).join('')}
            </div>
            <div style="display:flex;gap:6px;flex-wrap:wrap">
              ${installedHarnessIds().map(h => `<button class="filter-chip" onclick="triggerSecondOpinion('${h}', 'arena')">⚔️ Race ${HARNESS_COLORS[h].short}</button>`).join('')}
            </div>
          </div>

          <!-- Swarm Context Bounce & Handoff Pipeline -->
          <div id="swarmSection" style="background:#232320;border:1px solid var(--border);border-radius:12px;padding:10px 12px;margin-bottom:12px">
            <div style="font-size:12px;font-weight:650;margin-bottom:7px;display:flex;align-items:center;justify-content:space-between">
              <span>⚡ Swarm Context Bounce &amp; Worktree Pipeline</span>
              <span style="font-size:11px;color:var(--text-muted)">Handoff &amp; Sandboxed Fork</span>
            </div>
            <div style="font-size:11.5px;color:var(--text-muted);margin-bottom:8px">
              Distill current conversation tasks, goals &amp; relevant files into an isolated branch running a peer agent.
            </div>
            <div style="display:flex;gap:6px;flex-wrap:wrap;margin-bottom:8px">
              ${installedHarnessIds().map(h => {
                const isSelected = swarmTarget() === h;
                const hc = HARNESS_COLORS[h] || HARNESS_COLORS.claude;
                return `<button class="filter-chip ${isSelected ? 'active' : ''}" style="${isSelected ? `background:${hc.soft};border-color:${hc.accent};color:${hc.accent}` : ''}" onclick="setSwarmTarget('${h}')">${hc.short}</button>`;
              }).join('')}
            </div>
            <div style="display:flex;gap:6px;margin-bottom:8px">
              <input id="swarmInstructionsInput" placeholder="Optional custom instructions for peer agent..." style="flex:1;background:#191917;border:1px solid var(--border-strong);border-radius:9px;padding:6px 9px;color:var(--text);font-size:12px" />
              <button class="new-thread-btn" style="padding:5px 12px;font-size:11.5px" onclick="triggerSwarmBounce()">⚡ Bounce</button>
            </div>
            ${curSwarmSessions.length > 0 ? `
              <div style="margin-top:8px;font-size:11px;font-weight:600;color:var(--text-muted);margin-bottom:4px">Active Swarm Sessions (${curSwarmSessions.length})</div>
              <div style="display:flex;flex-direction:column;gap:5px;max-height:140px;overflow-y:auto">
                ${curSwarmSessions.map(s => {
                  const sc = s.status === 'completed' ? '#4ade80' : s.status === 'merged' ? '#a78bfa' : s.status === 'discarded' ? '#94a3b8' : s.status === 'failed' ? '#ef4444' : '#fbbf24';
                  return `
                    <div style="background:#191917;border:1px solid var(--border);border-radius:8px;padding:6px 8px;font-size:11.5px">
                      <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:4px">
                        <div style="display:flex;align-items:center;gap:6px">
                          <span style="font-weight:650">${escapeHtml(s.source_harness.toUpperCase())} ➔ ${escapeHtml(s.target_harness.toUpperCase())}</span>
                          <span style="background:${sc}22;color:${sc};border-radius:4px;padding:1px 5px;font-size:9.5px;font-weight:600">${escapeHtml(s.status.toUpperCase())}</span>
                        </div>
                        <div style="font-size:10.5px;font-family:ui-monospace,monospace;color:var(--text-muted)"><code>${escapeHtml(s.branch)}</code></div>
                      </div>
                      <div style="display:flex;gap:5px;margin-top:5px;flex-wrap:wrap">
                        <button class="filter-chip" style="padding:2px 7px;font-size:10.5px" onclick="viewSwarmDiff('${escapeAttr(s.id)}')">Diff</button>
                        ${s.status !== 'merged' && s.status !== 'discarded' ? `
                          <button class="new-thread-btn" style="padding:2px 8px;font-size:10.5px" onclick="mergeSwarmSession('${escapeAttr(s.id)}')">Merge 🚀</button>
                          <button class="filter-chip" style="padding:2px 7px;font-size:10.5px;color:#f87171" onclick="discardSwarmSession('${escapeAttr(s.id)}')">Discard</button>
                        ` : ''}
                        <button class="filter-chip" style="padding:2px 7px;font-size:10.5px" onclick="loadConversation('${escapeAttr(s.target_conversation_id)}', true)">Open →</button>
                      </div>
                    </div>
                  `;
                }).join('')}
              </div>
            ` : ''}
            <div id="swarmDiffContainer" style="display:none;margin-top:8px;padding:9px;border-radius:9px;background:#131312;border:1px solid var(--border-strong);max-height:220px;overflow:auto;font-family:ui-monospace,monospace;font-size:11px;line-height:1.42;white-space:pre"></div>
          </div>

          <!-- Git Worktree Parallel Sandboxes -->
          ${ws.is_git ? `
            <div style="background:#232320;border:1px solid var(--border);border-radius:12px;padding:10px 12px;margin-bottom:12px">
              <div style="font-size:12px;font-weight:650;margin-bottom:7px">🌱 Git Worktree Sandboxes (${worktrees.length})</div>
              <div style="display:flex;gap:6px;margin-bottom:8px">
                <input id="newWorktreeSlug" placeholder="New worktree slug (e.g. water-shader)" style="flex:1;background:#191917;border:1px solid var(--border-strong);border-radius:9px;padding:6px 9px;color:var(--text);font-size:12px" />
                <button class="new-thread-btn" style="padding:5px 10px;font-size:11.5px" onclick="createNewWorktree('${escapeAttr(ws.workdir)}')">+ Fork Worktree</button>
              </div>
              ${worktrees.length > 0 ? `
                <div style="max-height:110px;overflow-y:auto;display:flex;flex-direction:column;gap:4px">
                  ${worktrees.map(wt => {
                    const isCurWt = wt.path === curWorkdir;
                    return `
                      <div style="display:flex;align-items:center;justify-content:space-between;background:#191917;border:1px solid ${isCurWt ? 'var(--accent)' : 'var(--border)'};border-radius:8px;padding:6px 9px;font-size:11.5px">
                        <div style="min-width:0;cursor:pointer" onclick="pickWorkdir('${escapeAttr(wt.path)}')">
                          <strong>${escapeHtml(wt.branch)}</strong> <span style="color:var(--text-muted)">(${escapeHtml(wt.head)})</span>
                          <div style="font-size:10.5px;color:var(--text-dim);overflow:hidden;text-overflow:ellipsis">${escapeHtml(wt.path)}</div>
                        </div>
                        <div style="display:flex;gap:6px;align-items:center;flex-shrink:0">
                          <button class="filter-chip ${isCurWt ? 'active' : ''}" style="padding:2px 8px;font-size:10.5px" onclick="pickWorkdir('${escapeAttr(wt.path)}')">${isCurWt ? 'Active ✓' : 'Use'}</button>
                          ${!wt.is_main ? `<button class="filter-chip" style="padding:2px 7px;font-size:10.5px;color:#f87171" onclick="deleteWorktree('${escapeAttr(ws.workdir)}', '${escapeAttr(wt.path)}')">×</button>` : ''}
                        </div>
                      </div>
                    `;
                  }).join('')}
                </div>
              ` : ''}
            </div>
          ` : ''}

          <!-- Live Local Dev Server & Port Preview -->
          ${ports.length > 0 ? `
            <div style="background:#232320;border:1px solid var(--border);border-radius:12px;padding:10px 12px;margin-bottom:12px">
              <div style="font-size:12px;font-weight:650;margin-bottom:7px;display:flex;align-items:center;justify-content:space-between">
                <span>🌐 Live Local Dev Servers (${ports.length})</span>
                <span style="font-size:11px;color:var(--text-muted)">Reverse Proxy &amp; Sandbox</span>
              </div>
              <div style="display:flex;flex-direction:column;gap:5px">
                ${ports.map(pt => `
                  <div style="display:flex;align-items:center;justify-content:space-between;background:#191917;border:1px solid var(--border);border-radius:8px;padding:6px 9px;font-size:12px">
                    <span><strong>:${pt.port}</strong> · ${escapeHtml(pt.label)}</span>
                    <div style="display:flex;gap:5px;flex-wrap:wrap">
                      <button class="new-thread-btn" style="padding:3px 9px;font-size:11px" onclick="openSandboxModal(${pt.port})">📱 Sandbox</button>
                      <button class="filter-chip" style="padding:3px 8px;font-size:11px" onclick="togglePortPreview(${pt.port})">Inline</button>
                      <a href="/preview/${pt.port}/" target="_blank" class="filter-chip" style="padding:3px 8px;font-size:11px;text-decoration:none" title="Reverse proxy via MoE Deck port 7770">Proxy ↗</a>
                      <a href="http://${escapeAttr(window.location.hostname)}:${pt.port}" target="_blank" class="filter-chip" style="padding:3px 8px;font-size:11px;text-decoration:none" title="Direct connection">Direct ↗</a>
                    </div>
                  </div>
                `).join('')}
              </div>
              <div id="portPreviewWrap" style="display:none;margin-top:9px">
                <iframe id="portPreviewFrame" style="width:100%;height:320px;border:1px solid var(--border-strong);border-radius:10px;background:#fff"></iframe>
              </div>
            </div>
          ` : ''}

          ${prLinks.length > 0 ? `
            <div style="margin-bottom:12px">
              <div style="font-size:12px;font-weight:600;color:var(--text-muted);margin-bottom:6px">Recent Pull Requests</div>
              ${prLinks.map(url => `<a href="${escapeAttr(safeUrl(url))}" target="_blank" style="display:block;color:var(--accent);font-size:12.5px;margin-bottom:4px;text-decoration:underline">${escapeHtml(url)}</a>`).join('')}
            </div>
          ` : ''}

          <div style="font-size:12px;font-weight:600;color:var(--text-muted);margin-bottom:6px">Switch Project Directory</div>
          <div style="max-height:190px;overflow-y:auto">
            ${projects.map(p => {
              const isCur = p.workdir === curWorkdir;
              return `
                <div class="sheet-option ${isCur ? 'selected' : ''}" onclick="pickWorkdir('${escapeAttr(p.workdir)}')">
                  <div style="min-width:0">
                    <div style="font-weight:640;font-size:13px">${escapeHtml(p.name)} ${p.branch ? `<span style="font-size:11px;color:var(--text-muted)">(${escapeHtml(p.branch)})</span>` : ''}</div>
                    <div style="font-size:11px;color:var(--text-dim);overflow:hidden;text-overflow:ellipsis">${escapeHtml(p.workdir)}</div>
                  </div>
                  <span style="font-size:11.5px;color:var(--accent)">${isCur ? 'Current ✓' : 'Select'}</span>
                </div>
              `;
            }).join('')}
          </div>
        `;
      } catch (err) {
        if (version === sheetVersion) content.innerHTML = `<div style="color:var(--text-muted)">Could not load workspace info.</div>`;
      }
    }

    window.runPrAction = async function(workdir, action) {
      const res = await fetch('/api/pr-action', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ workdir, action, conversation_id: state.activeId })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        alert(data.error || 'PR action failed');
        return;
      }
      closeOverlays();
      await fetchState();
      if (state.activeId) await loadConversation(state.activeId, false);
    };

    window.triggerSecondOpinion = async function(harness, mode) {
      if (!state.activeId) return;
      closeOverlays();
      const res = await fetch(`/api/conversations/${encodeURIComponent(state.activeId)}/second-opinion`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ harness, mode })
      });
      if (res.ok) {
        const out = await res.json();
        await fetchState();
        if (out.conversation_id) {
          await loadConversation(out.conversation_id, true);
        }
      }
    };

    window.setSwarmTarget = function(harness) {
      state.swarmTargetHarness = harness;
      const sheet = document.getElementById('headerTitleWrap');
      if (sheet) sheet.click();
    };

    window.triggerSwarmBounce = async function() {
      if (!state.activeId) return;
      const harness = swarmTarget();
      if (!harness) return;
      const instructions = document.getElementById('swarmInstructionsInput')?.value?.trim() || null;
      const res = await fetch(`/api/conversations/${encodeURIComponent(state.activeId)}/bounce`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          target_harness: harness,
          custom_instructions: instructions,
          isolated_worktree: true
        })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        alert(data.error || 'Swarm bounce failed');
        return;
      }
      closeOverlays();
      await fetchState();
      if (data.session && data.session.target_conversation_id) {
        await loadConversation(data.session.target_conversation_id, true);
      }
    };

    window.viewSwarmDiff = async function(swarmId) {
      const container = document.getElementById('swarmDiffContainer');
      const res = await fetch(`/api/swarm/${encodeURIComponent(swarmId)}/diff`);
      if (res.ok) {
        const data = await res.json();
        if (container) {
          container.style.display = 'block';
          container.textContent = data.diff || '(No diff detected against base branch)';
        } else {
          alert(data.diff || 'No diff detected against base branch');
        }
      } else {
        const err = await res.json().catch(() => ({}));
        alert(err.error || 'Failed to fetch diff');
      }
    };

    window.mergeSwarmSession = async function(swarmId) {
      if (!confirm('Merge this swarm worktree branch back into the base project?')) return;
      const res = await fetch(`/api/swarm/${encodeURIComponent(swarmId)}/merge`, {
        method: 'POST'
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        alert(data.error || 'Merge failed');
        return;
      }
      alert(`Merged successfully: ${data.message || 'Clean merge'}`);
      closeOverlays();
      await fetchState();
      if (state.activeId) await loadConversation(state.activeId, false);
    };

    window.discardSwarmSession = async function(swarmId) {
      if (!confirm('Discard and delete this swarm sandbox worktree?')) return;
      const res = await fetch(`/api/swarm/${encodeURIComponent(swarmId)}/discard`, {
        method: 'POST'
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        alert(data.error || 'Discard failed');
        return;
      }
      closeOverlays();
      await fetchState();
      if (state.activeId) await loadConversation(state.activeId, false);
    };

    window.createNewWorktree = async function(workdir) {
      const inp = document.getElementById('newWorktreeSlug');
      if (!inp || !inp.value.trim()) return;
      const slug = inp.value.trim();
      const res = await fetch('/api/worktrees', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ workdir, slug, conversation_id: state.activeId })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) {
        alert(data.error || 'Failed to create worktree');
        return;
      }
      state.selectedWorkdir = data.path;
      closeOverlays();
      await fetchState();
    };

    window.deleteWorktree = async function(workdir, worktreePath) {
      const res = await fetch('/api/worktrees', {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ workdir, worktree_path: worktreePath })
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        alert(data.error || 'Failed to remove worktree');
        return;
      }
      closeOverlays();
      await fetchState();
    };

    let sandboxLogs = [];
    window.addEventListener('message', (event) => {
      if (event.data && event.data.type === 'MOEDECK_PREVIEW_CONSOLE') {
        const log = {
          level: event.data.level,
          messages: event.data.messages || [],
          time: new Date().toLocaleTimeString()
        };
        sandboxLogs.push(log);
        if (sandboxLogs.length > 200) sandboxLogs.shift();
        renderSandboxLogs();
      }
    });

    window.openSandboxModal = function(port) {
      const activePort = port || 3000;
      const url = `/preview/${activePort}/`;
      document.getElementById('sandboxUrlInput').value = url;
      document.getElementById('sandboxFrame').src = url;
      document.getElementById('sandboxModal').style.display = 'flex';
      setSandboxDevice('iphone');
    };

    window.closeSandboxModal = function() {
      document.getElementById('sandboxModal').style.display = 'none';
      document.getElementById('sandboxFrame').src = 'about:blank';
    };

    window.setSandboxDevice = function(device) {
      const frame = document.getElementById('sandboxDeviceFrame');
      const notch = document.getElementById('sandboxNotch');
      document.querySelectorAll('#deviceIphoneBtn, #deviceIpadBtn, #deviceFullBtn').forEach(b => b.classList.remove('active'));

      if (device === 'iphone') {
        document.getElementById('deviceIphoneBtn').classList.add('active');
        frame.style.width = '393px';
        frame.style.height = '852px';
        frame.style.borderRadius = '46px';
        frame.style.borderWidth = '12px';
        notch.style.display = 'block';
      } else if (device === 'ipad') {
        document.getElementById('deviceIpadBtn').classList.add('active');
        frame.style.width = '768px';
        frame.style.height = '1024px';
        frame.style.borderRadius = '24px';
        frame.style.borderWidth = '12px';
        notch.style.display = 'none';
      } else {
        document.getElementById('deviceFullBtn').classList.add('active');
        frame.style.width = '100%';
        frame.style.height = '100%';
        frame.style.borderRadius = '8px';
        frame.style.borderWidth = '1px';
        notch.style.display = 'none';
      }
    };

    let isLandscape = false;
    window.toggleSandboxOrientation = function() {
      const frame = document.getElementById('sandboxDeviceFrame');
      isLandscape = !isLandscape;
      const w = frame.style.width;
      const h = frame.style.height;
      if (w !== '100%') {
        frame.style.width = h;
        frame.style.height = w;
      }
    };

    window.refreshSandboxFrame = function() {
      const f = document.getElementById('sandboxFrame');
      if (f) f.src = f.src;
    };

    window.navigateSandboxUrl = function() {
      const val = document.getElementById('sandboxUrlInput')?.value?.trim();
      const f = document.getElementById('sandboxFrame');
      if (val && f) {
        f.src = val.startsWith('/') ? val : '/' + val;
      }
    };

    window.openSandboxExternal = function() {
      const val = document.getElementById('sandboxUrlInput')?.value?.trim() || '/preview/3000/';
      window.open(val, '_blank');
    };

    window.toggleSandboxQr = function() {
      const wrap = document.getElementById('sandboxQrWrap');
      if (wrap.style.display === 'none') {
        const val = document.getElementById('sandboxUrlInput')?.value?.trim() || '/preview/3000/';
        const portStr = window.location.port ? `:${window.location.port}` : '';
        const fullUrl = `${window.location.protocol}//${window.location.hostname}${portStr}${val}`;
        wrap.style.display = 'block';
        document.getElementById('sandboxQrContainer').innerHTML = `<img src="https://api.qrserver.com/v1/create-qr-code/?size=180x180&data=${encodeURIComponent(fullUrl)}" alt="QR Code" style="display:block;width:180px;height:180px" />`;
        document.getElementById('sandboxQrUrlText').textContent = fullUrl;
      } else {
        wrap.style.display = 'none';
      }
    };

    window.toggleSandboxConsole = function() {
      const d = document.getElementById('sandboxConsoleDrawer');
      d.style.display = (d.style.display === 'none' || !d.style.display) ? 'flex' : 'none';
    };

    window.clearSandboxLogs = function() {
      sandboxLogs = [];
      renderSandboxLogs();
    };

    function renderSandboxLogs() {
      const el = document.getElementById('sandboxConsoleLogs');
      const countEl = document.getElementById('sandboxLogCount');
      if (countEl) countEl.textContent = String(sandboxLogs.length);
      if (!el) return;
      el.innerHTML = sandboxLogs.map(l => {
        const color = l.level === 'error' ? '#f87171' : l.level === 'warn' ? '#fbbf24' : '#94a3b8';
        return `<div><span style="color:var(--text-dim)">[${escapeHtml(l.time)}]</span> <strong style="color:${color}">${escapeHtml(l.level.toUpperCase())}:</strong> ${escapeHtml(l.messages.join(' '))}</div>`;
      }).join('');
      el.scrollTop = el.scrollHeight;
    }

    window.togglePortPreview = function(port) {
      const wrap = document.getElementById('portPreviewWrap');
      const frame = document.getElementById('portPreviewFrame');
      if (!wrap || !frame) return;
      const targetUrl = `/preview/${port}/`;
      if (wrap.style.display === 'block' && frame.src.endsWith(targetUrl)) {
        wrap.style.display = 'none';
        frame.src = 'about:blank';
      } else {
        wrap.style.display = 'block';
        frame.src = targetUrl;
      }
    };

    window.saveConvTitle = async function(id) {
      const inp = document.getElementById('renameTitleInput');
      if (!inp || !inp.value.trim()) return;
      await fetch(`/api/conversations/${encodeURIComponent(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ title: inp.value.trim() })
      });
      closeOverlays();
      await fetchState();
    };

    window.togglePinConv = async function(id, pinned) {
      await fetch(`/api/conversations/${encodeURIComponent(id)}`, {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ pinned })
      });
      closeOverlays();
      await fetchState();
    };

    window.pickWorkdir = async function(workdir) {
      const targetId = state.activeId;
      if (targetId) {
        await apiRequest(`/api/conversations/${encodeURIComponent(targetId)}`, jsonRequest('PATCH', { workdir }));
      }
      if (state.activeId !== targetId) return;
      state.selectedWorkdir = workdir;
      localStorage.setItem('helm_last_workdir', workdir);
      closeOverlays();
      await fetchState();
    };

    window.viewGitDiff = async function(workdir) {
      const box = document.getElementById('gitDiffContainer');
      if (!box) return;
      if (box.style.display === 'block') {
        box.style.display = 'none';
        return;
      }
      box.style.display = 'block';
      box.textContent = 'Loading git diff…';
      try {
        const res = await fetch(`/api/git-diff?workdir=${encodeURIComponent(workdir)}`);
        if (!res.ok) {
          box.textContent = 'Failed to load diff.';
          return;
        }
        const data = await res.json();
        const raw = data.diff || 'No changes.';
        box.innerHTML = raw.split('\n').map(line => {
          if (line.startsWith('+') && !line.startsWith('+++')) {
            return `<span style="color:#4ade80">${escapeHtml(line)}</span>`;
          } else if (line.startsWith('-') && !line.startsWith('---')) {
            return `<span style="color:#f87171">${escapeHtml(line)}</span>`;
          } else if (line.startsWith('@@')) {
            return `<span style="color:#38bdf8">${escapeHtml(line)}</span>`;
          } else if (line.startsWith('diff --git')) {
            return `<strong style="color:var(--text)">${escapeHtml(line)}</strong>`;
          }
          return escapeHtml(line);
        }).join('\n');
      } catch (e) {
        box.textContent = 'Error loading diff.';
      }
    };

    function renderBounceStrip() {
      if (deferRender(renderBounceStrip)) return;
      const strip = document.getElementById('bounceStrip');
      const topConvs = state.conversations.slice(0, 6);
      replaceChangedHtml(strip, topConvs.map(c => {
        const hc = HARNESS_COLORS[c.harness] || HARNESS_COLORS.claude;
        const isActive = c.id === state.activeId;
        const hasApproval = Boolean(c.pending_approval);
        const busyClass = (c.status === 'busy' || hasApproval) ? 'busy' : '';
        return `
          <button class="bounce-tab ${isActive ? 'active' : ''}" onclick="selectConvFromBounce('${escapeAttr(c.id)}')" style="${isActive ? `border-color:${hasApproval ? '#ef4444' : hc.accent};background:${hasApproval ? 'rgba(239,68,68,0.2)' : hc.soft}` : ''}">
            <span class="bounce-dot ${busyClass}" style="background:${hasApproval ? '#ef4444' : hc.accent}"></span>
            <span class="harness-tag-mini" style="color:${hasApproval ? '#ef4444' : hc.accent}">${hasApproval ? '⚠️ APPR' : hc.short}</span>
            <span>${escapeHtml(c.title.length > 20 ? c.title.slice(0, 20) + '…' : c.title)}</span>
          </button>
        `;
      }).join(''));
    }

    window.selectConvFromBounce = function(id) {
      loadConversation(id, true);
    };

    // updated_at is seconds for some harnesses and milliseconds for others.
    const convTime = c => { const t = Number(c?.updated_at) || 0; return t < 1e12 ? t * 1000 : t; };

    function renderDrawerList() {
      if (deferRender(renderDrawerList)) return;
      updateDrawerFilterChips();
      const container = document.getElementById('convList');
      const keptScroll = container.scrollTop;
      const q = (state.drawerSearch || '').toLowerCase().trim();
      // Newest first; a pin only marks a chat, so old pins don't bury today's work.
      const archivedCount = state.conversations.filter(c => state.archived.has(c.id)).length;
      if (!archivedCount) state.showArchived = false;
      document.getElementById('convListHeading').textContent = state.showArchived ? 'Archived' : 'Conversations';
      const archivedToggle = state.showArchived
        ? `<button class="drawer-archived-toggle" onclick="toggleArchivedView()">← Back to chats</button>`
        : archivedCount ? `<button class="drawer-archived-toggle" onclick="toggleArchivedView()">Archived (${archivedCount})</button>` : '';
      const filtered = state.conversations.slice().sort((a, b) => convTime(b) - convTime(a)).filter(c => {
        if (state.archived.has(c.id) !== state.showArchived) return false;
        if (state.drawerFilter !== 'all' && c.harness !== state.drawerFilter) return false;
        if (!q) return true;
        return (
          (c.title || '').toLowerCase().includes(q) ||
          (c.subtitle || '').toLowerCase().includes(q) ||
          (c.last_preview || '').toLowerCase().includes(q) ||
          (c.profile_label || '').toLowerCase().includes(q) ||
          (c.harness || '').toLowerCase().includes(q)
        );
      });
      if (filtered.length === 0) {
        replaceChangedHtml(container, `
          <div style="padding:40px 18px;text-align:center;color:var(--text-muted)">
            <div style="font-size:32px;margin-bottom:8px">💬</div>
            <div style="font-weight:700;font-size:14px;color:var(--text);margin-bottom:4px">No conversations yet</div>
            <div style="font-size:12px;line-height:1.5;margin-bottom:14px">Start your first thread or run your agent CLI in the terminal.</div>
            <button class="new-thread-btn" onclick="document.getElementById('newThreadBtn').click()" style="display:inline-block">+ Start Thread</button>
          </div>
        ` + archivedToggle);
        return;
      }
      replaceChangedHtml(container, filtered.map(c => {
        const hc = HARNESS_COLORS[c.harness] || HARNESS_COLORS.claude;
        const isActive = c.id === state.activeId;
        const hasApproval = Boolean(c.pending_approval);
        return `
          <div class="conv-item ${isActive ? 'active' : ''}" data-conv-id="${escapeAttr(c.id)}" onclick="selectConvFromDrawer('${escapeAttr(c.id)}')">
            <div class="conv-item-top">
              <div style="display:flex;align-items:center;gap:7px;min-width:0">
                <span class="bounce-dot ${(c.status === 'busy' || hasApproval) ? 'busy' : ''}" style="background:${hasApproval ? '#ef4444' : hc.accent}"></span>
                <span class="conv-item-title">${c.pinned ? '<span aria-label="Pinned">📌</span> ' : ''}${escapeHtml(c.title)}</span>
              </div>
              <div style="display:flex;align-items:center;gap:5px">
                ${hasApproval ? `<span style="background:#ef4444;color:#fff;border-radius:6px;padding:1px 5px;font-size:9px;font-weight:750">⚠️ APPR</span>` : ''}
                ${c.profile_label ? `<span style="background:${hc.accent_soft};color:${hc.accent};border-radius:6px;padding:1px 5px;font-size:9px;font-weight:750">#${escapeHtml((String(c.profile_label).match(/\d+/) || ['?'])[0])}</span>` : ''}
                <span class="harness-tag-mini" style="color:${hc.accent}">${hc.short}</span>
              </div>
            </div>
            <div class="conv-item-preview">${hasApproval ? `⚠️ Awaiting security approval for tool` : escapeHtml(c.last_preview || c.subtitle)}</div>
          </div>
        `;
      }).join('') + archivedToggle);
      // The list re-renders on every poll; keep the user's scroll position.
      container.scrollTop = keptScroll;
    }

    window.selectConvFromDrawer = function(id) {
      // The tap that ends a long-press opened the actions sheet instead.
      if (drawerPress.fired) { drawerPress.fired = false; return; }
      closeOverlays();
      loadConversation(id, true);
    };

    // Long-press a chat in the drawer (or right-click) for Pin and Archive.
    const drawerPress = { timer: null, fired: false, x: 0, y: 0 };
    const convList = document.getElementById('convList');
    convList.addEventListener('pointerdown', e => {
      const item = e.target.closest('.conv-item');
      if (!item) return;
      drawerPress.fired = false; drawerPress.x = e.clientX; drawerPress.y = e.clientY;
      clearTimeout(drawerPress.timer);
      drawerPress.timer = setTimeout(() => { drawerPress.fired = true; navigator.vibrate?.(10); openConvActions(item.dataset.convId); }, 500);
    });
    convList.addEventListener('pointermove', e => {
      if (Math.hypot(e.clientX - drawerPress.x, e.clientY - drawerPress.y) > 10) clearTimeout(drawerPress.timer);
    });
    for (const end of ['pointerup', 'pointercancel', 'pointerleave']) convList.addEventListener(end, () => clearTimeout(drawerPress.timer));
    convList.addEventListener('contextmenu', e => {
      const item = e.target.closest('.conv-item');
      if (!item) return;
      e.preventDefault(); clearTimeout(drawerPress.timer); openConvActions(item.dataset.convId);
    });
    window.toggleArchivedView = function() {
      state.showArchived = !state.showArchived;
      renderDrawerList();
      document.getElementById('convList').scrollTop = 0;
    };
    window.openConvActions = function(id) {
      const c = state.conversations.find(c => c.id === id);
      if (!c) return;
      const archived = state.archived.has(id);
      document.getElementById('sheetContent').innerHTML = `
        <div class="sheet-title"><span>${escapeHtml(c.title)}</span></div>
        <div class="settings-group">
          <button class="settings-row" onclick="togglePinConv('${escapeAttr(id)}', ${!c.pinned})"><span class="settings-row-label">${c.pinned ? 'Unpin' : 'Pin'}</span><span aria-hidden="true">📌</span></button>
          <button class="settings-row" id="archiveConvBtn" onclick="setConvArchived('${escapeAttr(id)}', ${!archived})"><span class="settings-row-label">${archived ? 'Unarchive' : 'Archive'}<small>${archived ? 'Show it in the chat list again' : 'Hide it from the list. Nothing is deleted.'}</small></span><span aria-hidden="true">🗄</span></button>
        </div>`;
      openSheet();
    };
    window.setConvArchived = async function(id, archived) {
      try {
        await apiRequest(`/api/conversations/${encodeURIComponent(id)}`, jsonRequest('PATCH', { archived }));
        closeOverlays();
        await fetchState();
        showToast(archived ? 'Archived. It’s under Archived at the bottom of the list.' : 'Back in your chats.');
      } catch (e) { showToast(e.message || 'Could not archive this chat.'); }
    };

    window.loadFullHistory = function() {
      if (!state.activeId) return;
      state.fullHistoryId = state.activeId;
      loadConversation(state.activeId, false);
    };

    function refreshMessages() {
      if (state.detail) renderMessages(state.detail.messages, false);
    }
    function renderMessages(messages, forceScrollBottom) {
      if (!forceScrollBottom && deferRender(refreshMessages)) return;
      const truncated = (state.detail && state.detail.truncated) || 0;
      const tailText = messages.length ? JSON.stringify(messages[messages.length - 1]) : '';
      if (!forceScrollBottom && state.lastRenderedId === state.activeId && messages.length === state.lastMessageCount && tailText === state.lastTailText && truncated === state.lastTruncated) {
        return;
      }
      state.lastMessageCount = messages.length;
      state.lastRenderedId = state.activeId;
      state.lastTailText = tailText;
      state.lastTruncated = truncated;

      const scrollEl = document.getElementById('chatScroll');
      const wasNearBottom = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight < 140;

      const activeSwarm = (state.swarmSessions || []).find(s => s.target_conversation_id === state.activeId || s.source_conversation_id === state.activeId);
      let swarmBannerHtml = '';
      if (activeSwarm) {
        const isChild = activeSwarm.target_conversation_id === state.activeId;
        const statusColor = activeSwarm.status === 'completed' ? '#4ade80' : activeSwarm.status === 'merged' ? '#a78bfa' : activeSwarm.status === 'discarded' ? '#94a3b8' : activeSwarm.status === 'failed' ? '#ef4444' : '#fbbf24';
        swarmBannerHtml = `
          <div style="background:#20201d;border:1px solid var(--border-strong);border-radius:12px;padding:10px 12px;margin:8px 12px 14px;font-size:12px">
            <div style="display:flex;align-items:center;justify-content:space-between;gap:8px;margin-bottom:6px">
              <div style="display:flex;align-items:center;gap:6px">
                <span style="font-weight:700;color:var(--accent)">⚡ Swarm ${isChild ? 'Sandbox Peer' : 'Handoff Branch'}</span>
                <span style="background:${statusColor}22;color:${statusColor};border:1px solid ${statusColor}55;border-radius:6px;padding:1px 6px;font-size:10px;font-weight:600">${escapeHtml(activeSwarm.status.toUpperCase())}</span>
              </div>
              <div style="font-size:11px;font-family:ui-monospace,monospace;color:var(--text-muted)"><code>${escapeHtml(activeSwarm.branch)}</code></div>
            </div>
            <div style="font-size:11.5px;color:var(--text-muted);margin-bottom:8px">
              ${escapeHtml(activeSwarm.source_harness.toUpperCase())} ➔ <strong>${escapeHtml(activeSwarm.target_harness.toUpperCase())}</strong> · ${escapeHtml(activeSwarm.target_model)}
            </div>
            <div style="display:flex;gap:6px;flex-wrap:wrap">
              ${activeSwarm.branch && activeSwarm.branch !== 'HEAD' ? `
                <button class="filter-chip" onclick="viewSwarmDiff('${escapeAttr(activeSwarm.id)}')">View Diff</button>
                ${activeSwarm.status !== 'merged' && activeSwarm.status !== 'discarded' ? `
                  <button class="new-thread-btn" style="padding:3px 9px;font-size:11px" onclick="mergeSwarmSession('${escapeAttr(activeSwarm.id)}')">Merge to Base 🚀</button>
                  <button class="filter-chip" style="color:#f87171" onclick="discardSwarmSession('${escapeAttr(activeSwarm.id)}')">Discard</button>
                ` : ''}
              ` : ''}
              ${isChild ? `
                <button class="filter-chip" onclick="loadConversation('${escapeAttr(activeSwarm.source_conversation_id)}', true)">← Return to Source</button>
              ` : `
                <button class="filter-chip" onclick="loadConversation('${escapeAttr(activeSwarm.target_conversation_id)}', true)">Open Swarm Peer →</button>
              `}
            </div>
          </div>
        `;
      }

      const historyHtml = truncated > 0 ? `
        <div style="text-align:center;padding:10px 12px 4px">
          <button class="filter-chip" onclick="loadFullHistory()">↑ Load ${truncated} earlier message${truncated === 1 ? '' : 's'}</button>
        </div>` : '';

      const emptyStateHtml = messages.length === 0 ? `
        <div style="display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:55vh;gap:12px;padding:0 30px;text-align:center;color:var(--text-muted)">
          <img class="empty-mark" src="/icon.svg" alt="" aria-hidden="true" width="44" height="44" style="border-radius:11px">
          <div style="font-weight:750;color:var(--text);font-size:16px">What are we working on?</div>
          <div style="font-size:12.5px;max-width:310px;line-height:1.55">Choose a harness and model below, then describe what you want to do. Your projects and conversations stay together.</div>
          <div class="filter-chip" style="margin-top:4px;cursor:pointer" onclick="document.getElementById('composerInput').focus()">Start typing →</div>
        </div>` : '';

      scrollEl.innerHTML = emptyStateHtml + swarmBannerHtml + historyHtml + messages.map((m, idx) => renderTurn(m, idx)).join('');

      if (forceScrollBottom || wasNearBottom) {
        scrollEl.scrollTop = scrollEl.scrollHeight;
      }
    }

    // Legacy readers fill only text/thinking/tools; draw them as the same
    // timeline so every harness looks alike.
    function turnBlocks(m) {
      if (m.blocks && m.blocks.length) return m.blocks;
      const out = [];
      if (m.thinking && m.thinking.trim()) out.push({ type: 'thinking', text: m.thinking });
      for (const t of (m.tools || [])) out.push({ type: 'tool', name: t.name, summary: t.summary, status: t.status });
      const placeholder = m.streaming && /^(Thinking…|Working…|Running `)/.test(m.text || '');
      if (m.text && !placeholder) out.push({ type: 'text', text: m.text });
      return out;
    }

    function renderTurn(m, idx, readOnly = false) {
      const imgsHtml = (m.images || []).map(url =>
        `<img class="msg-img-card" src="${escapeAttr(url)}" onclick="openLightbox('${escapeAttr(url)}')" />`
      ).join('');
      const images = imgsHtml ? `<div class="msg-images">${imgsHtml}</div>` : '';
      if (m.role === 'user') {
        return `<div class="t-turn msg-user-turn"><div class="t-user"><span class="t-glyph">❯</span><span>${escapeHtml(m.text)}</span></div>${images}</div>`;
      }
      const hc = HARNESS_COLORS[m.harness] || HARNESS_COLORS.claude;
      const key = m.id || `idx-${idx}`;
      const lines = turnBlocks(m).map((b, bi) => {
        const bkey = `${key}:${bi}`;
        const open = state.expandedTools.has(bkey) ? ' open' : '';
        const toggle = `onclick="toggleBlock(this, '${escapeAttr(bkey)}')"`;
        if (b.type === 'text') {
          return `<div class="t-line t-text"><span class="t-glyph">●</span><div class="t-body prose">${renderMarkdown(b.text)}</div></div>`;
        }
        if (b.type === 'thinking') {
          const words = b.text.trim().split(/\s+/).length;
          return `<div class="t-line t-think${open}" ${toggle} role="button" tabindex="0" aria-expanded="${open ? 'true' : 'false'}"><span class="t-glyph">✻</span><div class="t-body">Thinking · ${words} words<div class="t-detail">${escapeHtml(b.text)}</div></div></div>`;
        }
        const status = String(b.status || 'done').replace(/[^a-z_]/g, '');
        const firstOut = (b.output || '').split('\n').find(l => l.trim()) || '';
        const peek = firstOut ? `<div class="t-peek">${escapeHtml(firstOut)}</div>` : '';
        const input = b.input && b.input !== b.summary ? `<div class="t-label">input</div><pre>${escapeHtml(b.input)}</pre>` : '';
        const output = b.output ? `<div class="t-label">output</div><pre class="t-out">${escapeHtml(b.output)}</pre>` : '';
        const detail = (input + output) || '<div class="t-label">No output recorded for this step. Load earlier messages for full history.</div>';
        return `<div class="t-line t-tool ${status}${open}" ${toggle} role="button" tabindex="0" aria-expanded="${open ? 'true' : 'false'}"><span class="t-glyph">●</span><div class="t-body"><span class="t-tool-name">${escapeHtml(b.name)}</span><span class="t-tool-arg">(${escapeHtml(b.summary)})</span>${peek}<div class="t-detail">${detail}</div></div></div>`;
      }).join('');
      const working = m.streaming ? `<div class="t-line t-working"><span class="t-glyph">✻</span><div class="t-body">Working…</div></div>` : '';
      return `
        <div class="t-turn msg-assistant">
          ${lines}${working}${images}
          ${m.streaming && !lines ? '' : `<div class="t-meta">
            <span style="color:${hc.accent}">${escapeHtml(hc.short)}</span>${m.model ? `<span>${escapeHtml(m.model)}</span>` : ''}
            ${readOnly ? '' : `<button onclick="copyText(${idx})" title="Copy">copy</button>
            <button onclick="speakText(${idx})" title="Read aloud">speak</button>`}
          </div>`}
        </div>`;
    }

    document.addEventListener('keydown', e => {
      const line = e.target.closest && e.target.closest('.t-line[role=button]');
      if (line && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); line.click(); }
    });

    window.toggleBlock = function(el, key) {
      if (getSelection && String(getSelection()).length) return; // let people select output
      const open = !state.expandedTools.has(key);
      if (open) state.expandedTools.add(key); else state.expandedTools.delete(key);
      el.classList.toggle('open', open);
      el.setAttribute('aria-expanded', open ? 'true' : 'false');
    };


    // ---- Multiplex panes -------------------------------------------------
    // Each pane is one conversation (one harness session). The focused pane
    // is state.activeId: it owns #chatScroll and receives the composer, like
    // the active pane in tmux. Phones show one pane and swipe between them.
    const MAX_PANES = 6;
    const isPhone = () => window.matchMedia('(max-width: 759px)').matches;
    let paneLayoutKey = '';

    const RECENT_PANE_MS = 7 * 24 * 3600 * 1000;

    function savePanes() {
      localStorage.setItem('helm_panes', JSON.stringify(state.panes));
      localStorage.setItem('helm_pinned_panes', JSON.stringify([...state.pinnedPanes]));
      localStorage.setItem('helm_dismissed_panes', JSON.stringify([...state.dismissedPanes]));
    }

    // One pane per harness: its newest chat from the last week (the newest
    // chat overall always counts), plus panes opened on purpose. When a
    // harness gets a newer chat, it takes the old one's place in the bar.
    function syncPanes(convs, now = Date.now()) {
      const byId = new Map(convs.map(c => [c.id, c]));
      for (const set of [state.pinnedPanes, state.dismissedPanes]) {
        for (const id of [...set]) if (!byId.has(id)) set.delete(id);
      }
      const newest = new Map();
      for (const c of convs) {
        const best = newest.get(c.harness);
        if (!best || convTime(c) > convTime(best)) newest.set(c.harness, c);
      }
      const overall = [...newest.values()].sort((a, b) => convTime(b) - convTime(a))[0];
      const auto = [...newest.values()]
        .filter(c => (c === overall || now - convTime(c) < RECENT_PANE_MS) && !state.dismissedPanes.has(c.id))
        .map(c => c.id);
      const wanted = new Set([...auto, ...state.pinnedPanes]);
      if (state.activeId && byId.has(state.activeId)) wanted.add(state.activeId);
      const panes = [];
      for (const id of state.panes) {
        if (wanted.has(id)) { if (!panes.includes(id)) panes.push(id); continue; }
        const harness = byId.get(id)?.harness;
        const successor = auto.find(a => byId.get(a).harness === harness && !state.panes.includes(a) && !panes.includes(a));
        if (successor) panes.push(successor);
      }
      for (const id of [...auto, ...state.pinnedPanes, ...wanted]) if (!panes.includes(id)) panes.push(id);
      while (panes.length > MAX_PANES) {
        const victim = panes.findIndex(p => p !== state.activeId && !state.pinnedPanes.has(p));
        panes.splice(victim < 0 ? panes.findIndex(p => p !== state.activeId) : victim, 1);
      }
      state.panes = panes;
      savePanes();
    }

    function addPane(id) {
      if (!id || state.panes.includes(id)) return;
      state.panes.push(id);
      // Drop the oldest pane that isn't the one being focused.
      while (state.panes.length > MAX_PANES) {
        const victim = state.panes.findIndex(p => p !== id);
        state.panes.splice(victim, 1);
      }
      savePanes();
    }

    function visiblePanes() {
      if (state.activeId && !state.panes.includes(state.activeId)) addPane(state.activeId);
      if (isPhone() || state.zoomed) return state.activeId ? [state.activeId] : [];
      return state.panes.slice();
    }

    function paneHeadHtml(id, idx) {
      const c = state.conversations.find(x => x.id === id);
      const hc = HARNESS_COLORS[c?.harness] || HARNESS_COLORS.claude;
      const project = c ? String(c.workdir || '').split('/').filter(Boolean).pop() || '' : '';
      const status = c?.pending_approval ? '<span style="color:var(--t-err)">approval</span>'
        : c?.status === 'busy' ? '<span style="color:var(--t-run)">working</span>' : '';
      const many = state.panes.length > 1;
      return `<button class="pane-label" draggable="true" data-pane-drag="${escapeAttr(id)}" title="Drag onto another pane to swap" onclick="focusPane('${escapeAttr(id)}')">`
        + `<span class="pane-idx">${idx}</span><b style="color:${hc.accent}">${escapeHtml(hc.short.toLowerCase())}</b>`
        + `<span class="pane-name">${escapeHtml(c?.title || 'loading…')}</span>${project ? `<span>${escapeHtml(project)}</span>` : ''}</button>`
        + `${status}<span class="pane-spacer"></span>`
        + (many && !isPhone() ? `<button class="pane-btn" onclick="toggleZoom('${escapeAttr(id)}')" aria-label="${state.zoomed ? 'Show all panes' : 'Zoom pane'}" title="${state.zoomed ? 'Show all panes' : 'Zoom pane'}">${state.zoomed ? '⤡' : '⤢'}</button>` : '')
        + (many ? `<button class="pane-btn" onclick="closePane('${escapeAttr(id)}')" aria-label="Close pane" title="Close pane (the conversation stays in the list)">×</button>` : '');
    }

    function renderPanes() {
      const grid = document.getElementById('paneGrid');
      const chat = document.getElementById('chatScroll');
      const ids = visiblePanes();
      if (!ids.length) { if (chat.parentElement !== grid) grid.replaceChildren(chat); renderTmuxBar(); return; }
      const key = JSON.stringify([ids, state.activeId, state.zoomed]);
      if (key !== paneLayoutKey) {
        paneLayoutKey = key;
        grid.dataset.count = String(ids.length);
        const sections = ids.map(id => {
          const sec = document.createElement('section');
          sec.className = 'pane';
          sec.dataset.paneId = id;
          sec.innerHTML = `<div class="pane-head"></div>`;
          if (id === state.activeId) {
            sec.classList.add('focused');
            sec.appendChild(chat);
          } else {
            const body = document.createElement('div');
            body.className = 'pane-scroll';
            body.onclick = () => focusPane(id);
            body.innerHTML = paneBodyHtml(id);
            sec.appendChild(body);
            requestAnimationFrame(() => { body.scrollTop = body.scrollHeight; });
          }
          return sec;
        });
        grid.replaceChildren(...sections);
      }
      grid.querySelectorAll('.pane').forEach(sec => {
        const id = sec.dataset.paneId;
        const c = state.conversations.find(x => x.id === id);
        sec.classList.toggle('busy', c?.status === 'busy');
        replaceChangedHtml(sec.querySelector('.pane-head'), paneHeadHtml(id, state.panes.indexOf(id)));
      });
      renderTmuxBar();
    }

    function paneBodyHtml(id) {
      const cached = state.paneCache.get(id);
      if (!cached) return '<div class="t-working">Loading…</div>';
      if (!cached.messages.length) return '<div class="t-working">No messages yet.</div>';
      return cached.messages.map((m, i) => renderTurn(m, i, true)).join('');
    }

    // Background panes refresh only when their conversation changed.
    let paneRefreshRunning = false;
    async function refreshBackgroundPanes() {
      if (paneRefreshRunning) return;
      paneRefreshRunning = true;
      try {
        for (const id of visiblePanes()) {
          if (id === state.activeId) continue;
          const c = state.conversations.find(x => x.id === id);
          const stamp = c ? `${c.updated_at}:${c.status}` : 'missing';
          const cached = state.paneCache.get(id);
          if (cached && cached.stamp === stamp) continue;
          const res = await fetch(`/api/conversations/${encodeURIComponent(id)}?tail=30`);
          if (!res.ok) continue;
          const detail = await res.json();
          state.paneCache.set(id, { stamp, messages: detail.messages || [] });
          const body = document.querySelector(`.pane[data-pane-id="${CSS.escape(id)}"] .pane-scroll`);
          if (body && !deferRender(refreshBackgroundPanes)) {
            const nearBottom = body.scrollHeight - body.scrollTop - body.clientHeight < 140;
            body.innerHTML = paneBodyHtml(id);
            if (nearBottom) body.scrollTop = body.scrollHeight;
          }
        }
      } catch (e) {
        console.error('Pane refresh error:', e);
      } finally {
        paneRefreshRunning = false;
      }
    }

    function renderTmuxBar() {
      const bar = document.getElementById('tmuxBar');
      bar.hidden = !state.tmuxBar;
      document.querySelector('.app-shell')?.classList.toggle('minimal', !state.tmuxBar);
      if (!state.tmuxBar) return;
      const time = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
      const wins = state.panes.map((id, i) => {
        const c = state.conversations.find(x => x.id === id);
        const hc = HARNESS_COLORS[c?.harness] || HARNESS_COLORS.claude;
        const active = id === state.activeId;
        const cls = ['tmux-win', active ? 'active' : '', c?.status === 'busy' ? 'busy' : '', c?.pending_approval ? 'approval' : ''].join(' ');
        const mark = active ? '*' : id === state.previousId ? '-' : '';
        const twin = state.panes.some(o => o !== id && state.conversations.find(x => x.id === o)?.harness === c?.harness);
        const name = hc.short.toLowerCase() + (twin && c?.title ? `:${c.title.slice(0, 10)}` : '');
        return `<button class="${cls}" onclick="focusPane('${escapeAttr(id)}')" aria-label="Window ${i}: ${escapeAttr(c?.title || hc.short)}" aria-current="${active ? 'true' : 'false'}">${i}:${escapeHtml(name)}${mark}</button>`;
      }).join('');
      replaceChangedHtml(bar, `<button onclick="openDrawer()" aria-label="Sessions" title="All sessions">[moedeck]</button>${wins}`
        + `<button onclick="document.getElementById('newThreadBtn').click()" aria-label="New pane" title="New conversation in a new pane">+</button>`
        + `<span class="tmux-fill"></span><button onclick="openQuickSettings()" aria-label="Quick settings" title="Quick settings">⚙<span class="tmux-mode"> ${escapeHtml(state.safetyMode || 'standard')}</span></button>`
        + `<span class="tmux-clock">${time}</span>`);
    }

    window.focusPane = function(id) {
      if (id && id !== state.activeId) loadConversation(id, true);
    };

    window.closePane = function(id) {
      const i = state.panes.indexOf(id);
      if (i < 0 || state.panes.length < 2) return;
      state.panes.splice(i, 1);
      state.paneCache.delete(id);
      if (!state.pinnedPanes.delete(id)) state.dismissedPanes.add(id);
      savePanes();
      if (id === state.activeId) {
        state.zoomed = false;
        loadConversation(state.panes[Math.min(i, state.panes.length - 1)], true);
      } else {
        renderPanes();
      }
    };

    window.toggleZoom = function(id) {
      state.zoomed = !state.zoomed;
      if (id !== state.activeId) loadConversation(id, true);
      else renderPanes();
      refreshBackgroundPanes();
    };

    window.openDrawer = function() {
      document.getElementById('openDrawerBtn').click();
    };

    function cyclePane(step) {
      if (state.panes.length < 2) return;
      const i = Math.max(0, state.panes.indexOf(state.activeId));
      const next = state.panes[(i + step + state.panes.length) % state.panes.length];
      const grid = document.getElementById('paneGrid');
      grid.classList.remove('swipe-next', 'swipe-prev');
      void grid.offsetWidth;
      grid.classList.add(step > 0 ? 'swipe-next' : 'swipe-prev');
      loadConversation(next, true);
    }

    // Phone: swipe sideways on a pane to switch windows. Ignore swipes that
    // start in horizontally scrollable content (code, images) or at the
    // left edge, which opens the drawer.
    (() => {
      const grid = document.getElementById('paneGrid');
      let sx = 0, sy = 0, tracking = false;
      // Pointer events plus `touch-action: pan-y` keep the browser from
      // claiming sideways gestures, so the swipe isn't cancelled on iOS.
      grid.addEventListener('pointerdown', e => {
        if (e.pointerType === 'mouse' || !e.isPrimary) { tracking = false; return; }
        sx = e.clientX; sy = e.clientY;
        tracking = sx > 40 && !e.target.closest('pre, .msg-images');
      }, { passive: true });
      grid.addEventListener('pointercancel', () => { tracking = false; });
      grid.addEventListener('pointerup', e => {
        if (!tracking || !e.isPrimary) return;
        tracking = false;
        const dx = e.clientX - sx, dy = e.clientY - sy;
        if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy) * 1.8) cyclePane(dx < 0 ? 1 : -1);
      }, { passive: true });

      // Desktop: drag a pane label onto another pane to swap them.
      grid.addEventListener('dragstart', e => {
        const id = e.target.closest?.('[data-pane-drag]')?.dataset.paneDrag;
        if (id) e.dataTransfer.setData('text/helm-pane', id);
      });
      grid.addEventListener('dragover', e => {
        const pane = e.target.closest?.('.pane');
        if (!pane || !e.dataTransfer.types.includes('text/helm-pane')) return;
        e.preventDefault();
        grid.querySelectorAll('.drop-target').forEach(p => p !== pane && p.classList.remove('drop-target'));
        pane.classList.add('drop-target');
      });
      grid.addEventListener('dragleave', e => e.target.closest?.('.pane')?.classList.remove('drop-target'));
      grid.addEventListener('drop', e => {
        const pane = e.target.closest?.('.pane');
        grid.querySelectorAll('.drop-target').forEach(p => p.classList.remove('drop-target'));
        const from = e.dataTransfer.getData('text/helm-pane');
        if (!pane || !from) return;
        e.preventDefault();
        const a = state.panes.indexOf(from), b = state.panes.indexOf(pane.dataset.paneId);
        if (a < 0 || b < 0 || a === b) return;
        [state.panes[a], state.panes[b]] = [state.panes[b], state.panes[a]];
        savePanes();
        renderPanes();
      });

      // Alt+1..9 focuses a window; Alt+Z zooms (tmux prefix-z).
      document.addEventListener('keydown', e => {
        if (!e.altKey || e.ctrlKey || e.metaKey) return;
        if (/^Digit[0-9]$/.test(e.code)) {
          const id = state.panes[Number(e.code.slice(5))];
          if (id) { e.preventDefault(); focusPane(id); }
        } else if (e.code === 'KeyZ' && state.activeId) {
          e.preventDefault(); toggleZoom(state.activeId);
        }
      });
      window.matchMedia('(max-width: 759px)').addEventListener('change', () => { renderPanes(); refreshBackgroundPanes(); });
    })();

    window.openQuickSettings = function() {
      openSheet();
      const size = localStorage.getItem('helm_term_font') || '13.5px';
      const mode = state.safetyMode || 'standard';
      const chip = (on, label, action) => `<button class="filter-chip ${on ? 'active' : ''}" aria-pressed="${on}" onclick="${action}">${label}</button>`;
      document.getElementById('sheetContent').innerHTML = `
        <div class="sheet-title"><span>Quick settings</span></div>
        <div class="quick-row"><span>Permissions</span><div class="quick-choices">
          ${['safe', 'standard', 'yolo'].map(m => chip(mode === m, m, `setSafetyMode('${m}').then(openQuickSettings)`)).join('')}
        </div></div>
        <div class="quick-row"><span>Text size</span><div class="quick-choices">
          ${[['12px', 'S'], ['13.5px', 'M'], ['15px', 'L']].map(([v, l]) => chip(size === v, l, `setTermFont('${v}')`)).join('')}
        </div></div>
        <div class="quick-row"><span>Walkie-talkie mic</span><div class="quick-choices">${chip(state.walkieTalkie, state.walkieTalkie ? 'on' : 'off', 'toggleWalkieTalkieMode();openQuickSettings()')}</div></div>
        <div class="quick-row"><span>Alerts when a turn finishes</span><div class="quick-choices">${chip(typeof Notification !== 'undefined' && Notification.permission === 'granted', 'enable', 'enableNotifications()')}</div></div>
        <div class="quick-row"><span>Window bar</span><div class="quick-choices">${chip(state.tmuxBar, state.tmuxBar ? 'on' : 'off', 'toggleTmuxBar()')}</div></div>
        <div class="quick-row"><span>Panes</span><div class="quick-choices">${chip(false, 'reset layout', 'resetPanes()')}</div></div>
        <div class="quick-row"><span>Everything else</span><div class="quick-choices">${chip(false, 'Settings ›', 'openSettings()')}</div></div>`;
    };

    window.toggleTmuxBar = function() {
      state.tmuxBar = !state.tmuxBar;
      localStorage.setItem('helm_tmux_bar', state.tmuxBar ? 'on' : 'off');
      paneLayoutKey = '';
      renderPanes();
      openQuickSettings();
    };

    window.setTermFont = function(size) {
      localStorage.setItem('helm_term_font', size);
      document.documentElement.style.setProperty('--term-font', size);
      openQuickSettings();
    };
    if (localStorage.getItem('helm_term_font')) document.documentElement.style.setProperty('--term-font', localStorage.getItem('helm_term_font'));

    window.resetPanes = function() {
      state.panes = [];
      state.pinnedPanes.clear();
      state.dismissedPanes.clear();
      state.zoomed = false;
      syncPanes(state.conversations);
      renderPanes();
      closeOverlays();
    };

    window.copyText = async function(idx) {
      if (!state.detail || !state.detail.messages[idx]) return;
      try {
        if (navigator.clipboard) {
          await navigator.clipboard.writeText(state.detail.messages[idx].text);
        } else {
          const field = document.createElement('textarea');
          field.value = state.detail.messages[idx].text;
          field.style.cssText = 'position:fixed;top:0;left:-9999px';
          document.body.appendChild(field);
          field.select();
          const copied = document.execCommand('copy');
          field.remove();
          if (!copied) throw new Error('Copy is unavailable in this browser.');
        }
        showToast('Copied.');
      } catch (err) { showToast(err.message || 'Could not copy this message.'); }
    };

    window.speakText = function(idx) {
      if (!state.detail || !state.detail.messages[idx] || !('speechSynthesis' in window)) return;
      window.speechSynthesis.cancel();
      const u = new SpeechSynthesisUtterance(state.detail.messages[idx].text);
      window.speechSynthesis.speak(u);
    };

    window.openLightbox = function(url) {
      document.getElementById('lightboxImg').src = url;
      document.getElementById('lightbox').classList.add('open');
    };
    document.getElementById('lightbox').addEventListener('click', () => {
      document.getElementById('lightbox').classList.remove('open');
    });

    function renderMarkdown(md) {
      if (!md) return '';
      const blocks = md.split(/```([\s\S]*?)```/g);
      let html = '';
      for (let i = 0; i < blocks.length; i++) {
        if (i % 2 === 1) {
          const code = blocks[i].replace(/^[a-zA-Z0-9_-]+\n/, '');
          html += `<pre><code>${escapeHtml(code)}</code></pre>`;
        } else {
          html += renderInlineBlocks(blocks[i]);
        }
      }
      return html;
    }

    function renderInlineBlocks(text) {
      const lines = text.split('\n');
      let out = '';
      let listMode = null; // 'ul' | 'ol' | null

      for (const rawLine of lines) {
        const line = rawLine.trimEnd();
        if (line.trim() === '---') continue;

        const ulMatch = line.match(/^\s*[-*•]\s+(.*)$/);
        const olMatch = line.match(/^\s*\d+\.\s+(.*)$/);

        if (ulMatch) {
          if (listMode !== 'ul') {
            if (listMode) out += `</${listMode}>`;
            out += '<ul>';
            listMode = 'ul';
          }
          out += `<li>${formatInline(ulMatch[1])}</li>`;
        } else if (olMatch) {
          if (listMode !== 'ol') {
            if (listMode) out += `</${listMode}>`;
            out += '<ol>';
            listMode = 'ol';
          }
          out += `<li>${formatInline(olMatch[1])}</li>`;
        } else {
          if (listMode) {
            out += `</${listMode}>`;
            listMode = null;
          }
          if (line.trim() === '') {
            continue;
          } else if (line.startsWith('#### ')) {
            out += `<h3>${formatInline(line.slice(5))}</h3>`;
          } else if (line.startsWith('### ')) {
            out += `<h3>${formatInline(line.slice(4))}</h3>`;
          } else if (line.startsWith('## ')) {
            out += `<h2>${formatInline(line.slice(3))}</h2>`;
          } else {
            out += `<p>${formatInline(line)}</p>`;
          }
        }
      }
      if (listMode) out += `</${listMode}>`;
      return out;
    }

    function formatInline(str) {
      // Strip file:// markdown links cleanly into compact code badges before escaping
      let cleaned = String(str || '').replace(/\[`?([^\]`]+)`?\]\(file:\/\/[^\s)]+\)/g, '`$1`');
      let s = escapeHtml(cleaned);
      s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
      s = s.replace(/`([^`]+)`/g, '<code>$1</code>');
      s = s.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
      return s;
    }

    function escapeHtml(s) {
      return String(s || '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
    }
    function escapeAttr(s) {
      return escapeHtml(s).replace(/'/g, '&#39;');
    }

    let settingsData = null;
    let settingsHarnessId = null;
    async function openSettings() {
      closeOverlays();
      const content = document.getElementById('sheetContent');
      content.innerHTML = '<div class="sheet-title">Settings</div><p>Loading…</p>';
      openSheet();
      document.getElementById('bottomSheet').classList.add('settings-sheet');
      try {
        const res = await fetch('/api/settings');
        if (!res.ok) throw new Error('Could not load settings');
        settingsData = await res.json();
        renderSettings();
      } catch (e) { content.innerHTML = `<div class="sheet-title">Settings</div><p role="alert">${escapeHtml(e.message)}</p>`; }
    }
    function settingsHeader(title, back = false) {
      return `<div class="sheet-title">${back ? '<button class="settings-back" onclick="renderSettings()" aria-label="Back to settings">←</button>' : ''}<span>${escapeHtml(title)}</span></div>`;
    }
    function renderSettings() {
      const installed = matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
      const secure = window.isSecureContext;
      document.getElementById('sheetContent').innerHTML = settingsHeader('Settings') + `
        <section class="settings-section">
          <div class="settings-section-heading"><h2>Harnesses & accounts</h2><button class="filter-chip" onclick="showAddHarness()">＋ Add harness</button></div>
          <p class="settings-help">Choose the agents available in your composer. Manage their accounts and models here.</p>
          <div class="settings-group">${settingsData.harnesses.filter(h => settingsData.settings.harnesses[h.id]?.enabled ?? h.available).map(h => `
            <button class="settings-row" onclick="openHarnessSettings('${escapeAttr(h.id)}')">
              <span class="bounce-dot" style="background:${h.accent}"></span><span class="settings-row-label">${escapeHtml(h.name)}<small>${h.available ? 'Installed' : 'Not installed'} · ${h.models.length>1 ? `${h.models.length-1} model choices` : 'Configured default'}</small></span><span aria-hidden="true">›</span>
            </button>`).join('') || '<p class="settings-help">No harnesses enabled. Add one to get started.</p>'}</div>
          <button class="settings-secondary" id="refreshCatalogBtn" onclick="refreshCatalogs()">Refresh model lists</button>
          <p class="settings-help" id="settingsRefreshStatus" role="status"></p>
        </section>
        <section class="settings-section"><h2>This app</h2><div class="settings-group">
          <div class="settings-row"><span class="settings-row-label">Connection</span><span>${location.protocol==='https:' ? 'HTTPS · secure' : secure ? 'Local · secure' : 'HTTP · limited'}</span></div>
          <div class="settings-row"><span class="settings-row-label">Launch mode</span><span>${installed ? 'Installed app' : 'Browser'}</span></div>
          <div class="settings-row"><span class="settings-row-label">Offline shell</span><span>${navigator.serviceWorker?.controller ? 'Ready' : secure ? 'Preparing' : 'Needs HTTPS'}</span></div>
          <div class="settings-row"><span class="settings-row-label">MoE Deck version</span><span id="helmVersion">…</span></div>
        </div><p class="settings-help">On iPhone: open the HTTPS address in Safari, then Share → Add to Home Screen → Open as Web App. An HTTP shortcut cannot use the offline shell.</p>
        <div id="helmUpdate" role="status"></div>
        <button class="settings-secondary" id="checkUpdateBtn" onclick="checkHelmUpdate(this)">Check for updates</button></section>
        <section class="settings-section"><h2>Voice & permissions</h2><div class="settings-group">
          <label class="settings-row"><span class="settings-row-label">Walkie-talkie<small>Send voice input when you release the mic</small></span><input type="checkbox" ${state.walkieTalkie ? 'checked' : ''} onchange="toggleWalkieTalkieMode()" /></label>
          <label class="settings-row"><span class="settings-row-label">Task sounds</span><input type="checkbox" ${state.audioChimes ? 'checked' : ''} onchange="state.audioChimes=this.checked;localStorage.setItem('helm_chimes',this.checked)" /></label>
          <label class="settings-row"><span class="settings-row-label">Permission mode</span><select onchange="setSafetyMode(this.value)">${['safe','standard','yolo'].map(m=>`<option ${m===state.safetyMode?'selected':''} value="${m}">${m}</option>`).join('')}</select></label>
        </div><p class="settings-help">Yolo bypasses approvals and sandbox protections. Use Standard for everyday work.</p></section>`;
      refreshHelmVersion();
    }
    window.openSettings = openSettings;
    window.showAddHarness = function() {
      document.getElementById('sheetContent').innerHTML = settingsHeader('Add a harness', true) + '<p class="settings-help">These are the supported adapters. Adding a harness enables it here; its CLI must already be installed on the computer.</p>' + settingsData.harnesses.map(h=>`<button class="settings-row" onclick="openHarnessSettings('${escapeAttr(h.id)}')"><span class="bounce-dot" style="background:${h.accent}"></span><span class="settings-row-label">${escapeHtml(h.name)}<small>${h.available?'Installed':'Needs installation on computer'}</small></span><span>›</span></button>`).join('');
    };
    window.openHarnessSettings = function(id) {
      const h = settingsData.harnesses.find(h=>h.id===id);
      if (!h) return;
      const config = settingsData.settings.harnesses[id] || {enabled:!!h?.available, models:[]};
      const catalog = settingsData.catalogs[id];
      settingsHarnessId = id;
      const source = id==='claude' ? 'Stable CLI aliases · version selected by Claude' : id==='codex' ? 'Codex local catalog · primary account' : id==='keel' ? 'Local & OpenAI-compatible providers · built-in local models plus your own endpoints' : catalog?.updated_at ? `Native catalog · ${new Date(catalog.updated_at).toLocaleString()}` : 'Configured default · no discovered catalog';
      document.getElementById('sheetContent').innerHTML = settingsHeader(h.name,true) + `
        <label class="settings-row"><span class="settings-row-label">Show in composer<small>${h.available?'Installed on this computer':'Install its CLI on the computer first'}</small></span><input id="harnessEnabled" type="checkbox" ${config.enabled?'checked':''} /></label>
        <section class="settings-section"><h2>Accounts</h2><p class="settings-help">${id==='codex'?'Model discovery currently uses the primary account. Other accounts may have different access.':''}</p>${accountRowHtml(h)}</section>
        <section class="settings-section"><h2>Models</h2><p class="settings-help">${escapeHtml(source)}</p>${catalog?.error?`<p class="settings-help" role="status">${escapeHtml(catalog.error)}</p>`:''}
          <div class="settings-models">${h.models.map(m=>`<div class="settings-model"><strong>${escapeHtml(m.label)}</strong><small>${escapeHtml(m.id)}${m.base_url?` · ${escapeHtml(m.base_url)}`:''}${m.thinking===false?' · no thinking':''}</small></div>`).join('')}</div>
          <h3>Custom model choices</h3><p class="settings-help">Use exact IDs from your provider. They pass to the harness unchanged. The configured default preserves that harness’s settings.${id==='keel'?' For Keel, set a base URL for any OpenAI-compatible <code>/v1</code> endpoint; the API key stays in an environment variable on the MoE Deck machine.':''}</p>
          <div id="customModels"></div><button class="settings-secondary" onclick="addCustomModelRow()">＋ Add model</button>
        </section>
        <p id="settingsSaveStatus" class="settings-help" role="status"></p>
        <button class="settings-save" onclick="saveHarnessSettings('${escapeAttr(id)}',this)">Save changes</button>`;
      for (const m of config.models) addCustomModelRow(m);
    };
    window.addCustomModelRow = function(model = {}) {
      const row = document.createElement('div');row.className='settings-model-editor';
      const thinking = model.thinking === true ? '1' : model.thinking === false ? '0' : '';
      const provider = settingsHarnessId === 'keel' ? `
        <label>Base URL (optional)<input name="modelBaseUrl" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="http://127.0.0.1:8080/v1" value="${escapeAttr(model.base_url||'')}" /></label>
        <label>API key env var (optional)<input name="modelApiKeyEnv" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="DEEPSEEK_API_KEY" value="${escapeAttr(model.api_key_env||'')}" /></label>
        <label>Thinking<select name="modelThinking"><option value="" ${thinking===''?'selected':''}>Harness default</option><option value="1" ${thinking==='1'?'selected':''}>On</option><option value="0" ${thinking==='0'?'selected':''}>Off</option></select></label>` : '';
      row.innerHTML=`<label>Model ID<input name="modelId" autocomplete="off" autocapitalize="none" spellcheck="false" placeholder="Exact provider model ID" value="${escapeAttr(model.id||'')}" /></label><label>Display name<input name="modelLabel" value="${escapeAttr(model.label||'')}" placeholder="Name in your picker" /></label>${provider}<button class="settings-secondary" onclick="this.parentElement.remove()">Remove choice</button>`;
      document.getElementById('customModels').appendChild(row);
    };
    window.saveHarnessSettings = async function(id, button) {
      button.disabled=true;
      const status=document.getElementById('settingsSaveStatus');status.textContent='Saving…';
      const next=structuredClone(settingsData.settings);
      next.harnesses[id]={enabled:document.getElementById('harnessEnabled').checked,models:[...document.querySelectorAll('.settings-model-editor')].map(row=>{
        const model={id:row.querySelector('[name=modelId]').value.trim(),label:row.querySelector('[name=modelLabel]').value.trim(),default_effort:'default',efforts:['default']};
        const baseUrl=row.querySelector('[name=modelBaseUrl]')?.value.trim();
        const apiKeyEnv=row.querySelector('[name=modelApiKeyEnv]')?.value.trim();
        const thinking=row.querySelector('[name=modelThinking]')?.value;
        if(baseUrl)model.base_url=baseUrl;
        if(apiKeyEnv)model.api_key_env=apiKeyEnv;
        if(thinking==='1')model.thinking=true;else if(thinking==='0')model.thinking=false;
        return model;
      })};
      try {
        const res=await fetch('/api/settings',{method:'PUT',headers:{'Content-Type':'application/json'},body:JSON.stringify(next)});
        const data=await res.json();if(!res.ok)throw Error(data.error||'Could not save');
        settingsData.settings=next;await fetchState();
        const fresh=await fetch('/api/settings');if(fresh.ok)settingsData=await fresh.json();
        status.textContent='Saved';
      } catch(e){status.textContent=e.message;}
      button.disabled=false;
    };
    window.refreshCatalogs = async function() {
      const button=document.getElementById('refreshCatalogBtn');button.disabled=true;
      const status=document.getElementById('settingsRefreshStatus');status.textContent='Checking native model lists…';
      try {
        const res=await fetch('/api/catalog/refresh',{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'});
        if(!res.ok)throw Error('Model refresh failed');
        const fresh=await fetch('/api/settings');settingsData=await fresh.json();await fetchState();renderSettings();
        document.getElementById('settingsRefreshStatus').textContent=Object.values(settingsData.catalogs).some(c=>c.error)?'Some catalogs could not refresh. Open the harness for details.':'Model lists refreshed. Codex uses its local CLI catalog.';
      }catch(e){status.textContent=e.message;button.disabled=false;}
    };
    // MoE Deck updates itself from origin/main: build, swap the binary, restart.
    const shortSha = sha => sha ? sha.slice(0,7) : 'unknown';
    function showHelmUpdate(html) { const el=document.getElementById('helmUpdate'); if (el) el.innerHTML=html; }
    async function refreshHelmVersion() {
      try { const s=await (await fetch('/api/update/status')).json(); const el=document.getElementById('helmVersion'); if (el) el.textContent=shortSha(s.current); if (s.phase!=='idle') followHelmUpdate(s.current); } catch {}
    }
    window.checkHelmUpdate = async function(button) {
      button.disabled=true; showHelmUpdate('<p class="settings-help">Checking main…</p>');
      try {
        const res=await fetch('/api/update'); const c=await res.json();
        if (!res.ok) throw Error(c.error||'Could not check for updates');
        if (c.up_to_date) { showHelmUpdate(`<p class="settings-help">Up to date (${shortSha(c.current)}).</p>`); navigator.serviceWorker?.getRegistration().then(r=>r?.update()); return; }
        const n=c.commits.length;
        showHelmUpdate(`<p class="settings-help">${n} new ${n===1?'change':'changes'} on main:</p><ul class="settings-help">${c.commits.map(l=>`<li>${escapeHtml(l.replace(/^\S+ /,''))}</li>`).join('')}</ul><button class="settings-save" onclick="startHelmUpdate(this,false)">Update &amp; restart</button>`);
      } catch(e) { showHelmUpdate(`<p class="settings-help" role="alert">${escapeHtml(e.message)}</p>`); }
      finally { button.disabled=false; }
    };
    window.startHelmUpdate = async function(button, force) {
      button.disabled=true;
      try {
        const res=await fetch('/api/update',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({force})});
        const s=await res.json(); if (!res.ok) throw Error(s.error||'Could not start the update');
        followHelmUpdate(s.current);
      } catch(e) { button.disabled=false; showHelmUpdate(`<p class="settings-help" role="alert">${escapeHtml(e.message)}</p>`); }
    };
    let followingUpdate=false;
    async function followHelmUpdate(from) {
      if (followingUpdate) return; followingUpdate=true;
      try {
        for (;;) {
          let s=null; try { s=await (await fetch('/api/update/status')).json(); } catch {}
          if (s && s.current!==from) { showHelmUpdate(`<p class="settings-help">Updated to ${shortSha(s.current)}. Reloading…</p>`); const r=await navigator.serviceWorker?.getRegistration(); await r?.update().catch(()=>{}); location.reload(); return; }
          if (s?.phase==='failed') { showHelmUpdate(`<p class="settings-help" role="alert">${escapeHtml(s.message)}</p>${s.log?`<pre class="settings-help" style="white-space:pre-wrap;overflow:auto;max-height:40vh">${escapeHtml(s.log)}</pre>`:''}`); return; }
          if (s) showHelmUpdate(`<p class="settings-help">${escapeHtml(s.message)}</p>${s.phase==='waiting'?'<button class="settings-secondary" onclick="startHelmUpdate(this,true)">Restart now</button>':''}`);
          else showHelmUpdate('<p class="settings-help">Restarting MoE Deck…</p>');
          await new Promise(r=>setTimeout(r,2000));
        }
      } finally { followingUpdate=false; }
    }

    // Harness Selector Sheet
    document.getElementById('harnessPillBtn').addEventListener('click', () => {
      const content = document.getElementById('sheetContent');
      content.innerHTML = `
        <div class="sheet-title">
          <span>Choose a harness</span>
          <span style="font-size:11.5px;color:var(--text-muted)">Switch mid-thread</span>
        </div>
        ${state.harnesses.filter(h => h.available).map(h => {
          const isSel = h.id === state.selectedHarness;
          return `
            <button type="button" aria-pressed="${isSel}" class="sheet-option ${isSel ? 'selected' : ''}" onclick="pickHarness('${escapeAttr(h.id)}')" style="${isSel ? 'background:#2b2b2e' : ''}">
              <div style="display:flex;align-items:center;gap:10px">
                <span class="bounce-dot" style="width:10px;height:10px;background:${h.accent}"></span>
                <div>
                  <div style="font-weight:650;color:var(--text)">${escapeHtml(h.name)}</div>
                  <div style="font-size:12px;color:var(--text-muted)">${h.available ? 'Ready on this computer' : 'Not installed'}</div>
                </div>
              </div>
              <span style="font-size:12px;color:${h.accent};font-weight:600">${isSel ? 'Active ✓' : 'Select'}</span>
            </button>

          `;
        }).join('')}
        <button class="settings-secondary" onclick="openClaudeRemoteControl()">Claude app · Remote Control</button>
        <button class="settings-secondary" onclick="openSettings()">Manage harnesses & accounts</button>
      `;
      openSheet();
    });

    let rcPollTimer = null;
    async function openClaudeRemoteControl(autoStart = false) {
      closeOverlays();
      const content = document.getElementById('sheetContent');
      const profile = state.selectedHarness === 'claude' ? state.selectedProfile : null;
      const workdir = state.selectedWorkdir === state.home ? '' : state.selectedWorkdir;
      const accounts = state.profiles.filter(p => p.harness === 'claude');
      content.innerHTML = `<div class="sheet-title">Claude Remote Control</div>
        <p class="settings-help">Start a separate Claude session in your project, then use it from the Claude app’s Code tab. MoE Deck can read its local conversation history.</p>
        <div id="rcSessions" class="rc-actions"></div>
        <form id="rcStartForm" class="project-form">
          <label>Project folder<input id="rcWorkdir" value="${escapeAttr(workdir || '')}" placeholder="/path/to/your/project" required autocomplete="off" autocapitalize="none" spellcheck="false" /></label>
          <label>Claude account<select id="rcProfile"><option value="">Primary account</option>${accounts.map(p => `<option value="${escapeAttr(p.id)}" ${p.id === profile ? 'selected' : ''}>${escapeHtml(p.label)}${p.logged_in ? '' : ' · sign-in needed'}</option>`).join('')}</select></label>
          <button class="settings-save" id="rcStartBtn" type="submit">Start Remote Control</button>
        </form>
        <p class="settings-help" id="rcStatus" role="status">Claude asks its own permission and trust questions. Safe mode starts in Claude’s Plan mode; other MoE Deck modes use Claude’s normal permissions.</p>
        <p class="settings-help" id="rcMode"></p>
        <p id="rcError" role="alert" style="color:#fca5a5"></p>
        <a id="rcOpenLink" class="rc-link" target="_blank" rel="noopener" hidden>Open in Claude</a>
        <button class="settings-secondary" id="rcStopBtn" disabled>Stop Remote Control</button>
        <details id="rcTerminal" open style="margin-top:14px"><summary>Native terminal & replies</summary><pre id="rcOutput" class="rc-console">Not started.</pre>
        <form class="project-form" id="rcInputForm" style="margin-top:12px">
          <label>Terminal reply<input id="rcTerminalInput" placeholder="Answer a question from Claude…" autocomplete="off" autocapitalize="none" spellcheck="false" disabled /></label>
          <button type="submit" class="settings-secondary" id="rcEnterBtn" disabled>Send Enter</button>
        </form>
        <div class="rc-actions"><button class="settings-secondary" data-rc-key="up" disabled aria-label="Terminal up">↑</button><button class="settings-secondary" data-rc-key="down" disabled aria-label="Terminal down">↓</button><button class="settings-secondary" data-rc-key="escape" disabled>Esc</button></div></details>`;
      const version = openSheet();
      let sessionId = null;
      let pending = false;
      let knownSessions = [];
      let lastStatus = null;
      const field = document.getElementById('rcWorkdir');
      const account = document.getElementById('rcProfile');
      const start = document.getElementById('rcStartBtn');
      const error = document.getElementById('rcError');
      const current = () => version === sheetVersion;
      function update(view) {
        if (!current()) return;
        const switching = sessionId !== view.id;
        sessionId = view.id;
        if (lastStatus !== view.status) document.getElementById('rcTerminal').open = view.status !== 'ready';
        lastStatus = view.status;
        const running = ['starting', 'ready', 'stopping'].includes(view.status);
        document.getElementById('rcStartForm').hidden = running;
        if (switching || running) {
          field.value = view.workdir;
          account.value = view.profile || '';
        }
        start.disabled = running;
        start.textContent = running ? 'Remote Control running' : 'Start Remote Control';
        field.disabled = account.disabled = running;
        document.getElementById('rcStopBtn').disabled = !running || view.status === 'stopping';
        document.getElementById('rcTerminalInput').disabled = !running;
        document.getElementById('rcEnterBtn').disabled = !running;
        document.querySelectorAll('[data-rc-key]').forEach(button => { button.disabled = !running; });
        const status = { starting: 'Waiting for Claude. Check the terminal below for any question.',
          ready: 'Ready to connect. Open Claude → Code, or use the link below.', stopping: 'Stopping Remote Control…',
          stopped: 'Remote Control stopped.', exited: 'Claude Remote Control exited.', error: 'Claude could not keep Remote Control running. See its terminal output below.' };
        document.getElementById('rcStatus').textContent = status[view.status] || view.status;
        const accountName = accounts.find(p => p.id === view.profile)?.label || 'Primary account';
        document.getElementById('rcMode').textContent = `${view.name} · ${accountName} · Permissions: ${view.permission_mode}`;
        const output = document.getElementById('rcOutput');
        const atBottom = output.scrollHeight - output.scrollTop - output.clientHeight < 40;
        if (output.textContent !== view.output) output.textContent = view.output || 'Waiting for native output…';
        if (atBottom) output.scrollTop = output.scrollHeight;
        const link = document.getElementById('rcOpenLink');
        link.hidden = view.status !== 'ready' || !view.url?.startsWith('https://claude.ai/code/');
        if (!link.hidden) link.href = view.url;
      }
      async function poll() {
        if (!current() || !sessionId) return;
        const target = sessionId;
        try {
          const view = await apiRequest(`/api/claude-rc/${encodeURIComponent(target)}`);
          if (!current() || sessionId !== target) return;
          update(view);
          if (!['starting', 'ready', 'stopping'].includes(view.status)) return;
        }
        catch (err) { if (!current() || sessionId !== target) return; error.textContent = err.message; }
        if (current() && sessionId === target) rcPollTimer = setTimeout(poll, 1000);
      }
      function showSessions(sessions) {
        if (!current()) return;
        knownSessions = sessions;
        const list = document.getElementById('rcSessions');
        list.replaceChildren();
        for (const view of sessions.filter(s => ['starting', 'ready', 'stopping'].includes(s.status))) {
          if (view.id === sessionId) continue;
          const button = document.createElement('button');
          button.className = 'settings-secondary';
          button.textContent = `${view.name} · ${view.profile || 'Primary'}`;
          button.addEventListener('click', () => { clearTimeout(rcPollTimer); update(view); showSessions(knownSessions); poll(); });
          list.appendChild(button);
        }
      }
      async function launch() {
        if (pending || !current()) return;
        pending = true;
        start.disabled = true;
        field.disabled = account.disabled = true;
        start.textContent = 'Starting…';
        error.textContent = '';
        try {
          const view = await apiRequest('/api/claude-rc', jsonRequest('POST', { workdir: field.value.trim(), profile: account.value || null }));
          update(view);
          showSessions([...knownSessions.filter(s => s.id !== view.id), view]);
          clearTimeout(rcPollTimer);
          await poll();
        } catch (err) {
          if (current()) { error.textContent = err.message; start.disabled = field.disabled = account.disabled = false; start.textContent = 'Start Remote Control'; }
        } finally { pending = false; }
      }
      document.getElementById('rcStartForm').addEventListener('submit', e => { e.preventDefault(); launch(); });
      async function sendInput(key = 'enter') {
        if (!sessionId || !current()) return;
        const target = sessionId;
        const input = document.getElementById('rcTerminalInput');
        const text = key === 'enter' ? input.value : '';
        try {
          await apiRequest(`/api/claude-rc/${encodeURIComponent(target)}/input`, jsonRequest('POST', { text, key }));
          if (!current() || sessionId !== target) return;
          if (key === 'enter' && input.value === text) input.value = '';
          error.textContent = '';
        } catch (err) { if (current()) error.textContent = err.message; }
      }
      document.getElementById('rcInputForm').addEventListener('submit', e => { e.preventDefault(); sendInput(); });
      document.querySelectorAll('[data-rc-key]').forEach(button => button.addEventListener('click', () => sendInput(button.dataset.rcKey)));
      document.getElementById('rcStopBtn').addEventListener('click', async () => {
        if (!sessionId || !current()) return;
        const target = sessionId;
        try {
          const view = await apiRequest(`/api/claude-rc/${encodeURIComponent(target)}/stop`, jsonRequest('POST', {}));
          if (current() && sessionId === target) update(view);
        }
        catch (err) { if (current()) error.textContent = err.message; }
      });
      try {
        const data = await apiRequest('/api/claude-rc');
        if (!current()) return;
        showSessions(data.sessions);
        const existing = data.sessions.find(s => s.workdir === field.value && (s.profile || '') === account.value && ['starting','ready','stopping'].includes(s.status));
        if (existing) { update(existing); await poll(); }
        else if (autoStart && field.value) await launch();
      } catch (err) { if (current()) error.textContent = err.message; }
    }

    function accountRowHtml(h) {
      const accs = (state.profiles || []).filter(p => p.harness === h.id);
      const accountBtn = (active, onclick, label) =>
        `<button class="effort-btn ${active ? 'selected' : ''}" style="max-width:100%" onclick="${onclick}">${label}</button>`;
      return `
        <div style="display:flex;flex-wrap:wrap;gap:6px;align-items:center;margin:-4px 0 12px;padding:9px 10px;border:1px dashed var(--border-strong);border-radius:11px">
          <span style="font-size:11px;font-weight:700;color:var(--text-muted);width:100%;letter-spacing:.04em;text-transform:uppercase">Accounts</span>
          ${accountBtn(state.selectedProfile === null, `pickProfile('${escapeAttr(h.id)}', null)`, '👑 Primary')}
          ${accs.map(p => `
            ${accountBtn(
              state.selectedProfile === p.id,
              `pickProfile('${escapeAttr(h.id)}', '${escapeAttr(p.id)}')`,
              `${p.logged_in ? '🟢' : '🔒'} ${escapeHtml(p.label)}${p.logged_in ? '' : ' · sign-in needed'}`
            )}
            <button class="effort-btn" style="padding:4px 8px;color:#ef4444" title="Remove ${escapeAttr(p.id)}" onclick="removeAccount('${escapeAttr(p.id)}')">✕</button>
          `).join('')}
          <button class="effort-btn" onclick="addAccount('${escapeAttr(h.id)}')">＋ Add account</button>
        </div>
      `;
    }

    // Primary account first, then every signed-in extra account for the harness.
    function switchableAccounts(harnessId) {
      const extra = (state.profiles || []).filter(p => p.harness === harnessId && p.logged_in);
      return [null, ...extra.map(p => p.id)];
    }

    window.cycleAccount = function() {
      const accounts = switchableAccounts(state.selectedHarness);
      const next = accounts[(accounts.indexOf(state.selectedProfile) + 1) % accounts.length];
      pickProfile(state.selectedHarness, next);
    };

    window.pickProfile = async function(harnessId, profileId) {
      state.selectedHarness = harnessId;
      state.selectedProfile = profileId || null;
      applyHarnessTheme(harnessId);
      if (state.activeId) {
        try {
          await fetch(`/api/conversations/${encodeURIComponent(state.activeId)}`, {
            method: 'PATCH',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ profile: profileId || 'default' })
          });
        } catch (e) { /* non-fatal: the next send also carries the pick */ }
      }
      closeOverlays();
      await fetchState();
    };

    window.removeAccount = async function(profileId) {
      if (!confirm(`Remove account profile \`${profileId}\`?\nIts shadow home (sessions + credentials) stays on disk (profiles/${profileId} in MoE Deck’s config folder) unless you delete it yourself.`)) return;
      const res = await fetch(`/api/profiles/${encodeURIComponent(profileId)}`, {
        method: 'DELETE',
        headers: { 'Content-Type': 'application/json' },
        body: '{}'
      });
      if (state.selectedProfile === profileId) state.selectedProfile = null;
      await fetchState();
      applyHarnessTheme(state.selectedHarness);
      const err = !res.ok ? (await res.json().catch(() => ({}))).error : null;
      if (err) alert(err);
    };

    window.addAccount = async function(harnessId) {
      const res = await fetch('/api/profiles', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ harness: harnessId })
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok || !data.profile) {
        alert(data.error || 'Failed to add account');
        return;
      }
      await fetchState();
      state.selectedProfile = data.profile.id;
      applyHarnessTheme(harnessId);
      const content = document.getElementById('sheetContent');
      content.innerHTML = `
        <div class="sheet-title"><span>Account added · ${escapeHtml(data.profile.id)}</span></div>
        <div style="font-size:13px;color:var(--text);line-height:1.55;padding:4px 2px 10px">
          MoE Deck created an isolated home for this account. To sign it in, open a terminal on the machine running MoE Deck and run:
        </div>
        <pre style="background:#111110;border:1px solid var(--border-strong);border-radius:10px;padding:10px 12px;font-size:12.5px;white-space:pre-wrap;word-break:break-all;color:#7dd3fc" id="loginCmdBlock">${escapeHtml(data.login_command)}</pre>
        <div style="font-size:12px;color:var(--text-muted);padding:8px 2px 2px;line-height:1.5">
          The harness login flow opens with <b>only</b> this account's credentials visible — your primary sign-in is untouched.
          After signing in, the 🔒 turns 🟢 here and parallel threads can use it. Shortest equivalent: <code>moedeck login ${escapeHtml(data.profile.id)}</code>
        </div>
      `;
    };

    window.pickHarness = function(harnessId) {
      state.selectedHarness = harnessId;
      state.selectedProfile = null;
      const h = state.harnesses.find(x => x.id === harnessId);
      if (h && h.models.length > 0) {
        state.selectedModel = h.models[0].id;
        state.selectedModelLabel = h.models[0].label;
        state.selectedEffort = h.models[0].default_effort || 'medium';
      }
      applyHarnessTheme(harnessId);
      closeOverlays();
    };

    // Model + Effort Selector Sheet
    document.getElementById('modelPillBtn').addEventListener('click', () => {
      const h = state.harnesses.find(x => x.id === state.selectedHarness) || state.harnesses[0];
      if (!h) return;
      const model = h.models.find(m => m.id === state.selectedModel);
      const efforts = model?.efforts?.length ? model.efforts : h.efforts;
      const content = document.getElementById('sheetContent');
      content.innerHTML = `
        <div class="sheet-title">
          <span>Model & reasoning</span>
        </div>
        ${h.id === 'codex' ? '<div style="font-size:12px;color:var(--text-muted);margin-bottom:12px">Codex local catalog · primary account. Default uses your Codex configuration.</div>' : ''}
        <input class="model-search" type="search" placeholder="Search models…" aria-label="Search models" oninput="filterModelChoices(this.value)" />
        ${h.models.map(m => {
          const isSel = m.id === state.selectedModel;
          return `
            <button type="button" aria-pressed="${isSel}" class="sheet-option model-choice ${isSel ? 'selected' : ''}" data-model-id="${escapeAttr(m.id)}" data-model-label="${escapeAttr(m.label)}" onclick="pickModel(this.dataset.modelId, this.dataset.modelLabel)">
              <div style="min-width:0"><div style="font-weight:640">${escapeHtml(m.label)}</div>${m.base_url?`<div style="font-size:11.5px;color:var(--text-muted);margin-top:2px;overflow-wrap:anywhere">${escapeHtml(m.base_url)}</div>`:''}</div>
              <span style="font-size:18px;color:var(--accent)">${isSel ? '✓' : ''}</span>
            </button>
          `;
        }).join('')}
        <div style="margin-top:12px;font-size:12.5px;font-weight:600;color:var(--text-muted)">Reasoning Effort</div>
        <div class="effort-grid">
          ${efforts.map(ef => `
            <button class="effort-btn ${ef === state.selectedEffort ? 'selected' : ''}" onclick="pickEffort('${escapeAttr(ef)}')">${escapeHtml(ef)}</button>
          `).join('')}
        </div>
      `;
      openSheet();
    });

    window.filterModelChoices = function(query) {
      for(const row of document.querySelectorAll('.model-choice')) row.hidden=!row.textContent.toLowerCase().includes(query.toLowerCase());
    };
    window.pickModel = function(id, label) {
      state.selectedModel = id;
      state.selectedModelLabel = label;
      const h = state.harnesses.find(h => h.id === state.selectedHarness);
      const model = h?.models.find(m => m.id === id);
      const efforts = model?.efforts?.length ? model.efforts : (h?.efforts || []);
      if (!efforts.includes(state.selectedEffort)) {
        state.selectedEffort = model?.default_effort || 'default';
      }
      applyHarnessTheme(state.selectedHarness);
      closeOverlays();
    };

    window.pickEffort = function(ef) {
      state.selectedEffort = ef;
      applyHarnessTheme(state.selectedHarness);
      closeOverlays();
    };

    // Running Tasks Sheet
    document.getElementById('runningTaskBar').addEventListener('click', () => {
      const busyConvs = state.conversations.filter(c => c.status === 'busy');
      const content = document.getElementById('sheetContent');
      content.innerHTML = `
        <div class="sheet-title">
          <span>Running Tasks (${busyConvs.length})</span>
        </div>
        ${busyConvs.map(c => {
          const hc = HARNESS_COLORS[c.harness] || HARNESS_COLORS.claude;
          return `
            <div class="sheet-option" style="cursor:default">
              <div style="min-width:0;padding-right:8px">
                <div style="display:flex;align-items:center;gap:7px;font-weight:640">
                  <span class="bounce-dot busy" style="background:${hc.accent}"></span>
                  <span>${escapeHtml(c.title)}</span>
                </div>
                <div style="font-size:12px;color:var(--text-muted);margin-top:3px">
                  ${escapeHtml(c.running_task_label || 'Running in background')}
                </div>
              </div>
              <button class="new-thread-btn" style="background:#ef4444;color:#fff;flex-shrink:0" onclick="stopConvTask('${escapeAttr(c.id)}')">Stop</button>
            </div>
          `;
        }).join('') || '<div style="color:var(--text-muted)">No background tasks running.</div>'}
      `;
      openSheet();
    });

    window.stopConvTask = async function(id) {
      try {
        await apiRequest(`/api/conversations/${encodeURIComponent(id)}/stop`, jsonRequest('POST', {}));
        closeOverlays();
        await fetchState();
      } catch (err) { showToast(err.message || 'Could not stop this turn.'); }
    };

    // Quick-Flip button in top right
    document.getElementById('quickFlipBtn').addEventListener('click', () => {
      const previous = state.conversations.find(c => c.id === state.previousId && c.id !== state.activeId)
        || state.conversations.find(c => c.id !== state.activeId);
      if (previous) loadConversation(previous.id, true);
    });

    document.getElementById('openDrawerBtn').addEventListener('click', () => {
      document.getElementById('drawer').classList.add('open');
      document.getElementById('backdrop').classList.add('open');
    });

    let sheetReturnFocus = null;
    let sheetVersion = 0;
    function openSheet() {
      sheetReturnFocus = document.activeElement;
      document.getElementById('drawer').classList.remove('open');
      document.getElementById('bottomSheet').classList.remove('settings-sheet');
      document.getElementById('bottomSheet').inert = false;
      document.getElementById('bottomSheet').classList.add('open');
      document.getElementById('backdrop').classList.add('open');
      document.querySelector('.sheet-close').focus({ preventScroll: true });
      return ++sheetVersion;
    }

    function closeOverlays() {
      clearTimeout(rcPollTimer);
      ++sheetVersion;
      document.getElementById('bottomSheet').inert = true;
      if (sheetReturnFocus?.isConnected) sheetReturnFocus.focus({ preventScroll: true });
      sheetReturnFocus = null;
      document.getElementById('drawer').classList.remove('open');
      document.getElementById('bottomSheet').classList.remove('open');
      document.getElementById('backdrop').classList.remove('open');
    }
    document.getElementById('backdrop').addEventListener('click', closeOverlays);
    document.addEventListener('keydown', e => {
      if (e.key === 'Escape') closeOverlays();
      const panel = document.getElementById('bottomSheet');
      if (e.key !== 'Tab' || !panel.classList.contains('open')) return;
      const controls = [...panel.querySelectorAll('button, input, select, textarea, a[href], [tabindex="0"]')]
        .filter(el => !el.disabled && el.getClientRects().length);
      const first = controls[0], last = controls[controls.length - 1];
      if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last?.focus(); }
      if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first?.focus(); }
    });

    document.querySelectorAll('#drawerFilters .filter-chip[data-filter]').forEach(btn => {
      btn.addEventListener('click', () => {
        document.querySelectorAll('#drawerFilters .filter-chip[data-filter]').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        state.drawerFilter = btn.dataset.filter || 'all';
        renderDrawerList();
      });
    });

    document.getElementById('drawerSearchInput').addEventListener('input', (e) => {
      state.drawerSearch = e.target.value || '';
      renderDrawerList();
    });

    let creatingThread = false;
    document.getElementById('newThreadBtn').addEventListener('click', () => openProjectSheet(true));
    document.getElementById('headerTitleWrap').addEventListener('click', () => openProjectSheet(false));

    async function openProjectSheet(newThread) {
      closeOverlays();
      const content = document.getElementById('sheetContent');
      const title = newThread ? 'New conversation' : 'Choose project';
      content.innerHTML = `<div class="sheet-title">${title}</div><p>Loading projects…</p>`;
      const version = openSheet();
      try {
        const data = await apiRequest('/api/projects');
        if (version !== sheetVersion) return;
        const paths = [...new Set([
          state.selectedWorkdir,
          ...state.conversations.map(c => c.workdir),
          ...(data.projects || []).map(p => p.workdir)
        ].filter(p => p && p !== state.home))];
        const installed = state.harnesses.filter(h => h.available);
        const selectedHarness = installed.find(h => h.id === state.selectedHarness) || installed[0];
        const workdir = state.selectedWorkdir === state.home ? '' : state.selectedWorkdir;
        content.innerHTML = `<div class="sheet-title">${title}</div>
          <form class="project-form" id="projectForm">
            ${newThread ? `<label>Harness<select id="newHarnessSelect" required>${installed.map(h => `<option value="${escapeAttr(h.id)}" ${h.id === selectedHarness?.id ? 'selected' : ''}>${escapeHtml(h.name)}</option>`).join('')}</select></label>
              <label>Model<select id="newModelSelect" required></select></label>` : ''}
            <label>Project<select id="projectSelect"><option value="">Choose a project…</option>${paths.map(p => `<option value="${escapeAttr(p)}" ${p === workdir ? 'selected' : ''}>${escapeHtml(p.split('/').filter(Boolean).pop())} · ${escapeHtml(p)}</option>`).join('')}</select></label>
            <label>Folder on this computer<input id="projectPathInput" value="${escapeAttr(workdir || '')}" placeholder="/path/to/your/project" required autocomplete="off" autocapitalize="none" spellcheck="false" /></label>
            <p role="alert" id="projectError"></p>
            <button class="settings-save" type="submit" ${newThread && !installed.length ? 'disabled' : ''}>${newThread ? 'Start conversation' : 'Use project'}</button>
          </form>
          ${!newThread && state.detail ? '<button class="settings-secondary" id="threadDetailsBtn">Thread details, files & git</button>' : ''}`;
        const projectSelect = document.getElementById('projectSelect');
        const pathInput = document.getElementById('projectPathInput');
        projectSelect.addEventListener('change', () => { pathInput.value = projectSelect.value; });
        if (newThread) {
          const harnessSelect = document.getElementById('newHarnessSelect');
          const modelSelect = document.getElementById('newModelSelect');
          function updateModels() {
            const harness = installed.find(h => h.id === harnessSelect.value);
            modelSelect.innerHTML = (harness?.models || []).map(m => `<option value="${escapeAttr(m.id)}" ${harness.id === state.selectedHarness && m.id === state.selectedModel ? 'selected' : ''}>${escapeHtml(m.label)}</option>`).join('');
          }
          harnessSelect.addEventListener('change', updateModels);
          updateModels();
        }
        document.getElementById('threadDetailsBtn')?.addEventListener('click', openThreadInspector);
        document.getElementById('projectForm').addEventListener('submit', async e => {
          e.preventDefault();
          if (creatingThread) return;
          const button = e.target.querySelector('[type=submit]');
          const error = document.getElementById('projectError');
          button.disabled = true;
          error.textContent = '';
          try {
            if (!newThread) {
              await pickWorkdir(pathInput.value.trim());
              return;
            }
            creatingThread = true;
            button.textContent = 'Starting…';
            const harness = installed.find(h => h.id === document.getElementById('newHarnessSelect').value);
            const model = harness.models.find(m => m.id === document.getElementById('newModelSelect').value);
            const created = await apiRequest('/api/conversations', jsonRequest('POST', {
              harness: harness.id, model: model.id,
              effort: harness.id === state.selectedHarness && model.id === state.selectedModel
                ? state.selectedEffort : model.default_effort,
              workdir: pathInput.value.trim(),
              profile: harness.id === state.selectedHarness ? state.selectedProfile : null
            }));
            if (version !== sheetVersion) return;
            closeOverlays();
            await loadConversation(created.id, true);
            await fetchState();
            if (state.activeId === created.id) composerInput.focus();
          } catch (err) {
            error.textContent = err.message || 'Could not open project.';
          } finally {
            creatingThread = false;
            button.disabled = false;
            button.textContent = newThread ? 'Start conversation' : 'Use project';
          }
        });
      } catch (err) {
        if (version === sheetVersion) content.innerHTML = `<div class="sheet-title">${title}</div><p role="alert">${escapeHtml(err.message)}</p><button class="settings-secondary" onclick="openProjectSheet(${newThread})">Retry</button>`;
      }
    }

    const composerInput = document.getElementById('composerInput');
    const slashMenu = document.getElementById('slashMenu');
    const atMenu = document.getElementById('atMenu');
    let atFetchTimer = null;
    let atAbort = null;

    composerInput.addEventListener('input', async () => {
      composerInput.style.height = 'auto';
      composerInput.style.height = Math.min(composerInput.scrollHeight, 120) + 'px';
      const val = composerInput.value;
      if (val.startsWith('/')) {
        slashMenu.classList.add('open');
        atMenu.classList.remove('open');
        return;
      }
      slashMenu.classList.remove('open');

      // Check for @file autocomplete token at end of input. Debounce so
      // typing doesn't fire one request per keystroke; abort stale requests
      // so late responses can't overwrite a newer query.
      const atMatch = val.match(/(?:^|\s)@([^\s]*)$/);
      if (atMatch) {
        const q = atMatch[1] || '';
        if (atFetchTimer) clearTimeout(atFetchTimer);
        if (atAbort) atAbort.abort();
        const controller = new AbortController();
        atAbort = controller;
        atFetchTimer = setTimeout(async () => {
          try {
            const res = await fetch(`/api/files?workdir=${encodeURIComponent(state.selectedWorkdir)}&q=${encodeURIComponent(q)}`, { signal: controller.signal });
            if (!res.ok) return;
            const data = await res.json();
            const files = data.files || [];
            if (files.length > 0) {
              atMenu.innerHTML = files.map(f => `
                <div class="slash-item" onclick="insertAtFile('${escapeAttr(f)}')">
                  <span class="slash-cmd" style="font-family:ui-monospace,monospace;font-size:12px">@${escapeHtml(f)}</span>
                </div>
              `).join('');
              atMenu.classList.add('open');
              return;
            }
            atMenu.classList.remove('open');
          } catch (e) {
            if (e.name !== 'AbortError') console.warn('File autocomplete failed:', e);
          }
        }, 120);
        return;
      }
      if (atFetchTimer) clearTimeout(atFetchTimer);
      if (atAbort) atAbort.abort();
      atMenu.classList.remove('open');
    });

    window.insertAtFile = function(filePath) {
      composerInput.value = composerInput.value.replace(/(?:^|\s)@([^\s]*)$/, (m) => {
        const prefix = m.startsWith(' ') ? ' ' : '';
        return `${prefix}@${filePath} `;
      });
      atMenu.classList.remove('open');
      composerInput.focus();
    };

    document.querySelectorAll('.slash-item').forEach(item => {
      item.addEventListener('click', () => {
        const cmd = item.dataset.cmd;
        slashMenu.classList.remove('open');
        if (cmd === '/new') {
          composerInput.value = '';
          document.getElementById('newThreadBtn').click();
        } else if (cmd === '/handoff') {
          composerInput.value = '';
          document.getElementById('harnessPillBtn').click();
        } else if (cmd === '/rc') {
          composerInput.value = '';
          openClaudeRemoteControl(true);
        } else if (cmd === '/preview') {
          composerInput.value = '';
          openSandboxModal();
        } else if (cmd === '/sprint') {
          composerInput.value = '';
          openSprintSheet();
        } else if (cmd === '/workdir ') {
          composerInput.value = '';
          openProjectSheet(false);
        } else if (cmd === '/diff' || cmd === '/review' || cmd === '/worktree' || cmd === '/bounce') {
          composerInput.value = '';
          openThreadInspector();
        } else if (cmd === '/stop') {
          composerInput.value = '';
          if (state.activeId) window.stopConvTask(state.activeId);
        } else {
          composerInput.value = cmd;
          composerInput.focus();
        }
      });
    });

    document.getElementById('attachBtn').addEventListener('click', () => {
      document.getElementById('fileInput').click();
    });

    document.getElementById('fileInput').addEventListener('change', async (e) => {
      const files = Array.from(e.target.files || []);
      for (const file of files) {
        const reader = new FileReader();
        reader.onload = async () => {
          try {
            const out = await apiRequest('/api/upload', jsonRequest('POST', {
              filename: file.name, data_base64: reader.result.split(',')[1]
            }));
            state.queuedImages.push(out.url);
            renderQueuedImages();
          } catch (err) { showToast(err.message || 'Could not attach this image.'); }
        };
        reader.onerror = () => showToast(`Could not read ${file.name}.`);
        reader.readAsDataURL(file);
      }
      e.target.value = '';
    });

    function renderQueuedImages() {
      const strip = document.getElementById('uploadPreviewStrip');
      if (state.queuedImages.length === 0) {
        strip.classList.add('hidden');
        strip.innerHTML = '';
        return;
      }
      strip.classList.remove('hidden');
      strip.innerHTML = state.queuedImages.map((url, i) => `
        <div class="upload-thumb-wrap">
          <img src="${escapeAttr(url)}" />
          <button class="upload-remove-btn" onclick="removeQueuedImage(${i})">×</button>
        </div>
      `).join('');
    }

    window.removeQueuedImage = function(i) {
      state.queuedImages.splice(i, 1);
      renderQueuedImages();
    };

    let sendingTurn = false;
    async function sendMessage() {
      if (sendingTurn) return;
      if (/^\/(?:rc|remote-control)\s*$/i.test(composerInput.value.trim())) {
        composerInput.value = '';
        slashMenu.classList.remove('open');
        openClaudeRemoteControl(true);
        return;
      }
      if (state.activeId && state.detail?.summary.id !== state.activeId) {
        showToast('Wait for this conversation to load before sending.');
        return;
      }
      const text = composerInput.value.trim();
      if (!text && state.queuedImages.length === 0) return;
      if (!state.activeId && (!state.selectedHarness || !state.selectedWorkdir || state.selectedWorkdir === state.home)) {
        openProjectSheet(true);
        return;
      }

      if (text.startsWith('/workdir ')) {
        state.selectedWorkdir = text.replace('/workdir ', '').trim();
        localStorage.setItem('helm_last_workdir', state.selectedWorkdir);
        composerInput.value = '';
        document.getElementById('convSubtitle').textContent = `${HARNESS_COLORS[state.selectedHarness]?.short} · ${state.selectedWorkdir}`;
        return;
      }

      const draftText = composerInput.value;
      const selection = { harness: state.selectedHarness, model: state.selectedModel,
        effort: state.selectedEffort, workdir: state.selectedWorkdir, profile: state.selectedProfile };
      const deliveryId = `turn_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      const images = [...state.queuedImages];
      state.queuedImages = [];
      renderQueuedImages();
      composerInput.value = '';
      state.drafts.delete(state.activeId);
      composerInput.style.height = 'auto';
      slashMenu.classList.remove('open');
      atMenu.classList.remove('open');

      sendingTurn = true;
      const sendBtn = document.getElementById('sendBtn');
      sendBtn.disabled = true;
      sendBtn.setAttribute('aria-label', 'Sending');
      let optNodes = null;
      let targetId = state.activeId;
      try {
        if (!targetId) {
          const createRes = await fetch('/api/conversations', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(selection)
          });
          if (!createRes.ok) {
            const err = await createRes.json().catch(() => ({}));
            throw new Error(err.error || `Could not create thread (${createRes.status})`);
          }
          const created = await createRes.json();
          if (!created.id) throw new Error('Could not create thread');
          targetId = created.id;
          if (!state.activeId) {
            state.activeId = targetId;
            localStorage.setItem('helm_active_id', targetId);
          }
        }

        if (!navigator.onLine) {
          queueOfflineMessage(targetId, text, images, selection, deliveryId);
          return;
        }

        optNodes = renderOptimisticTurn(targetId, text);

        const res = await fetch(`/api/conversations/${encodeURIComponent(targetId)}/messages`, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'X-Idempotency-Key': deliveryId
          },
          body: JSON.stringify({
            text: text || 'Attached screenshot',
            ...selection,
            images
          })
        });
        if (!res.ok) {
          if (res.status === 401) showLoginOverlay();
          const err = await res.json().catch(() => ({}));
          clearOptimisticTurn(optNodes);
          optNodes = null;
          throw new Error(err.error || `Message failed to send (${res.status})`);
        }
        localStorage.setItem('helm_last_workdir', selection.workdir || '');
        await fetchState();
        if (state.activeId === targetId) await loadConversation(targetId, false);
      } catch (err) {
        clearOptimisticTurn(optNodes);
        const offline = err instanceof TypeError || err.name === 'TypeError';
        if (offline) {
          // Network died mid-send: keep the turn in the outbox instead of
          // silently dropping it, and leave the composer clear.
          queueOfflineMessage(targetId, text, images, selection, deliveryId);
          showToast('Connection lost — message saved to the outbox.');
        } else {
          // Server rejected the turn: put the draft back so nothing is lost.
          state.drafts.set(targetId, { text: draftText, images });
          if (state.activeId === targetId && !composerInput.value) {
            composerInput.value = draftText;
            composerInput.style.height = 'auto';
            composerInput.style.height = Math.min(composerInput.scrollHeight, 120) + 'px';
            state.queuedImages = images;
            renderQueuedImages();
          }
          showToast(err.message || 'Message failed to send.');
        }
      } finally {
        sendingTurn = false;
        sendBtn.disabled = Boolean(state.activeId && state.detail?.summary.id !== state.activeId);
        sendBtn.setAttribute('aria-label', 'Send');
      }
    }

    function showToast(message) {
      let el = document.getElementById('helmToast');
      if (!el) {
        el = document.createElement('div');
        el.id = 'helmToast';
        el.style.cssText = 'position:fixed;left:50%;bottom:calc(96px + var(--safe-bottom));transform:translateX(-50%);background:#2a2a27;color:var(--text);border:1px solid var(--border-strong);border-radius:10px;padding:9px 14px;font-size:12.5px;z-index:9500;max-width:88vw;text-align:center;box-shadow:0 8px 24px rgba(0,0,0,.35)';
        document.body.appendChild(el);
      }
      el.textContent = message;
      el.style.display = 'block';
      clearTimeout(el._timer);
      el._timer = setTimeout(() => { el.style.display = 'none'; }, 4200);
    }

    // Instant feedback while the POST round-trips plus until the next
    // server-driven re-render arrives: the user's bubble is shown immediately
    // and a lightweight "working…" placeholder appears underneath.
    function renderOptimisticTurn(targetId, text) {
      if (state.activeId !== targetId) return null;
      const scrollEl = document.getElementById('chatScroll');
      if (!scrollEl) return null;
      const accent = (HARNESS_COLORS[state.selectedHarness] || HARNESS_COLORS.claude).accent;
      const userDiv = document.createElement('div');
      userDiv.className = 't-turn';
      userDiv.id = 'helm-opt-user';
      userDiv.innerHTML = `<div class="t-user"><span class="t-glyph">❯</span><span>${escapeHtml(text || 'Attached screenshot')}</span></div>`;
      const workDiv = document.createElement('div');
      workDiv.className = 't-turn';
      workDiv.id = 'helm-opt-work';
      workDiv.innerHTML = `<div class="t-line t-working"><span class="t-glyph" style="color:${accent}">✻</span><div class="t-body">Working…</div></div>`;
      scrollEl.appendChild(userDiv);
      scrollEl.appendChild(workDiv);
      scrollEl.scrollTop = scrollEl.scrollHeight;
      return { userDiv, workDiv };
    }

    // The next full renderMessages rebuild also removes these nodes; explicit
    // removal is only needed when we fall back to the offline-queue path.
    function clearOptimisticTurn(nodes) {
      if (!nodes) return;
      nodes.userDiv.remove();
      nodes.workDiv.remove();
    }

    function queueOfflineMessage(targetId, text, images, selection, deliveryId) {
      const turn = {
        id: deliveryId,
        targetId,
        text: text || 'Attached screenshot',
        ...selection,
        images,
        queuedAt: new Date().toLocaleTimeString()
      };
      state.offlineQueue.push(turn);
      localStorage.setItem('helm_offline_queue', JSON.stringify(state.offlineQueue));
      updateNetworkUI();

      if (state.activeId === targetId) {
        const scrollEl = document.getElementById('chatScroll');
        if (scrollEl) {
          const queuedDiv = document.createElement('div');
          queuedDiv.className = 'msg-block msg-user';
          queuedDiv.id = turn.id;
          queuedDiv.innerHTML = `
            <div class="msg-user-bubble" style="opacity:0.85;border:1px dashed #f59e0b">
              ${escapeHtml(turn.text)}
              <div style="font-size:10px;color:#f59e0b;margin-top:3px">⏳ Queued offline · Auto-sends on reconnect</div>
            </div>
          `;
          scrollEl.appendChild(queuedDiv);
          scrollEl.scrollTop = scrollEl.scrollHeight;
        }
      }
    }

    let isFlushingQueue = false;
    async function flushOfflineQueue() {
      if (isFlushingQueue || state.offlineQueue.length === 0 || !navigator.onLine) return;
      isFlushingQueue = true;
      updateNetworkUI();

      while (state.offlineQueue.length > 0 && navigator.onLine) {
        const item = state.offlineQueue[0];
        const attempts = (item.attempts || 0) + 1;
        try {
          const res = await fetch(`/api/conversations/${encodeURIComponent(item.targetId)}/messages`, {
            method: 'POST',
            headers: {
              'Content-Type': 'application/json',
              'X-Idempotency-Key': String(item.id)
            },
            body: JSON.stringify({
              text: item.text,
              harness: item.harness,
              model: item.model,
              effort: item.effort,
              workdir: item.workdir,
              profile: item.profile || null,
              images: item.images || []
            })
          });
          if (res.ok) {
            state.offlineQueue.shift();
            localStorage.setItem('helm_offline_queue', JSON.stringify(state.offlineQueue));
            updateNetworkUI();
          } else if (res.status === 400 || res.status === 401 || res.status === 403 || res.status === 404 || res.status === 413 || res.status === 422) {
            // Permanent failures (bad payload / auth / missing thread): drop the
            // item instead of dead-locking the whole queue behind it.
            console.warn(`Offline queue item ${item.id} dropped permanently (HTTP ${res.status})`);
            state.offlineQueue.shift();
            localStorage.setItem('helm_offline_queue', JSON.stringify(state.offlineQueue));
            updateNetworkUI();
          } else {
            // Transient (5xx / 429): keep the item, but give up after 5 tries.
            item.attempts = attempts;
            localStorage.setItem('helm_offline_queue', JSON.stringify(state.offlineQueue));
            if (attempts >= 5) {
              console.warn(`Offline queue item ${item.id} dropped after ${attempts} attempts`);
              state.offlineQueue.shift();
              localStorage.setItem('helm_offline_queue', JSON.stringify(state.offlineQueue));
              updateNetworkUI();
            } else {
              break;
            }
          }
        } catch (_) {
          // Network failure — the item stays queued for the next flush.
          break;
        }
      }

      isFlushingQueue = false;
      updateNetworkUI();
      await fetchState();
      if (state.activeId) await loadConversation(state.activeId, false);
    }

    function updateNetworkUI() {
      const ind = document.getElementById('offlineIndicator');
      const badge = document.getElementById('offlineQueueCountBadge');
      const count = (state.offlineQueue || []).length;
      if (!ind || !badge) return;

      if (!navigator.onLine || count > 0 || state.reconnecting) {
        ind.style.display = 'flex';
        if (!navigator.onLine) {
          ind.style.background = '#ef4444';
          ind.style.color = '#fff';
          ind.firstElementChild.textContent = '⚠️ Offline Mode: Network disconnected';
        } else if (state.reconnecting) {
          ind.style.background = '#f59e0b';
          ind.style.color = '#000';
          ind.firstElementChild.textContent = pollFailures > 2
            ? `🔄 Reconnecting… (attempt ${pollFailures})`
            : '🔄 Reconnecting…';
        } else {
          ind.style.background = '#f59e0b';
          ind.style.color = '#000';
          ind.firstElementChild.textContent = '⚡ Reconnected: Syncing queued turns…';
        }
        badge.textContent = `${count} queued`;
      } else {
        ind.style.display = 'none';
      }
    }

    window.addEventListener('online', () => {
      updateNetworkUI();
      flushOfflineQueue();
    });
    window.addEventListener('offline', () => {
      updateNetworkUI();
    });

    // Page (re)load while already online: flush any queue restored from
    // localStorage — the `online` event only fires on actual transitions.
    if (navigator.onLine) {
      flushOfflineQueue().catch(() => {});
    }

    window.openSprintSheet = async function() {
      const content = document.getElementById('sheetContent');
      content.innerHTML = `
        <div class="sheet-title">
          <span>🏃 Autonomous Multi-Step Sprint Engine</span>
          <span style="font-size:11.5px;color:var(--text-muted)">Chained Execution</span>
        </div>
        <div style="font-size:12px;color:var(--text-muted);margin-bottom:10px">
          Paste or write a multi-step Markdown checklist. MoE Deck will autonomously execute each stage sequentially in the background.
        </div>
        <div style="display:flex;gap:6px;margin-bottom:8px">
          <input id="sprintTitleInput" placeholder="Sprint title (e.g. Refactor shadow VFS)..." style="flex:1;background:#191917;border:1px solid var(--border-strong);border-radius:9px;padding:6px 9px;color:var(--text);font-size:12.5px" />
          <select id="sprintHarnessSelect" style="background:#191917;border:1px solid var(--border-strong);border-radius:9px;padding:6px 9px;color:var(--text);font-size:12px">
            ${installedHarnessIds().map(h => `<option value="${h}" ${h === 'claude' ? 'selected' : ''}>${HARNESS_COLORS[h].short}</option>`).join('')}
          </select>
        </div>
        <textarea id="sprintChecklistInput" placeholder="- [ ] Step 1: Write trait definitions&#10;- [ ] Step 2: Implement core logic&#10;- [ ] Step 3: Run cargo test and fix errors&#10;- [ ] Step 4: Verify formatting" style="width:100%;height:110px;background:#141412;border:1px solid var(--border-strong);border-radius:9px;padding:8px;font-family:ui-monospace,monospace;font-size:11.5px;color:var(--text);margin-bottom:8px" oninput="previewSprintSteps()"></textarea>
        <div id="sprintPreviewSteps" style="margin-bottom:10px;font-size:11px;color:var(--text-muted)"></div>
        <button class="new-thread-btn" style="width:100%;padding:8px;font-size:12.5px;margin-bottom:14px" onclick="launchAutonomousSprint()">🚀 Launch Autonomous Sprint</button>
        ${(state.sprints || []).length > 0 ? `
          <div style="font-size:12px;font-weight:650;margin-bottom:6px">Active &amp; Recent Sprints (${state.sprints.length})</div>
          <div style="display:flex;flex-direction:column;gap:6px;max-height:180px;overflow-y:auto">
            ${state.sprints.map(sp => {
              const statusColor = sp.status === 'completed' ? '#4ade80' : sp.status === 'running' ? '#fbbf24' : sp.status === 'cancelled' ? '#94a3b8' : '#ef4444';
              const passedCount = sp.steps.filter(s => s.status === 'passed').length;
              return `
                <div style="background:#191917;border:1px solid var(--border);border-radius:8px;padding:8px 10px;font-size:12px">
                  <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:4px">
                    <span style="font-weight:650">${escapeHtml(sp.title)}</span>
                    <span style="background:${statusColor}22;color:${statusColor};border-radius:4px;padding:1px 6px;font-size:10px;font-weight:600">${escapeHtml(sp.status.toUpperCase())} (${passedCount}/${sp.steps.length})</span>
                  </div>
                  <div style="display:flex;gap:5px;margin-top:6px">
                    ${sp.status === 'running' ? `<button class="filter-chip" style="padding:2px 7px;font-size:10.5px;color:#f87171" onclick="cancelSprint('${escapeAttr(sp.id)}')">Cancel</button>` : ''}
                    <button class="filter-chip" style="padding:2px 7px;font-size:10.5px" onclick="deleteSprint('${escapeAttr(sp.id)}')">Delete</button>
                  </div>
                </div>
              `;
            }).join('')}
          </div>
        ` : ''}
      `;
      openSheet();
    };

    window.previewSprintSteps = async function() {
      const text = document.getElementById('sprintChecklistInput')?.value || '';
      const previewEl = document.getElementById('sprintPreviewSteps');
      if (!previewEl) return;
      if (!text.trim()) {
        previewEl.innerHTML = '';
        return;
      }
      try {
        const res = await fetch('/api/sprints/parse', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ markdown: text })
        });
        if (res.ok) {
          const data = await res.json();
          const steps = data.steps || [];
          previewEl.innerHTML = `<strong>Parsed ${steps.length} sequential steps:</strong> ` + steps.map(s => escapeHtml(s.name)).join(' ➔ ');
        }
      } catch (_) {}
    };

    window.launchAutonomousSprint = async function() {
      const title = document.getElementById('sprintTitleInput')?.value?.trim() || 'Sprint Task';
      const harness = document.getElementById('sprintHarnessSelect')?.value || 'claude';
      const text = document.getElementById('sprintChecklistInput')?.value || '';
      if (!text.trim()) {
        alert('Please enter at least one task or checklist item');
        return;
      }

      const parseRes = await fetch('/api/sprints/parse', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ markdown: text })
      });
      const parseData = await parseRes.json().catch(() => ({}));
      const steps = parseData.steps || [];

      const curWorkdir = state.selectedWorkdir || (state.detail ? state.detail.summary.workdir : '');

      const createRes = await fetch('/api/sprints', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          title,
          harness,
          workdir: curWorkdir,
          profile: harness === state.selectedHarness ? state.selectedProfile : null,
          steps
        })
      });

      if (!createRes.ok) {
        const err = await createRes.json().catch(() => ({}));
        alert(err.error || 'Failed to launch sprint');
        return;
      }

      closeOverlays();
      await fetchState();
    };

    window.cancelSprint = async function(id) {
      await fetch(`/api/sprints/${encodeURIComponent(id)}/cancel`, { method: 'POST' });
      await fetchState();
      openSprintSheet();
    };

    window.deleteSprint = async function(id) {
      await fetch(`/api/sprints/${encodeURIComponent(id)}`, { method: 'DELETE' });
      await fetchState();
      openSprintSheet();
    };

    document.getElementById('sendBtn').addEventListener('click', sendMessage);
    composerInput.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' && !e.shiftKey && window.innerWidth > 640) {
        e.preventDefault();
        sendMessage();
      }
    });

    let audioCtx = null;
    function playAudioChime(type) {
      if (!state.audioChimes) return;
      try {
        const AudioContext = window.AudioContext || window.webkitAudioContext;
        if (!AudioContext) return;
        if (!audioCtx) audioCtx = new AudioContext();
        if (audioCtx.state === 'suspended') audioCtx.resume();

        const now = audioCtx.currentTime;
        if (type === 'start') {
          const osc = audioCtx.createOscillator();
          const gain = audioCtx.createGain();
          osc.type = 'sine';
          osc.frequency.setValueAtTime(440, now);
          osc.frequency.exponentialRampToValueAtTime(880, now + 0.08);
          gain.gain.setValueAtTime(0.08, now);
          gain.gain.exponentialRampToValueAtTime(0.001, now + 0.1);
          osc.connect(gain);
          gain.connect(audioCtx.destination);
          osc.start(now);
          osc.stop(now + 0.1);
        } else if (type === 'stop') {
          const osc = audioCtx.createOscillator();
          const gain = audioCtx.createGain();
          osc.type = 'sine';
          osc.frequency.setValueAtTime(880, now);
          osc.frequency.exponentialRampToValueAtTime(440, now + 0.08);
          gain.gain.setValueAtTime(0.08, now);
          gain.gain.exponentialRampToValueAtTime(0.001, now + 0.1);
          osc.connect(gain);
          gain.connect(audioCtx.destination);
          osc.start(now);
          osc.stop(now + 0.1);
        } else if (type === 'success') {
          [523.25, 659.25, 783.99].forEach((freq, idx) => {
            const osc = audioCtx.createOscillator();
            const gain = audioCtx.createGain();
            osc.type = 'triangle';
            osc.frequency.setValueAtTime(freq, now + idx * 0.06);
            gain.gain.setValueAtTime(0.06, now + idx * 0.06);
            gain.gain.exponentialRampToValueAtTime(0.001, now + idx * 0.06 + 0.25);
            osc.connect(gain);
            gain.connect(audioCtx.destination);
            osc.start(now + idx * 0.06);
            osc.stop(now + idx * 0.06 + 0.25);
          });
        } else if (type === 'alert') {
          [349.23, 493.88].forEach((freq) => {
            const osc = audioCtx.createOscillator();
            const gain = audioCtx.createGain();
            osc.type = 'sawtooth';
            osc.frequency.setValueAtTime(freq, now);
            gain.gain.setValueAtTime(0.1, now);
            gain.gain.exponentialRampToValueAtTime(0.001, now + 0.35);
            osc.connect(gain);
            gain.connect(audioCtx.destination);
            osc.start(now);
            osc.stop(now + 0.35);
          });
        }
      } catch (e) {
        console.warn('Audio chime error:', e);
      }
    }

    window.toggleWalkieTalkieMode = function() {
      state.walkieTalkie = !state.walkieTalkie;
      localStorage.setItem('helm_walkie', state.walkieTalkie);
      updateWalkieTalkieUI();
    };

    function updateWalkieTalkieUI() {
      const btn = document.getElementById('walkieTalkieToggleBtn');
      const lbl = document.getElementById('walkieTalkieLabel');
      if (btn && lbl) {
        if (state.walkieTalkie) {
          btn.classList.add('active');
          lbl.textContent = '📻 Walkie ON';
        } else {
          btn.classList.remove('active');
          lbl.textContent = '🎙️ Walkie';
        }
      }
    }

    let isRecording = false;
    let activeSpeechRec = null;
    let micMeterInterval = null;

    function startVoiceRecording() {
      if (isRecording) return;
      if (!window.isSecureContext && location.hostname !== 'localhost' && location.hostname !== '127.0.0.1') {
        showToast('Mic blocked by browser over insecure HTTP. Use HTTPS, Tailscale with cert, or keyboard mic.');
        return;
      }
      const SpeechRec = window.SpeechRecognition || window.webkitSpeechRecognition;
      if (!SpeechRec) {
        alert('Speech recognition not supported in this browser. Use mobile keyboard mic.');
        return;
      }

      isRecording = true;
      playAudioChime('start');

      const waveBar = document.getElementById('voiceWaveBar');
      const micBtn = document.getElementById('micBtn');
      const levelFill = document.getElementById('voiceLevelFill');
      const statusText = document.getElementById('voiceWaveStatus');
      if (waveBar) waveBar.style.display = 'flex';
      if (micBtn) micBtn.classList.add('active');
      if (statusText) statusText.textContent = state.walkieTalkie ? '📻 Walkie: Speak & Release to Send…' : '🎙️ Listening…';

      if (micMeterInterval) clearInterval(micMeterInterval);
      micMeterInterval = setInterval(() => {
        if (!isRecording) return;
        const randomLevel = Math.floor(Math.random() * 70) + 20;
        if (levelFill) levelFill.style.width = `${randomLevel}%`;
      }, 100);

      activeSpeechRec = new SpeechRec();
      activeSpeechRec.continuous = false;
      activeSpeechRec.interimResults = false;

      activeSpeechRec.onresult = async (ev) => {
        const transcript = ev.results[0][0].transcript;
        await handleVoiceTranscript(transcript);
      };

      activeSpeechRec.onerror = (err) => {
        console.warn('Speech recognition error:', err);
        if (err.error === 'not-allowed') {
          showToast('Mic permission denied. Mobile browsers require HTTPS or localhost for audio.');
        }
        stopVoiceRecording(false);
      };

      activeSpeechRec.onend = () => {
        stopVoiceRecording(false);
      };

      try {
        activeSpeechRec.start();
      } catch (err) {
        stopVoiceRecording(false);
      }
    }

    function stopVoiceRecording(triggerStop = true) {
      if (!isRecording) return;
      isRecording = false;
      playAudioChime('stop');

      if (micMeterInterval) {
        clearInterval(micMeterInterval);
        micMeterInterval = null;
      }

      const waveBar = document.getElementById('voiceWaveBar');
      const micBtn = document.getElementById('micBtn');
      if (waveBar) waveBar.style.display = 'none';
      if (micBtn) micBtn.classList.remove('active');

      if (triggerStop && activeSpeechRec) {
        try { activeSpeechRec.stop(); } catch(_) {}
      }
      activeSpeechRec = null;
    }

    window.cancelVoiceRecording = function() {
      if (activeSpeechRec) {
        try { activeSpeechRec.abort(); } catch(_) {}
      }
      stopVoiceRecording(false);
    };

    async function handleVoiceTranscript(rawText) {
      const text = (rawText || '').trim();
      if (!text) return;

      try {
        const res = await fetch('/api/voice/transcribe', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ speech_text: text })
        });
        if (res.ok) {
          const data = await res.json();
          const intent = data.intent || { action: 'chat_prompt', payload: text };

          if (intent.action === 'stop_task') {
            if (state.activeId) window.stopConvTask(state.activeId);
            return;
          } else if (intent.action === 'fix_ci') {
            const curWorkdir = state.selectedWorkdir || (state.detail ? state.detail.summary.workdir : '');
            window.runPrAction(curWorkdir, 'fix-ci');
            return;
          } else if (intent.action === 'check_ci') {
            document.getElementById('headerTitleWrap').click();
            return;
          } else if (intent.action === 'review_diff') {
            const h = state.selectedHarness || 'codex';
            window.triggerSecondOpinion(h, 'review-diff');
            return;
          } else if (intent.action === 'swarm_bounce') {
            if (intent.payload && intent.payload.target_harness) {
              state.swarmTargetHarness = intent.payload.target_harness;
              const input = document.getElementById('swarmInstructionsInput');
              if (input && intent.payload.custom_instructions) {
                input.value = intent.payload.custom_instructions;
              }
              window.triggerSwarmBounce();
              return;
            }
          }
        }
      } catch (e) {
        console.warn('Voice intent parse error, falling back to direct prompt:', e);
      }

      if (state.walkieTalkie) {
        composerInput.value = text;
        sendMessage();
      } else {
        composerInput.value = (composerInput.value ? composerInput.value + ' ' : '') + text;
        composerInput.dispatchEvent(new Event('input'));
        composerInput.focus();
      }
    }

    const micBtnEl = document.getElementById('micBtn');
    let pressTimer = null;
    let isPressAndHold = false;

    micBtnEl.addEventListener('pointerdown', () => {
      isPressAndHold = false;
      pressTimer = setTimeout(() => {
        isPressAndHold = true;
        startVoiceRecording();
      }, 220);
    });

    micBtnEl.addEventListener('pointerup', () => {
      clearTimeout(pressTimer);
      if (isPressAndHold) {
        stopVoiceRecording(true);
      } else {
        if (isRecording) {
          stopVoiceRecording(true);
        } else {
          startVoiceRecording();
        }
      }
    });

    micBtnEl.addEventListener('pointercancel', () => {
      clearTimeout(pressTimer);
      if (isRecording) stopVoiceRecording(false);
    });

    function checkNewApprovals(pending) {
      if (!pending || pending.length === 0) return;
      for (const a of pending) {
        if (!state.seenApprovalIds.has(a.id)) {
          state.seenApprovalIds.add(a.id);
          triggerHitlAlert(a.risk_level);
          if ('Notification' in window && Notification.permission === 'granted') {
            new Notification(`⚠️ Approval Required: ${a.tool_name}`, {
              body: `${a.harness.toUpperCase()}: ${a.target}`,
              icon: '/icon-192.png'
            });
          }
        }
      }
    }

    function triggerHitlAlert(risk) {
      if (navigator.vibrate) {
        if (risk === 'critical') navigator.vibrate([200, 80, 200, 80, 200]);
        else navigator.vibrate([120, 60, 120]);
      }
      try {
        const ctx = new (window.AudioContext || window.webkitAudioContext)();
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.type = 'triangle';
        osc.frequency.setValueAtTime(440, ctx.currentTime);
        osc.frequency.exponentialRampToValueAtTime(880, ctx.currentTime + 0.18);
        gain.gain.setValueAtTime(0.2, ctx.currentTime);
        gain.gain.exponentialRampToValueAtTime(0.01, ctx.currentTime + 0.3);
        osc.connect(gain);
        gain.connect(ctx.destination);
        osc.start();
        osc.stop(ctx.currentTime + 0.3);
      } catch (e) {}
    }

    function renderApprovalDock() {
      if (deferRender(renderApprovalDock)) return;
      const dock = document.getElementById('approvalDock');
      if (!dock) return;

      const appr = state.pendingApprovals.find(a => a.conversation_id === state.activeId)
        || state.pendingApprovals[0];

      if (!appr) {
        dock.classList.add('hidden');
        replaceChangedHtml(dock, '');
        return;
      }

      const riskClass = `risk-${appr.risk_level.toLowerCase()}`;
      dock.className = `approval-dock ${riskClass}`;

      const badgeColor = appr.risk_level === 'critical' ? '#ef4444'
        : appr.risk_level === 'high' ? '#f97316'
        : '#eab308';

      replaceChangedHtml(dock, `
        <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:7px">
          <div style="display:flex;align-items:center;gap:6px">
            <span style="font-size:15px">🛡️</span>
            <span style="font-size:12px;font-weight:700;letter-spacing:0.3px;color:#fff">HITL APPROVAL REQUIRED</span>
          </div>
          <span style="font-size:10px;font-weight:800;text-transform:uppercase;padding:2px 7px;border-radius:6px;background:${badgeColor};color:#fff">${escapeHtml(appr.risk_level)}</span>
        </div>
        <div style="font-size:11.5px;color:var(--text-muted);margin-bottom:6px">
          Agent <strong style="color:var(--accent);text-transform:uppercase">${escapeHtml(appr.harness)}</strong> requesting execution of <code style="color:#fff">${escapeHtml(appr.tool_name)}</code>:
        </div>
        <div style="background:#0e0e0e;border:1px solid rgba(255,255,255,0.12);border-radius:8px;padding:7px 10px;font-family:ui-monospace,monospace;font-size:11px;color:#f3f4f6;word-break:break-all;max-height:85px;overflow-y:auto;margin-bottom:9px">${escapeHtml(appr.target)}</div>
        ${appr.details ? `<details style="margin:-2px 0 9px"><summary style="font-size:10.5px;color:var(--text-muted);cursor:pointer;user-select:none">raw tool input (what actually runs)</summary><div style="background:#0e0e0e;border:1px solid rgba(255,255,255,0.12);border-radius:8px;padding:7px 10px;font-family:ui-monospace,monospace;font-size:10.5px;color:#fca5a5;word-break:break-all;max-height:110px;overflow-y:auto;margin-top:5px">${escapeHtml(appr.details)}</div></details>` : ''}
        <div style="display:grid;grid-template-columns:1fr 1fr 1fr;gap:6px">
          <button class="new-thread-btn" style="background:#10b981;color:#fff;justify-content:center;padding:7px 0;font-size:11.5px;font-weight:650" onclick="submitApprovalDecision('${escapeAttr(appr.id)}', 'allow_once')">✓ Allow</button>
          <button class="new-thread-btn" style="background:#4f46e5;color:#fff;justify-content:center;padding:7px 0;font-size:11.5px;font-weight:650" onclick="submitApprovalDecision('${escapeAttr(appr.id)}', 'allow_always')" title="Remember this exact call for this thread (High/Critical risk always re-prompts)">🛡️ Always</button>
          <button class="new-thread-btn" style="background:#ef4444;color:#fff;justify-content:center;padding:7px 0;font-size:11.5px;font-weight:650" onclick="submitApprovalDecision('${escapeAttr(appr.id)}', 'deny')">✕ Deny</button>
        </div>
      `);
    }

    window.submitApprovalDecision = async function(apprId, decision) {
      try {
        await apiRequest(`/api/approvals/${encodeURIComponent(apprId)}/decision`, jsonRequest('POST', { decision }));
        state.pendingApprovals = state.pendingApprovals.filter(a => a.id !== apprId);
        renderApprovalDock();
        await fetchState();
        if (state.activeId) await loadConversation(state.activeId, false);
      } catch (e) {
        showToast(e.message || 'Could not submit this permission decision.');
      }
    };

    window.setSafetyMode = async function(mode) {
      try {
        await apiRequest('/api/approvals/safety-mode', jsonRequest('POST', { mode }));
        state.safetyMode = mode;
        updateSafetyModeUI();
      } catch (e) {
        showToast(e.message || 'Could not change permission mode.');
      }
    };

    function updateSafetyModeUI() {
      const mode = (state.safetyMode || 'standard').toLowerCase();
      const badge = document.getElementById('safetyModeBadge');
      if (badge) badge.textContent = mode.toUpperCase();

      ['Safe', 'Standard', 'Yolo'].forEach(m => {
        const btn = document.getElementById(`safetyMode${m}`);
        if (btn) {
          if (m.toLowerCase() === mode) btn.classList.add('active');
          else btn.classList.remove('active');
        }
      });
    }

    // Adaptive poll loop: fast cadence while anything is busy/streaming so a
    // turn feels live, slow when idle. Paused entirely while the page is
    // hidden (iOS Safari background tabs), resumed instantly on visibility.
    // Network/server failures grow the interval so a hung daemon cannot churn.
    let pollTimer = null;
    let pollPaused = false;

    function scheduleNextPoll() {
      if (pollPaused) return;
      clearTimeout(pollTimer);
      const busy = state.conversations.some(c => c.status === 'busy')
        || (state.sprints || []).some(s => s.status === 'running');
      const backoff = Math.min(pollFailures * 2000, 10000);
      const delay = (busy ? 700 : 2000) + backoff;
      pollTimer = setTimeout(pollLoop, delay);
    }

    async function pollLoop() {
      await fetchState();
      if (!pollPaused) scheduleNextPoll();
    }

    document.addEventListener('visibilitychange', () => {
      if (document.hidden) {
        pollPaused = true;
        clearTimeout(pollTimer);
      } else {
        pollPaused = false;
        pollLoop();
      }
    });

    pollLoop();
  </script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_valid_json() {
        let val: serde_json::Value =
            serde_json::from_str(MANIFEST_JSON).expect("MANIFEST_JSON should be valid JSON");
        assert_eq!(val["display"], "standalone");
    }

    #[test]
    fn test_index_html_has_core_elements() {
        assert!(INDEX_HTML.contains("id=\"chatScroll\""));
        assert!(INDEX_HTML.contains("id=\"composerInput\""));
        assert!(INDEX_HTML.contains("id=\"headerHarnessDot\""));
        assert!(INDEX_HTML.contains("id=\"runningStopBtn\""));
    }

    #[test]
    fn test_embedded_javascript_syntax_if_node_present() {
        let script_start = INDEX_HTML.find("<script>").expect("script tag missing");
        let script_end = INDEX_HTML.rfind("</script>").expect("script close missing");
        let js = &INDEX_HTML[script_start + 8..script_end];

        if let Ok(mut child) = std::process::Command::new("node")
            .arg("--check")
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(js.as_bytes());
            }
            let output = child
                .wait_with_output()
                .expect("failed to wait on node --check");
            assert!(
                output.status.success(),
                "Embedded cockpit JS syntax error:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
