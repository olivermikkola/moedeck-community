---
name: Bug report
about: Something broke while using MoE Deck
title: ''
labels: bug
assignees: ''
---

**What happened?**
A clear description of the failure. If a harness (claude/codex/opencode/agy/keel) misbehaved, name it.

**To reproduce**
1. Run `moedeck` on … (OS, version via `moedeck --version`)
2. Tap / type …
3. See this: (error text, or paste of `/api/bootstrap` output with private fields removed)

**What did you expect instead?**

**Environment**
- OS:
- Browser (if cockpit-related):
- Harness CLIs installed (`claude`, `codex`, `opencode`, …):
- Was the daemon run via systemd or manually? (`systemctl --user status moedeck`)
