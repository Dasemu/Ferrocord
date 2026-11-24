//! With no plugin enabled the plugin system costs nothing: no wasm engine,
//! no plugin thread, no subscription. Its own test binary, so no other
//! test can have created an engine in this process.

use ferrocord::app::App;
use ferrocord::config::Config;
use ferrocord::plugins::{runtime, state};

#[cfg(target_os = "linux")]
fn plugin_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|c| c.trim() == "ferrocord-plugi")
        .count()
}

#[cfg(not(target_os = "linux"))]
fn plugin_threads() -> usize {
    0
}

#[test]
fn startup_without_plugins_creates_no_runtime() {
    // No plugins folder at all.
    let (app, _) = App::boot(Config::for_tests());
    assert!(!app.plugins.is_running());
    assert!(app.plugins.receiver().is_none());

    // A plugins folder with plugins installed but none enabled (the state
    // file lists only disabled ones).
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("p")).unwrap();
    let mut s = state::State::default();
    s.plugins.insert("p".into(), state::Record::default());
    s.save(dir.path()).unwrap();
    let mut cfg = Config::for_tests();
    cfg.plugins_dir = Some(dir.path().to_path_buf());
    let (mut app, _) = App::boot(cfg);
    assert!(app.plugins.may_have_enabled());
    assert!(!app.plugins.is_running());
    // What the boot task delivers: nothing is enabled, nothing starts.
    let _ = app.update(ferrocord::app::Message::Plugin(
        ferrocord::app::plugins::PluginMsg::Scanned {
            state: Some(Box::new(state::State::load(dir.path()))),
            scan: Box::new(ferrocord::plugins::package::scan(dir.path())),
            boot: true,
            reload: false,
        },
    ));
    assert!(!app.plugins.is_running());

    assert_eq!(runtime::engines_created(), 0, "a wasm engine was created");
    assert_eq!(plugin_threads(), 0, "a plugin thread was started");
}
