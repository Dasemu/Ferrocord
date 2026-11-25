//! Process shutdown: Quit must leave quickly and never hang.
//!
//! iced ends `daemon::run` by dropping its tokio runtime, and dropping a
//! tokio runtime waits for every `spawn_blocking` task, with no timeout.
//! One blocking task that never returns (a notification click wait on KDE,
//! a keyring unlock prompt, a stuck device enumeration) used to freeze the
//! window ("not responding") on Quit. So:
//!
//! * work that can last indefinitely never runs on the blocking pool
//!   (detached threads instead, see `app::notify`);
//! * [`begin`] arms a watchdog: if the process is still alive after
//!   [`EXIT_DEADLINE`], it exits anyway;
//! * helpers check [`is_quitting`] to kill their child processes at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Hard limit between "Quit" and the end of the process.
pub const EXIT_DEADLINE: Duration = Duration::from_millis(600);

/// Budget for last network writes (pending settings) before exiting.
pub const FLUSH_BUDGET: Duration = Duration::from_millis(350);

static QUITTING: AtomicBool = AtomicBool::new(false);

/// True once the app decided to quit.
pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::Acquire)
}

/// Marks the process as quitting and arms the exit watchdog. Returns false
/// when quitting had already begun.
pub fn begin() -> bool {
    arm(&QUITTING, EXIT_DEADLINE, || std::process::exit(0))
}

fn arm(flag: &AtomicBool, deadline: Duration, exit: impl FnOnce() + Send + 'static) -> bool {
    if flag.swap(true, Ordering::AcqRel) {
        return false;
    }
    let spawned = std::thread::Builder::new()
        .name("ferrocord-exit-watchdog".into())
        .spawn(move || {
            std::thread::sleep(deadline);
            tracing::warn!(
                deadline_ms = deadline.as_millis() as u64,
                "shutdown did not finish in time; exiting anyway"
            );
            exit();
        });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "exit watchdog not started");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn watchdog_fires_once_after_the_deadline() {
        let flag = AtomicBool::new(false);
        let (tx, rx) = mpsc::channel();
        let tx2 = tx.clone();
        let start = std::time::Instant::now();
        assert!(arm(&flag, Duration::from_millis(50), move || {
            let _ = tx.send(start.elapsed());
        }));
        // A second Quit does not arm another watchdog.
        assert!(!arm(&flag, Duration::from_millis(1), move || {
            let _ = tx2.send(Duration::ZERO);
        }));
        let fired = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(fired >= Duration::from_millis(50));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(flag.load(Ordering::Acquire));
    }

    #[test]
    fn deadline_is_well_under_a_second() {
        assert!(FLUSH_BUDGET < EXIT_DEADLINE);
        assert!(EXIT_DEADLINE < Duration::from_secs(1));
    }
}
