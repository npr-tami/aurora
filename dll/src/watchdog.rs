//! Freeze guard: checks every second whether the game's window still answers. When it hangs, it
//! writes down what the DLL was doing, then stops fly and any running watchpoint. If the game is
//! still stuck 5 s later, it drops every freeze.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowThreadProcessId, IsHungAppWindow, IsWindowVisible,
};

use crate::{fly, freeze, godmode, hwbp, log};

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// The last request the window sent, for the hang report.
static LAST_REQUEST: Mutex<String> = Mutex::new(String::new());

pub fn note(req: &str) {
    if let Ok(mut l) = LAST_REQUEST.lock() {
        l.clear();
        l.extend(req.chars().take(100));
    }
}

pub fn start() {
    if let Ok(mut w) = WORKER.lock()
        && w.is_none()
    {
        STOP.store(false, Ordering::SeqCst);
        *w = Some(std::thread::spawn(worker));
    }
}

pub fn shutdown() {
    STOP.store(true, Ordering::SeqCst);
    if let Ok(mut w) = WORKER.lock()
        && let Some(h) = w.take()
    {
        let _ = h.join();
    }
}

struct Search {
    pid: u32,
    best: Option<(HWND, i64)>,
}

unsafe extern "system" fn each_window(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: lparam is the &mut Search handed to EnumWindows, alive for the call.
    let s = unsafe { &mut *(lparam as *mut Search) };
    let mut pid = 0u32;
    // SAFETY: plain Win32 queries on a handle EnumWindows gave us.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == s.pid && IsWindowVisible(hwnd) != 0 {
            let mut r: RECT = std::mem::zeroed();
            if GetClientRect(hwnd, &mut r) != 0 {
                let area = (r.right - r.left) as i64 * (r.bottom - r.top) as i64;
                if area > s.best.map(|b| b.1).unwrap_or(0) {
                    s.best = Some((hwnd, area));
                }
            }
        }
    }
    1
}

/// The game's biggest visible window.
fn game_window() -> Option<HWND> {
    let mut s = Search { pid: std::process::id(), best: None };
    // SAFETY: the callback only touches `s`, which outlives the call.
    unsafe { EnumWindows(Some(each_window), &mut s as *mut Search as LPARAM) };
    s.best.map(|b| b.0)
}

fn report(stage: &str) {
    let (entity, armed, _) = godmode::status();
    let last = LAST_REQUEST.lock().map(|l| l.clone()).unwrap_or_default();
    log(&format!(
        "FREEZE GUARD ({stage}): game window not answering. fly {} (loop ticks {}), god entity {entity:#x} armed {armed}, watchpoint {:#x}, freezes [{}], last request: {last}",
        if fly::is_on() { "on" } else { "off" },
        fly::ticks(),
        hwbp::watching(),
        freeze::ids().join(", "),
    ));
}

fn worker() {
    let mut hung_for = 0u32;
    let mut window: Option<HWND> = None;
    let mut since_lookup = 0u32;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        // The weapon hooks need the player's body and network part refreshed every second.
        crate::weapon::tick();
        crate::airwalk::refresh();
        // Look the window up again now and then (loading screens, alt-tab).
        since_lookup += 1;
        if window.is_none() || since_lookup >= 10 {
            window = game_window();
            since_lookup = 0;
        }
        let Some(w) = window else { continue };
        // SAFETY: plain Win32 query; a stale handle just reads as "not hung".
        let hung = unsafe { IsHungAppWindow(w) } != 0;
        if !hung {
            if hung_for > 0 {
                log(&format!("freeze guard: game answering again after {hung_for} s"));
            }
            hung_for = 0;
            continue;
        }
        hung_for += 1;
        match hung_for {
            1 => {
                report("first");
                // Our likeliest hands in the game: fly and a watchpoint.
                let _ = fly::set(false);
                hwbp::cancel();
                log("freeze guard: fly stopped, watchpoint cancelled");
            }
            6 => {
                report("still stuck");
                freeze::clear();
                log("freeze guard: every freeze dropped (god mode hold included)");
            }
            _ => {}
        }
    }
}
