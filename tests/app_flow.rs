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

#[test]
fn masked_links_need_confirmation() {
    let mut a = app();
    let _ = a.update(Message::Ui(Ui::ConfirmLink(
        "https://example.com/real".into(),
    )));
    assert_eq!(a.pending_link.as_deref(), Some("https://example.com/real"));
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Ui(Ui::CancelLink));
    assert!(a.pending_link.is_none());
    let _ = a.update(Message::Ui(Ui::ConfirmLink(
        "https://example.com/real".into(),
    )));
    let _ = a.update(Message::Ui(Ui::OpenUrl("https://example.com/real".into())));
    assert!(a.pending_link.is_none());
}

#[test]
fn markdown_is_parsed_once_per_message() {
    let a = app();
    let id = ferrocord::net::model::Snowflake(42);
    let first = a.parsed_markdown(id, "**hi**");
    let second = a.parsed_markdown(id, "**hi**");
    assert!(std::rc::Rc::ptr_eq(&first, &second));
    // Edited content gets a fresh parse.
    let edited = a.parsed_markdown(id, "**hi** there");
    assert!(!std::rc::Rc::ptr_eq(&first, &edited));
}

#[test]
fn inline_custom_emoji_render_as_images_when_loaded() {
    let mut a = app();
    // The mock #general history ends with inline `<:ferris:1001>`.
    let key = ferrocord::app::emoji_url(&a.cfg.cdn_url, ferrocord::net::model::Snowflake(1001));
    a.images.insert(
        key.clone(),
        iced::widget::image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]),
        4,
    );
    assert!(a.image(&key).is_some());
    let _ = ferrocord::ui::view(&a);
}

// -- round 2: replies, edit/delete, reactions, uploads, members, search ------------

use ferrocord::app::Done;
use ferrocord::app::emoji::{PickerTarget, Section};
use ferrocord::app::upload::UploadEvent;
use ferrocord::net::model::{Emoji, Message as Msg, Presence, Snowflake};
use ferrocord::net::rest::UploadFile;

fn ui(a: &mut App, u: Ui) {
    let _ = a.update(Message::Ui(u));
}

fn window(a: &App) -> Vec<&Msg> {
    a.chat.messages[&mock::CH_GENERAL].messages.iter().collect()
}

fn last_of(a: &App, user: u64) -> Msg {
    window(a)
        .into_iter()
        .rev()
        .find(|m| m.author.id == Snowflake(user))
        .cloned()
        .expect("message by user")
}

#[test]
fn reply_flow_from_bar_to_pending_message() {
    let mut a = app();
    let target = last_of(&a, 3); // Bob
    ui(&mut a, Ui::StartReply(target.id));
    let r = a.reply.clone().expect("reply draft");
    assert_eq!(r.message, target.id);
    assert!(r.mention, "ping is on by default");
    ui(&mut a, Ui::ToggleReplyMention);
    assert!(!a.reply.as_ref().unwrap().mention);
    let _ = ferrocord::ui::view(&a);

    ui(&mut a, Ui::ComposerChanged("on it".into()));
    ui(&mut a, Ui::Send);
    assert!(a.reply.is_none(), "reply bar closes after sending");
    let pending = a.chat.messages[&mock::CH_GENERAL].messages.back().unwrap();
    assert!(pending.pending);
    assert_eq!(pending.reply_to, Some(target.id));
    assert_eq!(pending.kind, 19);
    let preview = a.chat.reply_preview(pending).unwrap();
    assert_eq!(preview.author.unwrap().id, Snowflake(3));
    assert!(!preview.deleted);
    // The reply header renders.
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn replying_to_yourself_never_pings_and_escape_cancels() {
    let mut a = app();
    let mine = last_of(&a, 1);
    ui(&mut a, Ui::StartReply(mine.id));
    assert!(!a.reply.as_ref().unwrap().mention);
    ui(&mut a, Ui::ToggleReplyMention);
    assert!(!a.reply.as_ref().unwrap().mention, "toggle is a no-op");
    ui(&mut a, Ui::Escape);
    assert!(a.reply.is_none());
}

#[test]
fn reply_draft_is_dropped_when_switching_channels() {
    let mut a = app();
    let target = last_of(&a, 3);
    ui(&mut a, Ui::StartReply(target.id));
    ui(&mut a, Ui::SelectChannel(mock::CH_HELP));
    assert!(a.reply.is_none());
}

#[test]
fn edit_flow_up_arrow_save_and_edited_marker() {
    let mut a = app();
    let mine = last_of(&a, 1);
    // Up arrow in an empty composer edits the last own message.
    ui(&mut a, Ui::EditLast);
    let e = a.editing.clone().expect("editing");
    assert_eq!(e.message, mine.id);
    assert_eq!(e.text, mine.content);
    let _ = ferrocord::ui::view(&a);
    ui(
        &mut a,
        Ui::EditChanged("agreed, and `let` chains are great".into()),
    );
    ui(&mut a, Ui::SubmitEdit);
    assert!(a.editing.is_none());
    let m = a.chat.message(mock::CH_GENERAL, mine.id).unwrap();
    assert_eq!(m.content, "agreed, and `let` chains are great");
    assert!(m.edited);
}

#[test]
fn edit_rules() {
    let mut a = app();
    // Not your message: ignored.
    let bobs = last_of(&a, 3);
    ui(&mut a, Ui::StartEdit(bobs.id));
    assert!(a.editing.is_none());
    // Up arrow does nothing while the composer has text.
    ui(&mut a, Ui::ComposerChanged("draft".into()));
    ui(&mut a, Ui::EditLast);
    assert!(a.editing.is_none());
    ui(&mut a, Ui::ComposerChanged(String::new()));

    let mine = last_of(&a, 1);
    // Unchanged text closes the editor without marking it edited.
    ui(&mut a, Ui::StartEdit(mine.id));
    ui(&mut a, Ui::SubmitEdit);
    assert!(a.editing.is_none());
    assert_eq!(
        a.chat.message(mock::CH_GENERAL, mine.id).unwrap().edited,
        mine.edited
    );
    // Too long: stays open with a toast.
    ui(&mut a, Ui::StartEdit(mine.id));
    ui(&mut a, Ui::EditChanged("x".repeat(2001)));
    ui(&mut a, Ui::SubmitEdit);
    assert!(a.editing.is_some());
    assert!(a.toast.is_some());
    // Emptying the message offers to delete it instead.
    ui(&mut a, Ui::EditChanged("  ".into()));
    ui(&mut a, Ui::SubmitEdit);
    assert!(a.editing.is_none());
    assert_eq!(a.pending_delete, Some((mock::CH_GENERAL, mine.id)));
    ui(&mut a, Ui::CancelDelete);
    // Escape cancels an edit.
    ui(&mut a, Ui::StartEdit(mine.id));
    ui(&mut a, Ui::Escape);
    assert!(a.editing.is_none());
}

/// One test, because it toggles the process-wide Shift state.
#[test]
fn delete_confirms_unless_shift_is_held() {
    use ferrocord::app::input::set_shift_for_tests;
    let mut a = app();
    let mine = last_of(&a, 1);
    let bobs = last_of(&a, 3);
    set_shift_for_tests(false);
    // Others' messages cannot be deleted.
    ui(&mut a, Ui::DeleteClicked(bobs.id));
    assert!(a.pending_delete.is_none());
    // Plain click opens the confirmation (which renders).
    ui(&mut a, Ui::DeleteClicked(mine.id));
    assert_eq!(a.pending_delete, Some((mock::CH_GENERAL, mine.id)));
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::CancelDelete);
    assert!(a.chat.message(mock::CH_GENERAL, mine.id).is_some());
    ui(&mut a, Ui::DeleteClicked(mine.id));
    ui(&mut a, Ui::ConfirmDelete);
    assert!(a.chat.message(mock::CH_GENERAL, mine.id).is_none());
    assert!(a.pending_delete.is_none());

    // Shift+click skips the confirmation.
    let again = mock::text_message(
        mock::CH_GENERAL,
        Snowflake::from_timestamp_ms(mock::now_ms(), 77),
        mock::user(1),
        "delete me fast",
    );
    let id = again.id;
    a.chat
        .messages
        .get_mut(&mock::CH_GENERAL)
        .unwrap()
        .upsert(again);
    set_shift_for_tests(true);
    ui(&mut a, Ui::DeleteClicked(id));
    set_shift_for_tests(false);
    assert!(a.pending_delete.is_none());
    assert!(a.chat.message(mock::CH_GENERAL, id).is_none());
}

#[test]
fn context_menu_opens_and_closes() {
    let mut a = app();
    let m = last_of(&a, 1);
    ui(&mut a, Ui::ContextMenu(m.id));
    assert_eq!(a.menu.unwrap().message, m.id);
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::Escape);
    assert!(a.menu.is_none());
    // Opening the reaction picker from the menu replaces the menu.
    ui(&mut a, Ui::ContextMenu(m.id));
    ui(
        &mut a,
        Ui::OpenEmojiPicker(PickerTarget::Reaction {
            channel: mock::CH_GENERAL,
            message: m.id,
        }),
    );
    assert!(a.menu.is_none() && a.emoji_picker.is_some());
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn reactions_toggle_and_picker_flows() {
    let mut a = app();
    let m = last_of(&a, 3);
    let thumbs = Emoji::unicode("👍");
    ui(&mut a, Ui::ToggleReaction(m.id, thumbs.clone()));
    let r = &a.chat.message(mock::CH_GENERAL, m.id).unwrap().reactions;
    assert_eq!((r[0].count, r[0].me), (1, true));
    ui(&mut a, Ui::ToggleReaction(m.id, thumbs.clone()));
    assert!(
        a.chat
            .message(mock::CH_GENERAL, m.id)
            .unwrap()
            .reactions
            .is_empty()
    );

    // Picker opened from the toolbar reacts to that message.
    ui(
        &mut a,
        Ui::OpenEmojiPicker(PickerTarget::Reaction {
            channel: mock::CH_GENERAL,
            message: m.id,
        }),
    );
    // The mock server has custom emoji, so it opens there.
    assert!(matches!(
        a.emoji_picker.as_ref().unwrap().picker.section,
        Section::Server(_)
    ));
    ui(&mut a, Ui::EmojiQuery("crab".into()));
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::PickEmoji(Emoji::unicode("🦀")));
    assert!(a.emoji_picker.is_none());
    assert_eq!(
        a.chat.message(mock::CH_GENERAL, m.id).unwrap().reactions[0].emoji,
        Emoji::unicode("🦀")
    );

    // Picker opened from the composer inserts text, custom emoji as markup.
    ui(&mut a, Ui::OpenEmojiPicker(PickerTarget::Composer));
    ui(&mut a, Ui::EmojiSection(Section::Unicode(3)));
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::PickEmoji(Emoji::unicode("🍕")));
    ui(&mut a, Ui::OpenEmojiPicker(PickerTarget::Composer));
    ui(
        &mut a,
        Ui::PickEmoji(Emoji {
            id: Some(Snowflake(1001)),
            name: "ferris".into(),
            animated: false,
        }),
    );
    assert_eq!(a.composer, "🍕 <:ferris:1001> ");
}

#[test]
fn failed_reaction_request_is_reverted_with_a_toast() {
    let mut a = app();
    let m = last_of(&a, 3);
    let e = Emoji::unicode("🎈");
    ui(&mut a, Ui::ToggleReaction(m.id, e.clone()));
    let _ = a.update(Message::Done(Done::ReactionFailed {
        channel: mock::CH_GENERAL,
        message: m.id,
        emoji: e,
        change: ferrocord::app::state::ReactionChange::Added,
        error: "Unknown Emoji".into(),
    }));
    assert!(
        a.chat
            .message(mock::CH_GENERAL, m.id)
            .unwrap()
            .reactions
            .is_empty()
    );
    assert!(a.toast.as_deref().unwrap().contains("Unknown Emoji"));
}

fn file(name: &str, size: u64) -> Result<(UploadFile, bool), String> {
    Ok((
        UploadFile {
            path: format!("/tmp/{name}").into(),
            name: name.into(),
            size,
        },
        false,
    ))
}

#[test]
fn attachments_upload_with_progress_and_caption() {
    let mut a = app();
    let _ = a.update(Message::Done(Done::FilesReady(vec![
        file("shot.png", 2_000),
        file("huge.bin", 11 * 1024 * 1024),
        file("empty.txt", 0),
    ])));
    // The oversized and empty files are refused with a toast.
    assert_eq!(a.draft.len(), 1);
    assert!(a.toast.is_some());
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::ComposerChanged("look at this".into()));
    ui(&mut a, Ui::Send);
    assert!(a.draft.is_empty() && a.composer.is_empty());
    assert_eq!(a.uploads.len(), 1);
    let id = a.uploads[0].id;
    assert_eq!(a.uploads[0].total, 2_000);
    let _ = ferrocord::ui::view(&a);

    let _ = a.update(Message::Done(Done::Upload {
        id,
        event: UploadEvent::Progress {
            sent: 500,
            total: 2_000,
        },
    }));
    assert!((a.uploads[0].fraction() - 0.25).abs() < 1e-6);

    let mut m = mock::text_message(
        mock::CH_GENERAL,
        Snowflake::from_timestamp_ms(mock::now_ms(), 9),
        mock::user(1),
        "look at this",
    );
    m.attachments = vec![];
    let _ = a.update(Message::Done(Done::Upload {
        id,
        event: UploadEvent::Done(Ok(Box::new(m.clone()))),
    }));
    assert!(a.uploads.is_empty());
    assert!(a.chat.message(mock::CH_GENERAL, m.id).is_some());
}

#[test]
fn upload_errors_and_cancel() {
    let mut a = app();
    let _ = a.update(Message::Done(Done::FilesReady(vec![file("a.txt", 10)])));
    ui(&mut a, Ui::Send);
    let id = a.uploads[0].id;
    let _ = a.update(Message::Done(Done::Upload {
        id,
        event: UploadEvent::Done(Err("Request Entity Too Large".into())),
    }));
    assert!(a.uploads.is_empty());
    assert_eq!(a.toast.as_deref(), Some("Request Entity Too Large"));

    let _ = a.update(Message::Done(Done::FilesReady(vec![file("b.txt", 10)])));
    ui(&mut a, Ui::Send);
    let id = a.uploads[0].id;
    ui(&mut a, Ui::CancelUpload(id));
    assert!(a.uploads.is_empty());
    // Removing a draft file.
    let _ = a.update(Message::Done(Done::FilesReady(vec![file("c.txt", 10)])));
    assert_eq!(a.draft.len(), 1);
    ui(&mut a, Ui::RemoveAttachment(0));
    assert!(a.draft.is_empty());
    // Dropped files are ignored while editing.
    let mine = last_of(&a, 1).id;
    ui(&mut a, Ui::StartEdit(mine));
    ui(&mut a, Ui::FileDropped("/tmp/x.png".into()));
}

#[test]
fn member_list_shows_groups_and_presence() {
    let mut a = app();
    let l = a.chat.member_list.as_ref().expect("mock member list");
    assert_eq!(l.loaded().count(), 11);
    assert_eq!(a.chat.presence_of(Snowflake(3)), Presence::Idle);
    assert_eq!(a.chat.presence_of(Snowflake(6)), Presence::Offline);
    let _ = ferrocord::ui::view(&a);
    // The panel can be toggled off and on.
    ui(&mut a, Ui::ToggleMembers);
    assert!(!a.members_open);
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::ToggleMembers);
    assert!(a.members_open);
    // DMs have no member list.
    ui(&mut a, Ui::SelectChannel(mock::DM_ALICE));
    assert!(a.chat.member_list.is_none());
    let _ = ferrocord::ui::view(&a);
    // Back in the server it is rebuilt.
    ui(&mut a, Ui::SelectChannel(mock::CH_GENERAL));
    assert!(a.chat.member_list.is_some());
}

#[test]
fn own_status_can_be_changed() {
    let mut a = app();
    assert_eq!(a.chat.my_status, Presence::Online);
    ui(&mut a, Ui::ToggleStatusMenu);
    assert!(a.status_menu);
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::SetStatus(Presence::Dnd));
    assert_eq!(a.chat.my_status, Presence::Dnd);
    assert!(!a.status_menu);
    assert_eq!(a.chat.presence_of(Snowflake(1)), Presence::Dnd);
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn search_results_and_jump_to_message() {
    let mut a = app();
    ui(&mut a, Ui::SearchChanged("let".into()));
    assert!(a.search.open);
    ui(&mut a, Ui::SubmitSearch);
    assert!(a.search.panel_visible());
    assert!(a.search.total >= 2, "total {}", a.search.total);
    let _ = ferrocord::ui::view(&a);
    let hit = a.search.results[0].clone();

    // Jump from another channel: switches back and flashes the message.
    ui(&mut a, Ui::SelectChannel(mock::CH_HELP));
    ui(
        &mut a,
        Ui::JumpTo {
            channel: hit.channel_id,
            message: hit.id,
        },
    );
    assert_eq!(a.open_channel(), Some(hit.channel_id));
    assert_eq!(a.highlight, Some(hit.id));
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Done(Done::ClearHighlight(hit.id)));
    assert!(a.highlight.is_none());

    // Escape closes the search panel; blank queries do nothing.
    ui(&mut a, Ui::SearchChanged("   ".into()));
    ui(&mut a, Ui::SubmitSearch);
    ui(&mut a, Ui::Escape);
    assert!(!a.search.panel_visible() && !a.search.open);
}

#[test]
fn search_in_dms_and_no_results() {
    let mut a = app();
    ui(&mut a, Ui::SelectChannel(mock::DM_ALICE));
    ui(&mut a, Ui::SearchChanged("iced".into()));
    ui(&mut a, Ui::SubmitSearch);
    assert!(a.search.total >= 1);
    for m in &a.search.results {
        assert_eq!(m.channel_id, mock::DM_ALICE);
    }
    ui(&mut a, Ui::SearchChanged("zzzz-nothing".into()));
    ui(&mut a, Ui::SubmitSearch);
    assert_eq!(a.search.total, 0);
    let _ = ferrocord::ui::view(&a);
    // Home without an open DM cannot search.
    ui(&mut a, Ui::CloseSearch);
    ui(&mut a, Ui::SelectHome);
}

#[test]
fn notification_click_opens_the_channel() {
    let mut a = app();
    ui(&mut a, Ui::OpenChannel(mock::DM_ALICE));
    assert_eq!(a.view, View::Home(Some(mock::DM_ALICE)));
    ui(&mut a, Ui::OpenChannel(mock::CH_RANDOM));
    assert_eq!(a.open_channel(), Some(mock::CH_RANDOM));
    assert!(matches!(a.view, View::Guild { guild, .. } if guild == mock::GUILD_RUST));
    // A channel that no longer exists is ignored.
    ui(&mut a, Ui::OpenChannel(Snowflake(999_999)));
    assert_eq!(a.open_channel(), Some(mock::CH_RANDOM));
}

#[test]
fn jump_to_present_reloads_a_detached_window() {
    let mut a = app();
    a.chat.messages.get_mut(&mock::CH_GENERAL).unwrap().detached = true;
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::JumpToPresent);
    let w = &a.chat.messages[&mock::CH_GENERAL];
    assert!(!w.detached);
}

#[test]
fn live_messages_are_not_appended_to_a_detached_window() {
    let mut a = app();
    a.chat.messages.get_mut(&mock::CH_GENERAL).unwrap().detached = true;
    let before = a.chat.messages[&mock::CH_GENERAL].messages.len();
    let m = mock::text_message(
        mock::CH_GENERAL,
        Snowflake::from_timestamp_ms(mock::now_ms() + 5_000, 1),
        mock::user(3),
        "new live message",
    );
    let _ = a.update(Message::Net(ferrocord::net::Event::MessageCreate(
        Box::new(m),
    )));
    assert_eq!(a.chat.messages[&mock::CH_GENERAL].messages.len(), before);
}

// -- tray and background mode ----------------------------------------------------

mod tray_flow {
    use std::sync::Arc;

    use ferrocord::app::{App, Message, Ui};
    use ferrocord::config::Config;
    use ferrocord::mock;
    use ferrocord::net::model::Presence;
    use ferrocord::tray::{self, Command, TrayModel};
    use tokio::sync::watch;

    fn app_with_tray() -> (App, watch::Receiver<TrayModel>) {
        let mut a = App::boot(Config::for_tests()).0;
        let (tx, rx) = watch::channel(TrayModel::default());
        let control = tray::Control::new(Arc::new(tx));
        let _ = a.update(Message::Tray(tray::Event::Ready(control)));
        (a, rx)
    }

    fn command(a: &mut App, c: Command) {
        let _ = a.update(Message::Tray(tray::Event::Command(c)));
    }

    #[test]
    fn closing_hides_to_the_tray_once_registered() {
        let (mut a, _rx) = app_with_tray();
        assert!(a.win.visible);
        let _ = a.update(Message::Ui(Ui::CloseRequested));
        assert!(!a.win.visible, "close hides while a tray exists");
        // Notifications must not be suppressed for a window nobody sees.
        let _ = a.update(Message::Ui(Ui::ShowWindow));
        assert!(a.win.visible);
    }

    #[test]
    fn left_click_toggles_the_window() {
        let (mut a, _rx) = app_with_tray();
        command(&mut a, Command::Toggle);
        assert!(!a.win.visible);
        command(&mut a, Command::Toggle);
        assert!(a.win.visible);
        command(&mut a, Command::Hide);
        assert!(!a.win.visible);
        command(&mut a, Command::Show);
        assert!(a.win.visible);
    }

    #[test]
    fn without_a_tray_closing_does_not_hide() {
        let mut a = App::boot(Config::for_tests()).0;
        let _ = a.update(Message::Ui(Ui::CloseRequested));
        // `iced::exit()` was requested; the window state keeps "visible".
        assert!(a.win.visible);
        let _ = a.update(Message::Ui(Ui::HideWindow));
        assert!(a.win.visible, "hiding needs a tray host");
    }

    #[test]
    fn losing_the_tray_restores_a_hidden_window() {
        let (mut a, _rx) = app_with_tray();
        command(&mut a, Command::Hide);
        assert!(!a.win.visible);
        let _ = a.update(Message::Tray(tray::Event::Unavailable));
        assert!(a.win.visible);
        let _ = a.update(Message::Ui(Ui::CloseRequested));
        assert!(a.win.visible, "no tray any more: close quits");
    }

    #[test]
    fn minimized_start_waits_for_the_tray() {
        let mut cfg = Config::for_tests();
        cfg.tray = true;
        cfg.minimized = true;
        let mut a = App::boot(cfg.clone()).0;
        assert!(!a.win.visible, "--minimized opens no window");
        let _ = a.update(Message::Tray(tray::Event::Unavailable));
        assert!(a.win.visible, "no tray host: show the window");
        // --minimized is ignored without a tray at all.
        cfg.tray = false;
        assert!(App::boot(cfg).0.win.visible);
    }

    #[test]
    fn menu_status_uses_the_status_setter() {
        let (mut a, rx) = app_with_tray();
        command(&mut a, Command::SetStatus(Presence::Dnd));
        assert_eq!(a.chat.my_status, Presence::Dnd);
        assert_eq!(rx.borrow().status, Presence::Dnd);
        assert!(rx.borrow().logged_in);
    }

    #[test]
    fn menu_toggles_update_prefs_and_tray() {
        let (mut a, rx) = app_with_tray();
        assert!(!a.prefs.muted);
        command(&mut a, Command::ToggleMute);
        assert!(a.prefs.muted);
        assert!(rx.borrow().muted);
        command(&mut a, Command::ToggleStartMinimized);
        assert!(a.prefs.start_minimized && rx.borrow().start_minimized);
        command(&mut a, Command::ToggleMute);
        assert!(!rx.borrow().muted);
    }

    #[test]
    fn tray_shows_the_unread_mention_count() {
        let (mut a, rx) = app_with_tray();
        // The mock account has unread mentions and DMs.
        let n = a.chat.attention_count();
        assert!(n > 0);
        assert_eq!(rx.borrow().mentions, n);
        assert!(rx.borrow().needs_attention());
        assert!(rx.borrow().tooltip_title().contains(&n.to_string()));
        // Reading a mention-carrying channel lowers it.
        let (c, k) = [mock::DM_ALICE, mock::CH_HELP]
            .into_iter()
            .map(|c| (c, a.chat.mention_count(c)))
            .find(|(_, k)| *k > 0)
            .unwrap();
        let _ = a.update(Message::Ui(Ui::SelectChannel(c)));
        assert_eq!(rx.borrow().mentions, n - k);
    }

    #[test]
    fn clicking_a_notification_shows_the_hidden_window_on_that_chat() {
        let (mut a, _rx) = app_with_tray();
        command(&mut a, Command::Hide);
        assert!(!a.win.visible);
        let _ = a.update(Message::Ui(Ui::OpenChannel(mock::CH_HELP)));
        assert!(a.win.visible);
        assert_eq!(a.open_channel(), Some(mock::CH_HELP));
    }

    #[test]
    fn settings_toggles_flip_preferences() {
        let mut a = App::boot(Config::for_tests()).0;
        assert!(a.prefs.close_to_tray);
        let _ = a.update(Message::Ui(Ui::OpenSettings));
        assert!(a.settings_open);
        let _ = a.update(Message::Ui(Ui::ToggleCloseToTray));
        assert!(!a.prefs.close_to_tray);
        assert!(!a.win.close_to_tray);
        // Mock mode flips the autostart toggle without touching the disk.
        let _ = a.update(Message::Ui(Ui::ToggleAutostart));
        assert!(a.autostart);
        let _ = a.update(Message::Ui(Ui::Escape));
        assert!(!a.settings_open);
    }
}
