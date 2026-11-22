# Outbound event webhooks

MoE Deck can POST a small JSON body to one or more URLs when something on the gateway needs a person. The usual target is an assistant that exposes a webhook: it accepts the POST and starts a run. The gateway only makes **outbound** HTTPS (or HTTP) calls. Nothing on the public internet needs to connect in, which matches a machine that is reachable on your Tailscale network and closed to inbound traffic from the internet.

Webhooks are **off** until you add at least one target. A stock install does not phone home.

## Events

| `event` | When it fires |
|---|---|
| `approval_needed` | A harness is blocked on a human approval (not a call the safety mode already allowed). |
| `turn_finished` | An agent turn MoE Deck started has ended. `outcome` is `success` or `error`. A failed turn is this event with `outcome` `error`, not a separate type. Stopping the turn counts as `error`; the summary says why. |
| `session_stalled` | The same stuck rules as the agent status chips: a live session that still claims to be busy has been silent for 10 minutes, or for 30 minutes while a tool call is in progress. The gateway checks about once a minute, and sends this once per stall. It sends again only after the session leaves the stalled state and stalls again. |

Each event type is debounced **per conversation**. The first one goes out immediately. Further events of that same type for that conversation, inside the debounce window, collapse into one later POST that carries the latest summary. An approval and a finished turn in the same window are different types, so both are delivered. The default window is 30 seconds (`debounce_secs` / `MOEDECK_WEBHOOK_DEBOUNCE_SECS`, `0` disables it, max 3600).

`moedeck webhook test` and `POST /api/webhooks/test` skip the debounce so you can see a delivery at once.

## Payload

The body is one JSON object. It never includes the passcode, a session cookie, an API key, a secret header value, or the full transcript. Summaries are plain text, at most 300 characters, with credential-shaped text replaced by `[redacted]`.

```json
{
  "event": "turn_finished",
  "timestamp": "2026-11-10T16:25:00Z",
  "harness": "claude",
  "conversation_id": "claude:abc",
  "title": "Fix login",
  "summary": "All checks passed.",
  "outcome": "success",
  "path": "/?c=claude%3Aabc",
  "url": "http://100.64.0.1:7770/?c=claude%3Aabc"
}
```

| Field | Meaning |
|---|---|
| `event` | `approval_needed`, `turn_finished`, or `session_stalled` |
| `timestamp` | UTC time the payload was built (`2026-11-10T16:25:00Z`) |
| `harness` | `claude`, `agy`, `codex`, `opencode`, or `keel` |
| `conversation_id` | MoE Deck conversation id |
| `title` | Conversation title, or `Untitled` |
| `summary` | Approval ask, or the start of the last assistant message |
| `outcome` | `success` or `error` on `turn_finished`; `stalled` on `session_stalled`. Omitted for `approval_needed`. |
| `path` | Path on the gateway that opens the conversation in the PWA |
| `url` | `path` joined to the gateway's base URL. Omitted when no base URL is known |

`/?c=<id>` opens that conversation. A pairing link may still carry `?token=`, but webhook URLs never do. After the cookie is set, a `c` parameter on that same link is kept and the token is dropped from the address bar.

An approval summary looks like: `Approval needed (high) for Bash: cargo test`.

## Configuration

The file is `$MOEDECK_CONFIG_DIR/webhooks.json` (default `~/.config/moedeck/webhooks.json`). The directory is owner-only. If the file is readable by anyone else, the gateway tightens it to mode `600` on startup. Prefer a `secret_env` name over putting the secret in the file. Either way the value is not written to logs, and it is passed to `curl` through a mode `600` config file rather than on the command line.

`curl` must be on `PATH`. Each attempt waits at most `timeout_secs` (default 5, range 1–30). A failed attempt is retried twice (three tries total) after 200ms and then 600ms, for network errors, HTTP 408, 429, and 5xx. Other 4xx responses are not retried. Redirects are not followed. Delivery runs in the background and does not block the agent loop.

### File

```json
{
  "debounce_secs": 30,
  "timeout_secs": 5,
  "public_base_url": "http://100.64.0.1:7770",
  "targets": [
    {
      "url": "https://bot.example/webhook",
      "events": ["approval_needed", "turn_finished", "session_stalled"],
      "secret_header": "Authorization",
      "secret_env": "GROK_WEBHOOK_SECRET"
    }
  ]
}
```

`public_base_url` is optional. When it is unset, the gateway uses its Tailscale (or LAN) address and the port it bound, and `url` becomes `http://<that-host>:<port>/?c=...`. Set this when that guess is wrong. It must be `http://host:port` or `https://host:port` with no userinfo, query, or token.

`events` may be a list or a comma-separated string. Omit it to subscribe to every event. Unknown names are ignored. A target whose names are all unknown is skipped.

`secret_header` plus a value sends that header on the POST (for example `Authorization` or a sender-key header). The value is `secret_env` (an environment variable **name**) when that variable is non-empty, otherwise the `secret` string in this file:

```json
{
  "targets": [
    {
      "url": "https://bot.example/webhook",
      "events": ["approval_needed", "turn_finished"],
      "secret_header": "X-Moedeck-Secret",
      "secret": "replace-with-the-sender-key"
    }
  ]
}
```

Do not commit a real `secret`. The live service reads this file from its own config dir. A `secret_env` variable must be in the service environment, not only in your interactive shell. A drop-in keeps it out of the unit `install` writes:

```ini
# ~/.config/systemd/user/moedeck.service.d/webhooks.conf
[Service]
Environment=GROK_WEBHOOK_SECRET=replace-with-the-sender-key
```

`chmod 600` that drop-in. Restart the service only when you want the live gateway to start sending (see below).

### Environment

The older `HELM_` prefix still works for every name below. Empty values count as unset.

| Variable | Effect |
|---|---|
| `MOEDECK_WEBHOOKS` | JSON object (same shape as the file) or a JSON array of targets. **Replaces** the file's target list. |
| `MOEDECK_WEBHOOK_URL` | Adds one target. |
| `MOEDECK_WEBHOOK_EVENTS` | Comma-separated events for that URL. Default: all events. |
| `MOEDECK_WEBHOOK_SECRET_HEADER` | Header name for that URL. |
| `MOEDECK_WEBHOOK_SECRET` | Header value for that URL. Not printed. |
| `MOEDECK_WEBHOOK_DEBOUNCE_SECS` | Overrides the debounce window. |
| `MOEDECK_WEBHOOK_TIMEOUT_SECS` | Overrides the per-attempt timeout. |
| `MOEDECK_WEBHOOK_BASE_URL` | Overrides `public_base_url`. |

```bash
export MOEDECK_WEBHOOK_URL="https://bot.example/webhook"
export MOEDECK_WEBHOOK_EVENTS="approval_needed,turn_finished,session_stalled"
export MOEDECK_WEBHOOK_SECRET_HEADER="Authorization"
export MOEDECK_WEBHOOK_SECRET="replace-with-the-sender-key"
export MOEDECK_WEBHOOK_DEBOUNCE_SECS=30
export MOEDECK_WEBHOOK_BASE_URL="http://100.64.0.1:7770"
```

Scalar variables win over the same fields inside `MOEDECK_WEBHOOKS`. `MOEDECK_WEBHOOK_URL` is added **after** file or `MOEDECK_WEBHOOKS` targets, so it can point a test at a second receiver without deleting the JSON.

`moedeck webhook` prints the public URL of each target, the events, and whether a secret header is set. It does not print the secret.

## Try it without restarting the live service

The live daemon is `moedeck.service` on port **7770**. Leave it alone. Use another config directory and, if you start a second process, another port.

`webhook test` does not bind a port and does not read the live config when `MOEDECK_CONFIG_DIR` points somewhere else.

```bash
CFG=$(mktemp -d)
mkdir -p "$CFG"
chmod 700 "$CFG"

# A local receiver standing in for the assistant's webhook.
python3 -u - <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(n)
        print("secret-header-present", bool(self.headers.get("X-Moedeck-Secret")))
        print(body.decode())
        self.send_response(204)
        self.end_headers()

    def log_message(self, fmt, *args):
        return

HTTPServer(("127.0.0.1", 9797), Handler).serve_forever()
PY
RECV=$!

cat > "$CFG/webhooks.json" <<'EOF'
{
  "debounce_secs": 30,
  "timeout_secs": 5,
  "public_base_url": "http://127.0.0.1:7771",
  "targets": [
    {
      "url": "http://127.0.0.1:9797/hook",
      "events": ["approval_needed", "turn_finished", "session_stalled"],
      "secret_header": "X-Moedeck-Secret",
      "secret_env": "MOEDECK_TEST_WEBHOOK_SECRET"
    }
  ]
}
EOF
chmod 600 "$CFG/webhooks.json"

export MOEDECK_CONFIG_DIR="$CFG"
export MOEDECK_TEST_WEBHOOK_SECRET="test-sender-key"
# Does not touch port 7770 or the live config dir.
moedeck webhook test --event turn_finished

# Optional: a second gateway on 7771. Redirect stdout so this stays a daemon
# instead of opening the terminal UI. It is not moedeck.service.
export MOEDECK_PORT=7771
export MOEDECK_PASSCODE=webhook-test
moedeck </dev/null >"$CFG/daemon.log" 2>&1 &
TEST_PID=$!

# Authenticated test against that second gateway only.
curl -sS -c "$CFG/cookies" -X POST "http://127.0.0.1:7771/api/auth/login" \
  -H "Content-Type: application/json" \
  -d '{"passcode":"webhook-test"}'
curl -sS -b "$CFG/cookies" -X POST "http://127.0.0.1:7771/api/webhooks/test" \
  -H "Content-Type: application/json" \
  -d '{"event":"approval_needed"}'

kill "$TEST_PID" "$RECV"
```

`webhook test` exits 0 when every matching target accepts the POST, 1 when a delivery fails, and 2 when nothing is configured or the event name is wrong. The API returns the same JSON (`delivered`, `failed`, `results`) with HTTP 200, or 502 if a target failed. Results name the target without userinfo or query string, and errors are redacted.

From a git checkout, `cargo build` and call `target/debug/moedeck` the same way if `moedeck` is not on `PATH`.

## Turning it on for the live service

Edit the live config (`~/.config/moedeck/webhooks.json`, or the `MOEDECK_CONFIG_DIR` that `moedeck.service` already uses). The running process does **not** reload this file. When you want the live gateway to start sending:

```bash
systemctl --user restart moedeck
```

Until that restart, port 7770 keeps its previous behavior. Confirm with `journalctl --user -u moedeck -n 50` and look for `webhook: outbound hooks on`. Failures are logged as `webhook: delivery failed` with the event, the public URL, and a redacted error. Secrets are not logged.
