# Ferrocord

A lightweight, native Discord desktop client written in Rust with
[iced](https://iced.rs). It talks to Discord's Gateway (WebSocket) and REST
API directly and aims to look and behave like the official client while
using a fraction of its memory.

> **Unofficial client.** Ferrocord is not affiliated with, endorsed by or
> supported by Discord Inc. "Discord" is a trademark of Discord Inc. No
> Discord logos, icons or fonts are included; the UI is recreated with Inter
> (SIL OFL 1.1) and Lucide icons (ISC).

> [!WARNING]
> **Using a third-party client breaks Discord's Terms of Service and can get
> your account banned.** Logging in with your account token through Ferrocord
> is "self-botting" as far as Discord is concerned. Use it at your own risk,
> preferably with an account you can afford to lose.

## Features

- Token login (token stored only in the OS keyring) and QR-code login
  (remote auth) using the official mobile app.
- Server rail with unread pills and mention badges, channel list with
  categories, direct messages and group DMs.
- Message history with Discord markdown (bold, italic, underline, strike,
  code, code blocks, spoilers, quotes, headings, lists, links, mentions,
  custom emoji, timestamps), embeds, image and file attachments.
- Send messages, typing indicators, unread and mention state (synced with
  read states / `MESSAGE_ACK`), desktop notifications for mentions and DMs.
- Local SQLite cache so the window opens instantly with your last state.
- Gateway: heartbeat with jitter, zombie detection, resume, reconnect with
  exponential backoff, `zlib-stream` compression. REST: per-bucket and global
  rate limiting with 429 retry.

Voice and video are out of scope.

## Build and run

Requirements: Rust stable (1.88+), and on Linux `pkg-config`, `libssl-dev`,
`libdbus-1-dev`, `libxkbcommon-dev`, `libwayland-dev`.

```sh
cargo build --release
./target/release/ferrocord            # real client
./target/release/ferrocord --mock     # demo data, no network
```

A Secret Service provider (KWallet on KDE, GNOME Keyring) must be running;
without one, login is disabled rather than storing the token in a file.

Useful environment variables:

| Variable | Purpose |
|---|---|
| `RUST_LOG` | Log filter (default `info`). Logs never contain tokens or message text. |
| `FERRO_THEME=light` | Light theme (dark is the default). |
| `FERROCORD_NOTIFY_PREVIEW=0` | Notifications show only the sender. |
| `FERROCORD_ZLIB=0` | Disable gateway transport compression. |
| `FERRO_TRACE_STARTUP=1` | Log `first_frame_ms`. |
| `ICED_BACKEND=wgpu` | Use the GPU renderer (default is the `tiny-skia` CPU renderer, which keeps idle memory around 50 MB). |

Screenshots of every main screen with mock data:

```sh
ICED_BACKEND=tiny-skia xvfb-run -a -s "-screen 0 1280x800x24" \
  cargo run -- --mock --screenshot guild docs/screenshots/guild.png
```

## Development

See [`CLAUDE.md`](CLAUDE.md), [`docs/architecture.md`](docs/architecture.md)
and [`docs/protocol.md`](docs/protocol.md).

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## License

MIT, see [LICENSE](LICENSE). Bundled fonts: Inter (SIL OFL 1.1, see
`assets/fonts/OFL.txt`). Icons: Lucide (ISC, see `assets/icons/LICENSE`).
