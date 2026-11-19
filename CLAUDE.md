# CLAUDE.md

## Overview
Ferrocord is an unofficial, lightweight native Discord desktop client in Rust
(edition 2024, tokio, iced 0.14). It speaks the user-account Gateway v10 and
REST API directly (`tokio-tungstenite` + `reqwest`). Voice is out of scope.

## Commands
- `cargo run -- --mock` — run with mock data (no network, in-memory stores).
- `cargo run -- --mock --screenshot <login|guild|dms|embeds> out.png` — render a screen to PNG
  (headless: `ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" ...`).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `cargo build --release`.
- Shared machine: `export CARGO_BUILD_JOBS=2`.

## Architecture
See `docs/architecture.md` (shared Ferro* skeleton) and `docs/protocol.md`
(gateway, REST, rate limits, QR login, UI reference). Layout: `src/app`
(state + pure reducers), `src/ui` (views + theme), `src/net` (gateway, REST,
remote auth), `src/store` (SQLite cache), `src/auth` (login + keyring).

## Conventions
- English everywhere; Conventional Commits (`feat:`, `fix:`, `chore:`, `docs:`, `test:`).
- No `unwrap()`/`expect()` outside tests (crate-level deny).
- Only `auth/secrets.rs` touches the `keyring` crate; tokens never hit disk or logs.
- Log via `tracing`; wrap user content in `error::Redacted`; types holding
  personal data implement `Debug` manually.
- Views never hard-code colours/sizes: use `ui::theme`.
- Every new crate needs a one-line justification in the PR description.

## Workflow quirks
- Work on `feat/*` branches off `development`; PRs target `development` only.
- Never push to `main`/`development` directly, never merge.
- `version.yml` bumps the version in `Cargo.toml` (do not bump by hand) and syncs
  `main` bumps back to `development`; protected branches need a bot bypass.

## Deployment
No packaging yet: `cargo build --release` produces `target/release/ferrocord`.
CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on ubuntu-latest.

## Gotchas
- Third-party clients violate Discord's ToS; keep the README warning intact.
- Keep iced default features on (they carry the x11/wayland backends).
- Integration tests use local mock WebSocket/HTTP servers; never real accounts.
- Endpoints can be overridden with `FERROCORD_GATEWAY_URL`, `FERROCORD_API_URL`,
  `FERROCORD_REMOTE_AUTH_URL`, `FERROCORD_CDN_URL`.
- Keyring tests must use `MemoryStore` (no Secret Service in CI).
- `main` defaults `ICED_BACKEND=tiny-skia` (wgpu costs 100-200 MB RSS).
