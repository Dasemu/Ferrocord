//! Emoji picker data: Unicode emoji by category (from the `emojis` crate)
//! plus the current server's custom emoji. Pure functions, unit-tested.

use emojis::Group;

use crate::net::model::*;

/// Unicode categories in the order of the official picker.
pub const CATEGORIES: [(Group, &str); 9] = [
    (Group::SmileysAndEmotion, "People"),
    (Group::PeopleAndBody, "Gestures"),
    (Group::AnimalsAndNature, "Nature"),
    (Group::FoodAndDrink, "Food"),
    (Group::Activities, "Activities"),
    (Group::TravelAndPlaces, "Travel"),
    (Group::Objects, "Objects"),
    (Group::Symbols, "Symbols"),
    (Group::Flags, "Flags"),
];

/// Most results shown for a search.
pub const MAX_RESULTS: usize = 120;

/// Where a picked emoji goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerTarget {
    /// Insert into the composer.
    Composer,
    /// React to a message.
    Reaction {
        channel: ChannelId,
        message: MessageId,
    },
}

/// Which list the picker shows when the search box is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Custom emoji of this server.
    Server(GuildId),
    /// Index into [`CATEGORIES`].
    Unicode(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmojiPicker {
    pub target: PickerTarget,
    pub query: String,
    pub section: Section,
}

impl EmojiPicker {
    /// Opens on the server's custom emoji when it has any, else people.
    pub fn open(target: PickerTarget, guild: Option<&Guild>) -> Self {
        let section = match guild {
            Some(g) if g.emojis.iter().any(|e| e.available) => Section::Server(g.id),
            _ => Section::Unicode(0),
        };
        Self {
            target,
            query: String::new(),
            section,
        }
    }
}

/// Unicode emoji of a category, without skin-tone variants.
pub fn category(index: usize) -> Vec<&'static emojis::Emoji> {
    let Some((g, _)) = CATEGORIES.get(index) else {
        return vec![];
    };
    g.emojis()
        .filter(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
        .collect()
}

/// Case-insensitive search over names and shortcodes. Prefix matches rank
/// first. Variants with a skin tone are skipped.
pub fn search(query: &str) -> Vec<&'static emojis::Emoji> {
    let q = query.trim().trim_matches(':').to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut starts = Vec::new();
    let mut contains = Vec::new();
    for e in emojis::iter() {
        if e.skin_tone()
            .is_some_and(|t| t != emojis::SkinTone::Default)
        {
            continue;
        }
        let name = e.name().to_lowercase();
        let codes: Vec<&str> = e.shortcodes().collect();
        if name.starts_with(&q) || codes.iter().any(|c| c.starts_with(&q)) {
            starts.push(e);
        } else if name.contains(&q) || codes.iter().any(|c| c.contains(&q)) {
            contains.push(e);
        }
        if starts.len() >= MAX_RESULTS {
            break;
        }
    }
    starts.extend(contains);
    starts.truncate(MAX_RESULTS);
    starts
}

/// Custom emoji matching `query` (all of them when empty).
pub fn custom_matching<'a>(guild: &'a Guild, query: &str) -> Vec<&'a CustomEmoji> {
    let q = query.trim().trim_matches(':').to_lowercase();
    guild
        .emojis
        .iter()
        .filter(|e| e.available && (q.is_empty() || e.name.to_lowercase().contains(&q)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guild_with(emojis: Vec<CustomEmoji>) -> Guild {
        Guild {
            id: Snowflake(1),
            name: "g".into(),
            icon: None,
            roles: vec![],
            owner_id: None,
            emojis,
            premium_tier: 0,
        }
    }

    #[test]
    fn categories_have_emoji_and_no_skin_variants() {
        for i in 0..CATEGORIES.len() {
            let v = category(i);
            assert!(!v.is_empty(), "category {i}");
            assert!(
                v.iter()
                    .all(|e| e.skin_tone().is_none_or(|t| t == emojis::SkinTone::Default))
            );
        }
        assert!(category(99).is_empty());
    }

    #[test]
    fn search_finds_by_name_and_shortcode() {
        let r = search("thumbs up");
        assert!(r.iter().any(|e| e.as_str() == "👍"));
        let r = search(":crab:");
        assert!(r.iter().any(|e| e.as_str() == "🦀"));
        assert!(search("   ").is_empty());
        assert!(search("zzzzqqqq").is_empty());
        assert!(search("a").len() <= MAX_RESULTS);
    }

    #[test]
    fn prefix_matches_rank_first() {
        let r = search("grin");
        assert!(r[0].name().to_lowercase().starts_with("grin"));
    }

    #[test]
    fn picker_opens_on_server_emoji_when_available() {
        let g = guild_with(vec![CustomEmoji {
            id: Snowflake(5),
            name: "blob".into(),
            animated: false,
            available: true,
        }]);
        let p = EmojiPicker::open(PickerTarget::Composer, Some(&g));
        assert_eq!(p.section, Section::Server(Snowflake(1)));
        let p = EmojiPicker::open(PickerTarget::Composer, Some(&guild_with(vec![])));
        assert_eq!(p.section, Section::Unicode(0));
        let p = EmojiPicker::open(PickerTarget::Composer, None);
        assert_eq!(p.section, Section::Unicode(0));
    }

    #[test]
    fn custom_search_skips_unavailable() {
        let g = guild_with(vec![
            CustomEmoji {
                id: Snowflake(5),
                name: "Party_Blob".into(),
                animated: true,
                available: true,
            },
            CustomEmoji {
                id: Snowflake(6),
                name: "party_gone".into(),
                animated: false,
                available: false,
            },
        ]);
        assert_eq!(custom_matching(&g, "party").len(), 1);
        assert_eq!(custom_matching(&g, "").len(), 1);
        assert!(custom_matching(&g, "nope").is_empty());
    }
}
