//! Updater flows through `App::update` in mock mode (no network, no disk).

use ferrocord::app::settings::{Category, SettingsUi};
use ferrocord::app::{App, Message, Ui};
use ferrocord::config::Config;
use ferrocord::update::{
    Banner, CheckStatus, InstallMethod, Phase, Release, ScriptLayout, UpdateMsg, Version,
};
use iced::advanced::widget::Tree;

fn app() -> App {
    App::boot(Config::for_tests()).0
}

fn newer(a: &App) -> Release {
    let v = Version {
        minor: a.updater.current.minor + 1,
        patch: 0,
        ..a.updater.current
    };
    Release {
        tag: format!("v{v}"),
        version: v,
        html_url: format!("https://github.com/FerroApps/Ferrocord/releases/tag/v{v}"),
        assets: vec![],
    }
}

fn found(a: &mut App, method: InstallMethod) {
    let release = newer(a);
    let _ = a.update(Message::Update(UpdateMsg::Checked {
        manual: false,
        method,
        exe: None,
        result: Ok(ferrocord::update::CheckOutcome::Release {
            release,
            etag: None,
        }),
    }));
}

fn script() -> InstallMethod {
    InstallMethod::Script(ScriptLayout {
        bin_dir: "/h/.local/bin".into(),
        data_dir: "/h/.local/share".into(),
    })
}

#[test]
fn nothing_runs_in_mock_mode() {
    let mut a = app();
    assert!(!a.updater.allowed);
    assert_eq!(a.updater.status, CheckStatus::Off);
    assert_eq!(a.updater.banner(), None);
    let _ = a.update(Message::Update(UpdateMsg::CheckNow));
    assert_eq!(a.updater.phase, Phase::Idle, "no check started");
    assert_eq!(a.updater.status, CheckStatus::Off);
    // Timer ticks are ignored, and Update does nothing without permission.
    let _ = a.update(Message::Update(UpdateMsg::Tick(a.updater.timer)));
    assert_eq!(a.updater.phase, Phase::Idle);
    found(&mut a, script());
    let _ = a.update(Message::Update(UpdateMsg::Start));
    assert_eq!(a.updater.phase, Phase::Idle);
}

#[test]
fn banner_appears_and_dismissal_sticks() {
    let mut a = app();
    found(&mut a, script());
    let v = newer(&a).version;
    assert_eq!(
        a.updater.banner(),
        Some(Banner::Available {
            version: v,
            self_update: true,
            cargo: false
        })
    );
    // The banner renders in the main view.
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Update(UpdateMsg::Dismiss));
    assert_eq!(a.updater.dismissed, Some(v));
    assert_eq!(a.updater.banner(), None);
    // Found again later: still dismissed.
    found(&mut a, script());
    assert_eq!(a.updater.banner(), None);
}

#[test]
fn cargo_installs_get_the_command_not_an_update_button() {
    let mut a = app();
    found(&mut a, InstallMethod::Cargo);
    assert!(matches!(
        a.updater.banner(),
        Some(Banner::Available {
            cargo: true,
            self_update: false,
            ..
        })
    ));
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn settings_updates_page_and_auto_toggle() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::OpenSettings));
    let _ = a.update(Message::Ui(Ui::Settings(SettingsUi::Select(
        Category::Updates,
    ))));
    assert_eq!(a.settings.category(), Category::Updates);
    found(&mut a, script());
    let _ = ferrocord::ui::view(&a);
    assert!(a.updater.auto);
    let t = a.updater.timer;
    let _ = a.update(Message::Update(UpdateMsg::ToggleAuto));
    assert!(!a.updater.auto);
    assert!(a.updater.timer > t, "the pending timer is invalidated");
    let _ = a.update(Message::Update(UpdateMsg::ToggleAuto));
    assert!(a.updater.auto);
}

#[test]
fn restart_prompt_and_later() {
    let mut a = app();
    found(&mut a, script());
    a.updater.phase = Phase::Downloading {
        received: 0,
        total: None,
    };
    let v = newer(&a).version;
    let _ = a.update(Message::Update(UpdateMsg::Install(
        ferrocord::update::InstallEvent::Done(Ok(v)),
    )));
    assert_eq!(a.updater.banner(), Some(Banner::Ready(v)));
    let _ = ferrocord::ui::view(&a);
    // Restart in mock mode never spawns anything.
    let _ = a.update(Message::Update(UpdateMsg::Restart));
    let _ = a.update(Message::Update(UpdateMsg::Later));
    assert_eq!(a.updater.banner(), None);
    assert!(matches!(a.updater.phase, Phase::Ready { .. }));
}

#[test]
fn cancel_stops_a_download() {
    let mut a = app();
    a.updater.phase = Phase::Downloading {
        received: 10,
        total: Some(100),
    };
    let _ = a.update(Message::Update(UpdateMsg::Cancel));
    assert_eq!(a.updater.phase, Phase::Idle);
    // A late progress event after cancel is ignored.
    let _ = a.update(Message::Update(UpdateMsg::Install(
        ferrocord::update::InstallEvent::Progress(20, Some(100)),
    )));
    assert_eq!(a.updater.phase, Phase::Idle);
}

/// The banner has a fixed slot: showing it keeps the body widget tree.
#[test]
fn banner_keeps_the_body_widget_tree() {
    fn shape(t: &Tree) -> String {
        let kids: Vec<String> = t.children.iter().map(shape).collect();
        format!("{:?}[{}]", t.tag, kids.join(","))
    }
    let mut a = app();
    let el = ferrocord::ui::view(&a);
    let mut tree = Tree::new(&el);
    drop(el);
    let before = shape(&tree.children[0].children[1]);
    found(&mut a, script());
    let el = ferrocord::ui::view(&a);
    tree.diff(&el);
    drop(el);
    assert_eq!(shape(&tree.children[0].children[1]), before);
}
