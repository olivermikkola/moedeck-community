# Reliability audit

This audit follows the November 2026 reports of missing Codex chats, duplicate
messages, completed commands labelled Running, misleading OpenCode activity,
and inaccessible stop controls. A passing transport fixture is not evidence of
model quality or physical iPhone behavior.

## Conversation and activity requirements

| Requirement | Implementation and evidence |
| --- | --- |
| Discover the user's Codex chats | Read optional SQLite columns defensively, respect primary CODEX_HOME, and remove the twelve-chat discovery limit. Browser regression uses 18 chats in an older schema. |
| Display each update once | Prefer native user items over injected transport input and match assistant copies by ID. Group updates and command results into their turn. Distinct turns retain intentionally repeated answers. |
| Preserve the selected project during polling | Only choose a default project when no conversation or draft is active. Regression opens a native chat in a different folder from the suggested project and verifies repeated bootstrap calls preserve both that chat and a new draft workspace. |
| Retain legitimate repeated prompts | Match overlay messages against native occurrences, not against messages just appended from the same overlay. Gateway subprocess test repeats the same user prompt. |
| Display native attachments | Read local_image parts and serve them through the existing authenticated, restricted image route. Browser verifies the image loads. |
| Label completed work accurately | Completed commands say Ran; failed commands say the step failed. Completed turns have no ongoing status line. |
| Keep steps compact and usable | Individual command details require a separate expansion. Open disclosures survive polling. Older tool output can be loaded explicitly. |
| Know who controls a running turn | can_stop is derived from the gateway's current active-run map. Linux native Codex activity additionally requires a matching open transcript in a Codex process and an unfinished turn. A completed or abandoned transcript is not working merely because another Codex process exists. |
| Stop real work | The composer becomes Stop for owned runs. Browser test starts a real fixture subprocess, stops it, and verifies the PID exits. A native-app turn cannot be stopped through this gateway; the API returns an explicit conflict. Sending into an observed active native Codex turn is rejected. |
| Remove unreliable native OpenCode control | Native OpenCode histories are read-only and explicitly labelled. The shared-server/database heuristic is removed. New OpenCode chats launched in Deck retain their own live status and Stop. No native conversations are deleted. |
| Be honest where native observation is unsupported | Native Codex history is read-only on non-Linux hosts. No native liveness is inferred there. |
| Keep dictation under user control | Remove the random meter, automatic command inference and walkie auto-send. Recognition events drive the input state; cancel/navigation ignore late results; unsupported browsers do not show an unavailable microphone. Empty transcription is a failure. Browser tests use controlled recognition events; physical speech quality remains unverified. |
| Make scheduled work visible | Background work lists goals, their state and task count. Pause scheduling is backed by the existing API and verified on persisted state. It does not claim to cancel a turn already running. |

## Verification of this change

- 170 Rust tests, Clippy with warnings denied, formatting and macOS target
  type-check passed.
- All five browser/subprocess cases passed across Chromium and WebKit. These
  use real gateway HTTP routes and fixture subprocesses, not live model calls.
- Light and dark screenshots were visually inspected. Selector backgrounds
  now use theme colors; previously light text colors sat on dark hard-coded
  selector backgrounds in light mode.
- A temporary gateway read the actual local Codex data without sending any
  prompts, stops or mutations: 38 native chats discovered, the duplicated
  screenshot update appeared once, all four attachments loaded into the API,
  and this native conversation reported busy with can_stop false. The probe
  gateway was terminated afterward; the live service was untouched.

## Remaining audit queue

These items are not yet covered by a completed end-to-end audit. Keep this list
open until each visible control has either real behavior and useful failure
handling, or has been removed.

- Scheduler remediation: separate PR bounds failed remediation, preserves task
  identities/history, makes remediation opt-in and preserves pause during a run.
  Integrate and verify it together with the conversation work before deployment.
- Rename, pin, sprint cancel/delete, account changes and other mutation actions:
  audit rejected requests, network errors and whether the displayed result
  matches saved state. Some existing actions ignore HTTP failures.
- Peer handoff, second opinion, worktree merge/discard and PR actions: verify
  real disposable Git repositories, failure handling and reviewable results.
- Ports and previews: verify the discovered process is the advertised service,
  the page actually loads, and stopped services disappear.
- Voice hardware: verify recognition quality on a physical phone separately.
  Draft/lifecycle behavior and unsupported/error paths have browser coverage.
- Update/install paths: verify downloads, failure recovery, version reporting
  and the service worker's transition to the installed version.
- Real installed-harness smoke runs and physical phone verification remain
  distinct from the scripted CLI/browser transport suites.
- Delivery: no live service restart or replacement has been performed by these
  changes. Hosted CI is currently blocked by the account billing/spending-limit
  gate. Do not merge while required checks have not run.
