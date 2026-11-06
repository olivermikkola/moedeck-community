# Phone usability verification — 2026-11-05

The reported phone failures had two reproducible races: polling replaced a
thread button between touch down and click, and a slower previous thread
response could overwrite the newly selected conversation. The repairs keep
touch targets intact until the gesture finishes and accept detail responses
only for the current request and thread.

The header title now opens project selection. New conversations explicitly
choose a project, installed harness and model. Folder validation rejects
missing paths instead of silently using the home directory. A stale previous
thread ID falls back to an existing thread. Drafts stay with their thread;
pending sends retain their original destination and choices. The same delivery
ID survives network loss, so replaying an accepted send does not start another
turn. Rejected actions explain the failure. Image uploads and clipboard errors
also provide feedback.

## Automated behavior evidence

`tests/browser/mobile.test.cjs` starts the actual MoE Deck binary behind Chromium
and WebKit at a touch-enabled 393 × 852 viewport. The native CLI fixtures are
scripted subprocesses; model replies and coding ability are not measured.
Both engines passed the workflows listed in `tests/browser/README.md`,
including a separate phase with the real service worker enabled. CI runs these
checks and uploads screenshots.

## Real native harness evidence

An additional isolated gateway on port 17771 used the installed Codex CLI and
primary account, a disposable project and read-only permission mode. Through
the phone browser controls:

1. Choose Codex, its configured default model and the disposable project.
2. Send a request to remember `HELM-PHONE-PROOF-7K4`, without calling tools.
3. Wait for the completed answer, reload under the actual service worker,
   reopen the same thread and ask for the token.
4. Verify the exact reply and unchanged persisted native-session UUID.

The real-run result was:

```json
{"real_harness":"Codex","turns":2,"native_session_preserved":true,"service_worker_reopen":true,"harness":"codex","project":"/tmp/helm-mobile-qa/real-project","safety":"safe","answer":"HELM-PHONE-PROOF-7K4","browser_errors":[]}
```




## Remaining device verification

Headless WebKit is not physical iPhone proof. iOS keyboard positioning,
safe-area behavior, installation and gestures still need the user's device.
The existing phone service must load the new binary before these repairs are
available there; the isolated gateway result alone does not establish deployment.
