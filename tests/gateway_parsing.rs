//! Gateway payload parsing against recorded-shape fixtures.

use ferrocord::app::state::{ChatState, Ctx, Effect};
use ferrocord::net::Event;
use ferrocord::net::model::*;
use ferrocord::net::types::parse_dispatch;

fn fixture(name: &str) -> serde_json::Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn sf(s: &str) -> Snowflake {
    s.parse().unwrap()
}

fn ready() -> ReadyData {
    match parse_dispatch("READY", fixture("ready.json"))
        .unwrap()
        .unwrap()
    {
        Event::Ready(r) => *r,
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn ready_user_account_layout() {
    let r = ready();
    assert_eq!(r.me.as_ref().unwrap().display_name(), "Ferris Fan");
    assert_eq!(r.session_id, "8d6b2f0f2c3e4a61a8f7c3b2a1d0e9f8");
    assert_eq!(
        r.resume_gateway_url.as_deref(),
        Some("wss://gateway-us-east1-b.discord.gg")
    );
    // Unavailable guild skipped; `properties` and legacy layouts both work.
    assert_eq!(r.guilds.len(), 2);
    assert_eq!(r.guilds[0].name, "Rust Programming");
    assert_eq!(r.guilds[0].icon.as_deref(), Some("iconhash"));
    assert_eq!(r.guilds[1].name, "Legacy Layout Guild");
    // Channels get their guild id even though the wire omits it.
    let general = r
        .channels
        .iter()
        .find(|c| c.id == sf("4000000000000000002"))
        .unwrap();
    assert_eq!(general.guild_id, Some(sf("2000000000000000001")));
    assert_eq!(general.kind, ChannelKind::Text);
    assert_eq!(general.parent_id, Some(sf("4000000000000000001")));
    let news = r
        .channels
        .iter()
        .find(|c| c.id == sf("4000000000000000010"))
        .unwrap();
    assert_eq!(news.kind, ChannelKind::Announcement);
}

#[test]
fn ready_private_channels_and_users() {
    let r = ready();
    let dm = r
        .channels
        .iter()
        .find(|c| c.id == sf("6000000000000000001"))
        .unwrap();
    assert_eq!(dm.kind, ChannelKind::Dm);
    assert_eq!(dm.recipients, vec![sf("1000000000000000002")]);
    let group = r
        .channels
        .iter()
        .find(|c| c.id == sf("6000000000000000002"))
        .unwrap();
    assert_eq!(group.kind, ChannelKind::GroupDm);
    assert_eq!(group.last_message_id, None);
    // Users from `users` and from embedded recipients are both collected.
    assert!(r.users.iter().any(|u| u.username == "alice.dev"));
    assert!(r.users.iter().any(|u| u.username == "carol_codes"));
}

#[test]
fn ready_read_states_settings_and_roles() {
    let r = ready();
    // Non-channel read states (type 1) are ignored; `0` ids mean "never".
    assert_eq!(r.read_states.len(), 2);
    let general = r
        .read_states
        .iter()
        .find(|(c, _)| *c == sf("4000000000000000002"))
        .unwrap()
        .1;
    assert_eq!(general.last_read, Some(sf("5000000000000000005")));
    assert_eq!(general.mention_count, 2);
    let dm = r
        .read_states
        .iter()
        .find(|(c, _)| *c == sf("6000000000000000001"))
        .unwrap()
        .1;
    assert_eq!(dm.last_read, None);

    assert_eq!(r.muted_guilds, vec![sf("2000000000000000002")]);
    assert_eq!(r.muted_channels, vec![sf("4000000000000000003")]);
    assert_eq!(
        r.guild_order,
        vec![
            sf("2000000000000000002"),
            sf("2000000000000000001"),
            sf("2000000000000000003")
        ]
    );
    assert_eq!(
        r.my_roles[0],
        (sf("2000000000000000001"), vec![sf("3000000000000000001")])
    );
}

#[test]
fn message_create_full() {
    let Event::MessageCreate(m) = parse_dispatch("MESSAGE_CREATE", fixture("message_create.json"))
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(m.author.display_name(), "Alice");
    assert_eq!(m.kind, 19);
    assert_eq!(m.reply_to, Some(sf("5000000000000000009")));
    assert_eq!(m.mentions, vec![sf("1000000000000000001")]);
    assert_eq!(m.author_roles, vec![sf("3000000000000000001")]);
    assert_eq!(m.nonce.as_deref(), Some("1234567890123"));
    assert!(!m.edited);
    let a = &m.attachments[0];
    assert!(a.is_image());
    assert_eq!((a.width, a.height), (Some(800), Some(600)));
    let e = &m.embeds[0];
    assert_eq!(e.color, Some(0xff0000));
    assert_eq!(e.author_name.as_deref(), Some("Author"));
    assert_eq!(e.footer.as_deref(), Some("Footer"));
    assert_eq!(e.fields.len(), 1);
    // Proxy URLs are preferred for media.
    assert!(
        e.thumbnail
            .as_ref()
            .unwrap()
            .url
            .contains("images-ext-1.discordapp.net")
    );
    // Debug output never contains the body.
    assert!(!format!("{m:?}").contains("look at"));
}

#[test]
fn message_update_is_partial() {
    let Event::MessageUpdate(p) = parse_dispatch("MESSAGE_UPDATE", fixture("message_update.json"))
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(p.content.as_deref(), Some("edited text"));
    assert!(p.edited);
    assert!(p.embeds.is_none());

    let Event::MessageUpdate(p) =
        parse_dispatch("MESSAGE_UPDATE", fixture("message_update_embeds_only.json"))
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert!(p.content.is_none());
    assert_eq!(p.embeds, Some(vec![]));
    assert!(!p.edited);
}

#[test]
fn typing_ack_delete_supplemental_guild_channel() {
    let Event::TypingStart { user, user_id, .. } =
        parse_dispatch("TYPING_START", fixture("typing_start.json"))
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(user_id, sf("1000000000000000002"));
    assert_eq!(user.unwrap().display_name(), "Alice");

    assert!(matches!(
        parse_dispatch("MESSAGE_ACK", fixture("message_ack.json"))
            .unwrap()
            .unwrap(),
        Event::MessageAck {
            mention_count: 0,
            ..
        }
    ));
    assert!(matches!(
        parse_dispatch("MESSAGE_DELETE", fixture("message_delete.json"))
            .unwrap()
            .unwrap(),
        Event::MessageDelete { .. }
    ));
    let Event::MemberRoles(roles) =
        parse_dispatch("READY_SUPPLEMENTAL", fixture("ready_supplemental.json"))
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(roles[0].1, sf("1000000000000000002"));

    let Event::GuildUpsert {
        guild,
        channels,
        partial,
    } = parse_dispatch("GUILD_CREATE", fixture("guild_create.json"))
        .unwrap()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(guild.name, "New Guild");
    assert_eq!(channels[0].guild_id, Some(guild.id));
    assert!(!partial);

    let Event::ChannelUpsert(c, users) =
        parse_dispatch("CHANNEL_CREATE", fixture("channel_create_dm.json"))
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert!(c.kind.is_private());
    assert_eq!(users[0].username, "dave");
}

#[test]
fn malformed_known_event_is_an_error_not_a_panic() {
    assert!(parse_dispatch("MESSAGE_CREATE", serde_json::json!({"id": "x"})).is_err());
    assert!(parse_dispatch("READY", serde_json::json!([])).is_err());
}

/// End to end through the reducer: READY then a mention.
#[test]
fn ready_then_mention_updates_unread_state() {
    let mut s = ChatState::default();
    let ctx = Ctx {
        now_ms: 0,
        open_channel: None,
        window_focused: true,
        notification_preview: true,
    };
    s.apply_event(&Event::Ready(Box::new(ready())), &ctx);
    let general = sf("4000000000000000002");
    assert!(s.is_unread(general));
    assert_eq!(s.mention_count(general), 2);
    // Muted voice channel and muted guild do not light up the rail.
    assert!(s.guild_badge(sf("2000000000000000001")).unread);
    assert!(!s.guild_badge(sf("2000000000000000002")).unread);

    let ev = parse_dispatch("MESSAGE_CREATE", fixture("message_create.json"))
        .unwrap()
        .unwrap();
    let fx = s.apply_event(&ev, &ctx);
    assert_eq!(s.mention_count(general), 3);
    assert!(matches!(&fx[0], Effect::Notify(n) if n.title.contains("#general")));
    // Author colour from the member roles in the event.
    let Event::MessageCreate(m) = ev else {
        panic!()
    };
    assert_eq!(s.author_color(m.guild_id, &m), Some(3066993));
}
