//! Profile and profile-effect requests against a local mock HTTP server.

mod common;

use std::sync::Arc;

use common::{Response, http};
use ferrocord::auth::secrets::Secret;
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

#[tokio::test]
async fn user_profile_sends_the_popout_query_and_parses_the_body() {
    let server = http(Arc::new(|req, _| {
        if req.method == "GET" && req.path.starts_with("/users/852892297661906993/profile") {
            Response::json(200, fixture("user_profile.json"))
        } else {
            Response::json(404, json!({"message": "Unknown User"}))
        }
    }))
    .await;
    let rest = client(&server.base());
    let guild = Snowflake(80351110224678913);
    let p = rest
        .user_profile(Snowflake(852892297661906993), Some(guild))
        .await
        .unwrap();
    assert_eq!(p.bio, "Professional alien");
    assert_eq!(p.badges.len(), 2);
    assert_eq!(p.mutual_guilds.len(), 2);
    assert_eq!(p.effect, Some(Snowflake(1139323097930027068)));
    assert_eq!(
        p.user.avatar_decoration.as_ref().map(|d| d.asset.as_str()),
        Some("a_fed43ab12698df65902ba06727e20c0e")
    );
    let req = server.recorded().pop().unwrap();
    assert_eq!(req.header("authorization"), Some("t0k"));
    for part in [
        "with_mutual_guilds=true",
        "with_mutual_friends=true",
        "with_mutual_friends_count=true",
        "guild_id=80351110224678913",
        "type=popout",
    ] {
        assert!(req.path.contains(part), "{} missing {part}", req.path);
    }
    // Without a guild there is no guild_id.
    rest.user_profile(Snowflake(852892297661906993), None)
        .await
        .unwrap();
    assert!(!server.recorded().pop().unwrap().path.contains("guild_id"));
}

#[tokio::test]
async fn blocked_or_unknown_profiles_are_errors_not_panics() {
    let server = http(Arc::new(|_, _| {
        Response::json(403, json!({"message": "Missing Access"}))
    }))
    .await;
    let rest = client(&server.base());
    assert!(rest.user_profile(Snowflake(1), None).await.is_err());
    // A 200 without a user is a protocol error too.
    let server = http(Arc::new(|_, _| {
        Response::json(200, json!({"user_profile": {}}))
    }))
    .await;
    assert!(
        client(&server.base())
            .user_profile(Snowflake(1), None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn profile_effect_configs_are_fetched_and_parsed() {
    let server = http(Arc::new(|req, _| {
        if req.path == "/user-profile-effects" {
            Response::json(200, fixture("profile_effects.json"))
        } else {
            Response::json(404, json!({"message": "Unknown"}))
        }
    }))
    .await;
    let effects = client(&server.base()).profile_effects().await.unwrap();
    assert_eq!(effects.len(), 2);
    assert_eq!(effects[0].title, "Sparkles");
    assert!(effects[0].static_frame.is_some());
}
