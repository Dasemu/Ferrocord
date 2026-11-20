//! Which attachments and embeds are videos the app can offer to play, and
//! where their file and poster live. Pure data: nothing is fetched here.

use crate::net::model::{Attachment, Embed};

/// Containers the in-app player opens (by the name Discord shows; the
/// helper checks the bytes).
const EXTENSIONS: [&str; 5] = [".mp4", ".m4v", ".mov", ".webm", ".mkv"];

/// A video the chat can play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// Stable identity (attachment id, or message + embed index).
    pub id: String,
    /// File to download (Discord CDN or media proxy; checked against the
    /// media allowlist before any request).
    pub url: String,
    /// File name for the "open externally" and error texts.
    pub name: String,
    /// Size in bytes, 0 when unknown.
    pub size: u64,
    /// Picture size when Discord reports it.
    pub width: Option<u32>,
    /// Picture height.
    pub height: Option<u32>,
    /// Poster frame (an image URL), if any.
    pub poster: Option<String>,
}

/// Whether the path part of `url` ends with a video extension.
pub fn has_video_extension(url: &str) -> bool {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    EXTENSIONS.iter().any(|e| path.ends_with(e))
}

/// `base` with Discord's poster-frame query: the media proxy renders the
/// first frame of a video as an image when asked for a still format.
pub fn poster_url(base: &str, width: u32, height: u32) -> String {
    let base = base.split(['?', '#']).next().unwrap_or(base);
    format!("{base}?format=webp&width={width}&height={height}")
}

impl Clip {
    /// A video attachment (by content type or file name).
    pub fn from_attachment(a: &Attachment) -> Option<Clip> {
        let by_type = a
            .content_type
            .as_deref()
            .is_some_and(|c| c.starts_with("video/"));
        if !by_type && !has_video_extension(&a.filename) {
            return None;
        }
        let (w, h) = crate::ui::fit_size(a.width.unwrap_or(400), a.height.unwrap_or(225), 400, 300);
        let poster = a.proxy_url.as_deref().map(|p| {
            if p.starts_with("mock://") {
                p.to_owned()
            } else {
                poster_url(p, w, h)
            }
        });
        Some(Clip {
            id: format!("att:{}", a.id),
            url: a.url.clone(),
            name: a.filename.clone(),
            size: a.size,
            width: a.width,
            height: a.height,
            poster,
        })
    }

    /// An embed whose `video` is a file (not a page of another site, not a
    /// GIF the chat already shows as an image).
    pub fn from_embed(message: u64, index: usize, e: &Embed) -> Option<Clip> {
        if matches!(e.kind.as_deref(), Some("gifv" | "image")) {
            return None;
        }
        let v = e.video.as_ref()?;
        let url = [v.proxy_url.as_deref(), Some(v.url.as_str())]
            .into_iter()
            .flatten()
            .find(|u| has_video_extension(u))?;
        let (w, h) = crate::ui::fit_size(v.width.unwrap_or(400), v.height.unwrap_or(225), 400, 300);
        let poster = e
            .thumbnail
            .as_ref()
            .or(e.image.as_ref())
            .map(|t| t.download_url().to_owned())
            .or_else(|| {
                v.proxy_url
                    .as_deref()
                    .filter(|p| !p.starts_with("mock://"))
                    .map(|p| poster_url(p, w, h))
            });
        let name = url
            .split(['?', '#'])
            .next()
            .and_then(|p| p.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or("video")
            .to_owned();
        Some(Clip {
            id: format!("emb:{message}:{index}"),
            url: url.to_owned(),
            name,
            size: 0,
            width: v.width,
            height: v.height,
            poster,
        })
    }

    /// Size the inline player takes: at most 400 px wide, 300 px tall,
    /// never enlarged (the official client's box).
    pub fn inline_size(&self) -> (u32, u32) {
        crate::ui::fit_size(
            self.width.unwrap_or(400),
            self.height.unwrap_or(225),
            400,
            300,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::{EmbedMedia, EmbedVideo, Snowflake};

    fn att(name: &str, ct: Option<&str>) -> Attachment {
        Attachment {
            id: Snowflake(9),
            filename: name.into(),
            size: 1234,
            url: format!("https://cdn.discordapp.com/attachments/1/2/{name}"),
            proxy_url: Some(format!(
                "https://media.discordapp.net/attachments/1/2/{name}"
            )),
            content_type: ct.map(Into::into),
            width: Some(1280),
            height: Some(720),
        }
    }

    #[test]
    fn video_attachments_are_found_by_type_or_name() {
        assert!(Clip::from_attachment(&att("a.bin", Some("video/mp4"))).is_some());
        assert!(Clip::from_attachment(&att("CLIP.MOV", None)).is_some());
        assert!(Clip::from_attachment(&att("a.webm", Some("application/octet-stream"))).is_some());
        assert!(Clip::from_attachment(&att("a.png", Some("image/png"))).is_none());
        assert!(Clip::from_attachment(&att("notes.txt", None)).is_none());
    }

    #[test]
    fn the_poster_is_the_proxy_still_at_the_inline_size() {
        let c = Clip::from_attachment(&att("a.mp4", Some("video/mp4"))).unwrap();
        assert_eq!(
            c.poster.as_deref(),
            Some(
                "https://media.discordapp.net/attachments/1/2/a.mp4?format=webp&width=400&height=225"
            )
        );
        assert_eq!(c.inline_size(), (400, 225));
        assert_eq!(c.id, "att:9");
        // A query already on the proxy URL is replaced, not doubled.
        assert_eq!(
            poster_url("https://m.example/x.mp4?ex=1&hm=2", 10, 20),
            "https://m.example/x.mp4?format=webp&width=10&height=20"
        );
    }

    #[test]
    fn portrait_and_small_videos_keep_their_shape() {
        let mut a = att("a.mp4", Some("video/mp4"));
        a.width = Some(720);
        a.height = Some(1280);
        assert_eq!(Clip::from_attachment(&a).unwrap().inline_size(), (169, 300));
        a.width = Some(160);
        a.height = Some(90);
        assert_eq!(Clip::from_attachment(&a).unwrap().inline_size(), (160, 90));
    }

    fn embed(kind: &str, video_url: &str, proxy: Option<&str>) -> Embed {
        Embed {
            kind: Some(kind.into()),
            thumbnail: Some(EmbedMedia {
                url: "https://media.discordapp.net/thumb.png".into(),
                proxy_url: None,
                width: None,
                height: None,
            }),
            video: Some(EmbedVideo {
                url: video_url.into(),
                proxy_url: proxy.map(Into::into),
                width: Some(640),
                height: Some(360),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn video_embeds_play_only_when_the_video_is_a_file() {
        let file = embed(
            "video",
            "https://cdn.discordapp.com/attachments/1/2/v.mp4",
            Some("https://media.discordapp.net/attachments/1/2/v.mp4"),
        );
        let c = Clip::from_embed(5, 0, &file).unwrap();
        assert_eq!(c.url, "https://media.discordapp.net/attachments/1/2/v.mp4");
        assert_eq!(c.name, "v.mp4");
        assert_eq!(c.id, "emb:5:0");
        assert_eq!(
            c.poster.as_deref(),
            Some("https://media.discordapp.net/thumb.png")
        );
        // A YouTube player page is a link card, never a clip.
        let yt = embed("video", "https://www.youtube.com/embed/abc", None);
        assert!(Clip::from_embed(5, 0, &yt).is_none());
        // GIFs keep their bare-image rendering.
        let gif = embed("gifv", "https://media.tenor.com/x.mp4", None);
        assert!(Clip::from_embed(5, 0, &gif).is_none());
        assert!(Clip::from_embed(5, 0, &Embed::default()).is_none());
    }

    #[test]
    fn extensions_ignore_the_query() {
        assert!(has_video_extension("https://x/y.MP4?ex=1.png"));
        assert!(!has_video_extension("https://x/y.png?name=a.mp4"));
    }
}
