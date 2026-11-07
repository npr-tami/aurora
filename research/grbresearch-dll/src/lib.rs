//! grbresearch-dll: research recorders inside the game, kept out of the mod. Injected only when a
//! research step asks for it. One thread polls request files in `%APPDATA%\Aurora\Dumper\research\`
//! (shared with grbresearch.exe): `watch.request` (who writes / reads / runs an address, see
//! `watch`), `regs.request` (who runs these code spots, with which registers, see `regs`) and
//! `research-unload.request` (take every hook and watchpoint off, then unload this DLL, so a new
//! build can go in without restarting the game). It shares no code with aurora-dll. Load it once: a
//! second load is only counted by Windows, and while it is, an unload request is refused (logged)
//! so the DLL never stays in without its worker.

mod hwbp;
mod memory;
mod regs;
mod watch;

use std::ffi::c_void;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::{HINSTANCE, TRUE};
use windows_sys::Win32::System::LibraryLoader::{DisableThreadLibraryCalls, FreeLibraryAndExitThread};
use windows_sys::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_sys::Win32::System::Threading::CreateThread;
use windows_sys::core::BOOL;

pub const DLL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Our module handle, for unloading ourselves.
static MODULE: AtomicUsize = AtomicUsize::new(0);
static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// Log file: `%APPDATA%\Aurora\Dumper\research-dll.log`. Appends, never panics. Not written when
/// `Aurora` is not there: see `aurora_core::paths::dumper_for_writing`.
pub fn log(msg: &str) {
    let lock = LOG.get_or_init(|| {
        let file = aurora_core::paths::dumper_for_writing().and_then(|d| OpenOptions::new().create(true).append(true).open(d.join("research-dll.log")).ok());
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

/// Where the request files go and the results come out: `%APPDATA%\Aurora\Dumper\research\`.
pub fn folder() -> PathBuf {
    aurora_core::paths::dumper().join("research")
}

/// Read a request file and remove it, so each request runs once. It is always removed once read,
/// even when it is not valid text (broken bytes become U+FFFD). Writers should save `name.tmp` and
/// then rename it to `name`: only the exact name is read, so a half-written file is never parsed. A
/// file that cannot be read yet (still open) is tried again next time.
pub fn take_request(name: &str) -> Option<String> {
    let f = folder().join(name);
    let bytes = std::fs::read(&f).ok()?;
    if let Err(e) = std::fs::remove_file(&f) {
        log(&format!("{name}: read, but not removed ({e})"));
    }
    Some(decode_request(&bytes))
}

/// Request bytes as text: UTF-16 when it starts with that BOM (Windows PowerShell 5's `>` writes
/// it), else UTF-8 with any broken bytes replaced; a leading BOM is dropped.
fn decode_request(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

/// How many loads of our module Windows still counts (one injection = 1; loading it twice = 2, and
/// one unload then leaves it in with no worker). Read from the loader's own entry for it (PEB > Ldr
/// > entry > DdagNode > LoadCount, Windows 8 and later), under the loader lock so the list cannot
/// change meanwhile. None when that fails.
fn load_count(module: usize) -> Option<u32> {
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    type Lock = unsafe extern "system" fn(u32, *mut u32, *mut usize) -> i32;
    type Unlock = unsafe extern "system" fn(u32, usize) -> i32;
    let ntdll: Vec<u16> = "ntdll.dll\0".encode_utf16().collect();
    // SAFETY: ntdll is always loaded; the two exports have had these shapes since NT 5.
    // The loader structures are read only while the loader lock is held, and the walk is bounded.
    unsafe {
        let nt = GetModuleHandleW(ntdll.as_ptr());
        let lock = std::mem::transmute::<unsafe extern "system" fn() -> isize, Lock>(GetProcAddress(
            nt,
            c"LdrLockLoaderLock".as_ptr() as *const u8,
        )?);
        let unlock = std::mem::transmute::<unsafe extern "system" fn() -> isize, Unlock>(GetProcAddress(
            nt,
            c"LdrUnlockLoaderLock".as_ptr() as *const u8,
        )?);
        let mut state = 0u32;
        let mut cookie = 0usize;
        if lock(0, &mut state, &mut cookie) < 0 {
            return None;
        }
        let peb: *const u8;
        std::arch::asm!("mov {}, gs:[0x60]", out(reg) peb, options(nostack, readonly, preserves_flags));
        let ldr = *(peb.add(0x18) as *const *const u8);
        // InLoadOrderModuleList; each entry starts with its link, DllBase at 0x30, DdagNode at
        // 0x98, and the node's LoadCount at 0x18.
        let head = ldr.add(0x10);
        let mut e = *(head as *const *const u8);
        let mut found = None;
        for _ in 0..4096 {
            if e.is_null() || e == head {
                break;
            }
            if *(e.add(0x30) as *const usize) == module {
                let node = *(e.add(0x98) as *const *const u8);
                found = (!node.is_null()).then(|| *(node.add(0x18) as *const u32));
                break;
            }
            e = *(e as *const *const u8);
        }
        unlock(0, cookie);
        found
    }
}

/// An address as a request file gives it: `0x1234abcd`, `1234abcd`, or `exe+0x2a5a4c0` (any loaded
/// module by name works too: `vulkan-1.dll+0x1000`).
pub fn parse_addr(s: &str) -> Option<u64> {
    let hex = |h: &str| u64::from_str_radix(h.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok();
    match s.split_once('+') {
        Some((module, off)) => {
            let m = memory::find_module(if module.eq_ignore_ascii_case("exe") { "" } else { module })?;
            Some(m.base + hex(off)?)
        }
        None => hex(s),
    }
}

/// The one thread: polls the request files twice a second until asked to unload.
fn worker() {
    log(&format!("grbresearch-dll {DLL_VERSION} attached, pid {}", std::process::id()));
    // Inside an existing Aurora only (never makes Aurora itself; see dumper_for_writing).
    if aurora_core::paths::dumper_for_writing().is_some() {
        let _ = std::fs::create_dir_all(folder());
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log(&format!("panic: {info}"));
        prev(info);
    }));
    let module = MODULE.load(Ordering::SeqCst);
    loop {
        std::thread::sleep(Duration::from_millis(500));
        if take_request("research-unload.request").is_some() {
            // Load once. Loaded twice, one unload would leave it in with no worker, so it keeps
            // working instead and stays until the game closes.
            match load_count(module) {
                Some(n) if n > 1 => log(&format!(
                    "unload refused: this DLL is loaded {n} times (load it only once); it keeps working until the game closes"
                )),
                Some(_) => break,
                None => {
                    log("load count unknown: unloading as asked");
                    break;
                }
            }
        }
        watch::check_request();
        regs::check_request();
    }
    log("unload asked: stopping the recorders");
    // A watch ends at once and clears its watchpoint; a regs run takes its jumps out.
    watch::shutdown();
    regs::shutdown();
    // A game thread may still be leaving our exception handler.
    std::thread::sleep(Duration::from_millis(200));
    if hwbp::must_stay_loaded() {
        // A thread may still have DR1 armed, and our handler is what catches it.
        log("not unloading: a watch could not clear DR1 on every thread, so its handler stays; this DLL stays loaded until the game closes");
        return;
    }
    log("unloading");
    // SAFETY: our own module handle. Nothing of ours runs after this: FreeLibrary and
    // ExitThread in one call never return into the unloaded code.
    unsafe { FreeLibraryAndExitThread(module as _, 0) };
}

unsafe extern "system" fn thread_start(_: *mut c_void) -> u32 {
    worker();
    0
}

/// Entry point Windows calls on load.
///
/// # Safety
/// Called by the loader with a valid module handle; nothing else may call it.
#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(hinst: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(hinst as usize, Ordering::SeqCst);
        // SAFETY: plain Win32 calls with the module handle we were given. Work happens on
        // Our own thread (not a std one, so it can end inside FreeLibraryAndExitThread); DllMain
        // returns at once (loader lock).
        unsafe {
            DisableThreadLibraryCalls(hinst);
            CreateThread(std::ptr::null(), 0, Some(thread_start), std::ptr::null(), 0, std::ptr::null_mut());
        }
    }
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_read_lossily() {
        // A UTF-8 BOM, then a Latin-1 byte (not UTF-8) in a comment: still parsed.
        let raw = b"\xEF\xBB\xBFsecs 20\n# caf\xE9\nspot a; 90 90 90 90 90; 0; 5\n";
        let text = decode_request(raw);
        assert!(text.starts_with("secs 20"), "BOM dropped");
        assert!(text.contains('\u{FFFD}'));
        assert_eq!(regs::parse(&text).unwrap().secs, 20);
        // A bad byte right in a watch line only spoils that word.
        assert_eq!(decode_request(b"0x1234 500 w\xFF").split_whitespace().next(), Some("0x1234"));
        // Windows PowerShell 5's `>` writes UTF-16 with a BOM.
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain("0x10 500 a".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(decode_request(&utf16), "0x10 500 a");
    }

    #[test]
    fn load_count_follows_loads() {
        use windows_sys::Win32::Foundation::FreeLibrary;
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
        let name: Vec<u16> = "msimg32.dll\0".encode_utf16().collect();
        // SAFETY: a small system DLL loaded into this test process and freed again.
        unsafe {
            let a = LoadLibraryW(name.as_ptr());
            assert!(!a.is_null());
            let one = load_count(a as usize).expect("count readable");
            let b = LoadLibraryW(name.as_ptr());
            let two = load_count(a as usize);
            FreeLibrary(b);
            let back = load_count(a as usize);
            FreeLibrary(a);
            assert_eq!(two, Some(one + 1));
            assert_eq!(back, Some(one));
        }
    }
}
