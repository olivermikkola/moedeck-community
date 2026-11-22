# Phone behavior tests

Build MoE Deck, then run the real gateway behind touch-enabled Chromium and WebKit:

```sh
cargo build --locked
cd tests/browser
npm ci
npx playwright install --with-deps chromium webkit
npm test
```

`MOEDECK_BIN` can select an already-built binary. `MOEDECK_BROWSERS=chromium` selects
one engine. Screenshots go to `artifacts/` (uploaded by CI).

Each test uses a temporary home, configuration, projects and scripted native
CLIs. No personal credentials, conversations or model endpoints are used.
The scripts exercise real subprocess arguments, streaming, native resume,
stopping, attachment delivery and blocking approval I/O. They measure gateway
behavior, not model quality. Local failure evidence stays in the printed
`/tmp/moedeck-phone-*` directory.

Covered workflows include passcode login, new conversation/project selection,
quoted model labels, stale previous-thread IDs, drafts across thread switches,
switching during a pending send, out-of-order detail responses, polling during
a held touch, rejected actions, offline reconnect, accepted sends with lost
responses, attachments, Allow/Deny, settings, and reopening a conversation
under the actual service worker. Fault-injection contexts block service
workers so browser request interception is deterministic; the PWA reopening
phase uses a separate context with the real worker enabled.

WebKit automation is not physical iPhone keyboard, safe-area or gesture proof.

`harness.test.cjs` also exercises Claude/AGY native session adoption and resume,
AGY's nested stream format, OpenCode's newest messages and truncation counts,
visible provider failures and Stop, repeated prompts, and delivery of Safe
policy to a private OpenCode process. Its SQLite database and CLI processes
are fixtures. Run it separately with `node --test harness.test.cjs`; `npm test`
runs it and both browser suites sequentially. Failure evidence remains in
the printed `/tmp/moedeck-harness-*` directory.

The Claude Remote Control phase additionally verifies a real PTY (not `-p`),
explicit replies to native questions, validated app links, no duplicate start,
Claude JSONL history discovery, separate profile homes without inherited primary
tokens, visible native startup errors, and process-group cleanup on Stop and
gateway termination. Chromium exercises graceful shutdown; WebKit exercises
abrupt shutdown and PTY hangup. Process cleanup assertions use Linux `/proc`.

`reliability.test.cjs` covers native Codex discovery with an older database
schema and more than twelve chats, paired message formats, repeated answers,
local image attachments, grouped command results, persistent step expansions,
on-demand full tool output, and native versus gateway-owned activity. It holds
real transcript file descriptors in fixture processes; an abandoned transcript
must not inherit liveness from a different process. Native OpenCode history is
read-only: a shared OpenCode daemon must not manufacture live agent entries.
The suite also verifies the composer's Stop terminates its owned process and
that pausing scheduled work persists through the actual goal API.

Set `TMPDIR` to a disk-backed scratch directory when `/tmp` is tmpfs. These tests
create disposable gateway homes and browser profiles under `TMPDIR`.

`voice.test.cjs` checks dictation start, stop, cancel, permission errors, timeout,
late callbacks, draft navigation and unsupported browsers. Recognition events
are simulated; the gateway and UI are real. It verifies that dictated command
phrases remain editable text and never trigger actions or send model requests.
Microphone access and recognition accuracy still need physical-device testing.
