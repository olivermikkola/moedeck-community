# Security Policy

MoE Deck is a single-binary daemon that can execute coding agents and shell
commands on your machine — treat its exposure surface seriously.

## Supported versions

| Version | Support |
|---------|---------|
| `main` / latest tagged release | ✅ active |
| older tags | ❌ none — upgrade |

## Reporting a vulnerability

Please use GitHub **Private Vulnerability Reporting**
(Repository → Security → Report a vulnerability). This keeps the report
confidential until a fix ships. For anything that allows remote code
execution of destructive commands without a passcode, mention the exact
knobs involved (e.g. `MOEDECK_PASSCODE`).

## Built-in defenses (as configured by default)

- **Passcode auth** — every peer must complete cookie login; comparisons are constant-time
  over SHA-256 digests.
- **Per-session cookies with revocation** — each login issues a unique,
  expiring HMAC token (`SESSION_TTL_SECS`); changing the passcode
  (or manifesting any future "log out everywhere") invalidates every cookie
  issued before that moment. Users will need to re-login after upgrading from
  the older static-cookie scheme.
- **Always a passcode** — the first start generates a random one (`moedeck pair`
  shows it). Every non-public request needs a logged-in session, whatever
  network it comes from: loopback, LAN and Tailscale peers included. The
  passcode can be changed but not switched off. Older configs' `trust_tailscale`
  key is ignored.
- **DNS-rebinding guard** — the `Host` header must be loopback, a `.ts.net`
  MagicDNS name, or a local/Tailscale IP. Requests without a Host header fail
  closed.
- **CSRF guard** — state-mutating cockpit requests must be JSON and, when the
  browser sends an `Origin`, that origin must match the request `Host` exactly
  (`null` or dotless attacker labels are rejected). The reverse-proxy preview
  route has its own, stricter same-origin gate that runs *before* anything is
  forwarded to a local port.
- **Preview proxy containment** — referer-based port resolution only accepts
  same-origin referers; upstream CORS-widening headers are stripped; proxied
  pages cannot `postMessage` to arbitrary origins; hung dev servers cannot pin
  the daemon (header/body/connect timeouts). Remaining theoretical gap: any
  local port's HTML still executes inside MoE Deck's origin once *you* open its
  preview — treat preview tabs of untrusted content like browser tabs, and see
  the roadmap item for a dedicated preview origin.
- **Native approvals** — supported native approval requests are forwarded to
  the cockpit before execution. Support and permissions vary by harness and
  safety mode; tool progress is not itself an approval request. See the
  harness-specific sections in the README before relying on an approval flow.
- **Login lockout** — 20 consecutive failed passcodes (parallel attempts
  included) lock login for 5 minutes.
