//! Animated media inside messages: GIF / animated WebP attachments and embed
//! images, GIF / APNG stickers and animated custom emoji.
//!
//! With "Play GIFs automatically" on (the default, as in the official
//! client) they are decoded by [`super::anim`] and drawn by the
//! self-playing [`crate::ui::anim_image`] widget, which plays them while the
//! window is focused and the image is on screen without any app tick. With
//! it off, emoji keep the hover behaviour of `cosmetics` and the rest shows
//! its first frame.
//!
//! Only the newest [`RECENT_MESSAGES`] of the open conversation are
//! considered, so the decoded frames stay within the cache budget.
//! Attachments and stickers reuse the bytes the image loader already
//! downloaded; emoji fetch their `.gif` once.

use std::sync::Arc;
use std::time::Instant;

use iced::Task;

use super::anim::{self, Anim, Class};
use super::cosmetics::emoji_anim_url;
use super::markdown::{Block, Inline};
use super::message::Done;
use super::{App, Message, preview_key};
use crate::net::model::{self, Attachment, Snowflake, StickerFormat};

/// Messages (newest first) whose media may animate.
pub const RECENT_MESSAGES: usize = 30;

/// The URL's path ends in an extension that can hold an animation.
pub fn looks_animated(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    path.ends_with(".gif") || path.ends_with(".webp")
}

/// A GIF or WebP attachment (may or may not be animated).
pub fn attachment_may_animate(a: &Attachment) -> bool {
    a.is_image()
        && (matches!(a.content_type.as_deref(), Some("image/gif" | "image/webp"))
            || looks_animated(&a.filename))
}

/// Media of `m` that may animate, with the class that bounds its decoding.
/// Content emoji are added by [`App::message_anim_assets`] (they need the
/// parsed markdown).
pub fn message_media(m: &model::Message, cdn: &str, out: &mut Vec<(String, Class)>) {
    for a in m.attachments.iter().filter(|a| attachment_may_animate(a)) {
        out.push((preview_key(a), Class::Media));
    }
    for s in &m.stickers {
        if matches!(s.format, StickerFormat::Gif | StickerFormat::Apng)
            && let Some(url) = s.image_url(cdn)
        {
            out.push((url, Class::Sticker));
        }
    }
    for key in crate::ui::embeds::media_keys(m) {
        if looks_animated(&key) {
            out.push((key, Class::Media));
        }
    }
    for r in &m.reactions {
        if r.emoji.animated
            && let Some(id) = r.emoji.id
        {
            out.push((emoji_anim_url(cdn, id), Class::Emoji));
        }
    }
}

/// Ids of the animated custom emoji in parsed message content.
pub fn animated_emoji(blocks: &[Block], out: &mut Vec<Snowflake>) {
    fn walk(v: &[Inline], out: &mut Vec<Snowflake>) {
        for i in v {
            match i {
                Inline::Emoji {
                    id, animated: true, ..
                } => out.push(*id),
                Inline::Bold(c)
                | Inline::Italic(c)
                | Inline::Underline(c)
                | Inline::Strike(c)
                | Inline::Spoiler(c) => walk(c, out),
                Inline::Link { label, .. } => walk(label, out),
                _ => {}
            }
        }
    }
    for b in blocks {
        match b {
            Block::Paragraph(v) | Block::Heading(_, v) | Block::Subtext(v) => walk(v, out),
            Block::Quote(inner) => animated_emoji(inner, out),
            Block::List(items) => items.iter().for_each(|v| walk(v, out)),
            Block::CodeBlock { .. } => {}
        }
    }
}

impl App {
    /// Message media animate at all ("Play GIFs automatically" on and
    /// "Reduce motion" off).
    pub fn autoplay_media(&self) -> bool {
        self.prefs.autoplay_gifs && !self.prefs.reduce_motion
    }

    /// Message animations are moving now (the window is focused).
    pub fn media_playing(&self) -> bool {
        self.autoplay_media() && self.window_focused
    }

    /// The decoded animation to draw for a message media `key`, if any.
    pub fn media_anim(&self, key: &str) -> Option<&Arc<Anim>> {
        if !self.autoplay_media() {
            return None;
        }
        self.anim.cache.get(key)
    }

    /// The decoded animation of a custom emoji, when it autoplays.
    pub fn emoji_anim(&self, id: Snowflake, animated: bool) -> Option<&Arc<Anim>> {
        if !animated {
            return None;
        }
        self.media_anim(&emoji_anim_url(self.cfg.cdn_url.trim_end_matches('/'), id))
    }

    /// Everything that may animate in the newest messages of the open
    /// conversation (empty when autoplay is off).
    pub(super) fn message_anim_assets(&self) -> Vec<(String, Class)> {
        let mut out = Vec::new();
        if !self.autoplay_media() {
            return out;
        }
        let Some(c) = self.open_channel() else {
            return out;
        };
        let Some(w) = self.chat.messages.get(&c) else {
            return out;
        };
        let cdn = self.cfg.cdn_url.trim_end_matches('/');
        for m in w.messages.iter().rev().take(RECENT_MESSAGES) {
            message_media(m, cdn, &mut out);
            if m.content.contains("<a:") {
                let mut ids = Vec::new();
                animated_emoji(&self.parsed_markdown(m.id, &m.content), &mut ids);
                out.extend(
                    ids.into_iter()
                        .map(|id| (emoji_anim_url(cdn, id), Class::Emoji)),
                );
            }
        }
        out
    }

    /// Mock mode: the mock GIF attachment (first frame as the still image,
    /// all frames as its animation).
    pub(super) fn mock_load_media_anims(&mut self) {
        let frames = crate::mock::mock_anim_frames();
        if let Some((w, h, px, _)) = frames.first().cloned() {
            self.images.insert(
                crate::mock::MOCK_ANIM_GIF.into(),
                iced::widget::image::Handle::from_rgba(w, h, px),
                (w * h * 4) as usize,
            );
        }
        self.anim.cache.insert(
            crate::mock::MOCK_ANIM_GIF.into(),
            Arc::new(anim::from_rgba_frames(frames)),
            Class::Media,
        );
    }

    /// The image loader fetched `key`: when it is a message GIF / sticker
    /// that may animate, decode the same bytes off the UI thread.
    pub(super) fn decode_media_anim(&mut self, key: &str, bytes: &[u8]) -> Task<Message> {
        if anim::sniff(bytes).is_none() {
            return Task::none();
        }
        let Some(class) = self
            .message_anim_assets()
            .into_iter()
            .find(|(k, c)| k == key && matches!(c, Class::Media | Class::Sticker))
            .map(|(_, c)| c)
        else {
            return Task::none();
        };
        if !self.anim.cache.want(key, Instant::now()) {
            return Task::none();
        }
        let key = key.to_owned();
        let bytes = bytes.to_vec();
        Task::perform(
            async move {
                let anim = tokio::task::spawn_blocking(move || {
                    anim::decode(&bytes, &class.limits()).ok().map(Arc::new)
                })
                .await
                .ok()
                .flatten();
                (key, anim)
            },
            move |(key, anim)| Message::Done(Done::Anim { key, class, anim }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_check_ignores_query_and_case() {
        assert!(looks_animated(
            "https://media.discordapp.net/a/b/cat.GIF?width=400&height=300"
        ));
        assert!(looks_animated("https://cdn.discordapp.com/emojis/1.webp"));
        assert!(!looks_animated(
            "https://cdn.discordapp.com/a/b/photo.png?x=.gif"
        ));
        assert!(!looks_animated("https://example.com/clip.mp4"));
    }

    fn attachment(name: &str, ct: Option<&str>) -> Attachment {
        Attachment {
            id: Snowflake(1),
            filename: name.into(),
            size: 0,
            url: String::new(),
            proxy_url: None,
            content_type: ct.map(Into::into),
            width: None,
            height: None,
        }
    }

    #[test]
    fn only_gif_and_webp_attachments_may_animate() {
        assert!(attachment_may_animate(&attachment("a.gif", None)));
        assert!(attachment_may_animate(&attachment("x", Some("image/webp"))));
        assert!(!attachment_may_animate(&attachment(
            "a.png",
            Some("image/png")
        )));
        assert!(!attachment_may_animate(&attachment(
            "a.mp4",
            Some("video/mp4")
        )));
    }
}
