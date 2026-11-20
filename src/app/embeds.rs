//! Pure embed logic: bounds on untrusted text, grouping of multi-image
//! embeds, media sizing, the field grid, video routing and spoiler
//! detection. The view (`ui/embeds.rs`) only draws what `plan` decides, so
//! every rule here is unit-tested without a window.

use std::borrow::Cow;

use crate::net::model::{Embed, EmbedField, EmbedMedia, Message};
use crate::ui::fit_size;

/// Widest / tallest large media (image, video poster) in a card.
pub const MAX_MEDIA_W: u32 = 400;
pub const MAX_MEDIA_H: u32 = 300;
/// Longest side of the small thumbnail on the right of a card.
pub const THUMB_SMALL: u32 = 80;
/// A thumbnail at least this wide is shown large, below the text, for
/// `article` / `link` embeds (the official client's "large thumbnail").
pub const LARGE_THUMB_MIN_W: u32 = 300;
/// Gap between the cells of the image grid.
pub const GRID_GAP: u32 = 4;
/// Total height of the 2-4 image grid.
pub const GRID_H: u32 = 224;
/// At most this many images are shown in a grid.
pub const MAX_GRID_IMAGES: usize = 4;
/// Discord's own limits; embed text is untrusted, so they are enforced here
/// too.
pub const MAX_EMBEDS: usize = 10;
pub const MAX_FIELDS: usize = 25;
pub const MAX_TITLE: usize = 256;
pub const MAX_DESCRIPTION: usize = 4096;
pub const MAX_FIELD_NAME: usize = 256;
pub const MAX_FIELD_VALUE: usize = 1024;
pub const MAX_FOOTER: usize = 2048;
pub const MAX_AUTHOR: usize = 256;

/// `s` cut to at most `max` characters (an ellipsis marks the cut). Never
/// scans more than `max + 1` characters, so a huge field costs nothing.
pub fn clip(s: &str, max: usize) -> Cow<'_, str> {
    match s.char_indices().nth(max) {
        None => Cow::Borrowed(s),
        Some((end, _)) => {
            let keep = s[..end]
                .char_indices()
                .nth(max.saturating_sub(1))
                .map_or(end, |(i, _)| i);
            Cow::Owned(format!("{}…", &s[..keep]))
        }
    }
}

/// Only http(s) links may ever reach the browser.
pub fn web_url(u: &str) -> Option<&str> {
    let t = u.trim();
    let lower = t.get(..8).unwrap_or(t).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")).then_some(t)
}

// -- grouping ---------------------------------------------------------------

/// One card to draw: a lead embed plus images merged in from embeds that
/// share its `url` (Twitter/X sends one embed per photo).
#[derive(Debug, Clone)]
pub struct Group<'a> {
    pub embed: &'a Embed,
    extra: Vec<&'a EmbedMedia>,
}

impl<'a> Group<'a> {
    /// Every image of the group, lead first, at most [`MAX_GRID_IMAGES`].
    pub fn images(&self) -> Vec<&'a EmbedMedia> {
        self.embed
            .image
            .iter()
            .chain(self.extra.iter().copied())
            .take(MAX_GRID_IMAGES)
            .collect()
    }
}

fn is_image_only(e: &Embed) -> bool {
    e.image.is_some()
        && e.title.is_none()
        && e.description.is_none()
        && e.fields.is_empty()
        && e.author_name.is_none()
        && e.footer.is_none()
        && e.video.is_none()
}

/// Merges embeds that share a `url` into one group (the first is the lead).
/// A follower must carry nothing but an image.
pub fn group_embeds(embeds: &[Embed]) -> Vec<Group<'_>> {
    let mut out: Vec<Group<'_>> = Vec::new();
    for e in embeds.iter().take(MAX_EMBEDS) {
        if let Some(url) = e.url.as_deref().filter(|u| !u.is_empty())
            && is_image_only(e)
            && let Some(lead) = out
                .iter_mut()
                .find(|g| g.embed.url.as_deref() == Some(url) && g.embed.video.is_none())
            && lead.images().len() < MAX_GRID_IMAGES
            && let Some(img) = e.image.as_ref()
        {
            lead.extra.push(img);
            continue;
        }
        out.push(Group {
            embed: e,
            extra: Vec::new(),
        });
    }
    out
}

// -- sizing -----------------------------------------------------------------

/// A media slot: what to download and the size it is drawn at, fixed before
/// the download so the history does not jump when it arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBox {
    /// Download URL (`None`: nothing to fetch, only the box).
    pub key: Option<String>,
    pub w: u32,
    pub h: u32,
}

impl MediaBox {
    fn of(m: &EmbedMedia, max_w: u32, max_h: u32, unknown: (u32, u32)) -> Self {
        let (w, h) = match (m.width, m.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
            _ => unknown,
        };
        let (w, h) = fit_size(w, h, max_w, max_h);
        MediaBox {
            key: Some(m.download_url().to_owned()),
            w,
            h,
        }
    }
}

/// How one card is laid out.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    /// Small thumbnail on the right of the text.
    pub thumb: Option<MediaBox>,
    /// Large image / large thumbnail / video poster below the text.
    pub large: Option<MediaBox>,
    /// The large box is a video poster: draw a play button over it.
    pub video: bool,
    /// 2-4 images of one link (Twitter style).
    pub grid: Vec<MediaBox>,
    /// Inline fields per row.
    pub field_cols: usize,
}

/// Videos the card shows as a poster with a play button: not GIFs, which are
/// drawn bare.
pub fn is_video_card(e: &Embed) -> bool {
    e.video.is_some() && e.kind.as_deref() != Some("gifv")
}

fn thumb_is_large(e: &Embed, th: &EmbedMedia) -> bool {
    matches!(e.kind.as_deref(), Some("article" | "link"))
        && th.width.is_some_and(|w| w >= LARGE_THUMB_MIN_W)
}

pub fn plan(g: &Group<'_>) -> Plan {
    let e = g.embed;
    let imgs = g.images();
    let mut p = Plan {
        field_cols: 3,
        ..Plan::default()
    };
    if imgs.len() >= 2 {
        p.grid = imgs
            .iter()
            .map(|m| MediaBox::of(m, MAX_MEDIA_W, MAX_MEDIA_H, (MAX_MEDIA_W, MAX_MEDIA_W)))
            .collect();
        return p;
    }
    if is_video_card(e) {
        let poster = e.thumbnail.as_ref().or(e.image.as_ref());
        let v = e.video.as_ref();
        // The poster's own aspect ratio (the software renderer cannot crop
        // or clip an image), else the video's, else 16:9.
        let (w, h) = match poster
            .and_then(|m| m.width.zip(m.height))
            .or_else(|| v.and_then(|v| v.width.zip(v.height)))
        {
            Some((w, h)) if w > 0 && h > 0 => (w, h),
            _ => (16, 9),
        };
        // Scale up as well: posters of 320x180 still fill the card.
        let (bw, bh) = fit_size(w * 100, h * 100, MAX_MEDIA_W, MAX_MEDIA_H);
        p.large = Some(MediaBox {
            key: poster.map(|m| m.download_url().to_owned()),
            w: bw,
            h: bh,
        });
        p.video = true;
        return p;
    }
    if let Some(img) = e.image.as_ref().or(imgs.first().copied()) {
        p.large = Some(MediaBox::of(
            img,
            MAX_MEDIA_W,
            MAX_MEDIA_H,
            (MAX_MEDIA_W, MAX_MEDIA_H),
        ));
        if let Some(th) = &e.thumbnail {
            p.thumb = Some(MediaBox::of(
                th,
                THUMB_SMALL,
                THUMB_SMALL,
                (THUMB_SMALL, THUMB_SMALL),
            ));
        }
    } else if let Some(th) = &e.thumbnail {
        if thumb_is_large(e, th) {
            p.large = Some(MediaBox::of(
                th,
                MAX_MEDIA_W,
                MAX_MEDIA_H,
                (MAX_MEDIA_W, MAX_MEDIA_H),
            ));
        } else {
            p.thumb = Some(MediaBox::of(
                th,
                THUMB_SMALL,
                THUMB_SMALL,
                (THUMB_SMALL, THUMB_SMALL),
            ));
        }
    }
    if p.thumb.is_some() {
        p.field_cols = 2;
    }
    p
}

/// Cells of the image grid, as columns of `(w, h)`: 2 images side by side, 3
/// as one tall plus two stacked, 4 as 2x2. `total_w` is the grid width.
pub fn grid_columns(n: usize, total_w: u32) -> Vec<Vec<(u32, u32)>> {
    let half = total_w.saturating_sub(GRID_GAP) / 2;
    let cell = (GRID_H - GRID_GAP) / 2;
    match n {
        0 | 1 => Vec::new(),
        2 => vec![vec![(half, GRID_H)], vec![(half, GRID_H)]],
        3 => vec![vec![(half, GRID_H)], vec![(half, cell), (half, cell)]],
        _ => vec![
            vec![(half, cell), (half, cell)],
            vec![(half, cell), (half, cell)],
        ],
    }
}

// -- fields -----------------------------------------------------------------

/// One row of the field grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRow {
    pub idx: Vec<usize>,
    pub inline: bool,
}

/// Discord's grid: a non-inline field has a row to itself, consecutive inline
/// fields share rows of at most `cols` cells (3, or 2 beside a thumbnail).
pub fn field_rows(fields: &[EmbedField], cols: usize) -> Vec<FieldRow> {
    let cols = cols.max(1);
    let mut rows: Vec<FieldRow> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    for (i, f) in fields.iter().enumerate().take(MAX_FIELDS) {
        if !f.inline {
            if !cur.is_empty() {
                rows.push(FieldRow {
                    idx: std::mem::take(&mut cur),
                    inline: true,
                });
            }
            rows.push(FieldRow {
                idx: vec![i],
                inline: false,
            });
            continue;
        }
        if cur.len() == cols {
            rows.push(FieldRow {
                idx: std::mem::take(&mut cur),
                inline: true,
            });
        }
        cur.push(i);
    }
    if !cur.is_empty() {
        rows.push(FieldRow {
            idx: cur,
            inline: true,
        });
    }
    rows
}

// -- video ------------------------------------------------------------------

/// What a click on a video card does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoAction {
    /// A direct file on Discord's media proxy: the inline player (`Ui::PlayVideo`).
    Play(String),
    /// An iframe-only provider (YouTube, Twitch...): the link, via the
    /// confirm-link dialog.
    Browser(String),
    None,
}

fn is_direct_video(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let path = u.path().to_ascii_lowercase();
    [".mp4", ".webm", ".mov", ".m4v"]
        .iter()
        .any(|e| path.ends_with(e))
}

/// Routes a video embed. `allowed` is the media-host allowlist
/// (`RestClient::is_allowed_media_url`): only proxied files play inline.
pub fn video_action(e: &Embed, allowed: &dyn Fn(&str) -> bool) -> VideoAction {
    let Some(v) = &e.video else {
        return VideoAction::None;
    };
    if let Some(p) = v.proxy_url.as_deref()
        && allowed(p)
        && is_direct_video(p)
    {
        return VideoAction::Play(p.to_owned());
    }
    e.url
        .as_deref()
        .and_then(web_url)
        .or_else(|| web_url(&v.url))
        .map_or(VideoAction::None, |u| VideoAction::Browser(u.to_owned()))
}

/// Hook for the animation engine: the looping MP4 behind a `gifv` embed.
/// Today the card shows the still frame (`Embed::bare_media`).
pub fn gifv_source(e: &Embed) -> Option<&str> {
    (e.kind.as_deref() == Some("gifv"))
        .then(|| e.video.as_ref().map(|v| v.download_url()))
        .flatten()
}

// -- suppress and spoilers --------------------------------------------------

/// The embeds to draw: none when the message suppresses them.
pub fn visible_embeds(m: &Message) -> &[Embed] {
    if m.embeds_suppressed() {
        &[]
    } else {
        &m.embeds[..m.embeds.len().min(MAX_EMBEDS)]
    }
}

/// The "X" on a message's embeds: authors and `MANAGE_MESSAGES` holders, for
/// delivered messages that still show embeds.
pub fn can_suppress(m: &Message, mine: bool, can_manage: bool) -> bool {
    !m.pending
        && !m.failed
        && !m.embeds_suppressed()
        && !m.embeds.is_empty()
        && (mine || can_manage)
}

/// Whether the message text wraps `url` in `||spoiler||` marks: its embed
/// media then stays hidden until clicked.
pub fn url_is_spoilered(content: &str, url: &str) -> bool {
    if url.is_empty() || !content.contains("||") {
        return false;
    }
    let mut parts = content.split("||").enumerate();
    // Odd segments are inside a pair; an unclosed last one does not count.
    let total = content.matches("||").count();
    parts.any(|(i, seg)| i % 2 == 1 && i < total && seg.contains(url))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::EmbedVideo;

    fn media(url: &str, w: u32, h: u32) -> EmbedMedia {
        EmbedMedia {
            url: url.into(),
            proxy_url: Some(format!("https://media.discordapp.net/{url}")),
            width: Some(w),
            height: Some(h),
        }
    }

    fn field(inline: bool) -> EmbedField {
        EmbedField {
            name: "n".into(),
            value: "v".into(),
            inline,
        }
    }

    fn lead(url: &str) -> Embed {
        Embed {
            kind: Some("rich".into()),
            url: Some(url.into()),
            title: Some("tweet".into()),
            image: Some(media("a", 1200, 800)),
            ..Default::default()
        }
    }

    fn follower(url: &str, img: &str) -> Embed {
        Embed {
            kind: Some("rich".into()),
            url: Some(url.into()),
            image: Some(media(img, 1200, 800)),
            ..Default::default()
        }
    }

    #[test]
    fn clip_is_char_safe_and_bounded() {
        assert_eq!(clip("hello", 10), "hello");
        assert_eq!(clip("hello", 5), "hello");
        assert_eq!(clip("hello!", 5), "hell…");
        assert_eq!(clip("héé🦀🦀x", 4), "héé…");
        assert_eq!(clip("", 0), "");
        let huge = "é".repeat(5_000_000);
        assert_eq!(clip(&huge, 256).chars().count(), 256);
    }

    #[test]
    fn only_http_links_pass() {
        assert!(web_url("https://example.com/x").is_some());
        assert!(web_url("HTTP://example.com").is_some());
        assert!(web_url("javascript:alert(1)").is_none());
        assert!(web_url("file:///etc/passwd").is_none());
        assert!(web_url("ftp://x").is_none());
        assert!(web_url("").is_none());
        assert!(web_url("é").is_none());
    }

    #[test]
    fn same_url_embeds_merge_into_a_grid() {
        let u = "https://x.com/a/status/1";
        let es = vec![
            lead(u),
            follower(u, "b"),
            follower(u, "c"),
            follower(u, "d"),
            follower(u, "e"),
        ];
        let gs = group_embeds(&es);
        assert_eq!(gs.len(), 2, "the fifth image starts its own card");
        assert_eq!(gs[0].images().len(), 4);
        let p = plan(&gs[0]);
        assert_eq!(p.grid.len(), 4);
        assert!(p.large.is_none() && p.thumb.is_none());
    }

    #[test]
    fn different_urls_or_text_followers_do_not_merge() {
        let es = vec![lead("https://a"), follower("https://b", "b")];
        assert_eq!(group_embeds(&es).len(), 2);
        let mut with_text = follower("https://a", "b");
        with_text.description = Some("more".into());
        let es = vec![lead("https://a"), with_text];
        assert_eq!(group_embeds(&es).len(), 2);
        assert_eq!(group_embeds(&[]).len(), 0);
    }

    #[test]
    fn at_most_ten_embeds() {
        let es: Vec<Embed> = (0..30)
            .map(|i| Embed {
                url: Some(format!("https://e/{i}")),
                ..Default::default()
            })
            .collect();
        assert_eq!(group_embeds(&es).len(), MAX_EMBEDS);
    }

    #[test]
    fn grid_cells_fill_the_box() {
        for n in 2..=4 {
            let cols = grid_columns(n, 400);
            let cells: usize = cols.iter().map(Vec::len).sum();
            assert_eq!(cells, n);
            for col in &cols {
                let h: u32 =
                    col.iter().map(|c| c.1).sum::<u32>() + GRID_GAP * (col.len() as u32 - 1);
                assert_eq!(h, GRID_H);
            }
            let w: u32 = cols.iter().map(|c| c[0].0).sum::<u32>() + GRID_GAP;
            assert!(w <= 400);
        }
        assert!(grid_columns(1, 400).is_empty());
    }

    #[test]
    fn inline_fields_make_rows_of_three() {
        let fs = vec![field(true); 7];
        let rows = field_rows(&fs, 3);
        assert_eq!(
            rows.iter().map(|r| r.idx.len()).collect::<Vec<_>>(),
            vec![3, 3, 1]
        );
        assert!(rows.iter().all(|r| r.inline));
    }

    #[test]
    fn block_fields_break_rows() {
        let fs = vec![
            field(true),
            field(true),
            field(false),
            field(true),
            field(false),
        ];
        let rows = field_rows(&fs, 3);
        let shape: Vec<(Vec<usize>, bool)> = rows.into_iter().map(|r| (r.idx, r.inline)).collect();
        assert_eq!(
            shape,
            vec![
                (vec![0, 1], true),
                (vec![2], false),
                (vec![3], true),
                (vec![4], false)
            ]
        );
    }

    #[test]
    fn two_columns_beside_a_thumbnail() {
        let fs = vec![field(true); 5];
        let rows = field_rows(&fs, 2);
        assert_eq!(
            rows.iter().map(|r| r.idx.len()).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
        let e = Embed {
            kind: Some("rich".into()),
            thumbnail: Some(media("t", 200, 200)),
            fields: fs,
            ..Default::default()
        };
        let g = group_embeds(std::slice::from_ref(&e));
        assert_eq!(plan(&g[0]).field_cols, 2);
        let e2 = Embed {
            thumbnail: None,
            ..e
        };
        let g = group_embeds(std::slice::from_ref(&e2));
        assert_eq!(plan(&g[0]).field_cols, 3);
    }

    #[test]
    fn fields_are_capped() {
        let fs = vec![field(false); 200];
        assert_eq!(field_rows(&fs, 3).len(), MAX_FIELDS);
    }

    #[test]
    fn rich_image_goes_below_and_thumbnail_right() {
        let e = Embed {
            kind: Some("rich".into()),
            image: Some(media("i", 1600, 900)),
            thumbnail: Some(media("t", 128, 128)),
            ..Default::default()
        };
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        let large = p.large.unwrap();
        assert_eq!((large.w, large.h), (400, 225));
        assert_eq!(large.key.as_deref(), Some("https://media.discordapp.net/i"));
        let t = p.thumb.unwrap();
        assert_eq!((t.w, t.h), (80, 80));
    }

    #[test]
    fn small_images_are_not_upscaled_and_unknown_sizes_get_a_fixed_box() {
        let e = Embed {
            kind: Some("rich".into()),
            image: Some(media("i", 100, 50)),
            ..Default::default()
        };
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        let l = p.large.unwrap();
        assert_eq!((l.w, l.h), (100, 50));
        let e = Embed {
            kind: Some("rich".into()),
            image: Some(EmbedMedia {
                url: "https://cdn.discordapp.com/x.png".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let l = plan(&group_embeds(std::slice::from_ref(&e))[0])
            .large
            .unwrap();
        assert_eq!((l.w, l.h), (400, 300));
        assert_eq!(l.key.as_deref(), Some("https://cdn.discordapp.com/x.png"));
    }

    #[test]
    fn link_previews_small_thumbnail_right_large_below() {
        let mut e = Embed {
            kind: Some("link".into()),
            thumbnail: Some(media("t", 96, 96)),
            ..Default::default()
        };
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        assert!(p.thumb.is_some() && p.large.is_none());
        e.kind = Some("article".into());
        e.thumbnail = Some(media("t", 1280, 720));
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        assert!(p.thumb.is_none());
        assert_eq!(
            (p.large.as_ref().unwrap().w, p.large.unwrap().h),
            (400, 225)
        );
        // A rich embed's thumbnail never goes large.
        e.kind = Some("rich".into());
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        assert!(p.thumb.is_some() && p.large.is_none());
    }

    fn youtube(proxy: Option<&str>) -> Embed {
        Embed {
            kind: Some("video".into()),
            url: Some("https://www.youtube.com/watch?v=abc".into()),
            provider_name: Some("YouTube".into()),
            thumbnail: Some(media("yt", 480, 360)),
            video: Some(EmbedVideo {
                url: "https://www.youtube.com/embed/abc".into(),
                proxy_url: proxy.map(str::to_owned),
                width: Some(1280),
                height: Some(720),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn video_card_follows_the_poster_then_the_video_aspect() {
        // 4:3 poster: its own aspect (no cropping available).
        let e = youtube(None);
        let p = plan(&group_embeds(std::slice::from_ref(&e))[0]);
        assert!(p.video);
        let l = p.large.unwrap();
        assert_eq!((l.w, l.h), (400, 300));
        assert_eq!(l.key.as_deref(), Some("https://media.discordapp.net/yt"));
        // 16:9 poster.
        let mut e = youtube(None);
        e.thumbnail = Some(media("yt", 1280, 720));
        let l = plan(&group_embeds(std::slice::from_ref(&e))[0])
            .large
            .unwrap();
        assert_eq!((l.w, l.h), (400, 225));
        // No poster at all: still a box at the final size (video's aspect).
        let mut e = youtube(None);
        e.thumbnail = None;
        let l = plan(&group_embeds(std::slice::from_ref(&e))[0])
            .large
            .unwrap();
        assert_eq!((l.w, l.h, l.key), (400, 225, None));
    }

    #[test]
    fn video_routing() {
        let allow = |u: &str| u.starts_with("https://media.discordapp.net/");
        // YouTube: iframe only, so the page opens in the browser.
        assert_eq!(
            video_action(&youtube(None), &allow),
            VideoAction::Browser("https://www.youtube.com/watch?v=abc".into())
        );
        // A proxied mp4 plays inline.
        let mp4 = "https://media.discordapp.net/external/x/clip.mp4";
        assert_eq!(
            video_action(&youtube(Some(mp4)), &allow),
            VideoAction::Play(mp4.into())
        );
        // A proxy URL that is not a media file does not.
        assert!(matches!(
            video_action(
                &youtube(Some("https://media.discordapp.net/x/page")),
                &allow
            ),
            VideoAction::Browser(_)
        ));
        // A file on a host outside the allowlist never plays.
        assert!(matches!(
            video_action(&youtube(Some("https://evil.example/clip.mp4")), &allow),
            VideoAction::Browser(_)
        ));
        // Non-http page URLs never open.
        let mut e = youtube(None);
        e.url = Some("javascript:alert(1)".into());
        e.video.as_mut().unwrap().url = "file:///x".into();
        assert_eq!(video_action(&e, &allow), VideoAction::None);
        assert_eq!(video_action(&Embed::default(), &allow), VideoAction::None);
    }

    #[test]
    fn gifv_is_bare_with_a_hook_for_the_animation() {
        let mut e = youtube(Some("https://media.discordapp.net/x.mp4"));
        e.kind = Some("gifv".into());
        assert!(!is_video_card(&e));
        assert_eq!(gifv_source(&e), Some("https://media.discordapp.net/x.mp4"));
        assert_eq!(gifv_source(&youtube(None)), None);
    }

    #[test]
    fn suppress_rules() {
        use crate::mock;
        use crate::net::model::Snowflake;
        let mut m = mock::text_message(Snowflake(1), Snowflake(2), mock::user(1), "x");
        assert!(!can_suppress(&m, true, true), "no embeds yet");
        m.embeds = vec![lead("https://a")];
        assert!(can_suppress(&m, true, false));
        assert!(can_suppress(&m, false, true));
        assert!(!can_suppress(&m, false, false));
        m.pending = true;
        assert!(!can_suppress(&m, true, true));
        m.pending = false;
        m.flags = 1 << 2;
        assert!(!can_suppress(&m, true, true));
        assert!(visible_embeds(&m).is_empty());
        m.flags = 0;
        assert_eq!(visible_embeds(&m).len(), 1);
    }

    #[test]
    fn spoiler_detection() {
        let u = "https://a.example/x.png";
        assert!(url_is_spoilered(&format!("look ||{u}||"), u));
        assert!(url_is_spoilered(&format!("||nsfw {u}|| and more"), u));
        assert!(!url_is_spoilered(&format!("look {u}"), u));
        assert!(!url_is_spoilered(&format!("||{u}"), u), "unclosed");
        assert!(!url_is_spoilered(&format!("||other|| {u}"), u));
        assert!(!url_is_spoilered("", u));
        assert!(!url_is_spoilered("||x||", ""));
    }
}
