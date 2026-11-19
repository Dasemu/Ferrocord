# Lessons

Things learned while building Ferrocord that are worth remembering.

- **iced 0.14**: keep default features; `default-features = false` breaks the
  `softbuffer`/winit backends. Screenshots under Xvfb work with
  `ICED_BACKEND=tiny-skia` and `window::screenshot`.
- **keyring 4.x** uses the `v1` feature for the classic `Entry` API; missing
  entries are `keyring::Error::NoEntry`. Some error variants carry raw bytes,
  so never `to_string()` them into user-facing text.
- **RSA key generation** (QR login) takes seconds in debug builds; the dev
  profile optimises `rsa`/`num-bigint-dig`, and generation runs in
  `spawn_blocking`.
- **zlib-stream**: one inflate context per connection; frames are only
  complete when they end with `00 00 ff ff`.
- **READY for user accounts** varies with capability flags (guild data under
  `properties`, deduplicated `users`, versioned `read_state`). Parse leniently.
- **Snowflakes** give exact message timestamps (`id >> 22`), so messages need
  no ISO-8601 parsing.
- iced 0.14 `scrollable::direction()` replaces the scrollbar *and its anchor*: call `anchor_bottom()` after `direction()`.
- GitHub push protection rejects token-shaped test fixtures; assemble them at runtime.
- iced `scrollable::scroll_to` offsets of an `anchor_bottom` list are measured from the bottom; a custom `Operation` can read a container's bounds by `Id` to compute them (`app/jump.rs`).
- Mouse position and Shift state are recorded in atomics from the event subscription (returning no message), so reading them in `update` costs no extra view rebuilds.
- `stack` only lets clicks through to a backdrop when the popup layer reports no mouse interaction; wrap popups in `opaque`.
- Reaction echoes of our own actions must be idempotent with the optimistic update.
- Bot pushes made with `GITHUB_TOKEN` never trigger `push` workflows, so the release workflow chains off the version workflow with `workflow_run` (conclusion `success`, branch `main`, checkout `ref: main` for the bump commit).
