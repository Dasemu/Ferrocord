//! Video downloads against a local mock CDN: allowlist, size cap, token
//! never sent, errors, progress. No real network.

mod common;

use std::sync::Arc;

use common::{Response, http};
use ferrocord::auth::secrets::Secret;
use ferrocord::net::rest::RestClient;

fn temp_file(name: &str) -> (std::path::PathBuf, std::fs::File) {
    let dir = std::env::temp_dir().join(format!("ferrocord-dl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let f = std::fs::File::create(&path).unwrap();
    (path, f)
}

fn body(status: u16, text: String) -> Response {
    Response {
        status,
        headers: vec![("content-type".into(), "video/mp4".into())],
        body: text,
    }
}

#[tokio::test]
async fn a_video_is_streamed_to_the_file_without_the_token() {
    let payload = "v".repeat(700_000);
    let p = payload.clone();
    let server = http(Arc::new(move |_, _| body(200, p.clone()))).await;
    let token = Secret::new("tok-not-real".to_owned());
    let rest = RestClient::new("http://unused", &server.base(), Some(token)).unwrap();
    let (path, file) = temp_file("ok.mp4");
    let mut reports = Vec::new();
    let n = rest
        .download_file(
            &format!("{}/attachments/1/2/ok.mp4", server.base()),
            file,
            10_000_000,
            |received, total| reports.push((received, total)),
        )
        .await
        .unwrap();
    assert_eq!(n, 700_000);
    assert_eq!(std::fs::read(&path).unwrap(), payload.as_bytes());
    assert!(reports.len() >= 2, "progress is reported: {reports:?}");
    assert_eq!(reports.last().unwrap().0, 700_000);
    assert_eq!(reports.last().unwrap().1, Some(700_000));
    // The CDN never sees the account token.
    let rec = server.recorded();
    assert_eq!(rec.len(), 1);
    assert!(rec[0].header("authorization").is_none());
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn too_large_files_are_refused() {
    let payload = "v".repeat(300_000);
    let server = http(Arc::new(move |_, _| body(200, payload.clone()))).await;
    let rest = RestClient::new("http://unused", &server.base(), None).unwrap();
    let (path, file) = temp_file("big.mp4");
    let err = rest
        .download_file(
            &format!("{}/a.mp4", server.base()),
            file,
            100_000,
            |_, _| {},
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("too large")
            || format!("{err:?}").contains("too large"),
        "{err:?}"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn other_hosts_are_never_contacted() {
    let server = http(Arc::new(|_, _| body(200, "x".into()))).await;
    let rest = RestClient::new("http://unused", &server.base(), None).unwrap();
    for url in [
        "https://evil.example.com/a.mp4".to_owned(),
        "http://cdn.discordapp.com/a.mp4".to_owned(),
        // Same machine, different port: not the configured CDN.
        "http://127.0.0.1:1/a.mp4".to_owned(),
        "file:///etc/passwd".to_owned(),
    ] {
        let (path, file) = temp_file("no.mp4");
        assert!(
            rest.download_file(&url, file, 1000, |_, _| {})
                .await
                .is_err(),
            "{url} was fetched"
        );
        let _ = std::fs::remove_file(path);
    }
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn an_expired_link_is_an_http_error() {
    let server = http(Arc::new(|_, _| body(404, "gone".into()))).await;
    let rest = RestClient::new("http://unused", &server.base(), None).unwrap();
    let (path, file) = temp_file("gone.mp4");
    match rest
        .download_file(&format!("{}/a.mp4", server.base()), file, 1000, |_, _| {})
        .await
    {
        Err(ferrocord::error::Error::Http { status: 404, .. }) => {}
        other => panic!("{other:?}"),
    }
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_redirect_off_the_cdn_is_not_accepted() {
    let other = http(Arc::new(|_, _| body(200, "x".repeat(10)))).await;
    let target = format!("{}/steal.mp4", other.base());
    let server = http(Arc::new(move |_, _| Response {
        status: 302,
        headers: vec![("location".into(), target.clone())],
        body: String::new(),
    }))
    .await;
    let rest = RestClient::new("http://unused", &server.base(), None).unwrap();
    let (path, file) = temp_file("redir.mp4");
    let r = rest
        .download_file(&format!("{}/a.mp4", server.base()), file, 1000, |_, _| {})
        .await;
    assert!(r.is_err(), "the redirect target was accepted");
    // Nothing from the other host reached the file.
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    let _ = std::fs::remove_file(path);
}
