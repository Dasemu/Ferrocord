//! App-level flows driven through `App::update` in mock mode (no window).

use ferrocord::app::{App, Message, Screen, Ui, View};
use ferrocord::config::Config;
use ferrocord::mock;

fn app() -> App {
    App::boot(Config::for_tests()).0
}

#[test]
fn mock_boot_lands_in_a_guild_channel() {
    let a = app();
    assert_eq!(a.screen(), Screen::Main);
    assert_eq!(a.open_channel(), Some(mock::CH_GENERAL));
    assert!(a.title().contains("#general"));
}

#[test]
fn selecting_channels_marks_them_read() {
    let mut a = app();
    assert!(a.chat.is_unread(mock::CH_HELP));
    let _ = a.update(Message::Ui(Ui::SelectChannel(mock::CH_HELP)));
    assert_eq!(a.open_channel(), Some(mock::CH_HELP));
    assert!(!a.chat.is_unread(mock::CH_HELP));
    assert_eq!(a.chat.mention_count(mock::CH_HELP), 0);

    let _ = a.update(Message::Ui(Ui::SelectHome));
    assert!(matches!(a.view, View::Home(Some(_))));
    let _ = a.update(Message::Ui(Ui::SelectChannel(mock::DM_ALICE)));
    assert_eq!(a.view, View::Home(Some(mock::DM_ALICE)));
    assert_eq!(a.chat.mention_count(mock::DM_ALICE), 0);
}

#[test]
fn voice_channels_cannot_be_opened() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::SelectChannel(
        ferrocord::net::model::Snowflake(140),
    )));
    assert_eq!(a.open_channel(), Some(mock::CH_GENERAL));
}

#[test]
fn sending_adds_a_pending_message_and_clears_composer() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::ComposerChanged("hello **there**".into())));
    let _ = a.update(Message::Ui(Ui::Send));
    assert!(a.composer.is_empty());
    let last = a.chat.messages[&mock::CH_GENERAL].messages.back().unwrap();
    assert!(last.pending);
    assert_eq!(last.content, "hello **there**");
    // Blank messages are ignored.
    let before = a.chat.messages[&mock::CH_GENERAL].messages.len();
    let _ = a.update(Message::Ui(Ui::ComposerChanged("   ".into())));
    let _ = a.update(Message::Ui(Ui::Send));
    assert_eq!(a.chat.messages[&mock::CH_GENERAL].messages.len(), before);
}

#[test]
fn categories_collapse_and_spoilers_reveal() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::ToggleCategory(mock::CAT_INFO)));
    assert!(a.collapsed.contains(&mock::CAT_INFO));
    let _ = a.update(Message::Ui(Ui::ToggleCategory(mock::CAT_INFO)));
    assert!(!a.collapsed.contains(&mock::CAT_INFO));
    let id = a.chat.messages[&mock::CH_GENERAL]
        .messages
        .back()
        .unwrap()
        .id;
    let _ = a.update(Message::Ui(Ui::RevealSpoiler(id)));
    assert!(a.revealed_spoilers.contains(&id));
}

#[test]
fn logout_returns_to_login() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::Logout));
    assert_eq!(a.screen(), Screen::Login);
    assert!(a.chat.guilds.is_empty());
}

#[test]
fn views_render_without_panicking() {
    let mut a = app();
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Ui(Ui::SelectHome));
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Ui(Ui::Logout));
    let _ = ferrocord::ui::view(&a);
}
