//! Favourite GIFs in the "frecency" user settings (`GET`/`PATCH
//! /users/@me/settings-proto/2`), the protobuf the official client keeps
//! them in.
//!
//! Field numbers follow `discord_protos.discord_users.v1.FrecencyUserSettings`
//! (the public discord-protos definitions, checked against the
//! `FrecencyUserSettings.proto` of discord-protos 1.2.428):
//!
//! ```text
//! FrecencyUserSettings { Versions versions = 1; FavoriteGIFs favorite_gifs = 2; ... }
//! FavoriteGIFs { map<string, FavoriteGIF> gifs = 1; bool hide_tooltip = 2; }
//! FavoriteGIF  { GIFType format = 1; string src = 2; uint32 width = 3;
//!                uint32 height = 4; uint32 order = 5; }
//! GIFType      { NONE = 0; IMAGE = 1; VIDEO = 2; }
//! ```
//!
//! Only the favourites are decoded. Writes are read-modify-write of the
//! whole message: every other field (versions, sticker and emoji frecency,
//! fields this code does not know, unknown fields inside kept favourites)
//! is copied through byte for byte.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("malformed settings protobuf")]
pub struct ProtoError;

const WT_VARINT: u8 = 0;
const WT_I64: u8 = 1;
const WT_LEN: u8 = 2;
const WT_I32: u8 = 5;

const SETTINGS_FAVORITE_GIFS: u32 = 2;
const FAVS_GIFS: u32 = 1;
const ENTRY_KEY: u32 = 1;
const ENTRY_VALUE: u32 = 2;
const GIF_FORMAT: u32 = 1;
const GIF_SRC: u32 = 2;
const GIF_WIDTH: u32 = 3;
const GIF_HEIGHT: u32 = 4;
const GIF_ORDER: u32 = 5;

/// `FrecencyUserSettings.GIFType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifFormat {
    None,
    Image,
    Video,
}

impl GifFormat {
    fn from_wire(v: u64) -> Self {
        match v {
            1 => Self::Image,
            2 => Self::Video,
            _ => Self::None,
        }
    }

    fn wire(self) -> u64 {
        match self {
            Self::None => 0,
            Self::Image => 1,
            Self::Video => 2,
        }
    }
}

/// One saved GIF: `url` is the map key (the Tenor page URL that gets sent),
/// `src` the media file (an MP4 for `Video`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteGif {
    pub url: String,
    pub src: String,
    pub format: GifFormat,
    pub width: u32,
    pub height: u32,
    pub order: u32,
}

/// One field as it was on the wire: its number, wire type and the raw
/// bytes of the whole field (tag included), so it can be written back
/// unchanged.
#[derive(Debug, Clone)]
struct RawField<'a> {
    number: u32,
    wire: u8,
    /// Payload: the varint/fixed bytes, or the inside of a length-delimited
    /// field.
    payload: &'a [u8],
    /// The whole field, tag included.
    raw: &'a [u8],
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, ProtoError> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or(ProtoError)?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(ProtoError)
}

fn fields(buf: &[u8]) -> Result<Vec<RawField<'_>>, ProtoError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let start = pos;
        let key = read_varint(buf, &mut pos)?;
        let number = u32::try_from(key >> 3).map_err(|_| ProtoError)?;
        if number == 0 {
            return Err(ProtoError);
        }
        let wire = (key & 7) as u8;
        let payload_start;
        match wire {
            WT_VARINT => {
                payload_start = pos;
                read_varint(buf, &mut pos)?;
            }
            WT_I64 => {
                payload_start = pos;
                pos = pos.checked_add(8).ok_or(ProtoError)?;
            }
            WT_LEN => {
                let len = usize::try_from(read_varint(buf, &mut pos)?).map_err(|_| ProtoError)?;
                payload_start = pos;
                pos = pos.checked_add(len).ok_or(ProtoError)?;
            }
            WT_I32 => {
                payload_start = pos;
                pos = pos.checked_add(4).ok_or(ProtoError)?;
            }
            _ => return Err(ProtoError),
        }
        if pos > buf.len() {
            return Err(ProtoError);
        }
        out.push(RawField {
            number,
            wire,
            payload: &buf[payload_start..pos],
            raw: &buf[start..pos],
        });
    }
    Ok(out)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_tag(out: &mut Vec<u8>, number: u32, wire: u8) {
    put_varint(out, (u64::from(number) << 3) | u64::from(wire));
}

fn put_bytes(out: &mut Vec<u8>, number: u32, bytes: &[u8]) {
    put_tag(out, number, WT_LEN);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn put_uint(out: &mut Vec<u8>, number: u32, v: u64) {
    // proto3: default values are not written.
    if v != 0 {
        put_tag(out, number, WT_VARINT);
        put_varint(out, v);
    }
}

fn varint_of(f: &RawField<'_>) -> Option<u64> {
    (f.wire == WT_VARINT).then(|| read_varint(f.payload, &mut 0).ok())?
}

fn string_of(f: &RawField<'_>) -> Option<String> {
    (f.wire == WT_LEN).then(|| String::from_utf8(f.payload.to_vec()).ok())?
}

/// A map entry of `FavoriteGIFs.gifs`: the key and the raw value message.
fn entry(raw: &[u8]) -> Result<(String, &[u8]), ProtoError> {
    let mut key = String::new();
    let mut value: &[u8] = &[];
    for f in fields(raw)? {
        match f.number {
            ENTRY_KEY => key = string_of(&f).ok_or(ProtoError)?,
            ENTRY_VALUE if f.wire == WT_LEN => value = f.payload,
            _ => {}
        }
    }
    Ok((key, value))
}

fn decode_gif(url: String, value: &[u8]) -> Result<FavoriteGif, ProtoError> {
    let mut g = FavoriteGif {
        url,
        src: String::new(),
        format: GifFormat::None,
        width: 0,
        height: 0,
        order: 0,
    };
    let small = |f: &RawField<'_>| varint_of(f).map(|v| u32::try_from(v).unwrap_or(u32::MAX));
    for f in fields(value)? {
        match f.number {
            GIF_FORMAT => g.format = GifFormat::from_wire(varint_of(&f).unwrap_or(0)),
            GIF_SRC => g.src = string_of(&f).ok_or(ProtoError)?,
            GIF_WIDTH => g.width = small(&f).unwrap_or(0),
            GIF_HEIGHT => g.height = small(&f).unwrap_or(0),
            GIF_ORDER => g.order = small(&f).unwrap_or(0),
            _ => {}
        }
    }
    Ok(g)
}

/// The favourite GIFs in `settings` (raw `FrecencyUserSettings` bytes),
/// newest first (highest `order`), as the official picker lists them.
pub fn favorite_gifs(settings: &[u8]) -> Result<Vec<FavoriteGif>, ProtoError> {
    let mut out = Vec::new();
    for top in fields(settings)? {
        if top.number != SETTINGS_FAVORITE_GIFS || top.wire != WT_LEN {
            continue;
        }
        for f in fields(top.payload)? {
            if f.number == FAVS_GIFS && f.wire == WT_LEN {
                let (url, value) = entry(f.payload)?;
                if url.is_empty() {
                    continue;
                }
                let g = decode_gif(url, value)?;
                // A repeated key: the last one wins (protobuf map rules).
                out.retain(|o: &FavoriteGif| o.url != g.url);
                out.push(g);
            }
        }
    }
    out.sort_by_key(|g| std::cmp::Reverse(g.order));
    Ok(out)
}

fn encode_gif(g: &FavoriteGif) -> Vec<u8> {
    let mut v = Vec::new();
    put_uint(&mut v, GIF_FORMAT, g.format.wire());
    if !g.src.is_empty() {
        put_bytes(&mut v, GIF_SRC, g.src.as_bytes());
    }
    put_uint(&mut v, GIF_WIDTH, u64::from(g.width));
    put_uint(&mut v, GIF_HEIGHT, u64::from(g.height));
    put_uint(&mut v, GIF_ORDER, u64::from(g.order));
    v
}

/// What to change in the favourites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FavoriteChange {
    /// Save a GIF (its `order` is set above every existing one).
    Add(FavoriteGif),
    /// Unsave the GIF with this URL.
    Remove(String),
}

/// Applies `change` to `settings` and returns the new message. Everything
/// but the touched map entry is copied unchanged.
pub fn apply_favorite(settings: &[u8], change: &FavoriteChange) -> Result<Vec<u8>, ProtoError> {
    let target = match change {
        FavoriteChange::Add(g) => g.url.as_str(),
        FavoriteChange::Remove(u) => u.as_str(),
    };
    let top = fields(settings)?;
    let max_order = favorite_gifs(settings)?
        .iter()
        .map(|g| g.order)
        .max()
        .unwrap_or(0);

    // The new FavoriteGIFs message: kept entries and other fields as they
    // were, the target entry dropped and (for Add) re-added at the end.
    let mut favs = Vec::new();
    let mut seen = false;
    for t in top.iter().filter(|t| t.number == SETTINGS_FAVORITE_GIFS) {
        if t.wire != WT_LEN {
            return Err(ProtoError);
        }
        seen = true;
        for f in fields(t.payload)? {
            if f.number == FAVS_GIFS && f.wire == WT_LEN && entry(f.payload)?.0 == target {
                continue;
            }
            favs.extend_from_slice(f.raw);
        }
    }
    if let FavoriteChange::Add(g) = change {
        let g = FavoriteGif {
            order: max_order.saturating_add(1),
            ..g.clone()
        };
        let mut e = Vec::new();
        put_bytes(&mut e, ENTRY_KEY, g.url.as_bytes());
        put_bytes(&mut e, ENTRY_VALUE, &encode_gif(&g));
        put_bytes(&mut favs, FAVS_GIFS, &e);
    }

    let mut out = Vec::with_capacity(settings.len() + 64);
    let mut written = false;
    for t in &top {
        if t.number == SETTINGS_FAVORITE_GIFS {
            // Merged into one field where the first one was.
            if !written {
                put_bytes(&mut out, SETTINGS_FAVORITE_GIFS, &favs);
                written = true;
            }
        } else {
            out.extend_from_slice(t.raw);
        }
    }
    if !seen {
        put_bytes(&mut out, SETTINGS_FAVORITE_GIFS, &favs);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(url: &str, order: u32) -> FavoriteGif {
        FavoriteGif {
            url: url.into(),
            src: format!("{url}.mp4"),
            format: GifFormat::Video,
            width: 220,
            height: 120,
            order,
        }
    }

    /// Settings with versions, one favourite, hide_tooltip, an unknown field
    /// inside the favourite and unknown top-level fields (sticker frecency
    /// and a made-up field 99).
    fn sample() -> Vec<u8> {
        let mut versions = Vec::new();
        put_uint(&mut versions, 1, 7);
        put_uint(&mut versions, 3, 42);
        let mut value = encode_gif(&gif("https://tenor.com/view/a", 3));
        put_bytes(&mut value, 9, b"future"); // unknown inside FavoriteGIF
        let mut e = Vec::new();
        put_bytes(&mut e, ENTRY_KEY, b"https://tenor.com/view/a");
        put_bytes(&mut e, ENTRY_VALUE, &value);
        let mut favs = Vec::new();
        put_bytes(&mut favs, FAVS_GIFS, &e);
        put_uint(&mut favs, 2, 1); // hide_tooltip
        let mut s = Vec::new();
        put_bytes(&mut s, 1, &versions);
        put_bytes(&mut s, SETTINGS_FAVORITE_GIFS, &favs);
        put_bytes(&mut s, 4, b"\x0a\x03abc");
        put_tag(&mut s, 99, WT_I32);
        s.extend_from_slice(&[1, 2, 3, 4]);
        s
    }

    #[test]
    fn decodes_favourites() {
        let favs = favorite_gifs(&sample()).unwrap();
        assert_eq!(favs, vec![gif("https://tenor.com/view/a", 3)]);
        assert!(favorite_gifs(&[]).unwrap().is_empty());
    }

    #[test]
    fn add_puts_the_new_gif_first_and_keeps_everything_else() {
        let before = sample();
        let after = apply_favorite(
            &before,
            &FavoriteChange::Add(gif("https://tenor.com/view/b", 0)),
        )
        .unwrap();
        let favs = favorite_gifs(&after).unwrap();
        assert_eq!(favs.len(), 2);
        assert_eq!(favs[0].url, "https://tenor.com/view/b");
        assert_eq!(favs[0].order, 4);
        assert_eq!(favs[1], gif("https://tenor.com/view/a", 3));
        // Every field other than favourites is byte-identical and in order.
        let other = |b: &[u8]| -> Vec<Vec<u8>> {
            fields(b)
                .unwrap()
                .into_iter()
                .filter(|f| f.number != SETTINGS_FAVORITE_GIFS)
                .map(|f| f.raw.to_vec())
                .collect()
        };
        assert_eq!(other(&before), other(&after));
        // The kept favourite keeps its unknown field; hide_tooltip stays.
        let favs_raw = fields(&after)
            .unwrap()
            .into_iter()
            .find(|f| f.number == SETTINGS_FAVORITE_GIFS)
            .unwrap()
            .payload
            .to_vec();
        assert!(favs_raw.windows(6).any(|w| w == b"future"));
        assert!(
            fields(&favs_raw)
                .unwrap()
                .iter()
                .any(|f| f.number == 2 && varint_of(f) == Some(1))
        );
    }

    #[test]
    fn remove_drops_only_that_entry() {
        let after = apply_favorite(
            &sample(),
            &FavoriteChange::Remove("https://tenor.com/view/a".into()),
        )
        .unwrap();
        assert!(favorite_gifs(&after).unwrap().is_empty());
        assert_eq!(
            fields(&after).unwrap().len(),
            fields(&sample()).unwrap().len()
        );
        // Removing something that is not saved changes nothing.
        let same = apply_favorite(&sample(), &FavoriteChange::Remove("nope".into())).unwrap();
        assert_eq!(
            favorite_gifs(&same).unwrap(),
            favorite_gifs(&sample()).unwrap()
        );
    }

    #[test]
    fn adding_twice_replaces_the_entry() {
        let once = apply_favorite(&[], &FavoriteChange::Add(gif("u", 0))).unwrap();
        let twice = apply_favorite(&once, &FavoriteChange::Add(gif("u", 0))).unwrap();
        let favs = favorite_gifs(&twice).unwrap();
        assert_eq!(favs.len(), 1);
        assert_eq!(favs[0].order, 2);
    }

    #[test]
    fn empty_settings_get_a_favourites_field() {
        let out = apply_favorite(&[], &FavoriteChange::Add(gif("u", 0))).unwrap();
        assert_eq!(favorite_gifs(&out).unwrap()[0].url, "u");
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            &[0x12, 0x05, 0x01][..],
            &[0xff; 11][..],
            &[0x00][..],
            &[0x0b][..],
            &[0x12, 0xff, 0xff, 0xff, 0xff, 0x0f][..],
        ] {
            assert!(favorite_gifs(bad).is_err(), "{bad:?}");
            assert!(apply_favorite(bad, &FavoriteChange::Remove("x".into())).is_err());
        }
    }
}
