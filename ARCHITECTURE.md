# Architecture: MoE Deck

MoE Deck is structured as a single-binary Rust daemon (`moedeck`) that bridges mobile/desktop Progressive Web App (PWA) clients over Tailscale/LAN to local CLI coding harnesses (`claude`, `agy`, `codex`, `opencode`).

```
+-----------------------------------------------------------------------+
|                       Mobile / Desktop PWA Client                     |
|  - Top Quick-Bounce Strip + 1-Tap Flip Pill                           |
|  - Editorial Markdown Stream + Collapsible Tool Steps + Image Cards   |
|  - Bottom Composer: [+]  [● Harness ▾]  [Model · Effort ▾]  [🎙️]  [↑] |
+-----------------------------------+-----------------------------------+
                                    | HTTP / JSON over Tailscale
+-----------------------------------v-----------------------------------+
|                        MoE Deck Rust Daemon (hyper + tokio)               |
|                                                                       |
|  +-----------------------+  +-------------------+  +---------------+  |
|  |   src/discovery.rs    |  |   src/runner.rs   |  |  src/auth.rs  |  |
|  | Lock-free JSONL tail  |  | Async subprocess  |  | Tailscale IP  |  |
|  | & session synthesizer |  | dispatcher &      |  | & HMAC-SHA256 |  |
|  | across harnesses      |  | context handoff   |  | cookie auth   |  |
|  +-----------+-----------+  +---------+---------+  +---------------+  |
+--------------|------------------------|-------------------------------+
               | (Read-only tail)       | (Stdio::null + piped stdout)
     +---------+---------+    +---------+---------+
     | On-Disk Sessions  |    |   CLI Harnesses   |
     | - ~/.claude/      |    | - claude          |
     | - ~/.gemini/      |    | - agy             |
     | - ~/.codex/       |    | - codex           |
     | - ~/.config/moedeck/ |    | - opencode        |
     +-------------------+    +-------------------+
```

---

## 1. Lock-Free Session Discovery (`src/discovery.rs`)

A core requirement of MoE Deck is that **sessions started outside MoE Deck** (e.g., an autonomous Claude Code loop started in a workstation terminal or an Antigravity session) must appear live inside MoE Deck without file-locking conflicts or full-file parsing overhead.

### Bounded Tail Seeking for Multi-Megabyte Logs
Active agent JSONL logs (especially those containing base64-encoded image `tool_result` payloads) routinely grow to 50–200 MB. Reading and parsing the entire file on mobile refresh would spike CPU and latency.
- **Head Probe (24 KB):** MoE Deck reads the first 24 KB of each `.jsonl` file to extract the canonical thread title (`custom-title` or the opening sentence of the initial user prompt, such as `"3D Auto ongelmat"`).
- **Tail Seek (3–12 MB):** MoE Deck seeks directly to the tail of the file (`SeekFrom::Start(len.saturating_sub(window))`) and pre-filters lines before JSON deserialization—skipping multi-hundred-kilobyte `tool_result` base64 lines in $O(1)$ string checks before invoking `serde_json`.
- **Step Folding:** Within a single user turn, intermediate one-sentence progress notes (`< 280` chars) and tool invocations (`Bash`, `Read`, `Edit`) are folded into a single `Vec<ToolSummaryItem>` attached to the substantive outcome message.
- **Visual Render Extraction:** When an agent invokes `Read` on an image path (`.png`, `.jpg`, `.webp`) in `/tmp/` or `~/Projects/`, `discovery.rs` automatically attaches a scoped `/api/local-image?path=...` URL to the turn so the user sees the exact renders the agent inspected.

---

## 2. Multi-Harness Subprocess Dispatcher (`src/runner.rs`)

When the user sends a prompt from MoE Deck, `dispatch_turn` normalizes differences across CLI harnesses:

### Piped Stdin as the HITL Channel
Each harness child is spawned with piped stdin so the interactive approval broker (`approval.rs`) can answer `[y/N]` prompts observed in the stream by writing `y\n`/`n\n` back into the live process (`RunnerState::send_stdin`). CLIs that check `isatty(STDIN_FILENO)` still stream immediately, because the prompt is passed as an argv (`-p`) rather than waited on over stdin.

### Per-Account Shadow Homes (`src/profiles.rs`)
One harness can be driven with **multiple accounts in parallel**. Every non-default account runs its CLI with `HOME` re-rooted to a *shadow home* under `~/.config/moedeck/profiles/<id>/home`: a symlink mirror of the real `$HOME` (so `~/Projects`, `.gitconfig`, binaries and caches behave normally) except the harness's isolation paths (`~/.gemini`, `~/.claude`, `~/.codex`, `~/.keel`, opencode's `XDG` data/config dirs) — those are private real directories where only that account signs in. Conversation ids carry the account (`agy:agy-2:<session>`), discovery scans every account's home, and the overlay pins a MoE Deck thread to its account across restarts. The default account is never touched: its "profile" is simply the real home, and deleting a profile unregisters it without deleting credentials unless `?purge=1`.

### Lock-Safe Execution & Context Handoff
If a native session is currently open and locked by another process (for example, an active `claude` session with `status: "busy"` in `~/.claude/sessions/*.json` or an open `agy` session), passing `--resume <id>` or `--conversation <id>` would either block or fail on session lock acquisition.
- **Idle Native Sessions:** Resumed natively when safe.
- **Busy / Cross-Harness Sessions:** MoE Deck constructs a compact `[Recent thread context]` block from the last 6 turns and dispatches a stateless `-p` invocation in the same `workdir`, persisting the user and assistant messages in `~/.config/moedeck/threads/<id>.json` (the thread overlay).
- **Sub-Transcript Filtering:** Any sub-transcript created on disk by a `[Recent thread context]` invocation is automatically ignored by `discovery.rs` so it never pollutes the top-level conversation list.

---

## 3. Authentication & Security Boundary (`src/auth.rs`)

- **Network Boundary:** Requests originating from Tailscale CGNAT (`100.64.0.0/10`), Tailscale IPv6 ULA (`fd7a:115c:a1e0::/48`), or loopback/private LAN are trusted when `trust_tailscale: true` in `~/.config/moedeck/auth.json`.
- **Signed Session Cookie:** Supports HMAC-SHA256 constant-time verified `helm_session` cookies for non-Tailscale contexts.
- **Scoped Image Serving (`/api/local-image`):** Rejects any path traversal (`..`) and strictly whitelists image extensions under `/tmp/`, `~/Projects/`, `~/.gemini/`, and `~/.config/moedeck/`.

---

## 4. Roadmap & Next Evolution

1. **Server-Sent Events (SSE) / WebSocket Push:** Replace the 1.8s background poll with an event-driven SSE stream using `notify` inotify watchers on `~/.claude/projects/` and `~/.gemini/antigravity-cli/brain/`.
2. **Web Push Notifications:** Alert the phone lock screen when a long-running background task (e.g., a 40-minute GitHub Actions CI watcher) transitions from `busy` to `idle`. (Partial: PWA `Notification` + `/sw.js` cover turn-completion today.)
3. ~~One-Tap PR & Worktree Actions~~ — shipped (`/api/pr-action`, `/api/worktrees`, live port previews).
4. **Per-Account Preview Origins:** Serve harness previews from a second origin so proxied local dev servers can't script the cockpit APIs (see SECURITY audit notes).
