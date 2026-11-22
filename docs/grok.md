# Grok Build

Grok Build is a MoE Deck harness when the `grok` CLI is installed. The cockpit
lists it only after `grok` is on `PATH` (MoE Deck also checks `~/.local/bin`
directly). Nothing in this check restarts `moedeck.service` or binds port 7770.

Headless turns use the same shape as the other print-mode harnesses.
Current reference, checked against the docs rather than assumed:

- [Install and overview](https://docs.x.ai/build/overview)
- [Headless and scripting](https://docs.x.ai/build/cli/headless-scripting)
- [CLI reference](https://docs.x.ai/build/cli/reference)
- [Sessions](https://docs.x.ai/build/features/sessions)
- [Permissions](https://docs.x.ai/build/features/permissions)
- [Sandbox](https://docs.x.ai/build/features/sandbox)
- [Settings reference](https://docs.x.ai/build/settings/reference) (`GROK_HOME`, `XAI_API_KEY`)

The headless page's short note that `--session-id` can "create or resume" does
not match the sessions page or the CLI reference. Those say `--session-id`
names a **new** UUID and does not resume. MoE Deck resumes with `--resume`.

## Install and sign in

On the mini PC, as the same user that owns `moedeck.service`:

```bash
curl -fsSL https://x.ai/cli/install.sh | bash
export PATH="$HOME/.local/bin:$PATH"
grok version
```

The installer puts `grok` in `~/.local/bin`. MoE Deck looks that directory up
on every turn, so a binary that just appeared there is visible to an
already-running gateway. You do not restart the service for the install.

Sign in with a browser on this machine, or with a device code when the mini
PC has no display. Device code prints a URL and a short code; finish it on
your phone:

```bash
grok login --device-auth
```

Plain `grok login` opens a browser when one is available. Either one writes
credentials under `~/.grok`. The running gateway reads that directory as the
service user. A login that landed there does not need a service restart.

For scripts, set an API key instead of a browser login. The key stays in the
environment. MoE Deck never puts it on the `grok` command line.

```bash
export XAI_API_KEY="xai-..."
```

A user service does not pick up a shell `export` by itself. Adding
`XAI_API_KEY` to `moedeck.service` means editing the unit and restarting it.
That is a later choice, not part of this check. The preview process below can
see a key you export in the same shell, and it leaves the service alone.

`grok models` refreshes the model list from Settings. An id you type there is
passed through with `--model`. Empty or "Configured default" leaves Grok's
own model choice alone. The picker starts with `grok-build` and `grok-4.7`,
the ids the current docs use for coding and for the settings example.

## What a turn runs

Every turn:

```text
grok --no-auto-update --output-format streaming-json --cwd <project> -p "<prompt>"
```

Stdin is closed. `grok -p` does not wait on it, and the streaming-json stream
is one-way. Tool approvals that can pause belong to ACP (`grok agent stdio`).
MoE Deck does not drive ACP, so **Grok cannot pause for a phone approval in
any safety mode**. Deny rules in `~/.grok/config.toml` and the project's
`.grok/config.toml` still apply. The CLI flag overrides that user's saved
permission mode for this process.

| MoE Deck mode | What `grok` is given | What actually happens |
|---|---|---|
| Safe | `--permission-mode dontAsk --sandbox read-only --tools read_file,grep,list_dir` | Read-only. Shell, edit, and write tools are not offered. `dontAsk` denies anything that would have prompted (including MCP tools the allowlist cannot remove). The `read-only` sandbox blocks project writes and, on Linux, child network. No approval card. |
| Standard | `--permission-mode auto` | Auto-review. Routine calls the classifier allows will run. A call it will not allow is **blocked and reported to the model** (`Auto mode blocked this action`), not held for the phone. No approval card. |
| Yolo | `--always-approve` | Tool calls run without a prompt. Deny rules and hooks still win. No approval card. |

A resumed chat adds `--resume <session-id>`. The id is `sessionId` on the
stream's final `end` event. The public sessions page says chats live under
`~/.grok/sessions/`, grouped by working directory. The grok-build session
guide names the files MoE Deck reads: `summary.json` (title, folder, model)
and `updates.jsonl` (the transcript). A session whose `summary.json` is
missing or unreadable is skipped, as is a subagent. Set `GROK_HOME` to move
that tree; MoE Deck follows an absolute `GROK_HOME` for the primary account.

## More than one account

An extra Grok account gets its own `~/.grok`, and MoE Deck sets `GROK_HOME` to
it so a key or home you exported for the primary account is not reused.
`moedeck login grok-2` runs the command the cockpit prints:

```bash
HOME=~/.config/moedeck/profiles/grok-2/home \
GROK_HOME=~/.config/moedeck/profiles/grok-2/home/.grok \
grok login --device-auth
```

Finish the device code on the phone with the other xAI account. Its sessions
show up as separate threads (`grok:grok-2:…`).

## Try it without touching the live service

Build this tree, then start a second gateway on another port and another
config directory. Leave `moedeck.service` running on 7770. Do not run
`moedeck install` for this check.

```bash
# From a checkout of this branch, not the binary the service is already running.
cargo build --release
export PATH="$HOME/.local/bin:$PATH"
grok version
grok login --device-auth          # skip if ~/.grok is already signed in
# optional, this process only:
# export XAI_API_KEY="xai-..."

MOEDECK_PORT=7771 \
MOEDECK_CONFIG_DIR="$HOME/.config/moedeck-grok-preview" \
MOEDECK_PASSCODE=preview \
./target/release/moedeck
```

Open `http://127.0.0.1:7771`, sign in with `preview`, and pick Grok. Send one
message on Safe and one on Yolo. Safe should not edit the project. Yolo may.
The thread list should already show chats that exist under `~/.grok/sessions/`.

That process uses your real `~/.grok` and `grok`, and it does not write the
live gateway's config. Stop it with Ctrl-C.

Do not point `MOEDECK_CONFIG_DIR` at the live `~/.config/moedeck` (or
`~/.config/helm`) while the service is up. Two processes must not share one
config directory.

Putting this build on port 7770 later means replacing the service binary,
which does restart `moedeck.service`. That is separate from the check above.

## Not confirmed on a live `grok`

This guide was checked against the xAI docs linked above and the grok-build
user guide. It was not checked by running the `grok` binary. If `grok --help`
on the mini PC disagrees, trust the binary.

- Streaming lines are assumed to be `{"type":"text","data":"..."}`,
  `{"type":"tool_call","toolName":"...","rawInput":{...}}`, and a final
  `{"type":"end","sessionId":"..."}`.
- On-disk chats are assumed to be `summary.json` plus `updates.jsonl` under
  `~/.grok/sessions/<encoded-cwd>/<session-id>/`. The public sessions page only
  says the directory is `~/.grok/sessions/`, grouped by working directory.
- Safe and Standard pass `--permission-mode`. That flag is in the grok-build
  user guide. The public CLI reference lists `--always-approve`, `--sandbox`,
  `--tools`, `--resume`, and `--output-format`, and does not print
  `--permission-mode` in its short flag table.
- The read-only tool ids `read_file`, `grep`, and `list_dir` are the names in
  the headless user guide's example, not a live `--help` dump.
- The install script is assumed to put `grok` in `~/.local/bin`.
