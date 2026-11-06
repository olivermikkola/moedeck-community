# Vision: MoE Deck

MoE Deck exists so one person can steer multiple autonomous coding agents across projects from a phone or desktop with the clarity of a single conversation and zero terminal ceremony.

## 1. The Problem With 2024–2025 Agent Tooling

Most developer tools for CLI coding agents fall into one of two traps:

1. **Terminal-on-a-Phone Syndrome:** Trying to cram `tmux` panes, raw `git diff` outputs, file trees, and virtual `Ctrl`/`Esc` keybars onto a 6-inch mobile screen. When you are away from your desk or on the couch, you do not want to be a terminal operator—you want to direct outcomes, inspect visual results, and make high-level decisions.
2. **Vendor & App Silos:** Claude Code has its own session format and mobile bridge; Antigravity (`agy`) has its own web portal and brain transcripts; Codex and OpenCode have their own CLIs. When one vendor hits a spend/rate limit, or when you want a second model's judgment on the same repository, you are forced to switch apps, lose thread context, or copy-paste across windows.

## 2. Core Design Principles

### One Captain, Multi-Harness Freedom
- **Harness-Agnostic Command:** A conversation belongs to your project and intent, not to a single CLI vendor. In MoE Deck, the **Harness Selector** sits right beside the **Model & Effort Selector** in the composer bar.
- **Seamless Mid-Thread Handoff:** You can start an investigation with **Claude Code**, flip the harness pill to **Antigravity** or **Codex**, and keep talking in the exact same thread. MoE Deck handles the context handoff automatically below deck.

### Instant Bouncing & Unmistakable Identity
- **Zero-Friction Thread Bouncing:** Switching between two or three active streams of work (e.g., a game-development task and a systems/tooling task) takes a single thumb tap via the top Quick-Bounce strip or the top-right Flip pill—never a multi-step menu dive.
- **At-a-Glance Visual Signatures:** While the dark editorial layout stays calm and uniform (`#141413`), the active harness's signature accent color adapts immediately so you always know which engine is driving:
  - **Claude Code (`claude`):** Warm Terracotta (`#D97757`)
  - **Antigravity (`agy`):** Electric Blue (`#4F8FF7`)
  - **Codex (`codex`):** Emerald Green (`#10A37F`)
  - **OpenCode (`opencode`):** Amethyst Purple (`#A855F7`)

### Outcomes Above the Line, Machinery Below Deck
- **Editorial Prose First:** Responses read like a clean briefing. Intermediate one-line tool narrations and 50-step bash loops are folded automatically into a single collapsible `⚡ N steps` pill.
- **Automatic Visual Proof:** When an agent generates or inspects images on disk (street-level game renders, before/after postcards, UI screenshots in `/tmp/` or the workspace), MoE Deck automatically surfaces them as inline, zoomable image cards inside the turn—without requiring manual file transfers.
- **Working Multimodal Input:** Attaching a phone screenshot or photo via `+` uploads directly to the host and binds the local path into the agent's prompt so visual feedback never gets dropped.

### Single-Binary Rust Simplicity & Silence
- **No Framework Sprawl:** MoE Deck compiles into a single standalone Rust binary (`hyper` + `tokio`) with the PWA frontend embedded at compile time.
- **Lightweight on Host Hardware:** Keep idle resource use low so the cockpit can share a machine with development workloads.
