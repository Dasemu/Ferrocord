//! Embeds: parsing of every embed type, MESSAGE_UPDATE merging, the
//! suppress-embeds REST call and the optimistic suppress action.

mod common;

use std::sync::Arc;

use common::{Response, http};
use ferrocord::app::embeds::{self, VideoAction};
use ferrocord::app::{App, Message, Ui};
use ferrocord::auth::secrets::Secret;
use ferrocord::config::Config;
use ferrocord::mock;
use ferrocord::net::Event;
use ferrocord::net::model::*;
use ferrocord::net::rest::RestClient;
use ferrocord::net::types::parse_dispatch;
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn message() -> ferrocord::net::model::Message {
    let Event::MessageCreate(m) =
        parse_dispatch("MESSAGE_CREATE", fixture("message_create_embeds.json"))
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    *m
}

fn patch(v: Value) -> MessagePatch {
    let Event::MessageUpdate(p) = parse_dispatch("MESSAGE_UPDATE", v).unwrap().unwrap() else {
        panic!()
    };
    *p
}

#[test]
fn rich_embed_fields_are_parsed_with_proxy_urls() {
    let m = message();
    let e = &m.embeds[0];
    assert_eq!(e.kind.as_deref(), Some("rich"));
    assert_eq!(e.author_url.as_deref(), Some("https://example.com/bot"));
    assert_eq!(
        e.author_icon.as_deref(),
        Some("https://images-ext-1.discordapp.net/a.png")
    );
    assert_eq!(
        e.footer_icon.as_deref(),
        Some("https://images-ext-1.discordapp.net/f.png")
    );
    assert_eq!(e.provider_url.as_deref(), Some("https://example.com"));
    assert!(e.timestamp_ms.is_some());
    assert_eq!(
        e.image.as_ref().unwrap().download_url(),
        "https://images-ext-1.discordapp.net/i.png"
    );
    assert_eq!(e.image.as_ref().unwrap().url, "https://example.com/i.png");
    assert_eq!(e.fields.len(), 2);
}

#[test]
fn every_embed_type_parses_and_plans() {
    let m = message();
    let kinds: Vec<_> = m.embeds.iter().map(|e| e.kind.clone().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "rich", "article", "link", "video", "gifv", "image", "rich", "rich", "rich"
        ]
    );
    let groups = embeds::group_embeds(&m.embeds);
    // The three same-URL tweet embeds became one card.
    assert_eq!(groups.len(), 7);
    let plans: Vec<_> = groups.iter().map(embeds::plan).collect();
    // rich: image below and thumbnail on the right, two field columns.
    assert!(plans[0].large.is_some() && plans[0].thumb.is_some());
    assert_eq!(plans[0].field_cols, 2);
    // article with a wide thumbnail: large below.
    assert!(plans[1].large.is_some() && plans[1].thumb.is_none());
    // link with a small thumbnail: right.
    assert!(plans[2].thumb.is_some() && plans[2].large.is_none());
    // YouTube: poster with play button, 16:9.
    assert!(plans[3].video);
    let l = plans[3].large.as_ref().unwrap();
    assert_eq!((l.w, l.h), (400, 225));
    // gifv and image are bare media.
    assert!(groups[4].embed.bare_media().is_some());
    assert_eq!(
        embeds::gifv_source(groups[4].embed),
        Some("https://images-ext-1.discordapp.net/x.mp4")
    );
    assert!(groups[5].embed.bare_media().is_some());
    // Twitter: three photos in a grid.
    assert_eq!(plans[6].grid.len(), 3);
}

#[test]
fn youtube_opens_the_browser_and_proxied_files_play() {
    let m = message();
    let rest = RestClient::new("http://api", "https://cdn.discordapp.com", None).unwrap();
    let allowed = |u: &str| rest.is_allowed_media_url(u);
    assert_eq!(
        embeds::video_action(&m.embeds[3], &allowed),
        VideoAction::Browser("https://www.youtube.com/watch?v=abc".into())
    );
    assert_eq!(
        embeds::video_action(&m.embeds[4], &allowed),
        VideoAction::Play("https://images-ext-1.discordapp.net/x.mp4".into())
    );
}

#[test]
fn old_cached_embeds_without_new_fields_still_load() {
    // The SQLite cache stores `Embed` as JSON from before these fields.
    let e: Embed = serde_json::from_value(json!({
        "kind": "rich", "title": "t", "fields": [],
        "image": { "url": "https://cdn.discordapp.com/x.png", "width": 1, "height": 2 }
    }))
    .unwrap();
    assert_eq!(e.flags, 0);
    assert!(e.video.is_none());
    assert_eq!(e.image.unwrap().proxy_url, None);
}

#[test]
fn unfurl_update_adds_embeds_without_touching_the_text() {
    let mut m = message();
    m.embeds.clear();
    m.content = "look https://example.com".into();
    let upd = patch(json!({
        "id": "5000000000000000020", "channel_id": "4000000000000000002",
        "embeds": [ { "type": "link", "title": "Example", "url": "https://example.com" } ]
    }));
    assert!(upd.content.is_none());
    m.apply_patch(&upd);
    assert_eq!(m.content, "look https://example.com");
    assert_eq!(m.embeds.len(), 1);
    assert!(!m.edited, "an unfurl is not an edit");
}

#[test]
fn text_edit_keeps_the_embeds_and_removal_clears_them() {
    let mut m = message();
    let n = m.embeds.len();
    m.apply_patch(&patch(json!({
        "id": "5000000000000000020", "channel_id": "4000000000000000002",
        "content": "edited", "edited_timestamp": "2026-11-18T13:00:00+00:00"
    })));
    assert_eq!(
        (m.content.as_str(), m.embeds.len(), m.edited),
        ("edited", n, true)
    );
    // Suppression: empty embeds and the new flags in one update.
    m.apply_patch(&patch(json!({
        "id": "5000000000000000020", "channel_id": "4000000000000000002",
        "embeds": [], "flags": 4
    })));
    assert!(m.embeds.is_empty());
    assert!(m.embeds_suppressed());
    assert_eq!(m.content, "edited");
    // A later update without flags keeps them.
    m.apply_patch(&patch(json!({
        "id": "5000000000000000020", "channel_id": "4000000000000000002", "content": "again"
    })));
    assert!(m.embeds_suppressed());
}

#[test]
fn update_reaches_the_cached_message_in_chat_state() {
    let mut a = App::boot(Config::for_tests()).0;
    let c = mock::CH_GENERAL;
    let id = {
        let w = a.chat.messages.get_mut(&c).unwrap();
        let m = w.messages.back_mut().unwrap();
        m.embeds.clear();
        (m.id, m.content.clone())
    };
    let p = MessagePatch {
        id: id.0,
        channel_id: c,
        embeds: Some(vec![Embed {
            kind: Some("link".into()),
            title: Some("T".into()),
            ..Default::default()
        }]),
        ..Default::default()
    };
    let _ = a.update(Message::Net(Event::MessageUpdate(Box::new(p))));
    let m = a.chat.messages[&c].messages.back().unwrap();
    assert_eq!(m.embeds.len(), 1);
    assert_eq!(m.content, id.1, "content untouched");
}

#[test]
fn huge_embed_text_does_not_panic_or_blow_up() {
    let big = "é🦀".repeat(2_000_000);
    let e = Embed {
        title: Some(big.clone()),
        description: Some(big.clone()),
        fields: vec![
            EmbedField {
                name: big.clone(),
                value: big,
                inline: true
            };
            500
        ],
        ..Default::default()
    };
    assert!(
        embeds::clip(e.title.as_deref().unwrap(), embeds::MAX_TITLE)
            .chars()
            .count()
            <= embeds::MAX_TITLE
    );
    assert_eq!(
        embeds::field_rows(&e.fields, 3).len(),
        embeds::MAX_FIELDS.div_ceil(3)
    );
}

#[tokio::test]
async fn suppress_embeds_patches_the_flags() {
    let server = http(Arc::new(|req, _| {
        if req.method == "PATCH" && req.path == "/channels/10/messages/20" {
            Response::json(
                200,
                json!({"id": "20", "channel_id": "10", "content": "x", "flags": 4,
                       "author": {"id": "2", "username": "a"}, "embeds": [], "type": 0}),
            )
        } else {
            Response::json(404, json!({}))
        }
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new("t0k"))).unwrap();
    let m = rest
        .set_message_flags(Snowflake(10), Snowflake(20), 1 << 2)
        .await
        .unwrap();
    assert!(m.embeds_suppressed());
    let r = &server.recorded()[0];
    assert_eq!(r.header("authorization"), Some("t0k"));
    let body: Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(body, json!({ "flags": 4 }));
}

#[tokio::test]
async fn refused_suppress_is_an_error() {
    let server = http(Arc::new(|_, _| {
        Response::json(
            403,
            json!({"message": "Missing Permissions", "code": 50013}),
        )
    }))
    .await;
    let rest = RestClient::new(&server.base(), "http://cdn", Some(Secret::new("t0k"))).unwrap();
    assert!(
        rest.set_message_flags(Snowflake(10), Snowflake(20), 4)
            .await
            .is_err()
    );
}

#[test]
fn suppress_action_hides_embeds_only_for_own_messages() {
    let mut a = App::boot(Config::for_tests()).0;
    let c = mock::CH_GENERAL;
    let me = a.chat.me_id().unwrap();
    let embed = Embed {
        kind: Some("link".into()),
        url: Some("https://example.com".into()),
        ..Default::default()
    };
    let (mine, theirs) = {
        let w = a.chat.messages.get_mut(&c).unwrap();
        let mut mine = None;
        let mut theirs = None;
        for m in w.messages.iter_mut() {
            if m.pending || m.failed {
                continue;
            }
            if m.author.id == me && mine.is_none() {
                m.embeds = vec![embed.clone()];
                mine = Some(m.id);
            } else if m.author.id != me && theirs.is_none() {
                m.embeds = vec![embed.clone()];
                theirs = Some(m.id);
            }
        }
        (mine.unwrap(), theirs.unwrap())
    };
    let find = |a: &App, id| {
        a.chat.messages[&c]
            .messages
            .iter()
            .find(|m| m.id == id)
            .unwrap()
            .clone()
    };
    // Someone else's message: no permission, nothing changes.
    let _ = a.update(Message::Ui(Ui::SuppressEmbeds(theirs)));
    assert_eq!(find(&a, theirs).embeds.len(), 1);
    // Own message: hidden at once.
    let _ = a.update(Message::Ui(Ui::SuppressEmbeds(mine)));
    let m = find(&a, mine);
    assert!(m.embeds.is_empty() && m.embeds_suppressed());
}

#[test]
fn only_web_links_reach_the_confirm_dialog() {
    let mut a = App::boot(Config::for_tests()).0;
    let _ = a.update(Message::Ui(Ui::ConfirmLink("javascript:alert(1)".into())));
    assert!(a.pending_link.is_none());
    let _ = a.update(Message::Ui(Ui::ConfirmLink("https://example.com".into())));
    assert_eq!(a.pending_link.as_deref(), Some("https://example.com"));
    // The inline player is not wired yet: playing asks before opening.
    let _ = a.update(Message::Ui(Ui::CancelLink));
    let _ = a.update(Message::Ui(Ui::PlayVideo(
        "https://media.discordapp.net/x.mp4".into(),
    )));
    assert_eq!(
        a.pending_link.as_deref(),
        Some("https://media.discordapp.net/x.mp4")
    );
}
