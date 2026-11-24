//! Custom emoji and stickers without Nitro.
//!
//! Discord lets a user without Nitro use a server's custom emoji only in
//! that server, never animated ones, and a server's stickers only in that
//! server. The official client shows those greyed out and opens the Nitro
//! upsell when one is picked. Ferrocord shows the same lock, and offers one
//! explicit, visible alternative: "Send as image", which uploads a small
//! scaled copy of the CDN image as an ordinary attachment.
//!
//! Everything here is pure (decisions, URLs, scaling and encoding) and
//! unit-tested; the app runs the scaling in `spawn_blocking`.

use std::io::Cursor;
use std::time::Duration;

use image::{AnimationDecoder, ImageDecoder};

use crate::app::anim::{Format, sniff};
use crate::net::model::*;

/// Side of an emoji sent as an image (the size of a "jumbo" emoji).
pub const EMOJI_IMAGE_SIZE: u32 = 48;
/// Side of a sticker sent as an image (the size stickers are shown at).
pub const STICKER_IMAGE_SIZE: u32 = 160;
/// Sources larger than this on either side are refused before decoding.
const MAX_SOURCE_DIM: u32 = 2048;
/// Most frames kept from an animated source (stickers are a few seconds).
const MAX_FRAMES: usize = 300;
/// Decoded-frame budget while converting (full-size RGBA frames).
const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;

/// Any Nitro tier (`premium_type` 1 Classic, 2 Nitro, 3 Basic) can use
/// custom emoji everywhere, animated ones included.
pub fn emoji_anywhere(premium_type: u8) -> bool {
    matches!(premium_type, 1..=3)
}

/// Server stickers everywhere: Nitro and Nitro Basic. Nitro Classic never
/// had them.
pub fn stickers_anywhere(premium_type: u8) -> bool {
    matches!(premium_type, 2 | 3)
}

/// What picking a custom emoji or sticker does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usable {
    /// Sent the normal way (`<:name:id>`, `sticker_ids`).
    Yes,
    /// Locked here without Nitro: the picker shows the lock and offers
    /// "Send as image".
    NeedsNitro,
    /// A Lottie sticker locked without Nitro: there is no image to send.
    NeedsNitroLottie,
    /// Nobody can use it (lost boost slot, or limited to roles the user
    /// lacks): not shown, like the official picker.
    Hidden,
}

impl Usable {
    pub fn locked(self) -> bool {
        matches!(self, Self::NeedsNitro | Self::NeedsNitroLottie)
    }
}

/// Whether the user may use custom emoji `e` of server `emoji_guild` in a
/// channel of `here` (`None`: a DM). `my_roles` are the user's roles in
/// `emoji_guild`; `owner` is true when the user owns that server.
pub fn emoji_usable(
    e: &CustomEmoji,
    emoji_guild: GuildId,
    here: Option<GuildId>,
    premium_type: u8,
    my_roles: Option<&[RoleId]>,
    owner: bool,
) -> Usable {
    if !e.available {
        return Usable::Hidden;
    }
    let role_ok = e.roles.is_empty()
        || owner
        || my_roles.is_some_and(|mine| e.roles.iter().any(|r| mine.contains(r)));
    if !role_ok {
        return Usable::Hidden;
    }
    if emoji_anywhere(premium_type) {
        return Usable::Yes;
    }
    if e.animated || here != Some(emoji_guild) {
        Usable::NeedsNitro
    } else {
        Usable::Yes
    }
}

/// Whether the user may send sticker `s` of server `sticker_guild` in a
/// channel of `here` (`None`: a DM).
pub fn sticker_usable(
    s: &Sticker,
    sticker_guild: GuildId,
    here: Option<GuildId>,
    premium_type: u8,
) -> Usable {
    if !s.available {
        return Usable::Hidden;
    }
    if here == Some(sticker_guild) || stickers_anywhere(premium_type) {
        return Usable::Yes;
    }
    if s.format == StickerFormat::Lottie {
        Usable::NeedsNitroLottie
    } else {
        Usable::NeedsNitro
    }
}

/// What the user may use in the open channel: the inputs of
/// [`emoji_usable`] and [`sticker_usable`] taken from the app state.
#[derive(Debug, Clone, Copy)]
pub struct Access<'a> {
    /// Server of the open channel (`None`: a DM or no channel).
    pub here: Option<GuildId>,
    pub premium_type: u8,
    pub my_roles: &'a std::collections::HashMap<GuildId, Vec<RoleId>>,
    pub me: Option<UserId>,
}

impl Access<'_> {
    pub fn emoji(&self, g: &Guild, e: &CustomEmoji) -> Usable {
        let owner = g.owner_id.is_some() && g.owner_id == self.me;
        let roles = self.my_roles.get(&g.id).map(Vec::as_slice);
        emoji_usable(e, g.id, self.here, self.premium_type, roles, owner)
    }

    pub fn sticker(&self, g: &Guild, s: &Sticker) -> Usable {
        sticker_usable(s, g.id, self.here, self.premium_type)
    }
}

/// CDN image of a custom emoji at the size it is sent as an image
/// (animated ones as GIF so they keep moving).
pub fn emoji_image_url(cdn: &str, e: &Emoji) -> Option<String> {
    let id = e.id?;
    let ext = if e.animated { "gif" } else { "png" };
    Some(format!(
        "{}/emojis/{id}.{ext}?size={EMOJI_IMAGE_SIZE}&quality=lossless",
        cdn.trim_end_matches('/')
    ))
}

/// File name of the uploaded copy: the emoji or sticker name, sanitised.
pub fn image_file_name(name: &str, ext: &str) -> String {
    let stem: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .take(48)
        .collect();
    let stem = if stem.is_empty() { "image" } else { &stem };
    format!("{stem}.{ext}")
}

/// A scaled copy ready to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScaledImage {
    pub bytes: Vec<u8>,
    /// `"png"` or `"gif"`.
    pub ext: &'static str,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
}

/// Why an image could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaleError {
    Unsupported,
    TooLarge,
    Encode,
}

impl ScaleError {
    pub fn user_message(&self) -> &'static str {
        match self {
            Self::Unsupported => "That image format could not be read.",
            Self::TooLarge => "That image is too large to convert.",
            Self::Encode => "The image could not be converted.",
        }
    }
}

/// Size that fits `(w, h)` inside a `max` square, keeping the aspect ratio
/// and never enlarging.
pub fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    if w <= max && h <= max {
        return (w.max(1), h.max(1));
    }
    if w >= h {
        let nh = (u64::from(h) * u64::from(max) / u64::from(w.max(1))).max(1);
        (max, nh as u32)
    } else {
        let nw = (u64::from(w) * u64::from(max) / u64::from(h.max(1))).max(1);
        (nw as u32, max)
    }
}

/// Scales an emoji or sticker image to fit a `max` square. Still images
/// become PNG; GIF, APNG and animated WebP stay animated as GIF (a GIF that
/// already fits is passed through untouched).
pub fn scale_for_upload(bytes: &[u8], max: u32) -> Result<ScaledImage, ScaleError> {
    let fmt = sniff(bytes).ok_or(ScaleError::Unsupported)?;
    let too_big = |(w, h): (u32, u32)| w > MAX_SOURCE_DIM || h > MAX_SOURCE_DIM || w == 0 || h == 0;
    let cur = Cursor::new(bytes);
    let frames = match fmt {
        Format::Gif => {
            let d =
                image::codecs::gif::GifDecoder::new(cur).map_err(|_| ScaleError::Unsupported)?;
            let dims = d.dimensions();
            if too_big(dims) {
                return Err(ScaleError::TooLarge);
            }
            let frames = collect(d.into_frames())?;
            if frames.len() > 1 && dims.0 <= max && dims.1 <= max {
                return Ok(ScaledImage {
                    bytes: bytes.to_vec(),
                    ext: "gif",
                    width: dims.0,
                    height: dims.1,
                    frames: frames.len(),
                });
            }
            frames
        }
        Format::Png => {
            let d =
                image::codecs::png::PngDecoder::new(cur).map_err(|_| ScaleError::Unsupported)?;
            if too_big(d.dimensions()) {
                return Err(ScaleError::TooLarge);
            }
            if d.is_apng().unwrap_or(false) {
                let a = d.apng().map_err(|_| ScaleError::Unsupported)?;
                collect(a.into_frames())?
            } else {
                still(bytes)?
            }
        }
        Format::Webp => {
            let d =
                image::codecs::webp::WebPDecoder::new(cur).map_err(|_| ScaleError::Unsupported)?;
            if too_big(d.dimensions()) {
                return Err(ScaleError::TooLarge);
            }
            if d.has_animation() {
                collect(d.into_frames())?
            } else {
                still(bytes)?
            }
        }
    };
    encode(frames, max)
}

fn still(bytes: &[u8]) -> Result<Vec<image::Frame>, ScaleError> {
    let img = image::load_from_memory(bytes).map_err(|_| ScaleError::Unsupported)?;
    Ok(vec![image::Frame::new(img.to_rgba8())])
}

fn collect(frames: image::Frames<'_>) -> Result<Vec<image::Frame>, ScaleError> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for f in frames {
        let f = f.map_err(|_| ScaleError::Unsupported)?;
        bytes += f.buffer().as_raw().len();
        if out.len() >= MAX_FRAMES || bytes > MAX_DECODED_BYTES {
            return Err(ScaleError::TooLarge);
        }
        out.push(f);
    }
    if out.is_empty() {
        return Err(ScaleError::Unsupported);
    }
    Ok(out)
}

fn resize(buf: &image::RgbaImage, max: u32) -> image::RgbaImage {
    let (w, h) = fit(buf.width(), buf.height(), max);
    if (w, h) == buf.dimensions() {
        buf.clone()
    } else {
        image::imageops::resize(buf, w, h, image::imageops::FilterType::Lanczos3)
    }
}

fn encode(frames: Vec<image::Frame>, max: u32) -> Result<ScaledImage, ScaleError> {
    if let [one] = frames.as_slice() {
        let img = resize(one.buffer(), max);
        let (width, height) = img.dimensions();
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)
            .map_err(|_| ScaleError::Encode)?;
        return Ok(ScaledImage {
            bytes: out.into_inner(),
            ext: "png",
            width,
            height,
            frames: 1,
        });
    }
    let n = frames.len();
    let mut size = (0, 0);
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new_with_speed(&mut out, 10);
        enc.set_repeat(image::codecs::gif::Repeat::Infinite)
            .map_err(|_| ScaleError::Encode)?;
        for f in frames {
            let delay = f.delay();
            let img = resize(f.buffer(), max);
            size = img.dimensions();
            // GIF delays are in 1/100 s; zero means "as fast as possible".
            let ms = Duration::from(delay).as_millis().max(20) as u32;
            let delay = image::Delay::from_numer_denom_ms(ms, 1);
            enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay))
                .map_err(|_| ScaleError::Encode)?;
        }
    }
    Ok(ScaledImage {
        bytes: out,
        ext: "gif",
        width: size.0,
        height: size.1,
        frames: n,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HERE: GuildId = Snowflake(1);
    const OTHER: GuildId = Snowflake(2);

    fn emoji(animated: bool) -> CustomEmoji {
        CustomEmoji {
            id: Snowflake(10),
            name: "blob".into(),
            animated,
            available: true,
            roles: vec![],
        }
    }

    fn sticker(format: StickerFormat) -> Sticker {
        Sticker {
            id: Snowflake(20),
            name: "wave".into(),
            format,
            available: true,
        }
    }

    #[test]
    fn emoji_without_nitro() {
        let e = emoji(false);
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 0, None, false),
            Usable::Yes
        );
        assert_eq!(
            emoji_usable(&e, OTHER, Some(HERE), 0, None, false),
            Usable::NeedsNitro
        );
        // DMs count as "another server".
        assert_eq!(
            emoji_usable(&e, HERE, None, 0, None, false),
            Usable::NeedsNitro
        );
        // Animated emoji need Nitro even in their own server.
        assert_eq!(
            emoji_usable(&emoji(true), HERE, Some(HERE), 0, None, false),
            Usable::NeedsNitro
        );
    }

    #[test]
    fn emoji_with_any_nitro_tier() {
        for tier in 1..=3 {
            assert_eq!(
                emoji_usable(&emoji(true), OTHER, None, tier, None, false),
                Usable::Yes,
                "tier {tier}"
            );
        }
        assert!(!emoji_anywhere(0) && !emoji_anywhere(9));
    }

    #[test]
    fn unavailable_and_role_locked_emoji_are_hidden() {
        let mut e = emoji(false);
        e.available = false;
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 2, None, false),
            Usable::Hidden
        );
        let mut e = emoji(false);
        e.roles = vec![Snowflake(7)];
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 2, Some(&[Snowflake(8)]), false),
            Usable::Hidden
        );
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 2, None, false),
            Usable::Hidden
        );
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 0, Some(&[Snowflake(7)]), false),
            Usable::Yes
        );
        assert_eq!(
            emoji_usable(&e, HERE, Some(HERE), 0, None, true),
            Usable::Yes
        );
        // Role-allowed but foreign: still Nitro-locked.
        assert_eq!(
            emoji_usable(&e, HERE, Some(OTHER), 0, Some(&[Snowflake(7)]), false),
            Usable::NeedsNitro
        );
    }

    #[test]
    fn stickers() {
        let s = sticker(StickerFormat::Png);
        assert_eq!(sticker_usable(&s, HERE, Some(HERE), 0), Usable::Yes);
        assert_eq!(sticker_usable(&s, OTHER, Some(HERE), 0), Usable::NeedsNitro);
        assert_eq!(sticker_usable(&s, HERE, None, 0), Usable::NeedsNitro);
        // Nitro Classic has no stickers anywhere; Nitro and Basic do.
        assert_eq!(sticker_usable(&s, OTHER, Some(HERE), 1), Usable::NeedsNitro);
        assert_eq!(sticker_usable(&s, OTHER, Some(HERE), 2), Usable::Yes);
        assert_eq!(sticker_usable(&s, OTHER, None, 3), Usable::Yes);
        let l = sticker(StickerFormat::Lottie);
        assert_eq!(
            sticker_usable(&l, OTHER, Some(HERE), 0),
            Usable::NeedsNitroLottie
        );
        assert_eq!(sticker_usable(&l, HERE, Some(HERE), 0), Usable::Yes);
        let mut gone = sticker(StickerFormat::Gif);
        gone.available = false;
        assert_eq!(sticker_usable(&gone, HERE, Some(HERE), 2), Usable::Hidden);
        assert!(Usable::NeedsNitroLottie.locked() && !Usable::Hidden.locked());
    }

    #[test]
    fn urls_and_names() {
        let e = Emoji::custom(&emoji(true));
        assert_eq!(
            emoji_image_url("https://cdn/", &e).as_deref(),
            Some("https://cdn/emojis/10.gif?size=48&quality=lossless")
        );
        let e = Emoji::custom(&emoji(false));
        assert!(
            emoji_image_url("https://cdn", &e)
                .is_some_and(|u| u.contains("/emojis/10.png?size=48"))
        );
        assert_eq!(emoji_image_url("https://cdn", &Emoji::unicode("x")), None);
        assert_eq!(image_file_name("party_blob", "gif"), "party_blob.gif");
        assert_eq!(image_file_name("../é/ x", "png"), "x.png");
        assert_eq!(image_file_name("日本", "png"), "image.png");
    }

    #[test]
    fn fit_keeps_aspect_and_never_enlarges() {
        assert_eq!(fit(320, 320, 160), (160, 160));
        assert_eq!(fit(400, 100, 160), (160, 40));
        assert_eq!(fit(100, 400, 48), (12, 48));
        assert_eq!(fit(32, 20, 48), (32, 20));
        assert_eq!(fit(5000, 1, 48), (48, 1));
    }

    fn rgba(w: u32, h: u32, shade: u8) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([shade, (x % 256) as u8, (y % 256) as u8, 255])
        })
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        rgba(w, h, 9)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn gif(w: u32, h: u32, frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
            for i in 0..frames {
                enc.encode_frame(image::Frame::from_parts(
                    rgba(w, h, (i * 60) as u8),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(80, 1),
                ))
                .unwrap();
            }
        }
        out
    }

    fn apng(w: u32, h: u32, frames: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            e.set_animated(frames, 0).unwrap();
            e.set_frame_delay(1, 10).unwrap();
            let mut wr = e.write_header().unwrap();
            for i in 0..frames {
                wr.write_image_data(rgba(w, h, (i * 70) as u8).as_raw())
                    .unwrap();
            }
            wr.finish().unwrap();
        }
        out
    }

    fn decode_dims(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).unwrap();
        (img.width(), img.height())
    }

    #[test]
    fn still_images_become_small_pngs() {
        let s = scale_for_upload(&png(128, 64), EMOJI_IMAGE_SIZE).unwrap();
        assert_eq!((s.ext, s.width, s.height, s.frames), ("png", 48, 24, 1));
        assert_eq!(decode_dims(&s.bytes), (48, 24));
        // Already small: kept at its size.
        let s = scale_for_upload(&png(32, 32), EMOJI_IMAGE_SIZE).unwrap();
        assert_eq!((s.width, s.height), (32, 32));
        // A one-frame GIF is a still too.
        let s = scale_for_upload(&gif(96, 96, 1), EMOJI_IMAGE_SIZE).unwrap();
        assert_eq!((s.ext, s.width), ("png", 48));
    }

    #[test]
    fn animated_gif_stays_animated() {
        let s = scale_for_upload(&gif(320, 320, 3), STICKER_IMAGE_SIZE).unwrap();
        assert_eq!((s.ext, s.width, s.height, s.frames), ("gif", 160, 160, 3));
        let d = image::codecs::gif::GifDecoder::new(Cursor::new(&s.bytes)).unwrap();
        assert_eq!(d.dimensions(), (160, 160));
        assert_eq!(d.into_frames().count(), 3);
        // A small GIF is uploaded untouched.
        let small = gif(40, 40, 2);
        let s = scale_for_upload(&small, EMOJI_IMAGE_SIZE).unwrap();
        assert_eq!(s.bytes, small);
    }

    #[test]
    fn apng_becomes_an_animated_gif() {
        let s = scale_for_upload(&apng(320, 160, 4), STICKER_IMAGE_SIZE).unwrap();
        assert_eq!((s.ext, s.width, s.height, s.frames), ("gif", 160, 80, 4));
        let d = image::codecs::gif::GifDecoder::new(Cursor::new(&s.bytes)).unwrap();
        assert_eq!(d.into_frames().count(), 4);
    }

    #[test]
    fn junk_and_huge_sources_are_refused() {
        assert_eq!(
            scale_for_upload(b"<html>", 48),
            Err(ScaleError::Unsupported)
        );
        assert_eq!(
            scale_for_upload(b"GIF89a", 48),
            Err(ScaleError::Unsupported)
        );
        assert_eq!(
            scale_for_upload(&png(2049, 4), 48),
            Err(ScaleError::TooLarge)
        );
        let mut truncated = png(64, 64);
        truncated.truncate(truncated.len() / 2);
        assert!(scale_for_upload(&truncated, 48).is_err());
        assert!(!ScaleError::Encode.user_message().is_empty());
    }
}
