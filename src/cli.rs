//! `--version` / `--help` for the three binaries (`ferrocord`,
//! `ferrocord-player`, `ferrocord-login`).
//!
//! These flags are answered before anything else starts (no window, no
//! logging, no single-instance lock) and exit with status 0.

/// The version line every binary prints: `<bin> <version>`.
pub fn version_line(bin: &str) -> String {
    format!("{bin} {}", env!("CARGO_PKG_VERSION"))
}

/// What to print and exit for an informational flag, if `args` (without
/// the program name) holds one. Only a first argument counts, so a file
/// named `--help` given after another argument is never swallowed.
pub fn info(bin: &str, usage: &str, args: &[String]) -> Option<String> {
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => Some(version_line(bin)),
        Some("--help" | "-h") => Some(format!(
            "{}\n\nusage: {usage}\n\n  -h, --help     print this help and exit\n  -V, --version  print the version and exit",
            version_line(bin)
        )),
        _ => None,
    }
}

/// Prints `text` to stdout. On Windows the GUI binaries have no console
/// of their own, so they attach to the terminal that started them first
/// (no effect when started from Explorer).
pub fn print(text: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        // SAFETY: plain Win32 calls with constant arguments. A redirected
        // stdout (pipe, file) is kept; only a missing one is replaced by
        // the parent's console. Failure (no parent console) is harmless.
        unsafe {
            let h = GetStdHandle(STD_OUTPUT_HANDLE);
            if h.is_null() || h as isize == -1 {
                AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
    }
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

/// Answers `--version` / `--help` and exits, or returns to start normally.
pub fn handle_info_flags(bin: &str, usage: &str) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(text) = info(bin, usage, &args) {
        print(&text);
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn version_flags_print_the_package_version() {
        let want = format!("ferrocord {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            info("ferrocord", "u", &args(&["--version"])),
            Some(want.clone())
        );
        assert_eq!(info("ferrocord", "u", &args(&["-V"])), Some(want));
        assert_eq!(
            info("ferrocord-player", "u", &args(&["-V"])).as_deref(),
            Some(format!("ferrocord-player {}", env!("CARGO_PKG_VERSION")).as_str())
        );
    }

    #[test]
    fn help_mentions_version_and_usage() {
        let h = info("ferrocord-login", "ferrocord-login", &args(&["--help"])).unwrap();
        assert!(h.contains("--version") && h.contains("-V"));
        assert!(h.contains("usage: ferrocord-login"));
        assert!(info("x", "u", &args(&["-h"])).is_some_and(|t| t.starts_with("x ")));
    }

    #[test]
    fn other_arguments_start_normally() {
        assert_eq!(info("ferrocord", "u", &args(&[])), None);
        assert_eq!(info("ferrocord", "u", &args(&["--mock"])), None);
        // Only the first argument is a flag: a file called `--version`
        // after another argument is not.
        assert_eq!(
            info("ferrocord", "u", &args(&["--mock", "--version"])),
            None
        );
        assert_eq!(info("ferrocord", "u", &args(&["-v"])), None);
    }
}
