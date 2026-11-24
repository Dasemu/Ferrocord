//! End to end with the example plugins (`examples/plugins`, built for
//! wasm32-unknown-unknown): the real `App` in mock mode, the real plugin
//! thread, the hooks driven through `update`.
//!
//! Build them first: `examples/plugins/build.sh`. Without the modules
//! these tests skip, unless `FERROCORD_EXPECT_PLUGINS=1` (CI) makes them
//! fail.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ferrocord::app::plugins::PluginMsg;
use ferrocord::app::{App, Message, Ui};
use ferrocord::config::Config;
use ferrocord::mock;
use ferrocord::net::Event;
use ferrocord::net::model::Snowflake;
use ferrocord::plugins::hostcalls::Out;
use ferrocord::plugins::package;
use futures::channel::mpsc;

const EXAMPLES: [&str; 3] = ["text-replacer", "word-highlighter", "quote-message"];

fn wasm_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/plugins/target/wasm32-unknown-unknown/release")
        .join(format!("{}.wasm", name.replace('-', "_")))
}

/// Installs the examples into a fresh plugins dir (quote-message as a
/// `.ferroplug` zip). `None` when the modules are not built.
fn install() -> Option<tempfile::TempDir> {
    if EXAMPLES.iter().any(|n| !wasm_path(n).is_file()) {
        assert!(
            std::env::var_os("FERROCORD_EXPECT_PLUGINS").is_none(),
            "example plugins are not built (run examples/plugins/build.sh)"
        );
        eprintln!("skipping: example plugins not built");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins");
    for n in EXAMPLES {
        let toml = std::fs::read(src.join(n).join("plugin.toml")).unwrap();
        let wasm = std::fs::read(wasm_path(n)).unwrap();
        if n == "quote-message" {
            use std::io::Write;
            let f = std::fs::File::create(dir.path().join(format!("{n}.ferroplug"))).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("plugin.toml", o).unwrap();
            z.write_all(&toml).unwrap();
            z.start_file("plugin.wasm", o).unwrap();
            z.write_all(&wasm).unwrap();
            z.finish().unwrap();
        } else {
            let d = dir.path().join(n);
            std::fs::create_dir(&d).unwrap();
            std::fs::write(d.join("plugin.toml"), toml).unwrap();
            std::fs::write(d.join("plugin.wasm"), wasm).unwrap();
        }
    }
    Some(dir)
}

struct Harness {
    app: App,
    rx: Option<mpsc::Receiver<Out>>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Option<Self> {
        let dir = install()?;
        let mut cfg = Config::for_tests();
        cfg.plugins_dir = Some(dir.path().to_path_buf());
        let mut app = App::boot(cfg).0;
        // Opening the page scans in a task; run the scan here.
        let _ = app.update(Message::Plugin(PluginMsg::OpenPage));
        let scan = package::scan(dir.path());
        assert_eq!(scan.found.len(), 3, "{:?}", scan.broken);
        let _ = app.update(Message::Plugin(PluginMsg::Scanned {
            state: None,
            scan: Box::new(scan),
            boot: false,
            reload: false,
        }));
        Some(Self {
            app,
            rx: None,
            _dir: dir,
        })
    }

    fn enable(&mut self, id: &str) {
        let _ = self
            .app
            .update(Message::Plugin(PluginMsg::Toggle(id.into())));
        assert_eq!(self.app.plugins.page.prompt.as_deref(), Some(id));
        let _ = self.app.update(Message::Plugin(PluginMsg::ConfirmEnable));
        assert!(self.app.plugins.state.is_enabled(id));
        if self.rx.is_none() {
            // What the subscription does in the real app.
            self.rx = self
                .app
                .plugins
                .receiver()
                .and_then(|r| r.lock().unwrap().take());
        }
        self.pump(|a| a.plugins.manifest(id).is_some());
    }

    /// Feeds plugin output to the app until `done` (5 s at most).
    fn pump(&mut self, done: impl Fn(&App) -> bool) {
        let t = Instant::now();
        loop {
            if let Some(rx) = self.rx.as_mut() {
                while let Ok(o) = rx.try_recv() {
                    let _ = self
                        .app
                        .update(Message::Plugin(PluginMsg::Out(Box::new(o))));
                }
            }
            if done(&self.app) {
                return;
            }
            assert!(t.elapsed() < Duration::from_secs(5), "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn last_content(&self) -> String {
        let c = self.app.open_channel().unwrap();
        self.app.chat.messages[&c]
            .messages
            .back()
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }

    fn type_and_send(&mut self, s: &str) {
        let _ = self.app.update(Message::Ui(Ui::ComposerChanged(s.into())));
        let _ = self.app.update(Message::Ui(Ui::Send));
    }
}

#[test]
fn outgoing_transform_rewrites_before_send() {
    let Some(mut h) = Harness::new() else { return };
    h.enable("text-replacer");
    h.type_and_send("/shrug");
    // The composer clears at once; the text goes out after the transform.
    assert!(h.app.composer.is_empty());
    h.pump(|a| a.plugins.pending_transform.is_none());
    assert_eq!(h.last_content(), r"¯\\\_(ツ)\_/¯");

    h.type_and_send("a --> b");
    h.pump(|a| a.plugins.pending_transform.is_none());
    assert_eq!(h.last_content(), "a → b");

    // Settings reach the plugin: shortcuts off, /shrug goes out as typed.
    let _ = h.app.update(Message::Plugin(PluginMsg::SetBool(
        "text-replacer".into(),
        "shortcuts".into(),
        false,
    )));
    h.type_and_send("/shrug");
    h.pump(|a| a.plugins.pending_transform.is_none());
    assert_eq!(h.last_content(), "/shrug");
}

#[test]
fn local_command_sends_only_on_the_users_action() {
    let Some(mut h) = Harness::new() else { return };
    h.enable("text-replacer");
    let before = h.last_content();
    h.type_and_send("/lenny hi");
    assert!(h.app.composer.is_empty());
    h.pump(|a| {
        let c = a.open_channel().unwrap();
        a.chat.messages[&c]
            .messages
            .back()
            .is_some_and(|m| m.content.contains("( ͡° ͜ʖ ͡°)"))
    });
    assert_ne!(h.last_content(), before);
    assert_eq!(h.last_content(), "hi ( ͡° ͜ʖ ͡°)");
    // An unknown command is a normal message (transforms still apply).
    h.type_and_send("/nope");
    h.pump(|a| a.plugins.pending_transform.is_none());
    assert_eq!(h.last_content(), "/nope");
}

#[test]
fn decorations_follow_observed_messages() {
    let Some(mut h) = Harness::new() else { return };
    h.enable("word-highlighter");
    let c = h.app.open_channel().unwrap();
    let id = Snowflake(9_000_000_000_000_000);
    let m = mock::text_message(c, id, mock::user(2), "Ferrocord is written in Rust");
    let _ = h
        .app
        .update(Message::Net(Event::MessageCreate(Box::new(m))));
    h.pump(|a| a.plugins.decorations.highlighted(id.0));
    let d = h.app.plugins.decorations.get(id.0);
    assert_eq!(d[0].1.text, "ferrocord, rust");

    // Badge style setting; an edit that drops the words clears it.
    let _ = h.app.update(Message::Plugin(PluginMsg::SetChoice(
        "word-highlighter".into(),
        "style".into(),
        "badge".into(),
    )));
    let patch = ferrocord::net::model::MessagePatch {
        id,
        channel_id: c,
        content: Some("nothing here".into()),
        embeds: None,
        attachments: None,
        flags: None,
        edited: true,
    };
    let _ = h
        .app
        .update(Message::Net(Event::MessageUpdate(Box::new(patch))));
    h.pump(|a| a.plugins.decorations.get(id.0).is_empty());

    // Disabling removes its decorations and stops the plugin thread.
    let _ = h.app.update(Message::Plugin(PluginMsg::Toggle(
        "word-highlighter".into(),
    )));
    assert!(!h.app.plugins.is_running());
}

#[test]
fn context_menu_action_quotes_into_the_composer() {
    let Some(mut h) = Harness::new() else { return };
    h.enable("quote-message");
    let c = h.app.open_channel().unwrap();
    let target = h.app.chat.messages[&c]
        .messages
        .iter()
        .rev()
        .find(|m| !m.content.is_empty() && !m.content.contains('\n'))
        .cloned()
        .unwrap();
    let labels: Vec<String> = ferrocord::ui::plugins::menu_entries(&h.app, target.id)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert_eq!(labels, ["Copy as Quote", "Quote in Composer"]);
    let _ = h.app.update(Message::Plugin(PluginMsg::Action {
        plugin: "quote-message".into(),
        action: "quote-composer".into(),
        message: target.id,
    }));
    h.pump(|a| !a.composer.is_empty());
    let text = h.app.composer.text();
    assert!(text.starts_with(&format!("> {}", target.content)), "{text}");
    assert!(text.contains(&format!("> — {}", target.author.display_name())));
}

#[test]
fn startup_restores_enabled_plugins_and_hot_reload_restarts_them() {
    let Some(mut h) = Harness::new() else { return };
    h.enable("text-replacer");
    // What `boot` reads next time: the state file of this run is in
    // memory only (mock mode); check the record itself.
    assert!(
        h.app.plugins.state.plugins["text-replacer"].covers(
            &h.app
                .plugins
                .scan
                .as_ref()
                .unwrap()
                .get("text-replacer")
                .unwrap()
                .manifest
        )
    );
    let dir = h.app.plugins.dir.clone().unwrap();
    let _ = h.app.update(Message::Plugin(PluginMsg::Reload));
    let _ = h.app.update(Message::Plugin(PluginMsg::Scanned {
        state: None,
        scan: Box::new(package::scan(&dir)),
        boot: false,
        reload: true,
    }));
    h.pump(|a| a.plugins.manifest("text-replacer").is_some());
    h.type_and_send("/tableflip");
    h.pump(|a| a.plugins.pending_transform.is_none());
    assert_eq!(h.last_content(), "(╯°□°)╯︵ ┻━┻");
}

/// Per-call cost of a message hook through the whole pipeline (worker
/// thread, MessagePack, host calls). `cargo test --release --test
/// plugins_examples -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_message_hook() {
    use ferrocord::plugins::abi;
    use ferrocord::plugins::manifest::Manifest;
    use ferrocord::plugins::worker::{LoadSpec, Source, Worker};
    let Some(dir) = install() else { return };
    let m = Manifest::parse(
        &std::fs::read_to_string(dir.path().join("word-highlighter/plugin.toml")).unwrap(),
    )
    .unwrap();
    let (tx, mut rx) = mpsc::channel(100_000);
    let mut w = Worker::new(None, tx, mock::now_ms);
    w.job(ferrocord::plugins::worker::Job::Load(Box::new(LoadSpec {
        manifest: m,
        source: Source::Path(dir.path().join("word-highlighter")),
        settings: Default::default(),
    })));
    let ev = abi::Event::MessageCreate {
        message: abi::MessageInfo {
            id: 1,
            channel_id: 2,
            author_name: "Ann".into(),
            content: "I really like writing Rust code for Ferrocord plugins, it is fast".into(),
            ..Default::default()
        },
    };
    for _ in 0..200 {
        w.event(&ev);
    }
    let n = 5000;
    let t = Instant::now();
    for _ in 0..n {
        w.event(&ev);
        while rx.try_recv().is_ok() {}
    }
    println!(
        "word-highlighter message_create: {:.1} us/call",
        t.elapsed().as_secs_f64() * 1e6 / f64::from(n)
    );
}
