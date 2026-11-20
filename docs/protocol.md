# Protocol

Ferrocord talks to Discord the way a browser session of the official client
does: a user-account **Gateway** WebSocket for real-time events, the **REST
API** for history and actions, and the **remote-auth gateway** for QR login.
There is no official SDK for user accounts; everything below is implemented in
`src/net/` on top of `tokio-tungstenite` and `reqwest`.

> Third-party clients break Discord's Terms of Service. See the README warning.

## 1. Library choice

| Need | Choice | Why |
|---|---|---|
| WebSocket | `tokio-tungstenite` (native-tls) | Mandated by the brief; mature, tokio-native. |
| HTTP | `reqwest` (native-tls, JSON) | Mandated by the brief; connection pooling, timeouts. |
| zlib-stream | `flate2` | One shared inflate context per connection. |
| QR login crypto | `rsa` (OAEP/SHA-256), `sha2`, `base64` | Required by the remote-auth handshake. |
| QR rendering | `qrcode` | Module matrix → RGBA image in the login view. |

Bot libraries (serenity, twilight) were not used: they target bot accounts,
intents and sharding, and do not model user-account READY payloads, read
states or remote auth.

## 2. Gateway (`net/gateway.rs`)

* URL: `wss://gateway.discord.gg/?v=10&encoding=json&compress=zlib-stream`
  (`FERROCORD_GATEWAY_URL` overrides the host; `FERROCORD_ZLIB=0` disables
  compression).
* Architecture: a pure state machine `Session` (`on_payload`, `heartbeat`,
  `connect_url`) returns `Action`s; the async runner owns the socket, timers
  and reconnect loop. The stream returned by `gateway::spawn` *is* the
  connection: dropping it (logout, quit) closes the socket with code 1000.

### Opcodes handled

| Op | Name | Direction | Handling |
|---|---|---|---|
| 0 | DISPATCH | ← | Tracks `s`; parsed by `types::parse_dispatch`. |
| 1 | HEARTBEAT | ↔ | Sent every `heartbeat_interval`; answered immediately when the server asks. |
| 2 | IDENTIFY | → | Token, `capabilities`, `properties` (honest: `browser = "Ferrocord"`), presence. |
| 3 | PRESENCE_UPDATE | → | Own status (`online`/`idle`/`dnd`/`invisible`); also saved with `PATCH /users/@me/settings`. |
| 14 | GUILD_SUBSCRIPTIONS | → | Member list ranges `{guild_id, typing, threads, channels: {id: [[0,99]]}}`, sent while the member sidebar is open. |
| 6 | RESUME | → | `{ token, session_id, seq }` on the `resume_gateway_url`. |
| 7 | RECONNECT | ← | Close with 4000 and resume. |
| 9 | INVALID_SESSION | ← | `d = true` → resume; `false` → reset session, wait 1–5 s, identify. |
| 10 | HELLO | ← | Start heartbeating (first beat after `interval × random[0,1)`), then IDENTIFY or RESUME. |
| 11 | HEARTBEAT_ACK | ← | Clears the pending flag, measures latency. |

### Commands from the app (`GatewayHandle`)

The app records what it wants (own status, member list ranges) in a shared
`GatewayHandle`; the runner sends op 3 / op 14 once the session is READY and
again after every reconnect or resume. Identical requests are not re-sent.
Member list ranges grow in steps of 100 rows up to 500 (`app/members.rs`);
`GUILD_MEMBER_LIST_UPDATE` ops (SYNC, INSERT, UPDATE, DELETE, INVALIDATE) are
applied by a bounded sparse list (at most 1500 rows). Presence comes from
READY `presences` (friends), `PRESENCE_UPDATE`, and member list items.

### Heartbeat, zombies, reconnect

* If a heartbeat is due while the previous one was never acknowledged the
  connection is a zombie: close with a non-1000 code and resume.
* Reconnect delay: first retry after a healthy session within 0.1–1 s,
  then exponential backoff `1 s · 2^(n-1)`, capped at 60 s, with jitter
  factor `[0.5, 1)` (`gateway::backoff`). A successful READY/RESUMED resets
  the attempt counter. The UI shows `Reconnecting { attempt, delay_ms }`.
* Close codes: 4004 → `AuthFailed` (session cleared, back to login);
  4010–4014 → fatal; 4007/4009 → new session; everything else → resume if a
  session exists.

### zlib-stream (`net/zlib.rs`)

Binary frames are fragments of a single zlib stream. They are buffered until
the `00 00 ff ff` suffix, then inflated with `FlushDecompress::Sync` on the
shared `Decompress` context. The buffer is capped at 64 MiB.

### Dispatch events consumed

`READY`, `READY_SUPPLEMENTAL`, `RESUMED`, `MESSAGE_CREATE`, `MESSAGE_UPDATE`
(partial), `MESSAGE_DELETE`, `MESSAGE_DELETE_BULK`, `TYPING_START`,
`MESSAGE_ACK`, `CHANNEL_CREATE/UPDATE/DELETE`, `THREAD_CREATE/UPDATE/DELETE`,
`GUILD_CREATE/UPDATE/DELETE`, `MESSAGE_REACTION_ADD/REMOVE/REMOVE_ALL/REMOVE_EMOJI`,
`PRESENCE_UPDATE`, `GUILD_MEMBER_LIST_UPDATE`, `GUILD_EMOJIS_UPDATE`, `GUILD_STICKERS_UPDATE`. Unknown events are ignored; a malformed known
event is logged (without its payload) and the connection stays up.

### User-account READY quirks

The parser (`WireReady::into_domain`) accepts both layouts that the
capability flags produce:

* guild name/icon either top-level or under `properties`;
* `users` (deduplicated) plus inline `recipients`, and `recipient_ids`;
* `read_state` as a plain array or `{ entries, partial, version }`;
  only `read_state_type == 0` (channels) is used; `last_message_id: 0` means
  never read;
* `user_guild_settings` (muted guilds / channel overrides) and
  `user_settings.guild_folders` / `guild_positions` for the rail order;
* `merged_members[i]` aligned with `guilds[i]` for the current user's roles
  (role mentions, author colours). `READY_SUPPLEMENTAL` adds other members.
* Unavailable guilds are skipped until their `GUILD_CREATE`.

Message timestamps come from the snowflake id (`id >> 22` + Discord epoch),
so no ISO-8601 parsing is needed on the hot path.

## 3. REST (`net/rest.rs`, `net/ratelimit.rs`)

Base: `https://discord.com/api/v9` (`FERROCORD_API_URL`). Header
`Authorization: <token>` (user tokens have no `Bot ` prefix) and an honest
`User-Agent: Ferrocord/<version>`.

| Call | Endpoint |
|---|---|
| Validate token | `GET /users/@me` |
| History | `GET /channels/{id}/messages?limit=50[&before=]` |
| Send | `POST /channels/{id}/messages` `{ content, nonce, tts }` |
| Typing | `POST /channels/{id}/typing` (at most every 8 s while typing) |
| Edit / delete | `PATCH` / `DELETE /channels/{c}/messages/{m}` |
| Reply | `POST .../messages` with `message_reference` and, if "@ OFF", `allowed_mentions {parse:[users,roles,everyone], replied_user:false}` |
| Reactions | `PUT/DELETE /channels/{c}/messages/{m}/reactions/{emoji}/@me` (`%`-encoded Unicode, or `name:id`) |
| Jump | `GET .../messages?around={m}` / `?after={m}` |
| Search | `GET /guilds/{g}/messages/search?content=&offset=` or `/channels/{c}/...` (202 + `retry_after` while indexing is retried, max 4 tries) |
| Upload | `POST .../messages` `multipart/form-data`: `payload_json` + `files[n]`, streamed from disk with an explicit `Content-Length` |
| Status | `PATCH /users/@me/settings {status}` |
| Mark read | `POST /channels/{c}/messages/{m}/ack` `{ token: null }` |
| QR ticket | `POST /users/@me/remote-auth/login` `{ ticket }` |
| GIF picker | `GET /gifs/trending-gifs` / `GET /gifs/search?q=` with `media_format=mp4&provider=tenor&locale=en-US`; picking sends the Tenor `url` as text (shown back as a `gifv` embed) |
| Sticker | `POST .../messages` `{ content: "", nonce, sticker_ids: [id] }` |
| Media | CDN `avatars/`, `icons/`, `emojis/`, `stickers/{id}.png\|.gif?size=160`, attachment `proxy_url?width=&height=`; GIF picker previews from `media.tenor.com` (HTTPS, never with the token) |

### Rate limits

* Route key = method + path with major parameters (`channels/`, `guilds/`,
  `webhooks/` ids) kept and other ids replaced by `:id`
  (`ratelimit::route_key`).
* `X-RateLimit-Bucket` maps a route to a bucket; bucket state is
  `remaining` + reset instant (`X-RateLimit-Reset-After`), keyed by
  `hash:major`. A request with `remaining == 0` waits until the reset.
* 429: wait `retry_after` from the JSON body (falling back to the
  `Retry-After` header), global if `X-RateLimit-Global` or `global: true`;
  retry up to 3 times.
* The limiter takes the clock as a parameter and is unit-tested; the mock
  REST tests check real waiting behaviour.

### Optimistic sending

The composer creates a pending message with a snowflake `nonce`; the
`MESSAGE_CREATE` echo (or the REST response) with the same nonce replaces
it. Failures turn it red with "Message could not be delivered.".

## 4. QR login / remote auth (`net/remote_auth.rs`)

`wss://remote-auth-gateway.discord.gg/?v=2` with `Origin: https://discord.com`.

1. ← `hello { heartbeat_interval, timeout_ms }`
2. → `init { encoded_public_key }` — fresh RSA-2048 key, SPKI DER, base64
3. ← `nonce_proof { encrypted_nonce }` (RSA-OAEP, SHA-256)
4. → `nonce_proof { proof }` = base64url-no-pad(SHA-256(nonce))
5. ← `pending_remote_init { fingerprint }` → QR of `https://discord.com/ra/<fingerprint>`
6. ← `pending_ticket { encrypted_user_payload }` → `id:discriminator:avatar:username`
7. ← `pending_login { ticket }` → REST ticket exchange → `encrypted_token` → decrypt
8. ← `cancel` when declined on the phone; the socket closes at `timeout_ms`.

Client heartbeats are `{ "op": "heartbeat" }`. The protocol is implemented
and tested end to end against a local mock (`tests/mock_remote_auth.rs`);
it has **not** been verified against the real service (no account here),
and Discord may require a captcha for the ticket exchange in some regions.

## 5. Read states, unread and mentions (`app/state.rs`)

* A channel is unread when `channel.last_message_id > read_state.last_read`
  (or it has messages and no read state).
* `mention_count` comes from READY and is incremented locally for messages
  that mention the user, their roles, `@everyone`/`@here`, or any DM message
  from someone else. `MESSAGE_ACK` from other sessions updates both.
* Opening a channel (or receiving a message while it is open and the window
  focused) marks it read locally and ACKs it over REST.
* Muted guilds/channels do not show the unread pill, but mention badges
  still count — like the official client.
* Desktop notifications (`notify-rust`, D-Bus) fire for mentions and DMs
  unless the conversation is open and focused. `FERROCORD_NOTIFY_PREVIEW=0`
  shows "New message" instead of the text.

## 6. Local cache (`store/`)

SQLite (`cache.sqlite3`, WAL). Tables: `meta` (selected guild/channel,
`me`), `users`, `guilds`, `channels`, `messages` (≤ 1000 per channel),
`read_states`. Domain objects are stored as JSON next to indexed ids.
Pending/failed local messages and credentials are never written. The
database is plain text (not encrypted); the data directory is `0700` and the
database, `-wal`/`-shm` and log files are `0600`.

## 7. UI reference

Measurements of the official dark theme reproduced in `ui/theme.rs`:

| Element | Value |
|---|---|
| Server rail | 72 px wide, `#1e1f22`, icons 48 px, 8 px gap, circle → 16 px radius on hover/selection |
| Rail pill | 4 px wide white; 8 px tall unread, 40 px selected |
| Channel sidebar | 240 px, `#2b2d31`; header 48 px with a darker bottom edge (gradient, see below) |
| Category | 12 px semibold uppercase `#949ba4`, chevron |
| Channel row | 32 px tall, radius 4, name 16 px medium; muted `#80848e`, unread white semibold, selected `rgba(78,80,88,.6)` |
| Mention badge | `#f23f43`, 16 px tall, 12 px bold white |
| Main pane | `#313338`; header 48 px, 16 px semibold title, topic `#b5bac1` |
| Message group | avatar 40 px at 16 px inset, name 16 px medium (role colour), timestamp 12 px `#949ba4`, 16 px top gap, grouped within 7 min |
| Mention row | background `rgba(240,177,50,.08)`, 2 px `#f0b132` left bar |
| Mention chip | `rgba(88,101,242,.3)` / `#c9cdfb` |
| Embed | `#2b2d31`, 4 px colour bar, max 520 px, 16 px padding |
| Composer | `#383a40`, radius 8, 44 px; typing indicator 24 px below |
| User panel | 52 px, `#232428` |
| Accent | `#5865f2`, links `#00a8fc` |

No style uses `container::Style::shadow`: tiny-skia draws a quad's shadow
unclipped by the damage region, so on partial redraws (scrolling, hover; the
norm on Windows) it piles up over unrepainted areas until they turn black.
Header "shadows" are a gradient on the bar itself; popups rely on their border.

Fonts: Inter (OFL) embedded; Noto Color Emoji from the system. Icons: Lucide
(ISC), plus our own `gif.svg` in the same style. No Discord logos, fonts or artwork are used.

## 8. Round 2 behaviour notes

* **Replies**: header from `referenced_message` (null = "Original message was
  deleted"); refreshed from the loaded window when the original is there.
  Replying to yourself never pings.
* **Upload limits** (`rest::upload_limit`): 10 MiB default; Nitro Classic/Basic
  50 MiB, Nitro 500 MiB; server boost tier 2 = 50 MiB, tier 3 = 100 MiB;
  at most 10 files. Pasted images are written to a private temp file and
  deleted afterwards.
* **Detached windows**: after jumping to an old message the window holds that
  region only; live messages are not appended until "Jump To Present" or
  scrolling to the bottom loads the newer pages.
* **Desktop helpers**: file dialog via `rfd` (xdg-portal, no GTK); Ctrl+V reads
  copied files or an image through `arboard` (X11 and Wayland data-control),
  encoded to PNG with a pixel and byte cap.

## 9. Round 3: header buttons, servers, discovery, folders

All calls go through `RestClient::request` (per-route buckets, 429 retry);
pure parsing/payloads live in `net/extras.rs`, calls in `net/rest/ext.rs`.

| Feature | Calls |
|---|---|
| Bell | `PATCH /users/@me/guilds/{guild\|@me}/settings` with `channel_overrides.{id}`: `message_notifications` (0 all, 1 mentions, 2 nothing, 3 inherit), `muted` + `mute_config {selected_time_window, end_time}` (`-1`/`null` = until turned back on). READY `user_guild_settings` and `USER_GUILD_SETTINGS_UPDATE` feed `ChatState::channel_notify`; expired timed mutes are dropped. |
| Pins | `GET /channels/{id}/pins` (array, or the paginated `{items:[{message}]}` shape); `DELETE /channels/{c}/pins/{m}`. The unpin button shows for DMs, the owner, Administrator and Manage Messages (server-wide role bits; channel overwrites are not applied, a 403 is reported). |
| Inbox | `GET /users/@me/mentions?limit=25&roles=true&everyone=true[&before=][&guild_id=]` (Mentions tab, "This Server" filter, load more). |
| Help | opens `https://support.discord.com/` in the browser. |
| Search | `GET /guilds/{id}/messages/search` / `/channels/{id}/messages/search`; `from:name` -> `author_id`, `has:image\|video\|sound\|file\|link\|embed\|sticker` -> `has`, `in:#channel` -> `channel_id` (servers only). Names resolve against known users/channels; an unknown one is an error shown in the panel. |
| Create server | `POST /guilds {name, icon?, channels: [], system_channel_id: null}`; icon is a PNG/JPEG/GIF/WebP `data:` URI (4 MB cap, no cropping). No `guild_template_code` is sent (see PR: template codes are unverified). |
| Join | `GET /invites/{code}?with_counts=true&with_expiration=true` (preview card) then `POST /invites/{code} {session_id}`. A 400 body with `captcha_key` becomes `Error::CaptchaRequired`: a clear message, one attempt, **no solving**. |
| Discover | `GET /discovery/categories`, `GET /discoverable-guilds?offset&limit[&category_id]`, `GET /discovery/search?query&offset&limit[&category_id]`, join with `PUT /guilds/{id}/members/@me {session_id}`. "Details" is a card from the listing (no lurking). Endpoint shapes are tolerant (`guilds`/`hits`/array). |
| Folders | READY `user_settings.guild_folders` (`id`, `name`, `color`, `guild_ids`; `id: null` = single server). Saved with `PATCH /users/@me/settings {guild_folders: [...]}` (the same legacy settings endpoint that stores the status). Ferrocord identifies with `capabilities: 0`, so the server keeps sending legacy JSON settings; the official client's `PATCH /users/@me/settings-proto/1` (base64 protobuf `GuildFolders`) would need the proto capability and an encoder, and is not implemented. `USER_SETTINGS_UPDATE` with `guild_folders` keeps other sessions in sync; a failed save rolls back. The list is cached in `meta("guild_folders")`. |

Rail gestures (`app/rail.rs`): press on a server, release on another -> new
folder in place; release on a folder -> joins it; release on the same item ->
click; a release anywhere else (`DragEnd`) cancels. Folders open/closed state
is local only.

## 10. Profile cosmetics and animations

Field names and URLs were checked against the public user-account docs
(discord-userdoccers `resources/user`, `resources/collectibles`,
`resources/presence`, `reference#cdn-formatting`) and Discord's official API
docs. What could not be confirmed against a live account is listed at the end.

### Payload fields (`net/cosmetics.rs`)

All of these are parsed leniently (`lenient`): a wrong shape drops the field,
never the message or member list.

| Where | Field | Use |
|---|---|---|
| user / partial user / member | `avatar_decoration_data { asset, sku_id }` | frame over the avatar |
| user | `collectibles.nameplate { asset, palette, label, sku_id }` | row background |
| user | `primary_guild { identity_guild_id, identity_enabled, tag, badge }` | server tag chip |
| user | `display_name_styles { font_id, effect_id, colors[] }` | name colour / gradient |
| user | `banner`, `accent_color` | popout banner |
| presence | `activities[]` (type, name, details, state, emoji, timestamps, assets, application_id) | popout activity and custom status |

`GUILD_MEMBER_LIST_UPDATE` member items may carry the member's own
`avatar_decoration_data`; it replaces the user's. The member-list
subscription (op 14) now sends `"activities": true`, like the official
client, so presences include activities.

### Profile popout

`GET /users/{id}/profile?type=popout&with_mutual_guilds=true&with_mutual_friends=true&with_mutual_friends_count=true[&guild_id=..]`.
The body's `user_profile` and `guild_member_profile` are layered (the guild
value wins when set; an empty guild bio / pronouns falls through). Used:
`bio`, `pronouns`, `banner`, `accent_color`, `theme_colors[2]`,
`profile_effect.id`, `badges[]`, `premium_since`, `guild_member`
(`roles`, `joined_at`), `mutual_guilds[]`, `mutual_friends[]`,
`mutual_friends_count`. Profiles are cached (48, LRU) and refreshed when
older than 5 minutes; a failed refresh keeps the old card.

### Profile effects

`GET /user-profile-effects` (once, lazily, when a shown profile has an
effect) returns the layer configs: `src`, `loop`, `width`, `height`,
`duration`, `start`, `loopDelay`, `position {x,y}`, `zIndex`,
`randomizedSources[]`. Layers are drawn over the popout card with the
timeline in `net::cosmetics::layer_time` (one-shot layers end, looping ones
repeat after `loopDelay`), scaled so the union of the layer rectangles spans
the card width. Frames are decoded at a reduced width (256 px) and scaled on
draw; they are dropped when the popout closes.

### CDN URLs

| Asset | URL |
|---|---|
| avatar (still) | `/avatars/{uid}/{hash}.png?size=80` (160 in the popout) |
| avatar (animated, `a_`) | same with `.gif` |
| decoration | `/avatar-decoration-presets/{asset}.png?size=96&passthrough=false` (still) / `passthrough=true` (APNG, `a_` assets) |
| banner | `/banners/{uid}/{hash}.png|gif?size=600`; member banner `/guilds/{gid}/users/{uid}/banners/{hash}` |
| nameplate | `/assets/collectibles/{asset}static.png` (the animated variant is `asset.webm`: not decoded) |
| server tag badge | `/guild-tag-badges/{gid}/{badge}.png?size=32` |
| profile badge | `/badge-icons/{icon}.png` (fetched from the CDN, not bundled; a neutral dot shows until loaded) |
| activity art | application asset `/app-assets/{app}/{id}.png`, `mp:` -> `media.discordapp.net`, `spotify:` -> `i.scdn.co/image` |
| animated emoji | `/emojis/{id}.gif?size=96` |

`i.scdn.co` was added to the media host allow-list for Spotify album art;
downloads never carry the token.

### Animation engine (`app/anim.rs`)

* GIF, APNG and animated WebP are decoded in `spawn_blocking` with the
  `image` crate; single-frame images are left to the static path.
* Caps per asset (source frames read, frames kept, bytes kept, width) are
  per `Class`; extra frames are merged (delay added to the kept frame) so the
  duration is unchanged. Sources above 2048 px are refused.
* `AnimCache` is an LRU with a 32 MB budget. Banner and effect frames are
  dropped when the popout closes.
* Lists and messages show the still image; hovering a row / message starts
  the clock for that avatar, decoration and (in messages) animated emoji.
  The popout animates everything. A `time::every(66 ms)` subscription exists
  only while `AnimState::active` is true (something animated is hovered or
  the popout shows one); "Reduce motion" in Settings turns all of it off.
* Handles are created once when data arrives (`Handle::from_rgba` in the
  decode task), never in `view`.

### Display name styles

Effects `solid`, `gradient`, `glow` (falls back to gradient), `prism` and
`gummy` are drawn as a solid colour or a per-character gradient; `neon`,
`toon` and `pop` (glow, outline, drop shadow) are drawn as the first colour.
Animation (prism scroll, gummy squash) is not drawn. Fonts other than the
default gg Sans are not bundled: a bold weight of Inter stands in for them.

### Not verified against a live account

* The response key of `GET /user-profile-effects` (`profile_effect_configs`
  is what clients use; a bare array and `configs` are accepted too) and the
  coordinate system of effect layers (the scale-to-card rule is inferred).
* `passthrough=true` for decoration assets not prefixed `a_` (those are
  treated as still).
* Server-specific display name styles and nameplates in member objects.
