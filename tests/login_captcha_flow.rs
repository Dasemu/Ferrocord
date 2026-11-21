//! QR login → captcha → Discord's login page → token, driven through
//! `App::update` in mock mode (no window, no helper process).

use ferrocord::app::{App, Done, Message, Screen, Ui};
use ferrocord::auth::secrets::Secret;
use ferrocord::auth::web_login::{WebLoginEvent, WebLoginFailure};
use ferrocord::auth::{AuthState, QrState, WebLoginReason, WebLoginState};
use ferrocord::config::Config;
use ferrocord::net::captcha::CaptchaChallenge;
use ferrocord::net::remote_auth::RaEvent;

fn logged_out() -> App {
    let mut a = App::boot(Config::for_tests()).0;
    let _ = a.update(Message::Ui(Ui::Logout));
    assert_eq!(a.screen(), Screen::Login);
    // The QR code is up.
    let _ = a.update(Message::Done(Done::Qr(RaEvent::QrReady(
        "https://discord.com/ra/abc".into(),
    ))));
    a
}

fn captcha() -> RaEvent {
    RaEvent::CaptchaRequired(Box::new(
        CaptchaChallenge::parse(
            r#"{"captcha_key":["captcha-required"],"captcha_sitekey":"k","captcha_service":"hcaptcha","captcha_rqdata":"d","captcha_rqtoken":"t"}"#,
        )
        .unwrap(),
    ))
}

fn token() -> Secret {
    Secret::new(
        [
            "MTIzNDU2Nzg5MDEyMzQ1Njc4",
            "GabCdE",
            "x".repeat(27).as_str(),
        ]
        .join("."),
    )
}

fn web(a: &mut App, ev: WebLoginEvent) {
    let _ = a.update(Message::Done(Done::WebLogin(ev)));
}

#[test]
fn captcha_opens_discords_page_and_its_token_logs_in() {
    let mut a = logged_out();
    let _ = a.update(Message::Done(Done::Qr(captcha())));
    assert_eq!(a.qr, Some(QrState::CaptchaRequired));
    // Mock mode never starts a browser: the page counts as open at once.
    assert_eq!(
        a.web_login,
        Some(WebLoginState::Open(WebLoginReason::Captcha))
    );
    let _ = ferrocord::ui::view(&a);
    web(&mut a, WebLoginEvent::Opened);
    web(&mut a, WebLoginEvent::Token(token()));
    assert_eq!(a.web_login, None);
    assert_eq!(
        a.auth,
        AuthState::Validating,
        "the token is validated first"
    );
    assert_eq!(a.qr, None);
}

#[test]
fn closing_discords_page_after_a_captcha_brings_a_new_qr_code() {
    let mut a = logged_out();
    let _ = a.update(Message::Done(Done::Qr(captcha())));
    web(&mut a, WebLoginEvent::Closed);
    assert_eq!(a.web_login, None);
    assert_eq!(a.qr, Some(QrState::Connecting));
    assert!(matches!(a.auth, AuthState::LoggedOut { .. }));
}

#[test]
fn cancel_closes_the_page_and_keeps_the_login_screen() {
    let mut a = logged_out();
    let _ = a.update(Message::Ui(Ui::OpenWebLogin));
    assert_eq!(
        a.web_login,
        Some(WebLoginState::Open(WebLoginReason::Chosen))
    );
    let _ = ferrocord::ui::view(&a);
    let _ = a.update(Message::Ui(Ui::CancelWebLogin));
    assert_eq!(a.web_login, None);
    // A late event from the killed helper is ignored.
    web(&mut a, WebLoginEvent::Token(token()));
    assert!(matches!(a.auth, AuthState::LoggedOut { .. }));
    assert!(
        matches!(a.qr, Some(QrState::Ready { .. })),
        "the QR code stays"
    );
}

#[test]
fn failures_explain_what_to_do_and_can_be_retried() {
    let mut a = logged_out();
    let _ = a.update(Message::Done(Done::Qr(captcha())));
    web(&mut a, WebLoginEvent::Failed(WebLoginFailure::NoWebEngine));
    let Some(WebLoginState::Failed(msg)) = &a.web_login else {
        panic!("{:?}", a.web_login)
    };
    assert!(msg.contains("then try again"), "{msg}");
    let _ = ferrocord::ui::view(&a);
    // "Try Discord's page again".
    let _ = a.update(Message::Ui(Ui::OpenWebLogin));
    assert_eq!(
        a.web_login,
        Some(WebLoginState::Open(WebLoginReason::Chosen))
    );
    // Opening twice does nothing.
    let _ = a.update(Message::Ui(Ui::OpenWebLogin));
    assert_eq!(
        a.web_login,
        Some(WebLoginState::Open(WebLoginReason::Chosen))
    );
}

#[test]
fn the_page_is_not_offered_while_logged_in() {
    let mut a = App::boot(Config::for_tests()).0;
    let _ = a.update(Message::Ui(Ui::OpenWebLogin));
    assert_eq!(a.web_login, None);
}

#[test]
fn logout_forgets_the_page_state() {
    let mut a = logged_out();
    let _ = a.update(Message::Ui(Ui::OpenWebLogin));
    let _ = a.update(Message::Ui(Ui::Logout));
    assert_eq!(a.web_login, None);
}
