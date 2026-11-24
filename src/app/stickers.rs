//! Sticker picker: the stickers of the user's servers, the open server's
//! first, filtered by name. Pure functions, unit-tested.

use iced::Point;

use crate::net::model::*;

#[derive(Debug, Clone, PartialEq)]
pub struct StickerPicker {
    /// Where the composer button was clicked (the popup opens above it).
    pub at: Point,
    pub query: String,
    /// A sticker picked that needs Nitro here: the picker shows the lock
    /// footer ("Send as image", or why a Lottie one cannot be sent).
    pub locked: Option<Snowflake>,
}

/// Servers with stickers matching `query` (case-insensitive substring of the
/// name; all when blank), each with its matches. The open server comes
/// first, the rest keep the server list order. Unavailable stickers (lost
/// boost slots) are left out.
pub fn sections<'a>(
    guilds: &'a [Guild],
    current: Option<GuildId>,
    query: &str,
) -> Vec<(&'a Guild, Vec<&'a Sticker>)> {
    let q = query.trim().to_lowercase();
    let mut out: Vec<(&Guild, Vec<&Sticker>)> = guilds
        .iter()
        .map(|g| {
            let hits = g
                .stickers
                .iter()
                .filter(|s| s.available && (q.is_empty() || s.name.to_lowercase().contains(&q)))
                .collect::<Vec<_>>();
            (g, hits)
        })
        .filter(|(_, hits)| !hits.is_empty())
        .collect();
    // Stable: only moves the open server to the front.
    out.sort_by_key(|(g, _)| Some(g.id) != current);
    out
}

/// Finds a sticker of any server by id.
pub fn find(guilds: &[Guild], id: Snowflake) -> Option<&Sticker> {
    find_with_guild(guilds, id).map(|(_, s)| s)
}

/// Finds a sticker of any server by id, with its server.
pub fn find_with_guild(guilds: &[Guild], id: Snowflake) -> Option<(&Guild, &Sticker)> {
    guilds
        .iter()
        .find_map(|g| g.stickers.iter().find(|s| s.id == id).map(|s| (g, s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;

    fn sticker(id: u64, name: &str) -> Sticker {
        Sticker {
            id: Snowflake(id),
            name: name.into(),
            format: StickerFormat::Png,
            available: true,
        }
    }

    fn guilds() -> Vec<Guild> {
        let mut a = mock::ready().guilds[0].clone();
        a.id = Snowflake(1);
        a.stickers = vec![sticker(10, "Wave"), sticker(11, "Party")];
        let mut b = a.clone();
        b.id = Snowflake(2);
        b.stickers = vec![sticker(20, "Cat wave"), {
            let mut s = sticker(21, "Gone");
            s.available = false;
            s
        }];
        let mut c = a.clone();
        c.id = Snowflake(3);
        c.stickers = vec![];
        vec![a, b, c]
    }

    fn ids(s: &[(&Guild, Vec<&Sticker>)]) -> Vec<(u64, Vec<u64>)> {
        s.iter()
            .map(|(g, st)| (g.id.0, st.iter().map(|s| s.id.0).collect()))
            .collect()
    }

    #[test]
    fn open_server_first_and_unavailable_hidden() {
        let g = guilds();
        assert_eq!(
            ids(&sections(&g, Some(Snowflake(2)), "")),
            vec![(2, vec![20]), (1, vec![10, 11])]
        );
        assert_eq!(
            ids(&sections(&g, None, "")),
            vec![(1, vec![10, 11]), (2, vec![20])]
        );
    }

    #[test]
    fn search_filters_by_name() {
        let g = guilds();
        assert_eq!(
            ids(&sections(&g, None, "WAVE ")),
            vec![(1, vec![10]), (2, vec![20])]
        );
        assert!(sections(&g, None, "nothing").is_empty());
        assert_eq!(
            find(&g, Snowflake(20)).map(|s| s.name.as_str()),
            Some("Cat wave")
        );
    }

    #[test]
    fn image_urls_by_format() {
        let mut s = sticker(5, "x");
        assert_eq!(
            s.image_url("https://cdn").as_deref(),
            Some("https://cdn/stickers/5.png?size=160")
        );
        s.format = StickerFormat::Gif;
        assert!(
            s.image_url("https://cdn")
                .unwrap()
                .ends_with("5.gif?size=160")
        );
        s.format = StickerFormat::Lottie;
        assert_eq!(s.image_url("https://cdn"), None);
    }
}
