//! Aurora-dll: loaded into GRB.exe by the launcher. Draws the menu and the ESP inside the game's
//! own frames, runs god mode, fly, teleport and the hotkeys, and answers the launcher's requests
//! from `aurora-core::proto` on a named pipe.

mod airwalk;
mod credits;
mod dinput;
mod entities;
mod fly;
mod gearscore;
mod globals;
mod freeze;
mod godmode;
mod hwbp;
mod inbox;
mod nvgzone;
mod keys;
mod memory;
mod posehook;
mod menu;
mod noclip;
mod nvg;
mod server;
mod silentaim;
mod tp;
mod unseen;
mod vkdraw;
mod weapon;
mod watchdog;

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HINSTANCE, TRUE};
use windows_sys::core::BOOL;
use windows_sys::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;

pub const DLL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The build's name in the first log line: `AURORA_BUILD` as `tools\package.ps1` sets it, such as
/// "<build>, commit <short hash>"; "dev build" for a plain cargo build.
pub const BUILD: &str = match option_env!("AURORA_BUILD") {
    Some(b) => b,
    None => "dev build",
};

/// The build card: which Launchpad protocols this DLL speaks, its version and its build name, as
/// plain bytes in the DLL's own section `.aurora`. The Launchpad reads it from the stamped file
/// before loading (no card, or a protocol it does not speak: not injected) and, in a later step,
/// from the game's memory after loading. `#[used]` keeps it in although nothing here reads it.
#[used]
#[unsafe(link_section = ".aurora")]
pub static AURORA_CARD: aurora_core::card::Card = aurora_core::card::Card::new(aurora_core::PROTO_VERSION, aurora_core::PROTO_VERSION, DLL_VERSION, BUILD);

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// `Aurora\Dumper` for the mod's own files (dll.log, flytrace.log, the target mark), or `None` and
/// nothing is written: always `None` in unit tests, and while `Aurora` is not there (see
/// `aurora_core::paths::dumper_for_writing`).
pub(crate) fn dumper_for_writing() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        return None;
    }
    aurora_core::paths::dumper_for_writing()
}

/// Log file: `%APPDATA%\Aurora\Dumper\dll.log`. Appends, never panics. Never written by unit tests,
/// nor while `Aurora` is not there: see [`dumper_for_writing`].
pub fn log(msg: &str) {
    let lock = LOG.get_or_init(|| {
        let file = dumper_for_writing().and_then(|d| OpenOptions::new().create(true).append(true).open(d.join("dll.log")).ok());
        Mutex::new(file)
    });
    if let Ok(mut guard) = lock.lock()
        && let Some(f) = guard.as_mut()
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{now}] {msg}");
    }
}

/// The start-up code searches are done; the Load On Inject profile waits for them, so the menu is
/// not stuck behind them.
pub static WARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The warm-up threads (code searches, the first entity scan), joined before unloading: a thread
/// still searching inside a freed DLL crashes the game.
static WARM: Mutex<Vec<(&'static str, std::thread::JoinHandle<()>)>> = Mutex::new(Vec::new());

/// Unload has begun: the warm-up stops between its steps and switches nothing on, god mode's learn
/// wait ends. Set only once the unload is sure to go ahead.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn stopping() -> bool {
    STOPPING.load(std::sync::atomic::Ordering::SeqCst)
}

pub fn begin_stop() {
    STOPPING.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// Sleep up to `ms`, in short steps; false when the unload began meanwhile.
pub fn nap(ms: u64) -> bool {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        if stopping() {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50).min(end.saturating_duration_since(std::time::Instant::now())));
    }
    !stopping()
}

/// Keep a thread to wait for on unload. Ended ones are dropped as new ones come.
pub fn keep_warm(what: &'static str, h: std::thread::JoinHandle<()>) {
    if let Ok(mut w) = WARM.lock() {
        w.retain(|(_, h)| !h.is_finished());
        w.push((what, h));
    }
}

/// Threads that did not end in time on unload. The start thread waits for them and frees the DLL
/// once they have ended, so a new inject works again without a game restart.
static LATE: Mutex<Vec<(String, std::thread::JoinHandle<()>)>> = Mutex::new(Vec::new());

/// Wait for every late thread, with a log line every 30 s. Returns once all have ended.
fn wait_late() {
    let t = std::time::Instant::now();
    let mut said = std::time::Instant::now();
    loop {
        let left: Vec<String> = match LATE.lock() {
            Ok(mut l) => {
                l.retain(|(_, h)| !h.is_finished());
                l.iter().map(|(w, _)| w.clone()).collect()
            }
            Err(_) => Vec::new(),
        };
        if left.is_empty() {
            if t.elapsed().as_secs() >= 1 {
                log(&format!("unload: the late threads ended after {} s", t.elapsed().as_secs()));
            }
            return;
        }
        if said.elapsed() >= std::time::Duration::from_secs(30) {
            said = std::time::Instant::now();
            log(&format!("unload: still waiting for {} before the DLL is freed ({} s)", left.join(", "), t.elapsed().as_secs()));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Wait for the warm-up threads (unload, after `begin_stop`).
pub fn join_warm() {
    let list: Vec<_> = WARM.lock().map(|mut w| w.drain(..).collect()).unwrap_or_default();
    for (what, h) in list {
        join_for(h, what, std::time::Duration::from_secs(15));
    }
}

/// Wait until the thread has ended, at most `limit`. True when it did.
fn wait_done(h: &std::thread::JoinHandle<()>, limit: std::time::Duration) -> bool {
    let t = std::time::Instant::now();
    while !h.is_finished() {
        if t.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    true
}

/// Join a thread on unload, but never hold the unload for ever. A thread still running after
/// `limit` is kept: the DLL is freed only once it has ended (`wait_late`), since freeing its code
/// under it would crash the game.
pub fn join_for(h: std::thread::JoinHandle<()>, what: &str, limit: std::time::Duration) -> bool {
    if wait_done(&h, limit) {
        let _ = h.join();
        return true;
    }
    log(&format!("unload: {what} still running after {} s; the DLL is freed once it ends", limit.as_secs()));
    if let Ok(mut l) = LATE.lock() {
        l.push((what.to_string(), h));
    }
    false
}

/// This DLL's module handle, for unloading itself.
static MODULE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Option B: after the load, Aurora touches nothing until the Launchpad has checked it (the hello,
/// `aurora_hello`) and said go (`aurora_go`). No go within this time, and the DLL frees itself:
/// nothing was hooked, so there is nothing to undo.
const GO_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
/// Set by `aurora_go`; the start thread waits on it.
static GO: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());

/// The Launchpad's hello: `p` is the buffer it wrote into the game (`aurora_core::hello`), run on a
/// thread it started here. The answer goes back into the same buffer: the challenge it sent, the
/// protocol it speaks, this DLL's version, build, pid and module address. Nothing else happens;
/// nothing is remembered. Returns `HELLO_OK`, or why the buffer was not answered.
///
/// # Safety
/// `p` must point at `HELLO_SIZE` writable bytes, or be null; the Launchpad allocates them.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn aurora_hello(p: *mut c_void) -> u32 {
    use aurora_core::hello::{HELLO_NULL, HELLO_OK, HELLO_SIZE, Hello};
    if p.is_null() {
        return HELLO_NULL;
    }
    // SAFETY: the caller gives HELLO_SIZE bytes; the label and size are checked before any
    // Write, so a buffer that is not a hello is only read.
    let hello = unsafe { &mut *(p as *mut Hello) };
    let proto = match Hello::check_ask(&hello.0[..HELLO_SIZE]) {
        Ok(proto) => proto,
        Err(code) => return code,
    };
    // Answer in the Launchpad's protocol: this DLL speaks only its own, so a Launchpad with another
    // one gets an answer that says so (its check then fails, and no go comes).
    let proto = if proto == aurora_core::PROTO_VERSION { proto } else { aurora_core::PROTO_VERSION };
    hello.answer(proto, DLL_VERSION, BUILD, std::process::id(), MODULE.load(std::sync::atomic::Ordering::SeqCst) as u64);
    HELLO_OK
}

/// The Launchpad's go, after a hello that checked out: the start thread stops waiting and starts
/// the mod. A second go changes nothing. Returns 1.
///
/// # Safety
/// Takes no pointer it reads; the argument is the thread start routine's and is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn aurora_go(_p: *mut c_void) -> u32 {
    let (lock, cv) = &GO;
    if let Ok(mut go) = lock.lock() {
        *go = true;
        cv.notify_all();
    }
    1
}

/// Wait for `aurora_go`, at most `GO_WAIT`. True when it came.
fn wait_for_go() -> bool {
    let (lock, cv) = &GO;
    let Ok(go) = lock.lock() else { return false };
    let (go, _) = match cv.wait_timeout_while(go, GO_WAIT, |go| !*go) {
        Ok(x) => x,
        Err(_) => return false,
    };
    *go
}
/// The in-game menu's Unload with no Launchpad connected: the mod shuts down through its own pipe
/// and then frees itself. See `menu::unload_now` and the end of the start thread below.
pub static SELF_UNLOAD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub static UNLOADER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// Set when the drawing hooks could not come out: the DLL then stays loaded, since freeing it would
/// crash the game. A thread that did not end in time no longer sets it.
pub static KEEP_LOADED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Entry point Windows calls on load.
///
/// # Safety
/// Called by the loader with a valid module handle; nothing else may call it.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        // SAFETY: plain Win32 call with the module handle we were given.
        unsafe { DisableThreadLibraryCalls(hinst) };
        MODULE.store(hinst as usize, std::sync::atomic::Ordering::SeqCst);
        // Work happens on our own thread; DllMain returns at once (loader lock).
        std::thread::spawn(|| {
            log(&format!("aurora-dll {DLL_VERSION} ({BUILD}) attached, pid {}", std::process::id()));
            // A panic on one of our threads otherwise leaves no trace in the log.
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                log(&format!("panic: {info}"));
                prev(info);
            }));
            // Option B: nothing is touched until the Launchpad has checked this copy (the hello)
            // and said go. No go in time: log why, and free the DLL; no hook, thread or file of the
            // mod's has started, so there is nothing to undo.
            let t = std::time::Instant::now();
            if !wait_for_go() {
                log(&format!("no go from the Launchpad within {} s (its hello check failed, or it is an older Launchpad without the hello): unloading, nothing was started", GO_WAIT.as_secs()));
                // Our panic hook points into this code: back to the one before it.
                let _ = std::panic::take_hook();
                // SAFETY: our own module handle; this call frees the DLL and ends this thread
                // Without returning into the freed code.
                unsafe { windows_sys::Win32::System::LibraryLoader::FreeLibraryAndExitThread(MODULE.load(std::sync::atomic::Ordering::SeqCst) as _, 0) }
            }
            log(&format!("go from the Launchpad after {} ms", t.elapsed().as_millis()));
            // Where to hook is read now, once (launchpad > Options).
            vkdraw::hook_start();
            menu::start();
            keep_warm("entity warm-up", entities::warm_up());
            // The code searches behind Undetected (enemy NPC eyesight only, see unseen.rs) and the
            // weapon switches take seconds; do them now so switching on is instant.
            let warm = std::thread::spawn(|| {
                let t = std::time::Instant::now();
                // The fixed places in the game's data first: the ESP, fly, gear score and credits
                // read through them (until found they skip, as for a bad pointer). An unload stops
                // it between steps.
                let steps: [(&str, fn()); 8] = [
                    ("globals", globals::find_all),
                    ("unseen", unseen::warm),
                    ("weapon", weapon::warm),
                    // Silent Aim's code search: nothing goes in until the player turns it on.
                    ("silent aim", silentaim::warm),
                    ("nvg", nvg::warm),
                    ("airwalk", airwalk::warm),
                    ("fly through", noclip::warm),
                    ("credits", credits::warm),
                ];
                for (name, step) in steps {
                    if stopping() {
                        log(&format!("code searches stopped for the unload before {name}"));
                        return;
                    }
                    step();
                }
                WARMED.store(true, std::sync::atomic::Ordering::SeqCst);
                log(&format!("code searches done in {} ms", t.elapsed().as_millis()));
            });
            keep_warm("code searches", warm);
            // God mode stays off; the first hit only teaches it who the player is.
            godmode::learn_background(10 * 60 * 1000);
            watchdog::start();
            server::run();
            log("server thread ended");
            if KEEP_LOADED.load(std::sync::atomic::Ordering::SeqCst) {
                log("not unloading: a drawing hook is still in (see above); the DLL stays loaded until the game closes");
            } else if SELF_UNLOAD.load(std::sync::atomic::Ordering::SeqCst) {
                // Everything is shut down (the Bye on our own pipe did it); the thread that sent
                // it, and any thread that was late, must be gone too before the code goes away.
                if let Some(h) = UNLOADER.lock().ok().and_then(|mut u| u.take()) {
                    let _ = h.join();
                }
                wait_late();
                std::thread::sleep(std::time::Duration::from_millis(500));
                log("unloading itself");
                // SAFETY: our own module handle; this call frees the DLL and ends this thread
                // Without returning into the freed code.
                unsafe {
                    windows_sys::Win32::System::LibraryLoader::FreeLibraryAndExitThread(
                        MODULE.load(std::sync::atomic::Ordering::SeqCst) as _,
                        0,
                    )
                }
            }
        });
    }
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unit_tests_write_no_log() {
        // The player's ask: the tests had left lines in the player's real dll.log. No folder is
        // given to them now, so the log never opens.
        assert_eq!(dumper_for_writing(), None);
        log("unit_tests_write_no_log");
        assert!(LOG.get().is_some_and(|l| l.lock().is_ok_and(|f| f.is_none())));
    }

    /// The build card says this DLL speaks this protocol, with this version and build name; the
    /// Launchpad finds it by its label.
    #[test]
    fn the_build_card_says_who_we_are() {
        let card = aurora_core::card::find(&AURORA_CARD.0).unwrap();
        assert!(card.speaks(aurora_core::PROTO_VERSION));
        assert_eq!((card.proto_min, card.proto_max), (aurora_core::PROTO_VERSION, aurora_core::PROTO_VERSION));
        assert_eq!(card.version, DLL_VERSION);
        assert_eq!(card.build, BUILD);
    }

    /// The hello answers a Launchpad's ask in place, with this DLL's card facts, and refuses what
    /// is not a hello; the go ends the wait (Gatekeeper step 3, option B).
    #[test]
    fn hello_answers_and_go_ends_the_wait() {
        use aurora_core::hello::{CHALLENGE_LEN, HELLO_NOT_A_HELLO, HELLO_NULL, HELLO_OK, Hello};
        let challenge = [3u8; CHALLENGE_LEN];
        let mut h = Hello::ask(aurora_core::PROTO_VERSION, 77, &challenge);
        // SAFETY: a HELLO_SIZE buffer of ours.
        assert_eq!(unsafe { aurora_hello(h.0.as_mut_ptr() as *mut c_void) }, HELLO_OK);
        let a = h.read_answer().unwrap();
        let card = aurora_core::card::find(&AURORA_CARD.0).unwrap();
        let want = aurora_core::hello::Expected { challenge: &challenge, proto: aurora_core::PROTO_VERSION, card: &card, pid: std::process::id(), module: 0 };
        assert_eq!(a.check(&want), Ok(()));
        assert_eq!(unsafe { aurora_hello(std::ptr::null_mut()) }, HELLO_NULL);
        let mut not = [0u8; aurora_core::hello::HELLO_SIZE];
        assert_eq!(unsafe { aurora_hello(not.as_mut_ptr() as *mut c_void) }, HELLO_NOT_A_HELLO);
        assert!(not.iter().all(|&b| b == 0), "a buffer that is not a hello is left alone");
        // The go.
        let waiter = std::thread::spawn(wait_for_go);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!waiter.is_finished());
        assert_eq!(unsafe { aurora_go(std::ptr::null_mut()) }, 1);
        assert!(waiter.join().unwrap());
        assert!(wait_for_go(), "a go given stays given");
    }

    #[test]
    fn wait_done_sees_an_ended_thread() {
        let h = std::thread::spawn(|| {});
        assert!(wait_done(&h, Duration::from_secs(5)));
    }

    #[test]
    fn wait_done_gives_up_on_a_stuck_thread() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        let t = std::time::Instant::now();
        assert!(!wait_done(&h, Duration::from_millis(100)));
        assert!(t.elapsed() < Duration::from_secs(2));
        drop(tx);
        assert!(wait_done(&h, Duration::from_secs(5)));
    }
}
