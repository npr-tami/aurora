//! `Bin\Launchpad.lock`, as Stand's: the file is there only while a Launchpad is open. The open one
//! holds it alone, so a second Launchpad can't open it and does not start; it brings the open one's
//! window forward instead. Windows deletes the file when the Launchpad closes, also when it crashes
//! (the handle closes with the process).

use std::fs::File;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use windows_sys::Win32::Foundation::{ERROR_SHARING_VIOLATION, GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_FLAG_DELETE_ON_CLOSE};

/// Take the lock: the file is made (or an old one opened) for this Launchpad alone, and goes away
/// when it closes. Keep the `File` for as long as the Launchpad runs.
pub fn take(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path)
}

/// The error means another Launchpad holds the lock.
pub fn held_elsewhere(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32)
}

/// Another Launchpad is open: its lock file is there and can't be opened. Opens nothing new: a
/// missing file stays missing (before the old folders move, nothing may be made).
pub fn other_open(path: &Path) -> bool {
    match std::fs::OpenOptions::new().read(true).share_mode(0).open(path) {
        Ok(_) => false,
        Err(e) => held_elsewhere(&e),
    }
}

/// Bring the open Launchpad's window forward, restored when it was minimised: by `title`, else by
/// the dotted title older Launchpads have.
pub fn show_other(title: &str) {
    if !show_titled(title) {
        show_titled(&title.replacen("Aurora ", "Aurora.", 1));
    }
}

/// `show_other` for one exact title; false when no window has it.
fn show_titled(title: &str) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that lives through the call; the
    // Window handle is only passed back to user32, which checks it.
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), wide.as_ptr());
        if hwnd.is_null() {
            return false;
        }
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn lock_is_there_only_while_held() {
        // AURORA_TEST_DIR when set, else the temp folder.
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("aurora-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Launchpad.lock");
        assert!(!super::other_open(&path));
        assert!(!path.exists(), "a check alone makes no file");
        let held = super::take(&path).unwrap();
        assert!(path.exists());
        // A second Launchpad: can't take it, and sees the first one is open.
        let second = super::take(&path).unwrap_err();
        assert!(super::held_elsewhere(&second), "{second}");
        assert!(super::other_open(&path));
        // The first one closes: the file goes with it, and the next one can start.
        drop(held);
        assert!(!path.exists());
        assert!(!super::other_open(&path));
        // An old file left there (not held) is taken over, and also goes on close.
        std::fs::write(&path, "").unwrap();
        assert!(!super::other_open(&path));
        drop(super::take(&path).unwrap());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
