//! The key table the fly and hotkey threads read: built in, but for the menu and command box keys
//! from Hotkeys.txt. Loaded at start, replaced when the menu changes a binding.

use std::sync::Mutex;

use aurora_core::keys::Keys;

static KEYS: Mutex<Option<Keys>> = Mutex::new(None);

pub fn get() -> Keys {
    let mut g = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    *g.get_or_insert_with(aurora_core::load_keys)
}

pub fn set(k: Keys) {
    let mut g = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some(k);
}

/// True while one of the game's own windows has focus. Keys typed into the in-game menu (or any
/// other program) must not fly the player or fire hotkeys.
pub fn game_focused() -> bool {
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain Win32 queries.
    unsafe {
        let w = GetForegroundWindow();
        if w.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(w, &mut pid);
        pid == GetCurrentProcessId()
    }
}
