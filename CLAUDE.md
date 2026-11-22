# CLAUDE.md

## Overview
Ferrocord is an unofficial, lightweight native Discord desktop client in Rust
(edition 2024, tokio, iced 0.14). It speaks the user-account Gateway v10 and
REST API directly (`tokio-tungstenite` + `reqwest`), plus voice channels and
DM/group calls (voice gateway v8, UDP/RTP, DAVE E2EE via `davey`, Opus, `cpal`).
Video (camera / Go Live) is not implemented yet: see `docs/voice.md` §6.

## Commands
- `cargo run -- --mock` — run with mock data (no network, in-memory stores).
- `cargo run -- --mock --screenshot <login|login-captcha|login-web|login-web-failed|guild|dms|embeds|voice|call|incoming-call|gifs|gif-trending|gif-favorites> out.png` — render a screen to PNG
  (headless: `ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" ...`).
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- `cargo build --release`.
- Shared machine: `export CARGO_BUILD_JOBS=2`.

## Architecture
See `docs/architecture.md` (shared Ferro* skeleton) and `docs/protocol.md`
(gateway, REST, rate limits, QR login, UI reference), `docs/voice.md` (voice). Layout: `src/app`
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
- `version.yml` (helper `.github/scripts/version.sh`) bumps `Cargo.toml` (do not bump
  by hand) and MERGES `main` into `development` after each main bump (real merge,
  no sync commit); protected branches need a bot bypass.

## Deployment
Releases are automatic: `.github/workflows/release.yml` chains off the "Version" workflow
via `workflow_run` (GITHUB_TOKEN bot pushes don't trigger `push` workflows), builds on
ubuntu-22.04, and creates GitHub Release `v<Cargo version>` with `ferrocord-v<ver>-linux-x86_64.tar.gz`
+ `.sha256` (skips if the release exists; manual re-run: workflow_dispatch). Users run
`install.sh` (`curl ... | sh`; `-s -- --uninstall`); test it with `FERRO_RELEASE_API=file://...`.
CI (`.github/workflows/ci.yml`) runs fmt, clippy and tests on ubuntu-latest.

## Gotchas
- Third-party clients violate Discord's ToS; keep the README warning intact.
- Keep iced default features on (they carry the x11/wayland backends).
- iced comes from the `ferroiced` fork (`[patch.crates-io]` in `Cargo.toml`):
  iced 0.14.1 plus a tiny-skia fast path for opaque images. Patch every iced
  crate to the same source; after changing it run `cargo update -p iced`.
- Integration tests use local mock WebSocket/HTTP servers; never real accounts.
- Endpoints can be overridden with `FERROCORD_GATEWAY_URL`, `FERROCORD_API_URL`,
  `FERROCORD_REMOTE_AUTH_URL`, `FERROCORD_CDN_URL`.
- Mouse position / Shift live in `app/input.rs` atomics (no per-event messages).
- File dialog: `rfd` (xdg-portal). Clipboard files/images: `arboard`.
- Keyring tests must use `MemoryStore` (no Secret Service in CI).
- `main` defaults `ICED_BACKEND=tiny-skia` (wgpu costs 100-200 MB RSS).
- Login captcha: Discord's page opens in the `ferrocord-login` helper (`wry`; WebView2 / WebKitGTK 4.1,
  build needs `libwebkit2gtk-4.1-dev`). Only the helper may link the webview (CI ldd check). Never render
  hCaptcha on our own page or fake an origin; never solve captchas. REST identity headers live in `net/identity.rs`.
- Voice: static libopus (needs `cmake`), `cpal` (`libasound2-dev`); the voice subscription exists
  only in a call. DAVE is mandatory (close 4017); keep `davey` calls under `catch_unwind`.
- Unicode emoji need the colour emoji span (`ui::emoji_font::emoji_text`/`emoji_runs`, family from `emoji_font()`); `text_editor` (composer) cannot take spans and falls back to the loaded font. The bundled TTF (OFL) is renamed "Ferrocord Color Emoji": a same-named system face (COLRv1 Noto, which swash draws blank/mono) would shadow it. A system Noto is used only if every face of it is CBDT/sbix (`pick_emoji_font`).
- `text_editor` `min_height`/`max_height` limit the text area, then the padding is added: pass `composer::text_height_limits`, never a total height (the 68 px composer bug).
- Composer is a `text_editor` (`app/composer.rs`): Enter sends, Shift+Enter newline; cursor columns are bytes. Container `shadow` is safe with ferroiced (tiny-skia clips it to the damage region), but
  the house style keeps the gradient header and sets no shadow (`ui/theme.rs` guard test).
