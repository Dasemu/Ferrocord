//! Window lifecycle and system tray glue: turns `window::Action`s into
//! iced window tasks and keeps the tray in sync with the app state.

use iced::Task;

use super::message::{Done, Message, Ui};
use super::window::{self, Action, Geometry};
use super::{App, input};
use crate::auth::AuthState;
use crate::tray::{self, TrayModel};

impl App {
    /// Carries out a window decision.
    pub(super) fn run_window_action(&mut self, action: Action) -> Task<Message> {
        match action {
            Action::None => Task::none(),
            Action::Open => {
                if self.window_id.is_some() {
                    return Task::none();
                }
                let settings =
                    window::window_settings(self.cfg.screenshot.is_none(), self.geometry);
                let (id, open) = iced::window::open(settings);
                self.window_id = Some(id);
                if self.window_opened {
                    // A re-opened window: the Focused event follows.
                    self.window_focused = false;
                }
                open.map(|_| Message::Done(Done::Ignored))
            }
            Action::Close => {
                let Some(id) = self.window_id.take() else {
                    return Task::none();
                };
                self.geometry = Some(Geometry {
                    size: input::window_size(),
                    position: input::window_position(),
                });
                // No Unfocused event arrives for a closed window; without
                // this, notifications for the open chat would stay muted.
                self.window_focused = false;
                self.menu = None;
                self.emoji_picker = None;
                self.gif_picker = None;
                self.sticker_picker = None;
                self.release_view_caches();
                iced::window::close(id)
            }
            Action::Raise => match self.window_id {
                Some(id) => Task::batch([
                    iced::window::minimize(id, false),
                    iced::window::gain_focus(id),
                ]),
                None => Task::none(),
            },
            Action::Exit => self.quit(),
        }
    }

    /// Quit (tray menu, Ctrl+Q, tray lost while hidden). Leaves within
    /// [`crate::shutdown::EXIT_DEADLINE`] whatever is still running: the
    /// window closes at once, helper processes and devices stop, every
    /// subscription (gateway, voice, tray, login helper) is dropped, pending
    /// settings get [`crate::shutdown::FLUSH_BUDGET`], then iced exits. If
    /// dropping iced's runtime then waits on a stuck blocking task, the
    /// watchdog ends the process anyway.
    pub(super) fn quit(&mut self) -> Task<Message> {
        if self.quitting {
            return Task::none();
        }
        self.quitting = true;
        tracing::info!("quitting");
        if self.cfg.exit_watchdog {
            crate::shutdown::begin();
        }
        // Ends `ferrocord-player` (killed at once while quitting) and the
        // microphone test's devices.
        self.video = None;
        self.settings.mic_test = None;
        let close = match self.window_id.take() {
            Some(id) => iced::window::close(id),
            None => Task::none(),
        };
        let flush = self.settings_flush_for_quit();
        let hang = self.cfg.mock_hang;
        let last_work = Task::perform(
            async move {
                let work = async move {
                    if let Some(f) = flush {
                        f.await;
                    }
                    if hang {
                        // Mock: a save that never answers.
                        std::future::pending::<()>().await;
                    }
                };
                if tokio::time::timeout(crate::shutdown::FLUSH_BUDGET, work)
                    .await
                    .is_err()
                {
                    tracing::warn!("pending settings not saved before quitting (timeout)");
                }
            },
            |()| Message::Done(Done::QuitReady),
        );
        Task::batch([close, last_work])
    }

    /// Drops state that only exists to speed up drawing; it is rebuilt on
    /// the next frame after the window comes back.
    fn release_view_caches(&mut self) {
        self.md_cache.borrow_mut().clear();
        self.md_cache.borrow_mut().shrink_to_fit();
    }

    pub(super) fn show_window(&mut self) -> Task<Message> {
        let a = self.win.show();
        self.run_window_action(a)
    }

    pub(super) fn on_tray(&mut self, ev: tray::Event) -> Task<Message> {
        match ev {
            tray::Event::Ready(control) => {
                self.win.tray_ready();
                self.tray_control = Some(control);
                Task::none()
            }
            tray::Event::Available => {
                self.win.tray_ready();
                Task::none()
            }
            tray::Event::Unavailable => {
                let a = self.win.tray_lost();
                self.run_window_action(a)
            }
            tray::Event::Command(c) => self.update_ui(window::command_to_ui(c)),
        }
    }

    /// What the tray should currently show.
    pub fn tray_model(&self) -> TrayModel {
        TrayModel {
            visible: self.win.visible,
            logged_in: matches!(self.auth, AuthState::LoggedIn { .. }),
            status: self.chat.my_status,
            muted: self.prefs.muted,
            start_minimized: self.prefs.start_minimized,
            // An incoming call asks for attention like an unread mention.
            // "Enable Unread Message Badge" off: no count on the icon.
            mentions: if self.settings.local.unread_badge {
                self.chat.attention_count() + u32::from(self.voice.incoming.is_some())
            } else {
                0
            },
        }
    }

    /// "Enable Taskbar Flashing": asks the window manager for attention
    /// (taskbar flash / urgent hint) for a new mention while unfocused.
    pub(super) fn flash_taskbar(&self) -> Task<Message> {
        match self.window_id {
            Some(id) if self.wants_flash() => iced::window::request_user_attention(
                id,
                Some(iced::window::UserAttention::Informational),
            ),
            _ => Task::none(),
        }
    }

    /// Whether a new mention flashes the taskbar now.
    pub(super) fn wants_flash(&self) -> bool {
        self.settings.local.taskbar_flash && !self.window_focused
    }

    /// Pushes the current state to the tray (a no-op when unchanged).
    pub(super) fn sync_tray(&self) {
        if let Some(control) = &self.tray_control {
            control.publish(self.tray_model());
        }
    }

    pub(super) fn save_pref(&self, key: &'static str, on: bool) -> Task<Message> {
        let store = self.store.clone();
        Task::perform(
            async move {
                store
                    .blocking(move |s| s.set_meta(key, if on { "1" } else { "0" }))
                    .await
            },
            |_| Message::Done(Done::Ignored),
        )
    }

    /// Window, tray and settings messages.
    pub(super) fn update_background(&mut self, u: Ui) -> Task<Message> {
        match u {
            Ui::CloseRequested => {
                let a = self.win.close_requested();
                self.run_window_action(a)
            }
            Ui::ToggleWindow => {
                let a = self.win.toggle();
                self.run_window_action(a)
            }
            Ui::ShowWindow => self.show_window(),
            Ui::HideWindow => {
                let a = self.win.hide();
                self.run_window_action(a)
            }
            Ui::Quit => self.quit(),
            Ui::OpenSettings => {
                self.settings_open = true;
                self.status_menu = false;
                // Always open on the main page, not on Plugins.
                self.plugins.page.prompt = None;
                self.menu = None;
                self.ctx_menu = None;
                // Device names for the voice section (blocking enumeration
                // runs off the UI thread).
                Task::batch([
                    self.open_settings_screen(),
                    self.update_voice(super::voice::VoiceUi::RefreshDevices),
                ])
            }
            Ui::CloseSettings => {
                self.settings_open = false;
                self.close_settings_screen()
            }
            Ui::ToggleCloseToTray => {
                self.prefs.close_to_tray = !self.prefs.close_to_tray;
                self.win.close_to_tray = self.prefs.close_to_tray;
                self.save_pref(window::KEY_CLOSE_TO_TRAY, self.prefs.close_to_tray)
            }
            Ui::ToggleStartMinimized => {
                self.prefs.start_minimized = !self.prefs.start_minimized;
                self.save_pref(window::KEY_START_MINIMIZED, self.prefs.start_minimized)
            }
            Ui::ToggleMuted => {
                self.prefs.muted = !self.prefs.muted;
                self.save_pref(window::KEY_MUTED, self.prefs.muted)
            }
            Ui::ToggleAutostart => {
                let want = !self.autostart;
                if self.cfg.mock {
                    // Mock mode never writes to disk.
                    self.autostart = want;
                    return Task::none();
                }
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || tray::autostart::set_enabled(want))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()))
                    },
                    |r| Message::Done(Done::AutostartChanged(r)),
                )
            }
            _ => Task::none(),
        }
    }
}
