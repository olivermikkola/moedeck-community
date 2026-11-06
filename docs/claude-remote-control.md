# Claude app fallback from MoE Deck

In MoE Deck's phone composer, send `/rc` (or `/remote-control`) to start Remote
Control in the current project. You can also tap the harness selector, then
**Claude app · Remote Control**, choose a project folder and Claude account,
and tap **Start Remote Control**.

MoE Deck runs the installed native CLI in a real pseudo-terminal:

```sh
claude remote-control --spawn session --name 'MoE Deck · project' --permission-mode default
```

This starts a separate native Claude session. It does not convert the current
MoE Deck conversation or send `/rc` to a model through print mode. Open the Claude
app's **Code** tab and find **MoE Deck · project**, or use **Open in Claude** when
the CLI supplies its connection link. Native session files are discovered by
MoE Deck's existing Claude history reader as Claude writes them.

If Claude asks a folder-trust question, expand **Native terminal & replies**,
read it and type your reply, then press **Send Enter**. Arrow and Esc buttons
are available for native prompts. MoE Deck never accepts trust questions itself.
Claude enforces its own account eligibility and authentication; native errors
remain visible in the terminal. See the [official Remote Control documentation](https://code.claude.com/docs/en/remote-control)
for account requirements and the Claude app connection flow.

Remote Controls keep running when the MoE Deck page closes or reloads. Reopen the
panel to find running sessions across projects. A second start for the same
project and Claude account reuses the existing managed process. **Stop Remote
Control** ends its process group. Gateway shutdown also closes the terminals;
there is no separate system service or persistent watcher to install.

Safe mode launches Claude in Plan mode. Other MoE Deck modes use Claude's normal
native permissions, including when MoE Deck itself is in Yolo mode. Remote Control
approvals belong to Claude and the Claude app. A selected secondary account
uses its own shadow home and does not inherit primary-account API/token
environment overrides.

## Verification

The automated Chromium and WebKit tests use scripted native CLI processes in
real PTYs. They check terminal input, app-link readiness, native errors,
duplicate-start protection, local Claude history discovery, profile isolation,
whole-group Stop, and shutdown/PTY-hangup cleanup. These are gateway behavior
tests; the fixture does not connect to the real Claude app.

A separate real run used installed Claude Code 2.1.309 with a disposable project
and the primary account. The native CLI connected and emitted a claude.ai link,
survived a MoE Deck page reload and a 15-second idle check, and stopped through the
MoE Deck button with no browser errors. No model prompt was sent for this proof.


Connection from the physical Claude phone app, and iPhone keyboard/gesture
behavior, still require device verification. A ready CLI/link is not proof that
the app has connected or completed an agent turn.
