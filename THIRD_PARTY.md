# Third-party assets

Ferrocord bundles the following third-party assets. No Discord logos, icons,
fonts or artwork are included.

| Asset | Location | Licence | Licence text |
|---|---|---|---|
| Inter font (Regular, Medium, SemiBold, Bold), © The Inter Project Authors | `assets/fonts/` | SIL Open Font License 1.1 | [`assets/fonts/OFL.txt`](assets/fonts/OFL.txt) |
| Lucide icons (45 SVGs, unmodified except for the removed licence comment), © Lucide Icons and Contributors, parts derived from Feather (© Cole Bemis) | `assets/icons/` | ISC (Feather parts: MIT) | [`assets/icons/LICENSE`](assets/icons/LICENSE) |

`assets/icons/reply-spine.svg` is an original drawing (MIT, like the project).
The `emojis` crate embeds Unicode emoji names and shortcodes (gemoji, MIT).

Rust crate dependencies keep their own licences (see `Cargo.lock`; all are
MIT, Apache-2.0, ISC, BSD, Zlib or similar permissive licences).
Noto Color Emoji is used from the system if installed and is not bundled.
