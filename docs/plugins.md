# Plugins

Ferrocord plugins add features without putting them in the base app. A
plugin is a small **WebAssembly module** plus a **manifest**. There is no web
stack: no CSS, no JavaScript, no webview. Plugins never draw: they declare
commands, menu items, settings and message decorations, and the host renders
all of it with the app's own theme.

> **Plugins are third-party code.** Ferrocord sandboxes them and limits each
> one to the permissions you grant, but only install plugins you trust.
> A plugin that automates your account (a "selfbot": automatic replies,
> spam, scheduled messages, scraping) breaks Discord's Terms of Service and
> can get the account banned. The host refuses that kind of use (see
> [Permission model](#3-permission-model)); do not write, share or install
> such plugins.

There is **no plugin store**. Plugins are installed by copying them into the
plugins folder.

Contents: [1. Install](#1-install-and-manage) ·
[2. Manifest](#2-the-manifest-plugintoml) · [3. Permissions](#3-permission-model) ·
[4. Limits](#4-limits) · [5. Extension points](#5-extension-points) ·
[6. ABI](#6-the-abi-version-1) · [7. SDK guide](#7-rust-sdk-guide) ·
[8. Other languages](#8-other-languages) · [9. Cost](#9-cost-and-runtime-choice) ·
[10. Host internals](#10-host-internals)

## 1. Install and manage

A plugin is either a **folder** or a **`.ferroplug` file** (a zip) holding
`plugin.toml` and `plugin.wasm`, placed in the plugins folder:

| OS | Plugins folder |
|---|---|
| Linux | `~/.config/ferrocord/plugins/` (`$XDG_CONFIG_HOME`) |
| Windows | `%APPDATA%\ferro\ferrocord\config\plugins\` |
| macOS | `~/Library/Application Support/dev.ferro.ferrocord/plugins/` |

`FERROCORD_PLUGINS_DIR` overrides it (also in `--mock` mode, which otherwise
has no plugins folder).

User Settings → **Plugins** (last sidebar category) lists what is installed. Every plugin is
**off by default**. Turning one on shows everything it may do (its
permissions and, for network access, every host it can reach) and needs a
click on **Enable**. The choice, the granted permissions and the plugin's
settings are saved in `plugins/state.json`. If a new version of a plugin asks
for more permissions or hosts, it is turned off until you enable it again.

**Open plugins folder** opens the folder in the file manager. **Reload
plugins** rescans the folder and restarts every enabled plugin from disk
(hot reload while developing one).

The plugin's private store (`storage` permission) lives in
`<data dir>/plugins.sqlite3`.

## 2. The manifest (`plugin.toml`)

```toml
id = "word-highlighter"          # a-z 0-9 . _ -, at most 64; unique
name = "Word Highlighter"
version = "1.0.0"
author = "Ferrocord contributors"
description = "Highlights messages that contain any of your words."
api_version = 1                  # the ABI this plugin was built for
permissions = ["read_messages", "decorations"]
network_hosts = []               # with the "network" permission only
events = ["message_create", "message_update", "history"]

[[settings]]                     # rendered by the host on the Plugins page
key = "words"                    # a-z 0-9 _
type = "string"                  # bool | int | string | choice
label = "Words"
description = "Comma-separated, case-insensitive."
default = "ferrocord, rust"
max_len = 300                    # string only (default 200, max 4000)
# min = 1, max = 10              # int only
# choices = ["a", "b"]           # choice only (1-16)

[[commands]]                     # local /commands (at most 16)
name = "lenny"
description = "Sends ( ͡° ͜ʖ ͡°)"

[[actions]]                      # message context-menu items (at most 4)
id = "copy-quote"
label = "Copy as Quote"
```

Unknown keys are errors, so a typo is caught instead of silently ignored.
Events must be backed by their permission (`message_*` and `history` need
`read_messages`, `presence` needs `read_presence`). `network` and
`network_hosts` go together; hosts are bare lowercase domain names (no
scheme, port, IP address or wildcard).

## 3. Permission model

The sandbox is the WebAssembly module itself: a plugin can only call the
two functions the host imports into it (`host_call`, `host_take`). It has no
file system, no clock, no sockets, no threads and no access to the app's
memory, so it can never read the Discord token, the keyring, the cache or
arbitrary files. Each host operation then checks a permission:

| Permission | Allows | Restriction |
|---|---|---|
| — | `log`, `toast`, `get_setting` / `set_setting` (schema-checked), `now_ms` | one toast per hook call |
| `read_messages` | `message_create` / `message_update` / `message_delete` / `history` events | read-only, asynchronous |
| `read_presence` | `presence` events | read-only |
| `transform_outgoing` | the outgoing transform hook | pure: only settings, log, clock |
| `send_messages` | `send_message(text)` | **only during a user action** (see below), one message per action, to the action's channel, ≤ 2000 chars |
| `reactions` | `add_reaction(emoji)` | user action only, Unicode emoji, on the action's message |
| `clipboard` | `clipboard_write(text)` | user action only |
| `composer` | `composer_insert(text)` | user action only |
| `storage` | `kv_get` / `kv_set` / `kv_delete` | own namespace, quotas (§4) |
| `network` | `http_fetch` | HTTPS `GET`/`POST` to `network_hosts` only, default port, no redirects, no cookies |
| `decorations` | `decorate(message, [...])` | ≤ 4 per message, ≤ 100 chars each |

**User actions.** A *grant* is created only when the user explicitly runs a
plugin's `/command` (typing it and pressing Enter) or picks its menu item.
It is scoped to that channel (and message), lasts 30 s (so an action can
make one HTTP round trip and send its result, e.g. a translation) and allows
one `send_message`. Events, the transform, settings changes, timers and
answers to requests not started by a user action carry no grant:
`send_message` there returns `NOT_USER_ACTION`. Plugins have no timers at
all. This is what makes automatic replies or scheduled messages impossible.

**No raw traffic.** Plugins cannot open connections to Discord, send gateway
payloads or call arbitrary REST routes. `send_message` and `add_reaction`
are typed requests that the app performs with its own code paths: the same
endpoints, headers, nonce handling and rate limiter as when you type the
message yourself. `http_fetch` uses a separate client (no Discord headers,
no cookies, no token) and the allowlist is checked twice: in the plugin
thread and again in the app before the request leaves.

**Faults.** Every call runs under `catch_unwind` with a fuel budget and a
memory cap. A trap (a Rust panic is an `unreachable` trap), running out of
fuel or broken ABI use is a *fault*: the call's requests are dropped, the
instance is restarted from scratch (its settings are kept), and after three
faults the plugin is turned off with a toast and the reason is shown on the
Plugins page. Nothing a plugin does can crash or freeze the app.

## 4. Limits

| What | Limit |
|---|---|
| `plugin.wasm` | 8 MiB; `.ferroplug` 16 MiB; `plugin.toml` 64 KiB |
| Linear memory | 16 MiB (one memory, `memory.grow` beyond fails) |
| Fuel (≈ wasm instructions) | load 50 M · event / setting 50 M · transform 20 M · command / action / HTTP answer 100 M |
| Transform chain | 150 ms wall clock in the plugin thread; the app sends the original text after 300 ms |
| Host calls per hook | 256; input ≤ 256 KiB, result ≤ 256 KiB |
| Event queue | 256 events; channel switches and history coalesce; the oldest is dropped when full |
| Key-value store | key ≤ 128 B, value ≤ 64 KiB, 1 MiB and 1024 keys per plugin |
| HTTP | 4 in flight per plugin; request body 64 KiB, 16 headers; response 1 MiB; 10 s timeout |
| Decorations | 4 per plugin and message, text 100 chars, tooltip 200; 4096 decorated messages kept |
| Toast | 200 chars, one per hook call |
| Faults | 3, then the plugin is turned off |

## 5. Extension points

All of them are declarative: the plugin returns data, the host draws.

1. **Lifecycle** — `load` (with the current settings) and `unload`
   (disable, reload, quit). Settings declared in the manifest are rendered
   on the Plugins page; changes reach the plugin through `setting_changed`.
2. **Observers** — `message_create`, `message_update`, `message_delete`,
   `history` (the newest ≤ 50 loaded messages of a channel when it opens or
   a page of history arrives, so decorations cover what is on screen),
   `channel_switch`, `presence`. Delivered read-only and asynchronously on
   the plugin thread; the UI never waits for them.
3. **Outgoing transform** — rewrites the composer text before it is sent
   (text replacements, `/shrug`-style shortcuts). Plugins run in load order,
   each one sees the previous one's output. Not applied to edits or file
   captions.
4. **Local commands** — `/name args` typed in the composer runs the plugin
   that registered `name` instead of sending; unknown `/words` are sent as
   normal text.
5. **Message context-menu actions** — extra items at the end of a message's
   right-click menu.
6. **Message decorations** — `badge` (a pill under the message, optional
   tooltip), `line` (a muted line, e.g. a translation) and `highlight`
   (tints the row; its text is shown as a badge).
7. **Host calls** — see §3 and §6.

## 6. The ABI (version 1)

Core WebAssembly (`wasm32`), no WASI, no component model. All payloads are
**MessagePack**, structs encoded as **maps keyed by field name**, so fields
can be added later without breaking older plugins (unknown keys are ignored,
missing ones default). Enums with data carry a `"type"` key. Setting values
are an untagged bool / integer / string. Byte fields are `bin`. The Rust
definitions in `crates/ferrocord-plugin-sdk/src/abi.rs` are the reference.

### Guest exports

| Export | Signature | Meaning |
|---|---|---|
| `memory` | memory | the only linear memory |
| `fc_abi_version` | `() -> i32` | must return `1` |
| `fc_alloc` | `(len: i32) -> i32` | returns a buffer of `len` bytes; the host writes a hook input into it and passes it to `fc_call`, which owns it from then on |
| `fc_call` | `(hook: i32, ptr: i32, len: i32) -> i64` | runs a hook. Returns `0` (no output), `-1` (input not understood; not a fault) or `(out_ptr << 32) \| out_len` pointing at the MessagePack output, which must stay valid until the next call |

A start function, if any, runs during instantiation under the load fuel.

### Hooks (`fc_call` first argument)

| Id | Hook | Input | Output |
|---|---|---|---|
| 1 | load | `LoadInfo {api_version, plugin_id, settings}` | — |
| 2 | unload | empty | — |
| 3 | event | `Event` (`{"type": "message_create", "message": {...}}`, ...) | — |
| 4 | transform | `TransformInput {channel_id, text}` | `TransformOutput {text: nil \| str}` |
| 5 | command | `CommandInput {name, args, channel_id, guild_id}` | — |
| 6 | action | `ActionInput {action, message: MessageInfo}` | — |
| 7 | setting_changed | `SettingChanged {key, value}` | — |
| 8 | http_response | `HttpResponse {request_id, status, body, error}` | — |

`MessageInfo` = `{id, channel_id, guild_id, author_id, author_name, content,
timestamp_ms, mine}` (ids are `u64` snowflakes).

### Host imports (module `ferrocord`)

| Import | Signature | Meaning |
|---|---|---|
| `host_call` | `(op: i32, ptr: i32, len: i32) -> i32` | runs operation `op` on the MessagePack input at `ptr`/`len`. `>= 0`: length of the result, which the host keeps; `< 0`: an error code |
| `host_take` | `(ptr: i32, len: i32) -> i32` | copies the pending result (at most `len` bytes) to `ptr`, returns the bytes copied |

| Op | Name | Input | Result |
|---|---|---|---|
| 1 | log | `{level: 0-3, message}` | — |
| 2 | toast | `{text}` | — |
| 3 | get_setting | `{key}` | value or nil |
| 4 | set_setting | `{key, value}` | — |
| 5 | kv_get | `{key}` | bin or nil |
| 6 | kv_set | `{key, value: bin}` | — |
| 7 | kv_delete | `{key}` | — |
| 8 | http_fetch | `{method, url, headers: [[k, v]], body: bin}` | request id (u64) |
| 9 | send_message | `{text}` | — |
| 10 | decorate | `{message_id, decorations: [{kind, text, tooltip}]}` (`kind`: `"badge"`, `"line"`, `"highlight"`) | — |
| 11 | clipboard_write | `{text}` | — |
| 12 | composer_insert | `{text}` | — |
| 13 | add_reaction | `{text}` (one Unicode emoji) | — |
| 14 | now_ms | nil | u64 |

| Code | Name | Meaning |
|---|---|---|
| -1 | DENIED | the permission is missing (or the op is not allowed in a transform) |
| -2 | BAD_INPUT | did not decode or failed validation |
| -3 | NOT_USER_ACTION | needs a user action |
| -4 | LIMIT | size, quota or budget exceeded |
| -5 | UNKNOWN_OP | |
| -6 | HOST_NOT_ALLOWED | URL not HTTPS to a listed host |
| -7 | UNAVAILABLE | storage failure |

Plugin logs are shown only with `FERROCORD_PLUGIN_LOG=1` (they may contain
message text); otherwise errors and warnings are logged redacted.

**Versioning.** `api_version` and `fc_abi_version` must both match the
host's. Additive changes (new hooks, ops, event types, fields) keep version
1: plugins ignore what they do not know and the host never calls a hook a
plugin did not ask for, except the lifecycle ones. A breaking change bumps
the version, and older plugins are then refused with a clear message.

## 7. Rust SDK guide

`crates/ferrocord-plugin-sdk` wraps the ABI: implement the hooks you need on
a `Default` type and register it.

```toml
# Cargo.toml
[package]
name = "shout"
version = "1.0.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
ferrocord-plugin-sdk = { git = "https://github.com/Dasemu/Ferrocord" }

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
panic = "abort"
strip = true
```

```rust
// src/lib.rs
use ferrocord_plugin_sdk::prelude::*;

#[derive(Default)]
struct Shout;

impl Plugin for Shout {
    fn transform(&mut self, input: TransformInput) -> Option<String> {
        input.text.strip_prefix("!shout ").map(str::to_uppercase)
    }

    fn command(&mut self, cmd: CommandInput) {
        if cmd.name == "hello" {
            let _ = host::send_message("Hello!");
        }
    }
}

register_plugin!(Shout);
```

```toml
# plugin.toml
id = "shout"
name = "Shout"
version = "1.0.0"
api_version = 1
permissions = ["transform_outgoing", "send_messages"]

[[commands]]
name = "hello"
description = "Says hello"
```

Build and install:

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
mkdir -p ~/.config/ferrocord/plugins/shout
cp plugin.toml ~/.config/ferrocord/plugins/shout/
cp target/wasm32-unknown-unknown/release/shout.wasm ~/.config/ferrocord/plugins/shout/plugin.wasm
# or zip both files into shout.ferroplug
```

Then User Settings → Plugins → Reload plugins, and enable it.

Host calls are in `ferrocord_plugin_sdk::host` (`toast`, `setting`,
`set_setting`, `kv_get`/`kv_set`/`kv_delete`, `http_fetch`,
`send_message`, `decorate`, `clipboard_write`, `composer_insert`,
`add_reaction`, `now_ms`, `log`). A panic is logged through `log` and then
traps; the host restarts the plugin.

**Testing.** On a native target the host calls go to a recorder, so plugin
logic runs under plain `cargo test`: `host::testing::reply(..)` queues an
answer, `host::testing::take_calls()` returns what was called.

**Examples** (`examples/plugins/`, `build.sh` builds them into
`dist/<id>/`):

| Plugin | Shows |
|---|---|
| `text-replacer` | outgoing transform (`/shrug`, `/tableflip`, `/unflip`, `from=>to` rules), a `/lenny` command that sends, bool and string settings |
| `word-highlighter` | observers (`message_create`, `message_update`, `history`) and decorations, string and choice settings |
| `quote-message` | context-menu actions with `clipboard_write` and `composer_insert` |

CI builds them for `wasm32-unknown-unknown` and `tests/plugins_examples.rs`
drives them through the real app.

## 8. Other languages

Anything that compiles to core wasm and can export the three functions and
`memory` works (C/C++ with clang, Zig, TinyGo, AssemblyScript, ...). You need
a MessagePack library, a bump or real allocator behind `fc_alloc`, and the
two imports. `tests/plugins_sandbox.rs` builds minimal modules straight from
WebAssembly text, which is a compact reference for the calling convention.

## 9. Cost and runtime choice

**Zero plugins enabled costs nothing.** At startup the app does one `stat`
of `plugins/state.json`; when no plugin is enabled there is no wasm engine,
no plugin thread, no subscription and no extra memory
(`tests/plugins_zero_cost.rs` asserts that no engine and no thread exist).
Enabling the first plugin starts one thread and the engine; disabling the
last one stops both.

**wasmi vs wasmtime**, measured with the same host ABI and the
`word-highlighter` example (release builds, x86-64, `message_create` hook
with two settings reads and one decoration):

| | wasmi 2.0 (interpreter) | wasmtime 51 (Cranelift JIT) |
|---|---|---|
| Binary size added (standalone) | **+1.5 MiB** | +10.8 MiB |
| RSS, engine only | +0.2 MiB | +2.1 MiB |
| RSS, 1 plugin | **+1.8 MiB** | +17.2 MiB |
| RSS, 5 plugins | **+4.6 MiB** | +21.2 MiB |
| Load (compile + instantiate), per plugin | **~3 ms** | ~420 ms |
| `message_create` hook, median | 70 µs | 7 µs |

Plugin hooks are short and run off the UI thread, so 70 µs per event is
invisible, while wasmtime's half-second compile per plugin and 17 MiB would
hurt cold start and the 150 MB idle budget. wasmi also has deterministic
fuel metering and needs no JIT memory (W^X-friendly). wasmi is used with
lazy translation (function bodies are validated up front, compiled to its
bytecode on first call).

The numbers for the whole app (release build, mock mode, idle) are in the
pull request that added plugins and are repeated in `README.md`.

## 10. Host internals

| Module | Role |
|---|---|
| `crates/ferrocord-plugin-sdk` | ABI types (shared by host and guest), guest SDK |
| `src/plugins/manifest.rs` | manifest parsing and validation, permissions, settings schema |
| `src/plugins/package.rs` | folder / `.ferroplug` discovery, size caps |
| `src/plugins/state.rs` | `state.json`: enabled, grants, setting values |
| `src/plugins/runtime.rs` | wasmi engine, store limits, fuel, the two imports |
| `src/plugins/hostcalls.rs` | every host operation: permission, user-action and limit checks |
| `src/plugins/worker.rs` | the plugin thread, priority + bounded event queues, faults |
| `src/plugins/kv.rs` | per-plugin SQLite store with quotas |
| `src/app/plugins.rs` | App glue: hook points, Plugins page messages, performing requests |
| `src/ui/plugins.rs` | Plugins page, enable prompt, decorations, menu entries |

The app touches plugins at narrow hook points only: `App::send`
(commands and transform), `on_net` (observers), `switch_view` and history
pages (`channel_switch` / `history`), the message context menu, the message
view (decorations) and the settings screen (the `Plugins` category).
