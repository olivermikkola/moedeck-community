# MoE Deck Competitive Landscape & Product Strategy (2026)

This document analyzes the current state of mobile and remote AI coding agent interfaces across major frontier labs and top open-source projects, and defines **MoE Deck's** competitive architecture.

---

## 1. Frontier AI Lab Offerings (2025–2026)

### A. Claude Code Remote Control (`claude.ai/code` & Claude iOS/Android App)
- **Architecture:** Launched in Feb 2026 (`claude remote-control`). The local CLI maintains an outbound HTTPS/WSS bridge to Anthropic's cloud (`api.anthropic.com`), allowing the Claude mobile app or `claude.ai/code` web UI to steer a local terminal session.
- **Strengths:**
  - Best-in-class dark editorial typography (`#141413` canvas, collapsible `N steps` tool summaries, clean pill composer).
  - `@file` path autocomplete from the local working directory.
  - Inline photo/screenshot upload from phone camera roll.
  - Keeps local filesystem and MCP servers intact.
- **Weaknesses & Lock-in:**
  - **100% locked to Claude (`api.anthropic.com`)**: Cannot run Google Antigravity (`agy`), OpenAI Codex (`codex`), or OpenCode (`opencode`).
  - **Breaks with custom routers/proxies**: Cannot authenticate when `ANTHROPIC_BASE_URL` points to a local router like OmniRoute (`localhost:20128`) or custom model providers.
  - **Session fragility**: Terminal disconnects or network drops require manual `tmux` attachment (`claude-rc.service`) and only expose the single session launched with `remote-control`.

### B. Cursor Background Agents (`cursor.com/agents` & Mobile PWA)
- **Architecture:** Spawns isolated cloud Ubuntu VMs cloned from a GitHub repository.
- **Strengths:**
  - Live git branch & diff inspection (`+`/`-` line counters, changed files list, 1-tap PR view).
  - Follow-up prompts can be sent mid-run from a phone browser.
  - Multi-model support inside Cursor's cloud billing wrapper.
- **Weaknesses:**
  - **Cloud-only execution**: Runs on remote VMs rather than your local workstation/Beelink (no access to local GPUs, local Xvfb/Vulkan display servers, unpushed branches, or local databases).
  - **Per-VM startup latency** and separate cloud compute billing.

### C. OpenAI Codex Web (`chatgpt.com/codex`) & Codex App Server
- **Architecture:** Cloud container execution (`chatgpt.com/codex`) plus the local `codex app-server` JSON-RPC daemon.
- **Strengths:**
  - Structured rollout logs (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`) with clear separation of reasoning, command execution, and final patches.
  - Diff-first review UX before creating a PR.
- **Weaknesses:**
  - Locked exclusively to OpenAI models unless manually reconfigured via `codex` CLI profiles; web UI does not discover local Claude, AGY, or OpenCode sessions.

### D. Cognition Devin 2.0
- **Architecture:** Autonomous cloud engineering environment with mobile Slack/Telegram/web notifications and an interactive IDE pane.
- **Strengths:**
  - Interactive planning and confidence signals; easy PR review from mobile.
- **Weaknesses:**
  - Heavy desktop multi-pane UI (shell + browser + editor) is cramped on a phone screen; cloud-isolated from local hardware.

---

## 2. Leading Open-Source Projects (2025–2026)

| Project | Focus | What Works Well | Where It Falls Short on Mobile |
| :--- | :--- | :--- | :--- |
| **OpenCode (`sst/opencode` & `opencode-manager`)** | Local SQLite-backed CLI (`opencode.db`) + HTTP server (`opencode serve`) | Clean SQLite schema (`session`, `message`, `part` tables); fast streaming; wide model support (Kimi, GLM, DeepSeek, Claude, GPT) | Single-harness only; web frontends expose desktop file trees and split panes rather than a thumb-friendly mobile chat |
| **AionUi (`iOfficeAI/AionUi`)** | Unified GUI for Claude Code, Gemini CLI, Codex, OpenCode | Proves high demand for a single dashboard across multiple local CLI agents | Built with Electron for desktop monitors; not a lightweight Rust daemon for Tailscale mobile use |
| **Claude-Code-Remote / Telegram Bridges** | Controlling CLI agents via Telegram/Discord bots | Zero install on mobile | Chat apps mangle long markdown diffs, lose collapsible tool accordions, drop image attachments (`photo` vs `document`), and cannot switch models/harnesses cleanly |
| **OpenHands (formerly OpenDevin)** | Containerized web agent platform | Rich trajectory viewer and git diff inspector | Heavy Docker runtime overhead; cannot attach to existing native host sessions (`~/.claude`, `~/.gemini`, `~/.codex`) |

---

## 3. Feature Matrix: MoE Deck vs. The Field

| Capability | Claude Remote Control | Cursor Web Agents | OpenCode Manager | AionUi | **MoE Deck Gateway** |
| :--- | :---: | :---: | :---: | :---: | :---: |
| **Runs on Local Hardware & GPU (Zero Cloud VM)** | ✅ | ❌ | ✅ | ✅ | **✅** |
| **Multi-Harness (`Claude` + `AGY` + `Codex` + `OpenCode`)** | ❌ | ❌ | ❌ | ✅ | **✅** |
| **1-Tap Mid-Thread Harness Handoff** | ❌ | ❌ | ❌ | ❌ | **✅** |
| **Works with Custom Routers (OmniRoute / Local Proxies)** | ❌ | ❌ | ✅ | ✅ | **✅** |
| **Native Discovery of Existing Host CLI Sessions** | ❌ | ❌ | ⚠️ (OpenCode only) | ⚠️ | **✅ (All 4)** |
| **Editorial Mobile UI (`#141413` + Collapsible Steps)** | ✅ | ⚠️ | ❌ | ❌ | **✅** |
| **`@file` Path Autocomplete in Mobile Composer** | ✅ | ✅ | ✅ | ❌ | **✅** |
| **Live Workspace Git Status, Diff Viewer & PR Links** | ❌ | ✅ | ✅ | ❌ | **✅** |
| **1-Tap Project Switcher (`~/Projects/*`) & Pinning** | ❌ | ✅ | ✅ | ✅ | **✅** |
| **Single Rust Binary (`<6 MB` RAM, Tailscale Auto-Auth)** | ❌ | N/A | ❌ | ❌ | **✅** |

---

## 4. MoE Deck's Core Differentiators

1. **True Multi-Harness Native Discovery & Execution:**
   MoE Deck reads and writes natively across:
   - **Claude Code** (`~/.claude/projects/*/*.jsonl` & `claude -p --output-format stream-json`)
   - **Antigravity CLI** (`~/.gemini/antigravity-cli/brain/*/.system_generated/logs/transcript.jsonl` & `agy -p --output-format stream-json`)
   - **OpenAI Codex** (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` & `codex exec --json --dangerously-bypass-approvals-and-sandbox`)
   - **OpenCode** (`~/.local/share/opencode/opencode.db` SQLite & `opencode run --format json --thinking --auto`)

2. **Seamless Cross-Harness Context Handoff:**
   Switch the bottom pill from `Claude` to `AGY`, `Codex`, or `OpenCode` in the middle of any conversation. MoE Deck automatically packages the recent conversation transcript and workspace state so the new harness picks up right where the previous one left off—while keeping all messages unified in a single thread.

3. **Zero-Friction Mobile Ergonomics:**
   - **Top Quick-Bounce Bar:** Jump between active threads across different harnesses in one thumb tap.
   - **Drawer Search & Harness Filter:** Filter threads by harness (`All`, `Claude`, `AGY`, `Codex`, `OpenCode`) or instant keyword search.
   - **`@file` Autocomplete:** Type `@` in the composer to fuzzy-search files in the thread's project directory.
   - **Workspace & Git Diff Sheet:** Tap the header title to inspect the current git branch, uncommitted file statuses, inline syntax-colored `git diff`, and open PR links, or switch the thread's target project directory (`~/Projects/*`).
