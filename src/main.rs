//! Ferrocord entry point: logging, config, `iced::application(..).run()`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use ferrocord::app::App;
use ferrocord::config::{APP_ID, Config};
use ferrocord::error::log_sink::LogSink;
use ferrocord::tray::instance::{self, Role};
use tracing_subscriber::EnvFilter;

fn main() -> iced::Result {
    let _ = ferrocord::config::PROCESS_START.set(std::time::Instant::now());
    // Default to the CPU renderer: it keeps idle RSS around 50 MB, while
    // wgpu costs 100-200 MB (driver dependent) for no visible gain in a
    // chat UI. `ICED_BACKEND=wgpu` opts back in.
    if std::env::var_os("ICED_BACKEND").is_none() {
        // SAFETY: called first thing in `main`, before any other thread
        // (tokio, iced, logging) exists, so nothing can read the
        // environment concurrently.
        unsafe { std::env::set_var("ICED_BACKEND", "tiny-skia") };
    }
    let cfg = match Config::from_args(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,iced_wgpu=warn,cosmic_text=warn",
        )
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(LogSink::new(cfg.log_path()))
        .with_ansi(false)
        .init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        mock = cfg.mock,
        "starting Ferrocord"
    );

    // One instance (and one tray icon): a second launch asks the running
    // one to show its window and exits. Screenshots never take the lock.
    if cfg.tray {
        let name = if cfg.mock {
            format!("{APP_ID}.mock")
        } else {
            APP_ID.to_owned()
        };
        match instance::acquire(&name, !cfg.minimized) {
            Role::Primary(p) => p.serve(instance::request_show_locally),
            Role::Secondary => {
                tracing::info!("already running; asked it to show its window");
                return Ok(());
            }
        }
    }

    let theme = ferrocord::ui::theme::FerroTheme::new(cfg.theme).iced_theme();
    // A daemon, not an application: hiding to the tray closes the window
    // (freeing the renderer) while the app keeps running.
    iced::daemon(
        move || App::boot(cfg.clone()),
        App::update,
        ferrocord::ui::view_window,
    )
    .title(|app: &App, _window: iced::window::Id| app.title())
    .subscription(App::subscription)
    .theme(move |_: &App, _window: iced::window::Id| theme.clone())
    .default_font(ferrocord::ui::theme::INTER)
    .antialiasing(true)
    .run()
}
