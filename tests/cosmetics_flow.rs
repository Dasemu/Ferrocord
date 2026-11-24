//! Profile popout, cosmetics and hover animation, driven through
//! `App::update` in mock mode (no window, no network).

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrocord::app::anim::{self, Class, Hover, Zone};
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
    // Alice has an animated avatar, decoration, banner and an effect,
    // all self-playing from the moment the popout opened (no app tick).
    let alice = mock::user(2);
    assert!(a.avatar_anim(&alice, 160, Phase::Popout).is_some());
    let popout_start = a.anim.popout_epoch().expect("popout clock");
    assert_eq!(a.anim_clock(Phase::Popout), (popout_start, true));
    assert!(!a.effect_layers().is_empty());
    assert!(
        a.effect_layers()
            .iter()
            .all(|l| matches!(l.image, ferrocord::app::cosmetics::EffectImage::Anim(..)))
    );
    // The whole UI renders with the popout open.
    let _ = ferrocord::ui::view(&a);
    // Closing stops the popout clock and frees effect + banner frames.
    ui(&mut a, Ui::CloseProfile);
    assert!(a.profiles.popout.is_none());
    assert_eq!(a.anim.popout_epoch(), None);
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
    assert_eq!(a.anim.popout_epoch(), None);
}

#[test]
fn popout_of_a_plain_user_has_no_animation() {
    let mut a = app();
    open(&mut a, BOB);
    assert!(a.open_profile().unwrap().1.is_some());
    // Bob has a still decoration and no effect.
    let bob = mock::user(3);
    assert!(a.avatar_anim(&bob, 160, Phase::Popout).is_none());
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

/// Self-playing animation widgets in the whole window.
fn animations_on_screen(a: &App) -> usize {
    let view = ferrocord::ui::view(a);
    ferrocord::ui::anim_image::count_in(&iced::advanced::widget::Tree::new(&view))
}

#[test]
fn list_avatars_are_stills_until_their_row_is_hovered() {
    let mut a = app();
    let row = Hover::row(ALICE, Zone::Members);
    // Like the official client: lists and messages show stills...
    assert_eq!(a.list_phase(row), Phase::Still);
    assert_eq!(a.list_phase(Hover::row(BOB, Zone::Members)), Phase::Still);
    let idle = animations_on_screen(&a);
    // ...and hovering a row animates that row's avatar and decoration
    // (two self-playing widgets, no app tick), from the start of the hover.
    let before = Instant::now();
    ui(&mut a, Ui::HoverEnter(row));
    assert_eq!(a.list_phase(row), Phase::Hover);
    let (epoch, playing) = a.anim_clock(Phase::Hover);
    assert!(playing && epoch >= before);
    assert_eq!(animations_on_screen(&a), idle + 2);
    // The same user elsewhere (DM list, a message) stays still.
    assert_eq!(a.list_phase(Hover::row(ALICE, Zone::DmList)), Phase::Still);
    assert_eq!(
        a.list_phase(Hover::message(ALICE, Snowflake(1))),
        Phase::Still
    );
    // Leaving the row stops it again.
    ui(&mut a, Ui::HoverLeave(row));
    assert_eq!(a.list_phase(row), Phase::Still);
    assert_eq!(animations_on_screen(&a), idle);
}

#[test]
fn the_popout_animates_without_a_hover() {
    let mut a = app();
    let idle = animations_on_screen(&a);
    open(&mut a, ALICE);
    assert_eq!(a.anim.hover, None);
    // Avatar, decoration, banner and the effect layers all play.
    let layers = a.effect_layers().len();
    assert!(layers > 0);
    assert_eq!(animations_on_screen(&a), idle + 3 + layers);
    ui(&mut a, Ui::CloseProfile);
    assert_eq!(animations_on_screen(&a), idle);
}

#[test]
fn reduce_motion_stops_every_animation() {
    let mut a = app();
    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(a.prefs.reduce_motion);
    let h = Hover::row(ALICE, Zone::Members);
    ui(&mut a, Ui::HoverEnter(h));
    assert_eq!(a.list_phase(h), Phase::Still);
    assert!(!a.cosmetics_playing());
    open(&mut a, ALICE);
    let alice = mock::user(2);
    assert!(a.avatar_anim(&alice, 160, Phase::Popout).is_none());
    assert!(!a.anim_clock(Phase::Popout).1);
    // The effect falls back to its (absent) static frame: nothing drawn.
    assert!(a.effect_layers().is_empty());
    let _ = ferrocord::ui::view(&a);
    // And back on.
    ui(&mut a, Ui::ToggleReduceMotion);
    assert!(!a.prefs.reduce_motion);
    assert!(a.avatar_anim(&alice, 160, Phase::Popout).is_some());
    assert!(a.anim_clock(Phase::Popout).1);
}

#[test]
fn animation_handles_keep_their_identity_across_view_calls() {
    let a = app();
    let u = mock::user(2);
    // The views get the same decoded animation (and so the same frame
    // handles, never re-uploaded) on every rebuild.
    let first = a
        .avatar_anim(&u, 80, Phase::Hover)
        .expect("mock avatar anim")
        .clone();
    let ids: Vec<_> = first.frames.iter().map(|f| f.handle.id()).collect();
    let _ = ferrocord::ui::view(&a);
    let _ = ferrocord::ui::view(&a);
    let again = a.avatar_anim(&u, 80, Phase::Hover).unwrap();
    assert!(Arc::ptr_eq(&first, again));
    let ids2: Vec<_> = again.frames.iter().map(|f| f.handle.id()).collect();
    assert_eq!(ids, ids2);
    // The still is cached too.
    let still = a.avatar_frame(&u, 80).unwrap().id();
    assert_eq!(still, a.avatar_frame(&u, 80).unwrap().id());
    // The mock avatar went through the real limits and kept every frame
    // (60 frames at 30 ms): it plays at its own frame rate.
    assert_eq!(first.frames.len(), 60);
    assert!(
        first
            .frames
            .iter()
            .all(|f| f.delay == Duration::from_millis(30))
    );
    let deco = u.avatar_decoration.as_ref().unwrap();
    let d = a.decoration_anim(deco, 96, Phase::Hover).unwrap();
    assert_eq!(d.frames.len(), 180, "60 fps decoration kept whole");
    // Exact APNG-style delays (1/60 s), not 16 or 20 ms per frame.
    let total = d.total.as_secs_f64();
    assert!((total - 3.0).abs() < 0.002, "{total}");
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
