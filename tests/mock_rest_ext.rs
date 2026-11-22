//! REST calls behind the header buttons, server creation/joining and
//! discovery, against a local mock HTTP server.

mod common;

use std::sync::Arc;

use common::{Response, http};
use ferrocord::auth::secrets::Secret;
use ferrocord::error::Error;
use ferrocord::net::extras::{self, MuteDuration, NotifyLevel};
use ferrocord::net::model::Snowflake;
use ferrocord::net::rest::RestClient;
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn client(base: &str) -> RestClient {
    RestClient::new(base, "http://cdn", Some(Secret::new("t0k"))).unwrap()
}

fn body(r: &common::Recorded) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

#[tokio::test]
async fn pins_list_and_unpin() {
    let server = http(Arc::new(|req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/channels/10/pins") => Response::json(200, fixture("pins.json")),
            ("GET", "/channels/11/pins") => Response::json(200, fixture("pins_paged.json")),
            ("DELETE", "/channels/10/pins/20") => Response {
                status: 204,
                headers: vec![],
                body: String::new(),
            },
            _ => Response::json(404, json!({"message": "Unknown"})),
        }
    }))
    .await;
    let rest = client(&server.base());
    let pins = rest.pins(Snowflake(10)).await.unwrap();
    assert_eq!(
        pins.iter().map(|m| m.id.0).collect::<Vec<_>>(),
        vec![30, 20]
    );
    assert_eq!(pins[0].content, "newest pin");
    // The paginated layout parses too.
    let paged = rest.pins(Snowflake(11)).await.unwrap();
    assert_eq!(paged[0].id.0, 41);
    rest.unpin(Snowflake(10), Snowflake(20)).await.unwrap();
    let last = server.recorded().pop().unwrap();
    assert_eq!(
        (last.method.as_str(), last.path.as_str()),
        ("DELETE", "/channels/10/pins/20")
    );
    assert_eq!(last.header("authorization"), Some("t0k"));
}

#[tokio::test]
async fn unpin_without_permission_is_a_403() {
    let server = http(Arc::new(|_, _| {
        Response::json(
            403,
            json!({"message": "Missing Permissions", "code": 50013}),
        )
    }))
    .await;
    let err = client(&server.base())
        .unpin(Snowflake(10), Snowflake(20))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 403, .. }));
    assert!(err.user_message().contains("permission"));
}

#[tokio::test]
async fn mentions_inbox_paths_and_parsing() {
    let server = http(Arc::new(|_, _| {
        Response::json(200, fixture("mentions.json"))
    }))
    .await;
    let rest = client(&server.base());
    let m = rest.mentions(None, None).await.unwrap();
    assert_eq!(m.len(), 2);
    assert_eq!(m[0].content, "hey @me, look");
    assert_eq!(m[0].guild_id, Some(Snowflake(2_000_000_000_000_000_001)));
    rest.mentions(Some(Snowflake(54)), Some(Snowflake(7)))
        .await
        .unwrap();
    let rec = server.recorded();
    assert_eq!(
        rec[0].path,
        "/users/@me/mentions?limit=25&roles=true&everyone=true"
    );
    assert_eq!(
        rec[1].path,
        "/users/@me/mentions?limit=25&roles=true&everyone=true&before=54&guild_id=7"
    );
}

#[tokio::test]
async fn mentions_retries_after_429() {
    let server = http(Arc::new(|_, n| {
        if n == 0 {
            Response::json(
                429,
                json!({"message": "slow down", "retry_after": 0.2, "global": false}),
            )
        } else {
            Response::json(200, json!([]))
        }
    }))
    .await;
    let t = std::time::Instant::now();
    client(&server.base()).mentions(None, None).await.unwrap();
    assert_eq!(server.recorded().len(), 2);
    assert!(t.elapsed() >= std::time::Duration::from_millis(180));
}

#[tokio::test]
async fn notification_settings_patch_bodies() {
    let server = http(Arc::new(|_, _| Response::json(200, json!({})))).await;
    let rest = client(&server.base());
    let now = 1_700_000_000_000;
    rest.mute_channel(Some(Snowflake(9)), Snowflake(5), MuteDuration::Hour1, now)
        .await
        .unwrap();
    rest.patch_guild_settings(
        Some(Snowflake(9)),
        extras::channel_override_body(Snowflake(5), extras::level_override(NotifyLevel::Mentions)),
    )
    .await
    .unwrap();
    // DMs use the `@me` pseudo-guild.
    rest.patch_guild_settings(
        None,
        extras::channel_override_body(Snowflake(6), extras::unmute_override()),
    )
    .await
    .unwrap();
    let rec = server.recorded();
    assert_eq!(rec[0].method, "PATCH");
    assert_eq!(rec[0].path, "/users/@me/guilds/9/settings");
    let b = body(&rec[0]);
    assert_eq!(b["channel_overrides"]["5"]["muted"], true);
    assert_eq!(
        b["channel_overrides"]["5"]["mute_config"]["selected_time_window"],
        3600
    );
    assert_eq!(
        b["channel_overrides"]["5"]["mute_config"]["end_time"],
        extras::iso_from_ms(now + 3_600_000)
    );
    assert_eq!(
        body(&rec[1])["channel_overrides"]["5"]["message_notifications"],
        1
    );
    assert_eq!(rec[2].path, "/users/@me/guilds/@me/settings");
    assert_eq!(body(&rec[2])["channel_overrides"]["6"]["muted"], false);
}

#[tokio::test]
async fn folders_are_saved_through_the_settings_endpoint() {
    let server = http(Arc::new(|_, _| Response::json(200, json!({})))).await;
    let rest = client(&server.base());
    let mut f = ferrocord::app::folders::Folders::from_settings(&[], &[Snowflake(1), Snowflake(2)]);
    f.drop_on(Snowflake(2), Snowflake(1), 42);
    f.rename(42, "Work");
    rest.set_guild_folders(f.to_wire()).await.unwrap();
    let r = &server.recorded()[0];
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("PATCH", "/users/@me/settings")
    );
    assert_eq!(
        body(r),
        json!({"guild_folders": [{"id": 42, "name": "Work", "color": null, "guild_ids": ["1", "2"]}]})
    );
}

#[tokio::test]
async fn invite_preview_and_join() {
    let server = http(Arc::new(|req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", p) if p.starts_with("/invites/rust-lang") => {
                Response::json(200, fixture("invite.json"))
            }
            ("POST", "/invites/rust-lang") => Response::json(200, fixture("invite_join.json")),
            _ => Response::json(404, json!({"message": "Unknown Invite", "code": 10006})),
        }
    }))
    .await;
    let rest = client(&server.base());
    let p = rest.invite("rust-lang").await.unwrap();
    assert_eq!(p.name, "Rustaceans");
    assert_eq!(p.members, Some(1200));
    assert!(p.verified);
    let (id, name) = rest.join_invite("rust-lang", Some("sess-1")).await.unwrap();
    assert_eq!((id, name.as_str()), (Snowflake(55), "Rustaceans"));
    let rec = server.recorded();
    assert_eq!(
        rec[0].path,
        "/invites/rust-lang?with_counts=true&with_expiration=true"
    );
    assert_eq!(body(&rec[1]), json!({"session_id": "sess-1"}));
    // Unknown codes are a 404 the UI words as "invalid or expired".
    let err = rest.invite("nope").await.unwrap_err();
    assert!(matches!(err, Error::Http { status: 404, .. }));
}

#[tokio::test]
async fn joining_with_a_captcha_demand_is_reported_not_solved() {
    let server = http(Arc::new(|_, _| {
        Response::json(400, fixture("captcha_required.json"))
    }))
    .await;
    let rest = client(&server.base());
    let err = rest.join_invite("abc", None).await.unwrap_err();
    assert!(matches!(err, Error::CaptchaRequired(_)), "{err:?}");
    assert!(err.user_message().contains("captcha"));
    // Exactly one attempt: no retry loop, no solver.
    assert_eq!(server.recorded().len(), 1);
}

#[tokio::test]
async fn create_guild_sends_name_and_icon() {
    let server = http(Arc::new(|_, _| {
        Response::json(201, fixture("guild_created.json"))
    }))
    .await;
    let rest = client(&server.base());
    let (id, name) = rest
        .create_guild("My new server", Some("data:image/png;base64,AAAA"))
        .await
        .unwrap();
    assert_eq!((id, name.as_str()), (Snowflake(77), "My new server"));
    let r = &server.recorded()[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/guilds"));
    let b = body(r);
    assert_eq!(b["name"], "My new server");
    assert_eq!(b["icon"], "data:image/png;base64,AAAA");
    assert_eq!(b["guild_template_code"], "2TffvPucqHkN");
}

#[tokio::test]
async fn discovery_feed_search_categories_and_join() {
    let server = http(Arc::new(|req, _| {
        match (req.method.as_str(), req.path.split('?').next().unwrap()) {
            ("GET", "/discovery/categories") => {
                Response::json(200, fixture("discovery_categories.json"))
            }
            ("GET", "/discoverable-guilds" | "/discovery/search") => {
                Response::json(200, fixture("discoverable_guilds.json"))
            }
            ("PUT", "/guilds/501/members/@me") => Response::json(201, json!({})),
            _ => Response::json(404, json!({"message": "Unknown"})),
        }
    }))
    .await;
    let rest = client(&server.base());
    let cats = rest.discovery_categories().await.unwrap();
    assert_eq!(cats.len(), 3);
    assert_eq!(cats[1].name, "Gaming");
    let page = rest.discover("", None, 0).await.unwrap();
    assert_eq!(page.total, Some(2));
    assert_eq!(page.guilds[0].name, "Rust Community");
    assert_eq!(page.guilds[0].members, Some(50123));
    assert!(page.guilds[0].partnered);
    rest.discover("rust", Some(5), 24).await.unwrap();
    rest.join_discoverable(Snowflake(501), Some("s"))
        .await
        .unwrap();
    let rec = server.recorded();
    assert_eq!(rec[1].path, "/discoverable-guilds?offset=0&limit=24");
    assert_eq!(
        rec[2].path,
        "/discovery/search?query=rust&offset=24&limit=24&category_id=5"
    );
    assert_eq!(rec[3].method, "PUT");
    assert_eq!(body(&rec[3]), json!({"session_id": "s"}));
}

#[tokio::test]
async fn favorite_gifs_read_modify_write_keeps_other_settings() {
    use base64::Engine;
    use ferrocord::net::frecency::{self, FavoriteChange, FavoriteGif, GifFormat};
    use std::sync::Mutex;
    let b64 = base64::engine::general_purpose::STANDARD;
    let fav = |url: &str| FavoriteGif {
        url: url.into(),
        src: format!("https://media.tenor.com/{url}.mp4"),
        format: GifFormat::Video,
        width: 220,
        height: 120,
        order: 0,
    };
    // versions { client_version: 7 }, one favourite, then a field this
    // client does not know (99 = 1).
    let mut initial = vec![0x0a, 0x02, 0x08, 0x07];
    initial.extend(frecency::apply_favorite(&[], &FavoriteChange::Add(fav("old"))).unwrap());
    initial.extend([0x98, 0x06, 0x01]);
    let stored = Arc::new(Mutex::new(initial.clone()));
    let s = stored.clone();
    let server = http(Arc::new(move |req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/users/@me/settings-proto/2") => {
                let cur = s.lock().unwrap().clone();
                Response::json(200, json!({ "settings": b64.encode(cur) }))
            }
            ("PATCH", "/users/@me/settings-proto/2") => {
                let v: Value = serde_json::from_str(&req.body).unwrap();
                let bytes = b64.decode(v["settings"].as_str().unwrap()).unwrap();
                *s.lock().unwrap() = bytes.clone();
                Response::json(200, json!({ "settings": b64.encode(bytes) }))
            }
            _ => Response::json(404, json!({"message": "Unknown"})),
        }
    }))
    .await;
    let rest = client(&server.base());

    let raw = rest.frecency_settings().await.unwrap();
    assert_eq!(frecency::favorite_gifs(&raw).unwrap()[0].url, "old");

    let after = rest
        .change_favorite_gif(&FavoriteChange::Add(fav("new")))
        .await
        .unwrap();
    assert_eq!(
        after.iter().map(|g| g.url.as_str()).collect::<Vec<_>>(),
        ["new", "old"]
    );
    let now = stored.lock().unwrap().clone();
    // Versions first and the unknown field last, byte for byte.
    assert_eq!(&now[..4], &initial[..4]);
    assert_eq!(&now[now.len() - 3..], &[0x98, 0x06, 0x01]);

    let after = rest
        .change_favorite_gif(&FavoriteChange::Remove("old".into()))
        .await
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].url, "new");
    assert_eq!(after[0].src, "https://media.tenor.com/new.mp4");

    let rec = server.recorded();
    let calls: Vec<_> = rec.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(calls, ["GET", "GET", "PATCH", "GET", "PATCH"]);
    // The official client's auth header goes with every call.
    assert!(rec.iter().all(|r| {
        r.headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
    }));
}

#[tokio::test]
async fn empty_frecency_settings_are_an_empty_list() {
    let server = http(Arc::new(|req, _| match req.path.as_str() {
        "/users/@me/settings-proto/2" => Response::json(200, json!({ "settings": "" })),
        _ => Response::json(404, json!({"message": "Unknown"})),
    }))
    .await;
    let raw = client(&server.base()).frecency_settings().await.unwrap();
    assert!(
        ferrocord::net::frecency::favorite_gifs(&raw)
            .unwrap()
            .is_empty()
    );
}
