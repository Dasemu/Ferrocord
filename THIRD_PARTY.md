# Third-party assets

Ferrocord bundles the following third-party assets. No Discord logos, icons,
fonts or artwork are included.

| Asset | Location | Licence | Licence text |
|---|---|---|---|
| Inter font (Regular, Medium, SemiBold, Bold), © The Inter Project Authors | `assets/fonts/` | SIL Open Font License 1.1 | [`assets/fonts/OFL.txt`](assets/fonts/OFL.txt) |
| Twemoji emoji font (COLRv0 build of jdecked/twemoji 17.0 by Mozilla's twemoji-colr pipeline via `@sableclient/twemoji-font` 1.1.1; converted to TTF, renamed "Ferrocord Twemoji", hinting removed), graphics © Twitter, Inc, jdecked and other contributors | `assets/fonts/Twemoji.ttf` | Graphics: CC-BY 4.0; font build code: Apache-2.0 | [`assets/fonts/Twemoji-LICENSE.txt`](assets/fonts/Twemoji-LICENSE.txt) |
| Lucide icons (45 SVGs, unmodified except for the removed licence comment), © Lucide Icons and Contributors, parts derived from Feather (© Cole Bemis) | `assets/icons/` | ISC (Feather parts: MIT) | [`assets/icons/LICENSE`](assets/icons/LICENSE) |

`assets/icons/reply-spine.svg` is an original drawing (MIT, like the project).
The `emojis` crate embeds Unicode emoji names and shortcodes (gemoji, MIT).

Video playback (the `ferrocord-player` helper only; the app links none of it):

| Component | Licence | Notes |
|---|---|---|
| OpenH264 (Cisco), via `openh264-sys2` | BSD-2-Clause | H.264 decoder built from source and linked statically; the licence text ships in `licenses/OpenH264-BSD-2-Clause.txt`. Built from source by the user, no Cisco binary is distributed, so the AVC patent licence Cisco grants to its own binaries does not apply: see below. |
| `rusty_vp9` | Apache-2.0 | Pure-Rust VP9 decoder. |
| `vp8-decoder` | (MIT OR Apache-2.0) AND BSD-3-Clause | Pure-Rust VP8 decoder (RFC 6386 reference code derived parts). |
| `matroska-demuxer` | Zlib OR MIT OR Apache-2.0 | WebM / Matroska demuxer. |
| `symphonia` (aac, isomp4) | MPL-2.0 | AAC decoder and MP4 audio demuxer (the video track is read by the helper's own MP4 reader). Symphonia is MPL-2.0: file-level copyleft, unmodified here. |
| libopus, via `audiopus_sys` | BSD-3-Clause | Statically linked into the helper. |
| `cpal` | Apache-2.0 | Sound output (links ALSA on Linux). |
| `libloading` | ISC | Opens the system FFmpeg (Linux) and `mfplat.dll` (Windows) at run time. |
| `windows` | MIT OR Apache-2.0 | Media Foundation COM bindings (Windows only). |

Platform decoders are **not bundled**. When the system has them, the helper
uses the system FFmpeg libraries on Linux (`libavcodec`/`libavutil`, LGPL or
GPL depending on the distribution's build, loaded with `dlopen` at run time,
never linked) and Media Foundation on Windows (an OS component; H.265 and AV1
come from Microsoft Store extensions the user installs). Ferrocord ships no
FFmpeg code, no H.265 or AV1 decoder and no licence-encumbered codec binary.

Patents: H.264 and AAC are patent-encumbered formats. Decoding them in
software is what every open-source player does; Ferrocord ships source and a
binary built from it, with no separate licence. H.265 and AV1 are decoded
only by what the operating system provides (see above), as browsers do. If this matters for your use,
the helper can be left out (`--no-default-features`) and the app then offers
"Open externally" for videos.

The iced GUI crates are built from [ferroiced](https://github.com/Kartudev/ferroiced),
a fork of [iced](https://github.com/iced-rs/iced) 0.14.1 (MIT, © Héctor Ramón
and Iced contributors) with one change in `iced_tiny_skia`.

Rust crate dependencies keep their own licences (see `Cargo.lock`; all are
MIT, Apache-2.0, ISC, BSD, Zlib or similar permissive licences).
Noto Color Emoji is used from the system if installed and is not bundled.

Plugins (`src/plugins/`): the WebAssembly interpreter `wasmi` (MIT OR
Apache-2.0), MessagePack via `rmp-serde` (MIT), manifests via `toml` (MIT OR
Apache-2.0), `.ferroplug` packages via `zip` (MIT). Plugins themselves are
third-party code that is not part of Ferrocord and not covered by its
licence; each plugin's licence is its author's.

Twemoji is the emoji art the official Discord client uses too; it is a
CC-BY project of Twitter and jdecked, not a Discord asset. No system emoji
font is used.
