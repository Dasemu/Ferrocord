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
- iced 0.14 `daemon` drops the compositor when the last window closes, so "hide to tray" = close the window; set `exit_on_close_request: false` and handle `CloseRequested` yourself. `iced::daemon` view closures need a named `fn(&App, window::Id)` (inference fails on lifetimes).
- ksni 0.3 shares zbus 5 with notify-rust/keyring; `spawn()` errors when no StatusNotifierWatcher exists, which is the "no tray host" signal. SNI signals carry no values, so test proxies need `CacheProperties::No`.
- Bot pushes made with `GITHUB_TOKEN` never trigger `push` workflows, so the release workflow chains off the version workflow with `workflow_run` (conclusion `success`, branch `main`, checkout `ref: main` for the bump commit).
- `keyring` 4 opens a *blocking* zbus connection: if any dependency turns on zbus `tokio` (ksni default features, `rfd`/`ashpd` `tokio`) it panics ("Cannot start a runtime from within a runtime") when called on a runtime worker. Keep zbus on `async-io` (`ksni` with `default-features = false, features = ["async-io"]`, `rfd` with `async-std`); `tests/real_keyring.rs` guards it against a real Secret Service.
- **iced diffs widget state by position and type.** Wrapping the body in a `stack`/`column` only while a popup, banner or dialog is open resets every scrollable and hover state under it. Give each optional layer a fixed slot holding an empty `Space` when closed (`ui::view`, test `tree_shape`). A popup backdrop needs `mouse_area(..).interaction(Idle)` so lower layers see a levitating cursor; the menu opens exactly under the pointer and rows below toggled their hover toolbar at its edge.
- `image::Handle::from_rgba` in `view()` gets a new id per frame (re-upload, blinking): cache handles in the app (`App::qr_handle`).
- Under Xvfb with tiny-skia a window repaints only when the next input event arrives, so screenshots taken right after an xdotool action can be one event stale; wiggle the pointer before capturing.
- A button captures press and release, so drag and drop between rail items uses `mouse_area` tiles (press/release/enter/exit) plus a global left-release `DragEnd`.
- **Video helper**: decoding runs in `ferrocord-player` (a separate process), so the app links no codec and a decoder crash or hostile file cannot take the chat down; the helper's only non-glibc library is ALSA's `libasound` (cpal), checked by `.github/scripts/check-helper-libs.sh`. OpenH264 is C++: link libstdc++ statically in CI/release (`CXXSTDLIB=static=stdc++` plus `-L` to `libstdc++.a`).
- `matroska-demuxer`'s `seek` lands on the first frame AFTER a timestamp, not on a keyframe: the helper scans the keyframe timestamps once and seeks to `key - 1`; seeking to 0 reopens the file (browser-recorded WebM has neither cues nor a duration).
- With a sound device the audio the device consumed is the playback clock; a stream whose callbacks never run (device opened, not playing) must fall back to the wall clock or the picture freezes.
- Image handles for video frames are created in `update` per frame; the event stream drops stale frames so a slow UI never queues megabytes.
