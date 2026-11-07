//! Open a folder in the user's own folder app. `cmd /C start` used the `open` verb, which is
//! Explorer's even when another app is the folder default; `ShellExecuteW` with no verb runs the
//! folder's default verb, as a
//! double-click does. On its own thread, with COM set up as the shell wants.

use std::os::windows::ffi::OsStrExt;
use std::path::Path;

pub fn open(dir: &Path) {
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    std::thread::spawn(move || {
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        // SAFETY: COM for this thread only; the path is NUL-terminated and lives to the end.
        unsafe {
            let com = CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
            ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), wide.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
            if com >= 0 {
                CoUninitialize();
            }
        }
    });
}
