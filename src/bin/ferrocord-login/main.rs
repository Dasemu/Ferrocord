//! `ferrocord-login`: Discord's own login page (`https://discord.com/login`)
//! in a system webview (WebView2 on Windows, WebKitGTK on Linux), for when
//! Discord wants a captcha or the user prefers its page.
//!
//! A separate process so the app links no browser engine; it exits after the
//! login. Protocol and safety rules: `ferrocord::auth::web_login`.
//!
//! * The page runs in a private (incognito) profile inside the directory the
//!   app passes in `FERROCORD_LOGIN_DATA_DIR`; the app wipes it afterwards.
//! * Only top-level HTTPS navigations are allowed; pop-ups are refused.
//! * A token is accepted only from the top-level `https://discord.com`
//!   document, printed once to stdout (a pipe to the app) and never logged.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// A GUI program: no console window on Windows (pipes still work).
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::io::Write;
use std::path::PathBuf;

use ferrocord::auth::secrets::Secret;
use ferrocord::auth::web_login::{self, HelperMsg, code};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

enum UserEvent {
    Token(Secret),
}

fn emit(msg: &HelperMsg) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", msg.to_line());
    let _ = out.flush();
}

fn fail(code: &str, message: impl Into<String>) -> ! {
    emit(&HelperMsg::Error {
        code: code.into(),
        message: message.into(),
    });
    std::process::exit(1);
}

fn main() {
    ferrocord::cli::handle_info_flags(
        "ferrocord-login",
        "ferrocord-login (started by ferrocord, with FERROCORD_LOGIN_DATA_DIR set)",
    );
    let Some(dir) = std::env::var_os(web_login::DATA_DIR_ENV).map(PathBuf::from) else {
        fail(code::FAILED, "no data directory given");
    };
    if !dir.is_dir() {
        fail(code::FAILED, "the data directory does not exist");
    }
    // WebKitGTK's DMA-BUF renderer shows a blank page on some GPU drivers
    // (NVIDIA, some VMs); the login page does not need it.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: single-threaded here, before GTK or any other thread starts.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = match WindowBuilder::new()
        .with_title("Log in to Discord \u{2014} Ferrocord")
        .with_inner_size(LogicalSize::new(1000.0, 780.0))
        .with_min_inner_size(LogicalSize::new(480.0, 600.0))
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => fail(code::FAILED, format!("cannot open a window: {e}")),
    };

    let mut context = WebContext::new(Some(dir));
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_incognito(true)
        .with_initialization_script_for_main_only(web_login::INIT_SCRIPT, true)
        .with_navigation_handler(|url| web_login::navigation_allowed(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_ipc_handler(move |req| {
            let page = req.uri().to_string();
            if let Some(token) = web_login::parse_page_message(&page, req.body()) {
                let _ = proxy.send_event(UserEvent::Token(token));
            }
        })
        .with_url(web_login::LOGIN_URL);

    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        match window.default_vbox() {
            Some(vbox) => builder.build_gtk(vbox),
            None => fail(code::FAILED, "no GTK container in the window"),
        }
    };
    let webview = match webview {
        Ok(w) => w,
        // WebView2 runtime missing (Windows) or WebKitGTK unusable (Linux).
        Err(e) => fail(code::NO_WEBVIEW, format!("{e}")),
    };

    let mut token: Option<Secret> = None;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => emit(&HelperMsg::Ready),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let _ = webview.clear_all_browsing_data();
                emit(&HelperMsg::Closed);
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Token(t)) if token.is_none() => {
                emit(&HelperMsg::Token(t.clone()));
                token = Some(t);
                let _ = webview.clear_all_browsing_data();
                window.set_visible(false);
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
