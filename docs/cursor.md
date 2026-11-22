# Cursor Agent

Cursor is a MoE Deck harness when the Cursor Agent CLI is installed. The cockpit
lists it only after `cursor-agent` is on `PATH` (the installer also links it
from `~/.local/bin`, which MoE Deck checks directly). Nothing here restarts
`moedeck.service` or binds port 7770.

The CLI's print mode is the same kind of non-interactive stream the other
harnesses use. Current reference:
[headless CLI](https://cursor.com/docs/cli/headless),
[authentication](https://cursor.com/docs/cli/reference/authentication),
[parameters](https://cursor.com/docs/cli/reference/parameters),
[output format](https://cursor.com/docs/cli/reference/output-format).

## Install and sign in

On the machine that runs MoE Deck (the mini PC), as the same user that owns
`moedeck.service`:

```bash
curl https://cursor.com/install -fsS | bash
export PATH="$HOME/.local/bin:$PATH"
cursor-agent --version
```

The installer symlinks that binary as both `agent` and `cursor-agent`. MoE Deck
detects `cursor-agent` only.

Sign in with a browser, or with an API key from
[Cursor Dashboard → API Keys](https://cursor.com/dashboard). A headless box
should print the login URL instead of trying to open a browser:

```bash
NO_OPEN_BROWSER=1 cursor-agent login
# or, for scripts and the service user:
export CURSOR_API_KEY="your_api_key_here"
cursor-agent status
```

`CURSOR_API_KEY` is read from the environment of the `cursor-agent` process.
MoE Deck does not put the key on the command line. A user service does not
pick up a shell `export` by itself. To use a key for the already-installed
service, add it to that user's systemd environment and restart the service
later, when you choose to — not as part of this check:

```bash
systemctl --user edit moedeck.service
# under [Service]:
# Environment=CURSOR_API_KEY=your_api_key_here
```

Browser login stores credentials under `~/.cursor`. The running gateway sees
a newly installed binary on the next turn, because it looks up `cursor-agent`
when a turn starts. You do not need to restart `moedeck.service` for a login
that landed in `~/.cursor`, or for a binary that appeared in `~/.local/bin`.

`cursor-agent models` refreshes the model list from Settings. An id you type
there is passed through with `--model`. Empty or "Configured default" leaves
the CLI's own model choice alone.

## What a turn runs

Safe mode:

```text
cursor-agent -p --output-format stream-json --trust --workspace <project> --mode ask "<prompt>"
```

Standard and Yolo add `--force` instead of `--mode ask`. Print mode does not
stop for a phone approval: without `--force` it proposes edits and does not
apply them. Deny rules in `~/.cursor/cli-config.json` still block a forced
command. A resumed chat adds `--resume <session-id>`. The session id comes
from the stream's `system` / `init` event, and existing chats are read from
`~/.cursor/chats/` and `~/.cursor/projects/*/agent-transcripts/`.

## Try it without touching the live service

Build this tree, then start a second gateway on another port and another
config directory. Leave `moedeck.service` running on 7770.

```bash
cargo build --release
MOEDECK_PORT=7771 \
MOEDECK_CONFIG_DIR="$HOME/.config/moedeck-cursor-preview" \
MOEDECK_PASSCODE=preview \
./target/release/moedeck
```

Open `http://127.0.0.1:7771`, sign in with `preview`, and pick Cursor. That
process uses your real `~/.cursor` and `cursor-agent`, and it does not write
the live gateway's config. Stop it with Ctrl-C.

Do not run `moedeck install` for this check, and do not point
`MOEDECK_CONFIG_DIR` at the live `~/.config/moedeck` (or `~/.config/helm`)
while the service is up. Two processes must not share one config directory.
