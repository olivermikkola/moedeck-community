# Contributing

Thanks for considering a contribution! MoE Deck is a focused product: one Rust
binary, one embedded cockpit UI, no external services.

## Ground rules

1. **Read `VISION.md` first.** Changes should advance the "One Captain,
   Multi-Harness" philosophy — a feature that adds a second service to run
   contradicts the single-binary design.
2. **No new runtime dependencies** without discussion. The dependency list in
   `Cargo.toml` is deliberately short and every addition has a design cost.

## Workflow

```bash
git clone https://github.com/olivermikkola/moedeck-community.git
cd moedeck
cargo build          # debug build for development
cargo test           # must pass
cargo clippy --all-targets -- -D warnings   # must be clean
cargo fmt --all --check                     # must be clean
```

The cockpit UI (`src/ui.rs`) is a single embedded HTML/JS/CSS document served
by the daemon — hot-reload it by rebuilding and refreshing; there is no
frontend build step on purpose.

## Commits & PRs

- Conventional-ish prefixes (`feat:`, `fix:`, `docs:`, `refactor:`, `chore:`)
  are appreciated; keep the subject under ~72 chars.
- One logical change per PR. UI behavior changes need a screenshot in the PR
  description.

## Reporting issues

Open a GitHub issue with the bug/feature template. For security matters,
follow [SECURITY.md](SECURITY.md) instead — do not file public issues.

## Phone behavior checks

UI changes must also pass `tests/browser` against a built MoE Deck binary. See
[the browser test instructions](tests/browser/README.md). The suite uses real
gateway HTTP routes and scripted native CLI subprocesses in disposable homes;
CI checks Chromium and WebKit and saves screenshots. Keep real-model evidence
separate from transport fixtures and physical-device verification.
