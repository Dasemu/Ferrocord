//! Profile popout, cosmetics and hover animation, driven through
//! `App::update` in mock mode (no window, no network).

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrocord::app::anim::{self, Class, Hover};
use ferrocord::app::cosmetics::Phase;
use ferrocord::app::{App, Done, Message, Ui, View};
use ferrocord::config::Config;
use ferrocord::mock;
use ferrocord::net::model::Snowflake;

const ALICE: Snowflake = Snowflake(2);
const BOB: Snowflake = Snowflake(3);

fn app() -> App {
    let (mut a, _) = App::boot(Config::for_tests());
    // Window geometry for popup placement.
    let _ = a.update(Message::Ui(Ui::SelectGuild(mock::GUILD_RUST)));
    a
}

fn ui(a: &mut App, u: Ui) {
    let _ = a.update(Message::Ui(u));
}

fn open(a: &mut App, user: Snowflake) {
    ui(
        a,
        Ui::OpenProfile {
            user,
            guild: Some(mock::GUILD_RUST),
        },
    );
}

#[test]
fn opening_a_profile_loads_it_and_starts_the_effect() {
    let mut a = app();
    assert!(a.profiles.popout.is_none());
    open(&mut a, ALICE);
    let (po, profile) = a.open_profile().expect("popout open");
    assert_eq!(po.user, ALICE);
    let p = profile.expect("mock profiles resolve immediately");
    assert!(p.bio.contains("Rustacean"));
    assert_eq!(p.badges.len(), 2);
    assert!(a.effects_ready(), "effect configs were requested");
    // Alice has an animated avatar, decoration, banner and an effect.
    assert!(a.anim.popout_hot);
    assert!(a.anim.active(false));
    assert!(!a.effect_layers().is_empty());
    // The whole UI renders with the popout open.
    let _ = ferrocord::ui::view(&a);
    // Closing stops the tick and frees effect + banner frames.
    ui(&mut a, Ui::CloseProfile);
    assert!(a.profiles.popout.is_none());
    assert!(!a.anim.active(false));
    assert!(!a.anim.cache.is_empty(), "avatar / decoration frames stay");
    let cdn = a.cfg.cdn_url.clone();
    assert!(
        a.anim.cache.get(mock::cosmetics::EFFECT_SRC).is_none(),
        "effect frames dropped ({cdn})"
    );
}

#[test]
fn escape_closes_the_popout_before_anything_else() {
    let mut a = app();
    open(&mut a, ALICE);
    ui(&mut a, Ui::Escape);
    assert!(a.profiles.popout.is_none());
    assert!(!a.anim.active(false));
}

#[test]
fn popout_of_a_plain_user_has_no_animation() {
    let mut a = app();
    open(&mut a, BOB);
    assert!(a.open_profile().unwrap().1.is_some());
    // Bob has a still decoration and no effect.
    assert!(!a.anim.popout_hot);
    assert!(!a.anim.active(false));
    assert!(a.effect_layers().is_empty());
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn failed_profile_requests_show_loading_and_never_panic() {
    let mut a = app();
    ui(
        &mut a,
        Ui::OpenProfile {
            user: Snowflake(9999),
            guild: None,
        },
    );
    // Unknown mock user: no profile, popout still renders.
    assert!(a.open_profile().unwrap().1.is_none());
    let _ = ferrocord::ui::view(&a);
    let key = (Snowflake(9999), None);
    let _ = a.update(Message::Done(Done::Profile {
        key,
        result: Err("Missing Access".into()),
    }));
    assert!(a.open_profile().unwrap().1.is_none());
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn list_avatars_play_by_themselves_without_ticking() {
    let mut a = app();
    assert!(!a.anim.active(a.prefs.reduce_motion));
    // Every list / message avatar is live, hovered or not...
    assert_eq!(a.phase_of(ALICE), Phase::Live);
    assert_eq!(a.phase_of(BOB), Phase::Live);
    // ...and hovering a user row never ticks the app: animated avatars and
    // decorations redraw themselves (`ui::anim_image`).
    let h = Hover {
        user: ALICE,
        message: None,
    };
    ui(&mut a, Ui::HoverEnter(h));
    assert!(!a.anim.active(a.prefs.reduce_motion));
    ui(&mut a, Ui::HoverLeave(h));
    assert!(a.cosmetics_playing());
    let _ = ferrocord::ui::view(&a);
}

#[test]
fn reduce_motion_stops_every_animation() {
    let mut a = app();
    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(a.prefs.reduce_motion);
    let h = Hover {
        user: ALICE,
        message: None,
    };
    ui(&mut a, Ui::HoverEnter(h));
    assert!(!a.anim.active(a.prefs.reduce_motion));
    assert_eq!(a.phase_of(ALICE), Phase::Still);
    assert!(!a.cosmetics_playing());
    open(&mut a, ALICE);
    assert!(!a.anim.active(a.prefs.reduce_motion));
    // The effect falls back to its (absent) static frame: nothing drawn.
    assert!(a.effect_layers().is_empty());
    let _ = ferrocord::ui::view(&a);
    // And back on.
    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(!a.prefs.reduce_motion);
    assert!(a.anim.active(a.prefs.reduce_motion));
}

#[test]
fn hovering_picks_animated_frames_over_time() {
    let mut a = app();
    let u = mock::user(2);
    let h = Hover {
        user: ALICE,
        message: None,
    };
    ui(&mut a, Ui::HoverEnter(h));
    let still = a.avatar_frame(&u, 80, Phase::Still).unwrap().id();
    let t0 = Instant::now();
    let mut ids = std::collections::HashSet::new();
    for i in 0..8 {
        ui(&mut a, Ui::AnimTick(t0 + Duration::from_millis(i * 150)));
        ids.insert(a.avatar_frame(&u, 80, Phase::Hover).unwrap().id());
    }
    assert!(ids.len() > 1, "frames advance with the clock");
    // Handles are created once: asking twice gives the same id.
    let again = a.avatar_frame(&u, 80, Phase::Still).unwrap().id();
    assert_eq!(still, again);
}

#[test]
fn animation_results_land_in_the_bounded_cache() {
    let mut a = app();
    let before = a.anim.cache.len();
    let anim = Arc::new(anim::from_rgba_frames(vec![
        (1, 1, vec![0, 0, 0, 255], Duration::from_millis(50)),
        (1, 1, vec![9, 0, 0, 255], Duration::from_millis(50)),
    ]));
    let _ = a.update(Message::Done(Done::Anim {
        key: "https://cdn.discordapp.com/x.gif".into(),
        class: Class::Emoji,
        anim: Some(anim),
    }));
    assert_eq!(a.anim.cache.len(), before + 1);
    // A failed decode is remembered, not retried immediately.
    let _ = a.update(Message::Done(Done::Anim {
        key: "https://cdn.discordapp.com/y.gif".into(),
        class: Class::Emoji,
        anim: None,
    }));
    assert!(
        !a.anim
            .cache
            .want("https://cdn.discordapp.com/y.gif", Instant::now())
    );
    assert!(a.anim.cache.bytes() <= anim::CACHE_BUDGET);
}

#[test]
fn cosmetics_render_in_lists_without_panicking() {
    let mut a = app();
    // Member list with nameplates, tags and decorations.
    let _ = ferrocord::ui::view(&a);
    // A DM with a nameplate user, and the DM profile panel.
    ui(&mut a, Ui::SelectHome);
    ui(&mut a, Ui::SelectChannel(mock::DM_ALICE));
    if !a.members_open {
        ui(&mut a, Ui::ToggleMembers);
    }
    assert!(matches!(a.view, View::Home(Some(_))));
    let _ = ferrocord::ui::view(&a);
    // The panel asked for Alice's profile.
    assert!(a.dm_panel_key().is_some());
}

#[test]
fn popout_view_keeps_its_widget_tree_shape() {
    // The popout slot is always present: opening it must not change how
    // many layers the main view has.
    let mut a = app();
    let _ = ferrocord::ui::view(&a);
    open(&mut a, ALICE);
    let _ = ferrocord::ui::view(&a);
    assert_eq!(
        ferrocord::ui::popups::layers(&a).len(),
        ferrocord::ui::popups::SLOTS
    );
}
