//! Ferrocord entry point: logging, config, `iced::application(..).run()`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use ferrocord::app::App;
use ferrocord::config::{APP_ID, Config};
use ferrocord::error::log_sink::LogSink;
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

    let window = iced::window::Settings {
        size: iced::Size::new(1280.0, 800.0),
        min_size: Some(iced::Size::new(940.0, 500.0)),
        resizable: cfg.screenshot.is_none(),
        platform_specific: iced::window::settings::PlatformSpecific {
            #[cfg(target_os = "linux")]
            application_id: APP_ID.into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = APP_ID;
    let theme = ferrocord::ui::theme::FerroTheme::new(cfg.theme).iced_theme();
    iced::application(
        move || App::boot(cfg.clone()),
        App::update,
        ferrocord::ui::view,
    )
    .title(App::title)
    .subscription(App::subscription)
    .theme(move |_: &App| theme.clone())
    .window(window)
    .default_font(ferrocord::ui::theme::INTER)
    .antialiasing(true)
    .run()
}
