//! Putting a verified, extracted release in place.
//!
//! Every file is first written next to its target under a temporary name
//! (same directory, so the final `rename` is atomic), then all targets
//! are switched. If any switch fails, the ones already done are rolled
//! back from the old copies, so a failure leaves the current install
//! untouched.
//!
//! - `install.sh` layout (Linux): the files and paths `install.sh` writes,
//!   each renamed over its target (renaming over a running ELF is fine).
//! - Windows zip folder: a running or locked `.exe` cannot be replaced, but
//!   it can be renamed: each one is moved aside to `<name>.old` and the new
//!   one moved in; the `.old` files are deleted on the next start.

use std::path::{Path, PathBuf};

use super::method::ScriptLayout;
use super::version::Version;
use super::{APP_ID, CRATE, UpdateError};

/// The binaries a release ships (the same list `install.sh` installs):
/// the app first, then the video and login helpers (optional: older
/// releases lack them).
pub const SHIPPED: [&str; 3] = [CRATE, "ferrocord-player", "ferrocord-login"];

/// `name` or `name.exe`.
pub fn exe_file(name: &str, windows: bool) -> String {
    if windows {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

/// One file to switch: `staged` (already complete, same directory) becomes
/// `target`.
#[derive(Debug, Clone)]
pub struct Step {
    pub staged: PathBuf,
    pub target: PathBuf,
}

/// How targets are switched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Hard-link (or copy) the old file to a backup, then rename the new
    /// one over the target: the target never disappears (Unix).
    RenameOver,
    /// Rename the old file aside to `<name>.old`, then move the new one in
    /// (Windows: running executables can be renamed, not overwritten).
    MoveAside,
}

/// Where the previous version of a switched target went.
#[derive(Debug)]
struct Done {
    target: PathBuf,
    old: Option<PathBuf>,
}

fn pid() -> u32 {
    std::process::id()
}

fn sibling(target: &Path, name: String) -> PathBuf {
    target.with_file_name(name)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A free `<name>.old` (or `<name>.<n>.old` while an older `.old` is still
/// locked by a process that has not restarted yet).
pub fn free_old_name(target: &Path) -> PathBuf {
    let name = file_name(target);
    let first = sibling(target, format!("{name}.old"));
    if !first.exists() || std::fs::remove_file(&first).is_ok() {
        return first;
    }
    (1..100)
        .map(|n| sibling(target, format!("{name}.{n}.old")))
        .find(|p| !p.exists() || std::fs::remove_file(p).is_ok())
        .unwrap_or_else(|| sibling(target, format!("{name}.{}.old", pid())))
}

/// Switches every step, or none: on error the targets switched so far get
/// their previous file back and the remaining staged files are removed.
pub fn commit(steps: &[Step], strategy: Strategy) -> Result<(), UpdateError> {
    let mut done: Vec<Done> = Vec::with_capacity(steps.len());
    for (i, step) in steps.iter().enumerate() {
        match switch(step, strategy) {
            Ok(d) => done.push(d),
            Err(e) => {
                tracing::warn!(error = %e, file = %file_name(&step.target), "update: switch failed, rolling back");
                rollback(&done, strategy);
                for s in &steps[i..] {
                    let _ = std::fs::remove_file(&s.staged);
                }
                return Err(UpdateError::io(e));
            }
        }
    }
    // Success: drop the backups (a running Windows exe keeps its `.old`
    // until the next start, see `cleanup`).
    for d in &done {
        if let Some(old) = &d.old {
            let _ = std::fs::remove_file(old);
        }
    }
    Ok(())
}

fn switch(step: &Step, strategy: Strategy) -> std::io::Result<Done> {
    let exists = std::fs::symlink_metadata(&step.target).is_ok();
    match strategy {
        Strategy::RenameOver => {
            let old = if exists {
                let backup = sibling(
                    &step.target,
                    format!(".{}.old.{}", file_name(&step.target), pid()),
                );
                let _ = std::fs::remove_file(&backup);
                if std::fs::hard_link(&step.target, &backup).is_err() {
                    std::fs::copy(&step.target, &backup)?;
                }
                Some(backup)
            } else {
                None
            };
            if let Err(e) = std::fs::rename(&step.staged, &step.target) {
                if let Some(b) = &old {
                    let _ = std::fs::remove_file(b);
                }
                return Err(e);
            }
            Ok(Done {
                target: step.target.clone(),
                old,
            })
        }
        Strategy::MoveAside => {
            let old = if exists {
                let aside = free_old_name(&step.target);
                std::fs::rename(&step.target, &aside)?;
                Some(aside)
            } else {
                None
            };
            if let Err(e) = std::fs::rename(&step.staged, &step.target) {
                if let Some(a) = &old {
                    let _ = std::fs::rename(a, &step.target);
                }
                return Err(e);
            }
            Ok(Done {
                target: step.target.clone(),
                old,
            })
        }
    }
}

fn rollback(done: &[Done], strategy: Strategy) {
    for d in done.iter().rev() {
        match (&d.old, strategy) {
            (Some(old), Strategy::RenameOver) => {
                if let Err(e) = std::fs::rename(old, &d.target) {
                    tracing::error!(error = %e, file = %file_name(&d.target), "update: rollback failed");
                }
            }
            (Some(old), Strategy::MoveAside) => {
                // The new file was never started: it can be removed.
                let _ = std::fs::remove_file(&d.target);
                if let Err(e) = std::fs::rename(old, &d.target) {
                    tracing::error!(error = %e, file = %file_name(&d.target), "update: rollback failed");
                }
            }
            (None, _) => {
                let _ = std::fs::remove_file(&d.target);
            }
        }
    }
}

/// Copies `src` to `dir/.<name>.new.<pid>` (the temporary name `install.sh`
/// uses) and returns the step that puts it at `dir/<name>`.
fn stage_copy(src: &Path, dir: &Path, name: &str, executable: bool) -> Result<Step, UpdateError> {
    let staged = dir.join(format!(".{name}.new.{}", pid()));
    std::fs::copy(src, &staged).map_err(UpdateError::io)?;
    set_executable(&staged, executable)?;
    Ok(Step {
        staged,
        target: dir.join(name),
    })
}

fn stage_bytes(bytes: &[u8], dir: &Path, name: &str) -> Result<Step, UpdateError> {
    let staged = dir.join(format!(".{name}.new.{}", pid()));
    std::fs::write(&staged, bytes).map_err(UpdateError::io)?;
    Ok(Step {
        staged,
        target: dir.join(name),
    })
}

#[cfg(unix)]
fn set_executable(p: &Path, executable: bool) -> Result<(), UpdateError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).map_err(UpdateError::io)
}

#[cfg(not(unix))]
fn set_executable(_p: &Path, _executable: bool) -> Result<(), UpdateError> {
    Ok(())
}

/// `install.sh`'s desktop entry rewrite: every `Exec=` line becomes the
/// absolute binary path (quoted when it holds a space) followed by the
/// field codes of the shipped first `Exec=` line.
pub fn render_desktop(template: &str, bin_path: &Path) -> String {
    let exec_args = template
        .lines()
        .find_map(|l| l.strip_prefix("Exec="))
        .map(|rest| match rest.find(' ') {
            Some(i) => &rest[i..],
            None => "",
        })
        .unwrap_or("");
    let bin = bin_path.display().to_string();
    let exec_bin = if bin.contains(' ') {
        format!("\"{bin}\"")
    } else {
        bin
    };
    let mut out = String::with_capacity(template.len() + 64);
    for line in template.lines() {
        if line.starts_with("Exec=") {
            out.push_str("Exec=");
            out.push_str(&exec_bin);
            out.push_str(exec_args);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Installs the extracted package `pkg` into an `install.sh` layout, as
/// `install.sh` would: the binaries, the desktop entry, the icon and
/// `installed-version`, then refreshes the desktop caches.
pub fn install_script(
    layout: &ScriptLayout,
    pkg: &Path,
    version: Version,
) -> Result<(), UpdateError> {
    let desktop_src = pkg.join("packaging").join(format!("{APP_ID}.desktop"));
    let icon_src = pkg.join("packaging").join(format!("{APP_ID}.svg"));
    if !pkg.join(CRATE).is_file() {
        return Err(UpdateError::BadArchive(format!(
            "no {CRATE} binary in the release"
        )));
    }
    if !desktop_src.is_file() || !icon_src.is_file() {
        return Err(UpdateError::BadArchive(
            "no desktop entry or icon in the release".into(),
        ));
    }
    let (apps, icons, state) = (layout.apps_dir(), layout.icon_dir(), layout.state_dir());
    for d in [&layout.bin_dir, &apps, &icons, &state] {
        std::fs::create_dir_all(d).map_err(UpdateError::io)?;
    }
    let template = std::fs::read_to_string(&desktop_src).map_err(UpdateError::io)?;
    let desktop = render_desktop(&template, &layout.bin_path(CRATE));

    let mut steps = Vec::new();
    let staged = (|| -> Result<(), UpdateError> {
        for name in SHIPPED {
            let src = pkg.join(name);
            if src.is_file() {
                steps.push(stage_copy(&src, &layout.bin_dir, name, true)?);
            }
        }
        steps.push(stage_bytes(
            desktop.as_bytes(),
            &apps,
            &format!("{APP_ID}.desktop"),
        )?);
        steps.push(stage_copy(
            &icon_src,
            &icons,
            &format!("{APP_ID}.svg"),
            false,
        )?);
        steps.push(stage_bytes(
            format!("{version}\n").as_bytes(),
            &state,
            "installed-version",
        )?);
        Ok(())
    })();
    if let Err(e) = staged {
        for s in &steps {
            let _ = std::fs::remove_file(&s.staged);
        }
        return Err(e);
    }
    commit(&steps, Strategy::RenameOver)?;
    refresh_caches(layout);
    Ok(())
}

/// `install.sh`'s `refresh_caches` (best effort; missing tools are fine).
fn refresh_caches(layout: &ScriptLayout) {
    if std::env::var_os("FERRO_UPDATE_NO_CACHE_REFRESH").is_some() {
        return;
    }
    let run = |cmd: &str, args: &[&std::ffi::OsStr]| -> bool {
        std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    };
    let apps = layout.apps_dir();
    run("update-desktop-database", &[apps.as_os_str()]);
    let hicolor = layout.data_dir.join("icons/hicolor");
    run(
        "gtk-update-icon-cache",
        &["-f".as_ref(), "-t".as_ref(), hicolor.as_os_str()],
    );
    if !run("kbuildsycoca6", &[]) {
        run("kbuildsycoca5", &[]);
    }
}

/// Installs the extracted package `pkg` into the Windows zip folder `dir`:
/// each shipped `.exe` is moved aside and replaced.
pub fn install_zip(dir: &Path, pkg: &Path) -> Result<(), UpdateError> {
    let app = exe_file(CRATE, true);
    if !pkg.join(&app).is_file() {
        return Err(UpdateError::BadArchive(format!("no {app} in the release")));
    }
    let mut steps = Vec::new();
    for name in SHIPPED {
        let file = exe_file(name, true);
        let src = pkg.join(&file);
        if src.is_file() {
            match stage_copy(&src, dir, &file, true) {
                Ok(s) => steps.push(s),
                Err(e) => {
                    for s in &steps {
                        let _ = std::fs::remove_file(&s.staged);
                    }
                    return Err(e);
                }
            }
        }
    }
    commit(&steps, Strategy::MoveAside)
}

/// Start-up cleanup in the install folder: `.old` executables left by a
/// Windows update and temporary folders of an interrupted update.
/// Returns how many entries were removed.
pub fn cleanup(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mine = format!(".{CRATE}-update-{}-", pid());
    let mut removed = 0;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let is_old = SHIPPED.iter().any(|s| {
            let exe = exe_file(s, true);
            name.strip_prefix(&exe)
                .is_some_and(|rest| rest == ".old" || is_numbered_old(rest))
        });
        let stale_tmp = name.starts_with(&format!(".{CRATE}-update-")) && !name.starts_with(&mine);
        let gone = if is_old {
            std::fs::remove_file(&path).is_ok()
        } else {
            stale_tmp
                && e.file_type().is_ok_and(|t| t.is_dir())
                && std::fs::remove_dir_all(&path).is_ok()
        };
        removed += usize::from(gone);
    }
    removed
}

/// `.<n>.old`.
fn is_numbered_old(rest: &str) -> bool {
    rest.strip_prefix('.')
        .and_then(|r| r.strip_suffix(".old"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// A private temporary folder inside `dir` (so every rename stays on one
/// filesystem), removed when dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new_in(dir: &Path) -> Result<Self, UpdateError> {
        std::fs::create_dir_all(dir).map_err(UpdateError::io)?;
        for _ in 0..8 {
            let path = dir.join(format!(
                ".{CRATE}-update-{}-{:08x}",
                pid(),
                fastrand::u32(..)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    crate::config::restrict_permissions(&path).map_err(UpdateError::io)?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(UpdateError::io(e)),
            }
        }
        Err(UpdateError::Io(
            "could not create a temporary folder".into(),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn desktop_entry_matches_install_sh() {
        let t = "[Desktop Entry]\nName=Ferrocord\nExec=ferrocord %U\nIcon=x\n";
        assert_eq!(
            render_desktop(t, Path::new("/home/u/.local/bin/ferrocord")),
            "[Desktop Entry]\nName=Ferrocord\nExec=/home/u/.local/bin/ferrocord %U\nIcon=x\n"
        );
        assert_eq!(
            render_desktop("Exec=ferrocord\n", Path::new("/home/my user/bin/ferrocord")),
            "Exec=\"/home/my user/bin/ferrocord\"\n"
        );
        // Every Exec= line gets the first line's field codes, like the awk.
        assert_eq!(
            render_desktop("Exec=a %U\n[Action]\nExec=b --x\n", Path::new("/b/f")),
            "Exec=/b/f %U\n[Action]\nExec=/b/f %U\n"
        );
    }

    fn steps_in(dir: &Path, names: &[&str]) -> Vec<Step> {
        names
            .iter()
            .map(|n| {
                let staged = dir.join(format!(".{n}.new"));
                write(&staged, &format!("new {n}"));
                Step {
                    staged,
                    target: dir.join(n),
                }
            })
            .collect()
    }

    #[test]
    fn commit_switches_every_file() {
        for strategy in [Strategy::RenameOver, Strategy::MoveAside] {
            let tmp = tempfile::tempdir().unwrap();
            let d = tmp.path();
            write(&d.join("a"), "old a");
            let steps = steps_in(d, &["a", "b"]);
            commit(&steps, strategy).unwrap();
            assert_eq!(read(&d.join("a")), "new a");
            assert_eq!(read(&d.join("b")), "new b");
            // No staged files or backups are left behind.
            let left: Vec<_> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert_eq!(left.len(), 2, "{strategy:?}: {left:?}");
        }
    }

    #[test]
    fn a_failed_switch_rolls_everything_back() {
        for strategy in [Strategy::RenameOver, Strategy::MoveAside] {
            let tmp = tempfile::tempdir().unwrap();
            let d = tmp.path();
            write(&d.join("a"), "old a");
            write(&d.join("c"), "old c");
            let steps = steps_in(d, &["a", "b", "c"]);
            // The last staged file vanished: its switch fails after `a`
            // and `b` were already switched.
            std::fs::remove_file(&steps[2].staged).unwrap();
            assert!(commit(&steps, strategy).is_err(), "{strategy:?}");
            assert_eq!(read(&d.join("a")), "old a", "{strategy:?}");
            assert!(!d.join("b").exists(), "{strategy:?}");
            assert_eq!(read(&d.join("c")), "old c", "{strategy:?}");
            for s in &steps {
                assert!(!s.staged.exists(), "{strategy:?}: staged file left");
            }
            let mut names: Vec<_> = std::fs::read_dir(d)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            assert_eq!(names, ["a", "c"], "{strategy:?}");
        }
    }

    #[test]
    fn move_aside_keeps_old_copies_free() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path().join("ferrocord.exe");
        write(&t, "x");
        assert_eq!(free_old_name(&t), tmp.path().join("ferrocord.exe.old"));
        // A removable stale `.old` is reused.
        write(&tmp.path().join("ferrocord.exe.old"), "stale");
        assert_eq!(free_old_name(&t), tmp.path().join("ferrocord.exe.old"));
        assert!(!tmp.path().join("ferrocord.exe.old").exists());
    }

    #[test]
    fn cleanup_removes_old_exes_and_stale_temp_dirs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        for f in [
            "ferrocord.exe.old",
            "ferrocord-player.exe.3.old",
            "ferrocord.exe",
            "notes.old",
            "ferrocord.exe.x.old",
        ] {
            write(&d.join(f), "x");
        }
        std::fs::create_dir(d.join(".ferrocord-update-1-deadbeef")).unwrap();
        let mine = TempDir::new_in(d).unwrap();
        assert_eq!(cleanup(d), 3);
        assert!(d.join("ferrocord.exe").exists());
        assert!(d.join("notes.old").exists());
        assert!(d.join("ferrocord.exe.x.old").exists());
        assert!(!d.join("ferrocord.exe.old").exists());
        assert!(!d.join(".ferrocord-update-1-deadbeef").exists());
        assert!(mine.path().exists(), "the running update's folder stays");
        let p = mine.path().to_path_buf();
        drop(mine);
        assert!(!p.exists());
    }

    fn fake_pkg(root: &Path, windows: bool) -> PathBuf {
        let pkg = root.join("pkg");
        for n in SHIPPED {
            write(&pkg.join(exe_file(n, windows)), &format!("v2 {n}"));
        }
        write(
            &pkg.join("packaging/dev.ferro.ferrocord.desktop"),
            "[Desktop Entry]\nExec=ferrocord %U\n",
        );
        write(&pkg.join("packaging/dev.ferro.ferrocord.svg"), "<svg/>");
        pkg
    }

    #[test]
    fn install_script_writes_what_install_sh_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ScriptLayout {
            bin_dir: tmp.path().join("bin"),
            data_dir: tmp.path().join("share"),
        };
        write(&layout.bin_path("ferrocord"), "v1");
        write(&layout.version_file(), "0.1.0\n");
        let pkg = fake_pkg(tmp.path(), false);
        // SAFETY: test-only; nothing else reads this variable concurrently
        // in a way that matters (it only skips the cache refresh).
        unsafe { std::env::set_var("FERRO_UPDATE_NO_CACHE_REFRESH", "1") };
        install_script(&layout, &pkg, Version::parse("0.2.0").unwrap()).unwrap();
        assert_eq!(read(&layout.bin_path("ferrocord")), "v2 ferrocord");
        assert_eq!(
            read(&layout.bin_path("ferrocord-player")),
            "v2 ferrocord-player"
        );
        assert_eq!(
            read(&layout.bin_path("ferrocord-login")),
            "v2 ferrocord-login"
        );
        assert_eq!(read(&layout.version_file()), "0.2.0\n");
        let desktop = read(&layout.apps_dir().join("dev.ferro.ferrocord.desktop"));
        assert!(desktop.contains(&format!(
            "Exec={} %U",
            layout.bin_path("ferrocord").display()
        )));
        assert_eq!(
            read(&layout.icon_dir().join("dev.ferro.ferrocord.svg")),
            "<svg/>"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(layout.bin_path("ferrocord")).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o755);
        }
        // Nothing temporary left in the bin dir.
        let n = std::fs::read_dir(&layout.bin_dir).unwrap().count();
        assert_eq!(n, 3);
    }

    #[test]
    fn install_script_refuses_an_incomplete_package() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = ScriptLayout {
            bin_dir: tmp.path().join("bin"),
            data_dir: tmp.path().join("share"),
        };
        let pkg = fake_pkg(tmp.path(), false);
        std::fs::remove_file(pkg.join("packaging/dev.ferro.ferrocord.svg")).unwrap();
        let v = Version::parse("0.2.0").unwrap();
        assert!(matches!(
            install_script(&layout, &pkg, v),
            Err(UpdateError::BadArchive(_))
        ));
        assert!(!layout.bin_path("ferrocord").exists());
    }

    #[test]
    fn install_zip_moves_exes_aside() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("Ferrocord");
        write(&dir.join("ferrocord.exe"), "v1");
        let pkg = fake_pkg(tmp.path(), true);
        install_zip(&dir, &pkg).unwrap();
        assert_eq!(read(&dir.join("ferrocord.exe")), "v2 ferrocord");
        assert_eq!(read(&dir.join("ferrocord-login.exe")), "v2 ferrocord-login");
        // Not running here, so the `.old` copy was deleted right away.
        assert!(!dir.join("ferrocord.exe.old").exists());
    }
}
