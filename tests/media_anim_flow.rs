//! Animated media in messages: the mock GIF attachment plays by itself while
//! autoplay is on and the window is focused, and the settings turn it off.

use ferrocord::app::anim::Class;
use ferrocord::app::media_anim::message_media;
use ferrocord::app::{App, Message, Ui};
use ferrocord::config::Config;
use ferrocord::mock;

fn showcase() -> App {
    let (mut a, _) = App::boot(Config::for_tests());
    let _ = a.update(Message::Ui(Ui::SelectGuild(mock::GUILD_RUST)));
    let _ = a.update(Message::Ui(Ui::SelectChannel(mock::CH_SHOWCASE)));
    a
}

fn ui(a: &mut App, u: Ui) {
    let _ = a.update(Message::Ui(u));
}

#[test]
fn the_mock_gif_autoplays_while_focused() {
    let mut a = showcase();
    assert!(a.prefs.autoplay_gifs, "autoplay is on by default");
    let anim = a.media_anim(mock::MOCK_ANIM_GIF).expect("mock GIF decoded");
    assert!(anim.frames.len() > 1);
    assert!(a.media_playing());
    // The conversation renders with the self-playing widget in it.
    let _ = ferrocord::ui::view(&a);

    // Unfocused: still drawn animated (first frame), but not playing.
    ui(&mut a, Ui::WindowFocus(false));
    assert!(a.media_anim(mock::MOCK_ANIM_GIF).is_some());
    assert!(!a.media_playing());
    ui(&mut a, Ui::WindowFocus(true));
    assert!(a.media_playing());
}

#[test]
fn autoplay_off_and_reduce_motion_show_the_still_image() {
    let mut a = showcase();
    ui(&mut a, Ui::ToggleAutoplayGifs);
    assert!(!a.prefs.autoplay_gifs);
    assert!(a.media_anim(mock::MOCK_ANIM_GIF).is_none());
    assert!(
        a.image(mock::MOCK_ANIM_GIF).is_some(),
        "the still frame stays"
    );
    let _ = ferrocord::ui::view(&a);
    ui(&mut a, Ui::ToggleAutoplayGifs);
    assert!(a.media_anim(mock::MOCK_ANIM_GIF).is_some());

    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(a.media_anim(mock::MOCK_ANIM_GIF).is_none());
    assert!(!a.media_playing());
    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(a.media_playing());
}

#[test]
fn gif_attachments_are_animation_candidates() {
    let a = showcase();
    let msgs = &a.chat.messages[&mock::CH_SHOWCASE].messages;
    let mut out = Vec::new();
    for m in msgs {
        message_media(m, &a.cfg.cdn_url, &mut out);
    }
    assert!(
        out.iter()
            .any(|(k, c)| k == mock::MOCK_ANIM_GIF && *c == Class::Media),
        "{out:?}"
    );
    // PNG screenshots are not.
    assert!(!out.iter().any(|(k, _)| k == "mock://image/1"));
}
