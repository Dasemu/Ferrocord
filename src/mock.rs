//! Mock data used by tests and `--mock` screenshot mode.
//!
//! Every name, server and message here is invented.

use crate::net::model::*;

pub const ME: UserId = Snowflake(1);

pub const GUILD_RUST: GuildId = Snowflake(100);
pub const GUILD_ICED: GuildId = Snowflake(200);
pub const GUILD_LINUX: GuildId = Snowflake(300);
pub const GUILD_GAMES: GuildId = Snowflake(400);
pub const GUILD_CAFE: GuildId = Snowflake(500);

pub const CAT_INFO: ChannelId = Snowflake(110);
pub const CAT_TEXT: ChannelId = Snowflake(111);
pub const CAT_VOICE: ChannelId = Snowflake(112);
pub const CH_WELCOME: ChannelId = Snowflake(120);
pub const CH_ANNOUNCEMENTS: ChannelId = Snowflake(121);
pub const CH_RULES: ChannelId = Snowflake(122);
pub const CH_GENERAL: ChannelId = Snowflake(130);
pub const CH_RANDOM: ChannelId = Snowflake(131);
pub const CH_HELP: ChannelId = Snowflake(132);
pub const CH_SHOWCASE: ChannelId = Snowflake(133);

pub const DM_ALICE: ChannelId = Snowflake(600);
pub const DM_BOB: ChannelId = Snowflake(601);
pub const DM_GROUP: ChannelId = Snowflake(602);
pub const DM_EVE: ChannelId = Snowflake(603);

pub const ROLE_MOD: RoleId = Snowflake(900);
pub const ROLE_MEMBER: RoleId = Snowflake(901);
pub const ROLE_CORE: RoleId = Snowflake(902);
pub const ROLE_BOT: RoleId = Snowflake(903);
pub const MOD_COLOR: u32 = 0x2ecc71;

const NAMES: [(&str, &str); 7] = [
    ("", ""),
    ("ferris_fan", "Ferris Fan"),
    ("alice.dev", "Alice"),
    ("bobthebuilder", "Bob"),
    ("carol_codes", "Carol"),
    ("clippy_bot", "Clippy Bot"),
    ("eve", "Eve"),
];

/// Mock user `n` (1 = the logged-in user).
pub fn user(n: u64) -> User {
    let (u, g) = NAMES[(n as usize).min(NAMES.len() - 1)];
    User {
        id: Snowflake(n),
        username: u.into(),
        global_name: Some(g.into()),
        avatar: None,
        bot: n == 5,
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1_700_000_000_000)
}

/// Id of a message `minutes_ago` minutes before now.
fn ago(minutes: u64, inc: u64) -> MessageId {
    Snowflake::from_timestamp_ms(now_ms() - minutes * 60_000, inc)
}

fn ch(
    id: ChannelId,
    kind: ChannelKind,
    guild: Option<GuildId>,
    name: &str,
    pos: i32,
    parent: Option<ChannelId>,
) -> Channel {
    Channel {
        id,
        kind,
        guild_id: guild,
        name: (!name.is_empty()).then(|| name.to_owned()),
        topic: None,
        position: pos,
        parent_id: parent,
        last_message_id: None,
        recipients: vec![],
        nsfw: false,
    }
}

fn guild(id: GuildId, name: &str, roles: Vec<Role>) -> Guild {
    Guild {
        id,
        name: name.into(),
        icon: None,
        roles,
        owner_id: Some(user(3).id),
    }
}

fn role(id: RoleId, name: &str, color: u32, position: i32) -> Role {
    Role {
        id,
        name: name.into(),
        color,
        position,
    }
}

/// A READY payload with five guilds, categories, DMs and read states.
pub fn ready() -> ReadyData {
    use ChannelKind::*;
    let g = Some(GUILD_RUST);
    let mut channels = vec![
        ch(CAT_INFO, Category, g, "Information", 0, None),
        ch(CAT_TEXT, Category, g, "Text Channels", 1, None),
        ch(CAT_VOICE, Category, g, "Voice Channels", 2, None),
        ch(CH_WELCOME, Text, g, "welcome", 0, Some(CAT_INFO)),
        ch(
            CH_ANNOUNCEMENTS,
            Announcement,
            g,
            "announcements",
            1,
            Some(CAT_INFO),
        ),
        ch(CH_RULES, Text, g, "rules", 2, Some(CAT_INFO)),
        ch(CH_GENERAL, Text, g, "general", 0, Some(CAT_TEXT)),
        ch(CH_RANDOM, Text, g, "random", 1, Some(CAT_TEXT)),
        ch(CH_HELP, Text, g, "help", 2, Some(CAT_TEXT)),
        ch(CH_SHOWCASE, Text, g, "showcase", 3, Some(CAT_TEXT)),
        ch(Snowflake(140), Voice, g, "Lounge", 0, Some(CAT_VOICE)),
        ch(
            Snowflake(141),
            Voice,
            g,
            "Pair Programming",
            1,
            Some(CAT_VOICE),
        ),
    ];
    for (gid, base) in [
        (GUILD_ICED, 210u64),
        (GUILD_LINUX, 310),
        (GUILD_GAMES, 410),
        (GUILD_CAFE, 510),
    ] {
        channels.push(ch(
            Snowflake(base),
            Category,
            Some(gid),
            "Text Channels",
            0,
            None,
        ));
        for (i, n) in ["general", "help", "off-topic"].iter().enumerate() {
            channels.push(ch(
                Snowflake(base + 1 + i as u64),
                Text,
                Some(gid),
                n,
                i as i32,
                Some(Snowflake(base)),
            ));
        }
    }
    channels
        .iter_mut()
        .find(|c| c.id == CH_GENERAL)
        .into_iter()
        .for_each(|c| c.topic = Some("Talk about anything Rust. Be kind, be patient.".into()));
    channels
        .iter_mut()
        .find(|c| c.id == CH_SHOWCASE)
        .into_iter()
        .for_each(|c| c.topic = Some("Show off what you built".into()));

    let mut dm = |id: ChannelId, kind: ChannelKind, name: &str, recipients: Vec<u64>| {
        let mut c = ch(id, kind, None, name, 0, None);
        c.recipients = recipients.into_iter().map(Snowflake).collect();
        channels.push(c);
    };
    dm(DM_ALICE, Dm, "", vec![2]);
    dm(DM_BOB, Dm, "", vec![3]);
    dm(DM_GROUP, GroupDm, "Weekend plans", vec![4, 5, 6]);
    dm(DM_EVE, Dm, "", vec![6]);

    // Last message ids: everything has recent activity.
    let mut read_states = Vec::new();
    for (i, c) in channels.iter_mut().enumerate() {
        if c.kind.is_textual() {
            c.last_message_id = Some(ago(5 + i as u64 * 7, 0));
        }
    }
    // Read everything, then make a few channels unread / mentioned.
    for c in &channels {
        if let Some(last) = c.last_message_id {
            read_states.push((
                c.id,
                ReadState {
                    last_read: Some(last),
                    mention_count: 0,
                },
            ));
        }
    }
    let mut set = |id: ChannelId, unread: bool, mentions: u32| {
        if let Some((_, rs)) = read_states.iter_mut().find(|(c, _)| *c == id) {
            if unread {
                rs.last_read = Some(Snowflake(1));
            }
            rs.mention_count = mentions;
        }
    };
    set(CH_ANNOUNCEMENTS, true, 1);
    set(CH_RANDOM, true, 0);
    set(CH_HELP, true, 2);
    set(Snowflake(211), true, 0); // Iced: unread
    set(Snowflake(311), true, 3); // Linux: 3 mentions
    set(Snowflake(512), true, 0); // Cafe: unread
    set(DM_ALICE, true, 2);
    set(DM_GROUP, true, 1);

    ReadyData {
        me: Some(user(1)),
        session_id: "mock-session".into(),
        resume_gateway_url: None,
        guilds: vec![
            guild(
                GUILD_RUST,
                "Rust Programming",
                vec![
                    role(ROLE_MEMBER, "Member", 0, 1),
                    role(ROLE_MOD, "Moderator", MOD_COLOR, 3),
                    role(ROLE_CORE, "Core Team", 0xe67e22, 4),
                    role(ROLE_BOT, "Bot", 0x9b59b6, 2),
                ],
            ),
            guild(GUILD_ICED, "Iced GUI", vec![]),
            guild(GUILD_LINUX, "Linux Desktop", vec![]),
            guild(GUILD_GAMES, "Game Night", vec![]),
            guild(GUILD_CAFE, "Open Source Café", vec![]),
        ],
        channels,
        users: (2..=6).map(user).collect(),
        read_states,
        guild_order: vec![GUILD_RUST, GUILD_ICED, GUILD_LINUX, GUILD_GAMES, GUILD_CAFE],
        my_roles: vec![(GUILD_RUST, vec![ROLE_MEMBER])],
        muted_guilds: vec![],
        muted_channels: vec![],
    }
}

/// A plain text message.
pub fn text_message(channel: ChannelId, id: MessageId, author: User, content: &str) -> Message {
    Message {
        id,
        channel_id: channel,
        guild_id: None,
        author,
        content: content.into(),
        edited: false,
        attachments: vec![],
        embeds: vec![],
        mentions: vec![],
        mention_roles: vec![],
        mention_everyone: false,
        author_roles: vec![],
        reply_to: None,
        nonce: None,
        kind: 0,
        pending: false,
        failed: false,
    }
}

fn gm(
    channel: ChannelId,
    minutes: u64,
    inc: u64,
    author: u64,
    roles: &[RoleId],
    content: &str,
) -> Message {
    let mut m = text_message(channel, ago(minutes, inc), user(author), content);
    m.guild_id = if channel.0 < 600 {
        Some(GUILD_RUST)
    } else {
        None
    };
    m.author_roles = roles.to_vec();
    m
}

/// Message history for a channel, oldest first.
pub fn history(channel: ChannelId) -> Vec<Message> {
    match channel {
        CH_SHOWCASE => showcase(),
        DM_ALICE => vec![
            gm(
                DM_ALICE,
                300,
                0,
                2,
                &[],
                "hey! did you see the new iced release?",
            ),
            gm(
                DM_ALICE,
                299,
                1,
                2,
                &[],
                "the **rich text** support is so good",
            ),
            gm(
                DM_ALICE,
                240,
                0,
                1,
                &[],
                "yes! I'm rewriting my client with it right now",
            ),
            gm(
                DM_ALICE,
                239,
                1,
                1,
                &[],
                "the Elm architecture makes the gateway events trivial to wire up",
            ),
            gm(
                DM_ALICE,
                30,
                0,
                2,
                &[],
                "nice, can you share a screenshot later? 👀",
            ),
            gm(
                DM_ALICE,
                29,
                1,
                2,
                &[],
                "also ||I already bought the ferris plushie||",
            ),
        ],
        DM_GROUP => vec![
            gm(DM_GROUP, 90, 0, 4, &[], "Board games on Saturday?"),
            gm(DM_GROUP, 85, 0, 6, &[], "I'm in. I'll bring snacks 🍕"),
            gm(
                DM_GROUP,
                60,
                0,
                5,
                &[],
                "Reminder set for **Saturday 18:00**.",
            ),
        ],
        DM_BOB => vec![
            gm(DM_BOB, 2900, 0, 3, &[], "thanks for the review yesterday!"),
            gm(DM_BOB, 2880, 0, 1, &[], "anytime 👍"),
        ],
        DM_EVE => vec![gm(DM_EVE, 4400, 0, 6, &[], "see you at the meetup")],
        _ => general(channel),
    }
}

fn general(channel: ChannelId) -> Vec<Message> {
    let mut v = vec![
        gm(
            channel,
            180,
            0,
            3,
            &[ROLE_MOD],
            "Morning everyone! Reminder: please read <#122> before posting in <#132>.",
        ),
        gm(
            channel,
            62,
            0,
            4,
            &[ROLE_MEMBER],
            "Has anyone tried the new `let` chains in edition 2024?",
        ),
        gm(
            channel,
            61,
            1,
            4,
            &[ROLE_MEMBER],
            "they make nested `if let` *so* much nicer",
        ),
        gm(
            channel,
            58,
            0,
            2,
            &[ROLE_CORE],
            "Yes! Example:\n```rust\nif let Some(user) = cache.get(&id)\n    && let Some(name) = user.global_name.as_deref()\n{\n    println!(\"hello {name}\");\n}\n```",
        ),
        gm(
            channel,
            57,
            1,
            2,
            &[ROLE_CORE],
            "Docs: https://doc.rust-lang.org/edition-guide/",
        ),
        gm(
            channel,
            40,
            0,
            6,
            &[],
            "> they make nested `if let` so much nicer\nagreed, __finally__ ~~no more rightward drift~~ 🎉",
        ),
        gm(
            channel,
            12,
            0,
            5,
            &[ROLE_BOT],
            "**Tip of the day:** run `cargo clippy -- -D warnings` before every push.",
        ),
        gm(
            channel,
            3,
            0,
            3,
            &[ROLE_MOD],
            "<@1> can you take a look at the pinned question in <#132>? ||thanks!|| <:ferris:1001>",
        ),
    ];
    v[7].mentions = vec![ME];
    v.iter_mut().for_each(|m| m.channel_id = channel);
    v
}

fn showcase() -> Vec<Message> {
    let mut a = gm(
        CH_SHOWCASE,
        20,
        0,
        4,
        &[ROLE_MEMBER],
        "Finished my terminal file manager! Release notes: https://example.com/fm/releases/v1.0",
    );
    a.embeds = vec![Embed {
        kind: Some("rich".into()),
        title: Some("fm v1.0 — a tiny terminal file manager".into()),
        description: Some(
            "Fast, keyboard-driven and written in **100% safe Rust**.\nSupports previews, bulk rename and `git` status."
                .into(),
        ),
        url: Some("https://example.com/fm/releases/v1.0".into()),
        color: Some(0xe67e22),
        author_name: Some("example.com".into()),
        author_url: None,
        provider_name: Some("Example Releases".into()),
        footer: Some("Released today".into()),
        fields: vec![
            EmbedField {
                name: "Downloads".into(),
                value: "12,340".into(),
                inline: true,
            },
            EmbedField {
                name: "Stars".into(),
                value: "1.2k".into(),
                inline: true,
            },
            EmbedField {
                name: "License".into(),
                value: "MIT".into(),
                inline: true,
            },
        ],
        image: None,
        thumbnail: None,
    }];
    let mut b = gm(
        CH_SHOWCASE,
        60,
        0,
        2,
        &[ROLE_CORE],
        "Screenshot of the GUI I'm working on:",
    );
    b.attachments = vec![Attachment {
        id: Snowflake(5001),
        filename: "screenshot.png".into(),
        size: 482_133,
        url: "mock://image/1".into(),
        proxy_url: None,
        content_type: Some("image/png".into()),
        width: Some(400),
        height: Some(225),
    }];
    let mut c = gm(
        CH_SHOWCASE,
        59,
        1,
        2,
        &[ROLE_CORE],
        "and the design doc if anyone wants to review",
    );
    c.attachments = vec![Attachment {
        id: Snowflake(5002),
        filename: "design-notes.pdf".into(),
        size: 1_532_211,
        url: "mock://file/2".into(),
        proxy_url: None,
        content_type: Some("application/pdf".into()),
        width: None,
        height: None,
    }];
    let mut d = gm(CH_SHOWCASE, 95, 0, 5, &[ROLE_BOT], "");
    d.embeds = vec![Embed {
        kind: Some("rich".into()),
        title: Some("Weekly build report".into()),
        description: Some("All **42** crates built successfully. ✅".into()),
        url: None,
        color: Some(0x57f287),
        author_name: Some("Clippy Bot".into()),
        author_url: None,
        provider_name: None,
        footer: Some("CI • ubuntu-latest".into()),
        fields: vec![
            EmbedField {
                name: "Duration".into(),
                value: "3m 12s".into(),
                inline: true,
            },
            EmbedField {
                name: "Warnings".into(),
                value: "0".into(),
                inline: true,
            },
        ],
        image: None,
        thumbnail: None,
    }];
    let e = gm(
        CH_SHOWCASE,
        5,
        0,
        6,
        &[],
        "that looks amazing <:party:1002> <:ferris:1001>",
    );
    vec![d, b, c, a, e]
}

/// Procedurally generated RGBA image (a stylised landscape) used as the
/// mock attachment preview. Returns (width, height, pixels).
pub fn mock_image(w: u32, h: u32) -> (u32, u32, Vec<u8>) {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let fy = y as f32 / h as f32;
            let fx = x as f32 / w as f32;
            let hill = 0.62 + 0.08 * (fx * 9.0).sin() + 0.05 * (fx * 23.0).cos();
            let (r, g, b) = if fy > hill {
                (40.0 + 30.0 * fx, 110.0 + 50.0 * (1.0 - fy), 80.0)
            } else {
                let sun = ((fx - 0.72).powi(2) + (fy - 0.3).powi(2)).sqrt();
                if sun < 0.08 {
                    (255.0, 214.0, 120.0)
                } else {
                    (88.0 + 120.0 * fy, 101.0 + 60.0 * fy, 242.0 - 60.0 * fy)
                }
            };
            px.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    (w, h, px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_data_is_consistent() {
        let r = ready();
        for c in &r.channels {
            if let Some(g) = c.guild_id {
                assert!(
                    r.guilds.iter().any(|x| x.id == g),
                    "channel {} has unknown guild",
                    c.id
                );
            }
        }
        assert!(!history(CH_GENERAL).is_empty());
        assert!(history(CH_SHOWCASE).iter().any(|m| !m.embeds.is_empty()));
        let (w, h, px) = mock_image(10, 5);
        assert_eq!(px.len(), (w * h * 4) as usize);
    }
}
