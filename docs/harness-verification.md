# Harness fixes verified on 2026-11-06

The audit found that OpenCode could write before MoE Deck's Deny response, native
OpenCode threads could hide gateway failures, and V2 history returned the first
400 messages instead of the latest messages. New Claude/Antigravity threads
also lacked native session binding. Antigravity 1.5 uses nested `event` payloads
that the earlier parser did not render.

The fixes enforce OpenCode Safe policy inside a private native process, render
actual tool progress without retrospective approval cards, keep gateway errors
and Stop/handoff messages, load a bounded recent history with omission counts,
and adopt/resume validated Claude and Antigravity session UUIDs. Antigravity's
nested text deltas, tool state and terminal result/status are parsed directly.

## Automated checks

```text
cargo test --locked -j 2                          88 passed; 0 failed
cargo clippy --locked --all-targets -j 2 -- -D warnings  clean
cargo fmt --all --check                           clean
cargo build --release --locked -j 2               passed
npm test                                         harness + Chromium + WebKit passed
```

The harness transport test uses actual gateway HTTP routes, SQLite and scripted
CLI subprocesses. It checks native follow-up arguments, model IDs and AGY
response decoding; the newest message and omission counts in a 404-message
thread; failed/stopped runs; repeated prompts; stale streaming snapshots;
handoff messages; and per-run OpenCode Safe configuration. The fixture is not
evidence that a real native permission implementation enforces policy.

## Real native runs through the gateway

All runs used disposable projects and an isolated MoE Deck configuration, with the
installed CLI and primary account. Existing user conversations were not used.
No new provider, persistent service or runtime dependency was added.

| Harness | Two-turn token check | Native session |
|---|---|---|
| Codex CLI 0.166.0, configured default | Both exact answers passed | Same UUID |
| OpenCode 2.2.5, configured default | Both exact answers passed | Same session |
| Antigravity 1.5.1, `gemini-3.8-flash-low` | Both exact answers passed | Same UUID |
| Keel 0.0.1, already-running local `bonsai-9b` | Both exact answers passed | Same session |
| Claude Code 2.1.309 | Both exact answers passed after the reported 03:00 quota reset | Same UUID |

The available AGY model list includes `gemini-3.8-flash-low`,
`gemini-3.8-flash-medium` and `gemini-3.8-flash-high`. The low variant succeeded
despite the default model's earlier 429. There is no automatic model substitution;
the requested native model ID is passed unchanged.

Additional real tool checks through MoE Deck returned:

```json
{"harness":"opencode","mode":"safe","operation":"read","answer":"MOEDECK_READ_PROOF","tool_status":"done","pending_approvals":0}
{"harness":"opencode","mode":"safe","operation":"write","answer":"DENIED","tool_status":"error","marker_written":false,"pending_approvals":0}
{"harness":"agy","model":"gemini-3.8-flash-low","operation":"read_file","answer":"MOEDECK_READ_PROOF","tool_status":"done"}
```

The OpenCode project deliberately allowed every permission, including an agent
allow-all and project policy allow-all. The native write tool still failed;
the marker did not exist before or after the run. A direct native subprocess
also reported `Blocked by configuration policy` with `executed: false`.
Read and write turns used the same native session. The user's shared OpenCode
server and configuration were unchanged.



The implementation follows OpenCode's [V2 permission actions][permissions] and
[policy evaluation][policies]. Policies can deny tools after agent rules and
saved approvals; allowing a read through this policy does not grant permission
that the agent itself lacks. Existing inline tool denials are reapplied after
the Safe read exceptions.

## Limits

The initial Claude attempt hit its session limit; a retry after 03:00 verified
both successful replies and the same native session. These checks do not cover
every account/model combination or native approval path. OpenCode Standard/Yolo still use native configured
permissions and auto mode, without a MoE Deck native permission bridge. Claude and
Antigravity retain their own permission behavior. Safe enforcement was exercised
against OpenCode 2.2.5, not every older CLI release.

Chromium and WebKit are headless browser evidence, not physical iPhone keyboard,
safe-area or gesture verification. Model checks establish integration behavior,
not comparative coding quality. Large OpenCode threads retain the newest 400
native messages and report the older omitted messages; this is not unlimited
history pagination.

[permissions]: https://opencode.ai/v2/docs/permissions
[policies]: https://opencode.ai/v2/docs/policies
