//! Desktop notifications for new messages.
//!
//! A notification with a click action is watched until the server closes
//! it. On KDE Plasma that can be never: notifications that time out move to
//! the history and `NotificationClosed` only comes when the user dismisses
//! them there. The watch therefore runs on a detached thread of its own,
//! never on tokio's blocking pool: iced drops its tokio runtime when the
//! app quits, and dropping a runtime waits for every blocking task, so one
//! open notification froze the window on "Quit" (the v0.17.0 hang).

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::oneshot;

/// Notifications whose click is watched at once (one parked thread each);
/// further ones are shown without the click action.
pub const MAX_CLICK_WATCHERS: usize = 8;

static WATCHING: AtomicUsize = AtomicUsize::new(0);

/// Takes one watcher slot if one is free.
fn take_slot() -> bool {
    // A compare-exchange loop: `fetch_update` is deprecated in newer Rust
    // (renamed `try_update`), which older toolchains don't have.
    let mut n = WATCHING.load(Ordering::Acquire);
    while n < MAX_CLICK_WATCHERS {
        match WATCHING.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(current) => n = current,
        }
    }
    false
}

fn release_slot() {
    WATCHING.fetch_sub(1, Ordering::AcqRel);
}

/// Shows a notification (`title`/`body` already markup-escaped). The
/// receiver yields true if the user clicked it, false when it was closed,
/// could not be shown or nobody watches it. Never blocks the caller, and
/// nothing it starts delays the end of the process.
pub fn show(title: String, body: String, sound: bool) -> oneshot::Receiver<bool> {
    let (tx, rx) = oneshot::channel();
    let watch = take_slot();
    let spawned = std::thread::Builder::new()
        .name("ferrocord-notify".into())
        .spawn(move || {
            let clicked = show_blocking(&title, &body, sound, watch);
            if watch {
                release_slot();
            }
            let _ = tx.send(clicked);
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "notification thread not started");
        if watch {
            release_slot();
        }
    }
    rx
}

/// Shows the notification and, with `watch`, waits for it to be closed.
fn show_blocking(title: &str, body: &str, sound: bool, watch: bool) -> bool {
    let mut note = notify_rust::Notification::new();
    note.appname(crate::config::APP_NAME)
        .summary(title)
        .body(body)
        .icon("dev.ferro.ferrocord");
    // "Enable Notification Sounds": the desktop's message sound
    // (freedesktop sound-name / Windows default toast audio), or an
    // explicitly silent notification.
    #[cfg(all(unix, not(target_os = "macos")))]
    if sound {
        note.sound_name("message-new-instant");
    } else {
        note.hint(notify_rust::Hint::SuppressSound(true));
    }
    #[cfg(windows)]
    if sound {
        note.sound_name("Default");
    }
    #[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
    let _ = sound;
    // The registered AppUserModelID (`win_app_id`): without it the toast is
    // attributed to PowerShell.
    #[cfg(windows)]
    note.app_id(crate::config::APP_ID);
    // Desktop entry and click actions are freedesktop (XDG) notification
    // features; Windows toasts have neither.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        note.hint(notify_rust::Hint::DesktopEntry(
            crate::config::APP_ID.into(),
        ));
        // "default" is the action a click on the notification body invokes
        // (freedesktop notification spec).
        if watch {
            note.action("default", "Open");
        }
    }
    match note.show() {
        Ok(handle) if watch => wait_for_click(handle),
        Ok(_) => false,
        Err(e) => {
            tracing::debug!(error = %e, "notification failed");
            false
        }
    }
}

/// Blocks until the notification is closed or acted on; true if the user
/// clicked it.
#[cfg(all(unix, not(target_os = "macos")))]
fn wait_for_click(handle: notify_rust::NotificationHandle) -> bool {
    let mut clicked = false;
    handle.wait_for_action(|action| clicked = action == "default");
    clicked
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn wait_for_click(_handle: impl Sized) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_slots_are_bounded() {
        let mut taken = 0;
        while take_slot() {
            taken += 1;
            assert!(taken <= MAX_CLICK_WATCHERS);
        }
        assert!(taken <= MAX_CLICK_WATCHERS);
        for _ in 0..taken {
            release_slot();
        }
        assert!(take_slot());
        release_slot();
    }
}
