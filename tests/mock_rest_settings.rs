//! User Settings REST calls against a local mock HTTP server: the
//! preloaded settings proto read-modify-write and the profile edits.

mod common;

use std::sync::{Arc, Mutex};

use base64::Engine;
use common::{Response, http};
use ferrocord::auth::secrets::Secret;
use ferrocord::net::rest::{ProfileEdit, RestClient};
use ferrocord::net::user_settings::{self as us, CustomStatus, Edit, Synced, Value};
use serde_json::{Value as Json, json};

fn client(base: &str) -> RestClient {
    RestClient::new(base, "http://cdn", Some(Secret::new("t0k"))).unwrap()
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Stored settings: versions, text_and_images with a field this client
/// does not know (77), privacy, and an unknown top-level field (99).
fn stored() -> Vec<u8> {
    let mut s = vec![0x0a, 0x02, 0x08, 0x07]; // versions { client_version: 7 }
    s.extend([0x32, 0x0a, 0x62, 0x00]); // text_and_images { render_embeds: false
    s.extend([0xea, 0x04, 0x05]); //   77: "hello" }
    s.extend(b"hello");
    s.extend([0x42, 0x02, 0x20, 0x01]); // privacy { default_guilds_restricted }
    s.extend([0x9a, 0x06, 0x02, 0xde, 0xad]); // 99: unknown bytes
    s
}

#[tokio::test]
async fn proto_patch_sends_only_touched_categories_with_unknown_fields() {
    let server_state = Arc::new(Mutex::new(stored()));
    let s = server_state.clone();
    let server = http(Arc::new(move |req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/users/@me/settings-proto/1") => {
                let cur = s.lock().unwrap().clone();
                Response::json(200, json!({ "settings": B64.encode(cur) }))
            }
            ("PATCH", "/users/@me/settings-proto/1") => {
                // Discord's rule: sent top-level fields replace the stored
                // ones, the others stay.
                let v: Json = serde_json::from_str(&req.body).unwrap();
                let partial = B64.decode(v["settings"].as_str().unwrap()).unwrap();
                let mut cur = s.lock().unwrap().clone();
                for top in [6u32, 8, 11, 12, 13] {
                    if let Ok(Some(_)) = probe(&partial, top) {
                        cur = replace_top(&cur, &partial, top);
                    }
                }
                *s.lock().unwrap() = cur;
                Response::json(200, json!({ "settings": v["settings"] }))
            }
            _ => Response::json(404, json!({"message": "Unknown"})),
        }
    }))
    .await;
    let rest = client(&server.base());

    let raw = rest.user_settings().await.unwrap();
    assert_eq!(
        us::get(&raw, Synced::RenderEmbeds),
        Ok(Some(Value::Bool(false)))
    );

    let next = rest
        .edit_user_settings(&[
            Edit::Set(Synced::RenderReactions, Value::Bool(false)),
            Edit::Set(Synced::Theme, Value::U32(2)),
            Edit::CustomStatus(Some(CustomStatus {
                text: "busy".into(),
                ..Default::default()
            })),
        ])
        .await
        .unwrap();
    assert_eq!(us::get(&next, Synced::Theme), Ok(Some(Value::U32(2))));

    let rec = server.recorded();
    let calls: Vec<_> = rec.iter().map(|r| r.method.as_str()).collect();
    assert_eq!(calls, ["GET", "GET", "PATCH"]);
    let patch: Json = serde_json::from_str(&rec[2].body).unwrap();
    let partial = B64.decode(patch["settings"].as_str().unwrap()).unwrap();
    // Only text_and_images (6), status (11) and appearance (13) go, each
    // whole: the unknown field 77 rides along inside text_and_images.
    let tops = top_numbers(&partial);
    assert_eq!(tops, [6, 13, 11]);
    assert!(partial.windows(5).any(|w| w == b"hello"));
    assert_eq!(
        us::get(&partial, Synced::RenderEmbeds),
        Ok(Some(Value::Bool(false)))
    );
    assert_eq!(
        us::get(&partial, Synced::RenderReactions),
        Ok(Some(Value::Bool(false)))
    );

    // The server copy kept versions, privacy and the unknown field 99.
    let now = server_state.lock().unwrap().clone();
    assert_eq!(&now[..4], &stored()[..4]);
    assert!(now.windows(5).any(|w| w == [0x9a, 0x06, 0x02, 0xde, 0xad]));
    assert_eq!(
        us::get(&now, Synced::DefaultGuildsRestricted),
        Ok(Some(Value::Bool(true)))
    );
    assert_eq!(us::custom_status(&now).unwrap().unwrap().text, "busy");
    assert!(rec.iter().all(|r| r.header("authorization") == Some("t0k")));
}

#[tokio::test]
async fn out_of_date_patch_is_an_error() {
    let server = http(Arc::new(|req, _| match req.method.as_str() {
        "GET" => Response::json(200, json!({ "settings": "" })),
        _ => Response::json(200, json!({ "settings": "", "out_of_date": true })),
    }))
    .await;
    let rest = client(&server.base());
    assert!(
        rest.edit_user_settings(&[Edit::Set(Synced::DeveloperMode, Value::Bool(true))])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn profile_edits_use_users_me_and_profile() {
    let server = http(Arc::new(|req, _| {
        match (req.method.as_str(), req.path.as_str()) {
            ("PATCH", "/users/@me") => Response::json(200, json!({ "id": "1" })),
            ("PATCH", "/users/@me/profile") => Response::json(200, json!({ "bio": "x" })),
            _ => Response::json(404, json!({"message": "Unknown"})),
        }
    }))
    .await;
    let rest = client(&server.base());
    rest.edit_profile(&ProfileEdit {
        global_name: Some("Ferris".into()),
        pronouns: Some("it/its".into()),
        bio: Some("crab\n".into()),
    })
    .await
    .unwrap();
    // Only the profile half when the name did not change.
    rest.edit_profile(&ProfileEdit {
        bio: Some("".into()),
        ..Default::default()
    })
    .await
    .unwrap();
    let rec = server.recorded();
    let got: Vec<(String, Json)> = rec
        .iter()
        .map(|r| (r.path.clone(), serde_json::from_str(&r.body).unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("/users/@me".into(), json!({ "global_name": "Ferris" })),
            (
                "/users/@me/profile".into(),
                json!({ "pronouns": "it/its", "bio": "crab" })
            ),
            ("/users/@me/profile".into(), json!({ "bio": "" })),
        ]
    );
}

#[tokio::test]
async fn rejected_profile_reports_http_error() {
    let server = http(Arc::new(|_, _| {
        Response::json(
            400,
            json!({ "message": "Invalid Form Body", "code": 50035 }),
        )
    }))
    .await;
    let rest = client(&server.base());
    let e = rest
        .edit_profile(&ProfileEdit {
            global_name: Some("x".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(
        e,
        ferrocord::error::Error::Http { status: 400, .. }
    ));
}

// -- tiny protobuf helpers for the fake server --------------------------------

fn varint(buf: &[u8], pos: &mut usize) -> u64 {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// (number, whole field bytes) of each top-level field.
fn top_fields(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let start = pos;
        let key = varint(buf, &mut pos);
        match key & 7 {
            0 => {
                varint(buf, &mut pos);
            }
            1 => pos += 8,
            2 => {
                let n = varint(buf, &mut pos) as usize;
                pos += n;
            }
            5 => pos += 4,
            w => panic!("wire type {w}"),
        }
        out.push(((key >> 3) as u32, buf[start..pos].to_vec()));
    }
    out
}

fn top_numbers(buf: &[u8]) -> Vec<u32> {
    top_fields(buf).into_iter().map(|(n, _)| n).collect()
}

fn probe(buf: &[u8], n: u32) -> Result<Option<()>, ()> {
    Ok(top_numbers(buf).contains(&n).then_some(()))
}

fn replace_top(cur: &[u8], partial: &[u8], n: u32) -> Vec<u8> {
    let new: Vec<u8> = top_fields(partial)
        .into_iter()
        .filter(|(k, _)| *k == n)
        .flat_map(|(_, b)| b)
        .collect();
    let mut out = Vec::new();
    let mut written = false;
    for (k, b) in top_fields(cur) {
        if k == n {
            if !written {
                out.extend(&new);
                written = true;
            }
        } else {
            out.extend(b);
        }
    }
    if !written {
        out.extend(new);
    }
    out
}
