//! Windows toast identity.
//!
//! A toast is attributed to the AppUserModelID it is sent with. Without one,
//! `notify-rust` falls back to PowerShell's, so every notification showed as
//! "Windows PowerShell". An unpackaged app gets its own name by registering
//! the id under `HKCU\Software\Classes\AppUserModelId\<id>` (per user, no
//! admin rights) and using it for its process and its toasts.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

use crate::config::{APP_ID, APP_NAME};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Registers [`APP_ID`] with the display name [`APP_NAME`] and makes it the
/// process's id. Failures are logged: toasts then keep the fallback name.
pub fn register() {
    let key_path = wide(&format!(r"Software\Classes\AppUserModelId\{APP_ID}"));
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated wide strings and out-pointer; the key
    // is closed below.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_path.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if rc != ERROR_SUCCESS {
        tracing::debug!(rc, "could not register the toast app id");
        return;
    }
    let mut values = vec![("DisplayName", APP_NAME.to_owned())];
    if let Some(icon) = icon_path() {
        values.push(("IconUri", icon));
    }
    for (name, value) in values {
        let name = wide(name);
        let data = wide(&value);
        // SAFETY: `key` is open; `data` is a NUL-terminated REG_SZ whose
        // byte length includes the terminator.
        let rc = unsafe {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                (data.len() * 2) as u32,
            )
        };
        if rc != ERROR_SUCCESS {
            tracing::debug!(rc, "could not set a toast app id value");
        }
    }
    // SAFETY: `key` was opened above.
    unsafe { RegCloseKey(key) };
    let id = wide(APP_ID);
    // SAFETY: NUL-terminated wide string, copied by the call.
    unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
}

/// The icon shown on toasts: an `ferrocord.ico` / `ferrocord.png` next to
/// the executable, if one ships with it.
fn icon_path() -> Option<String> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    ["ferrocord.png", "ferrocord.ico"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}
