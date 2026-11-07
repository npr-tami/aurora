//! Hardware watchpoint: "who writes / reads / runs this address?". Sets DR1 on every thread,
//! catches the single-step trap in a vectored exception handler, records rip and the registers,
//! lets the game carry on. Cleared after the time is up. A copy of the mod's hwbp.rs with one
//! change: it uses DR1, not DR0, so it never takes god mode's or the credits catch's watchpoint
//! (DR0) away from the mod, and the two handlers each answer only for their own register. Both take
//! one named lock (`Local\aurora-hwbp-<pid>`) around each thread's suspend..resume, so neither
//! writes back a stale copy of the other's debug registers.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use aurora_core::proto::WriteHit;
use windows_sys::Win32::Foundation::{
    CloseHandle, EXCEPTION_SINGLE_STEP, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, CONTEXT_DEBUG_REGISTERS_AMD64, EXCEPTION_CONTINUE_EXECUTION, EXCEPTION_POINTERS,
    GetThreadContext, RemoveVectoredExceptionHandler, SetThreadContext,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentThreadId, OpenThread, ReleaseMutex, ResumeThread, SuspendThread, THREAD_GET_CONTEXT,
    THREAD_SET_CONTEXT, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

use crate::{log, memory};

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
/// The catch list holds 256 hits; a watch ends early once it is full.
pub const MAX_RAW: usize = 256;
/// DR7 bits of DR1: L1 G1 enable (2, 3), RW1 (20, 21), LEN1 (22, 23).
const DR1_BITS: u64 = 0x00F0_000C;
/// DR7 bits L1 G1: DR1 is armed.
const DR1_ENABLE: u64 = 0xC;
/// DR6 bit B1: DR1 fired.
const DR6_B1: u64 = 0x2;
/// How long to wait for the shared debug-register lock before skipping a thread. It is only ever
/// held for one thread's suspend..resume.
const LOCK_WAIT_MS: u32 = 250;
/// Rounds of "check DR1 is off, clear the ones that are not" when a watch ends.
const CLEAR_RETRIES: usize = 5;

/// One raw catch, filled inside the exception handler (no allocation there).
#[derive(Clone, Copy, Default)]
struct Raw {
    rip: u64,
    thread: u32,
    regs: [u64; 16],
    ret: u64,
}

static WATCH_ADDR: AtomicU64 = AtomicU64::new(0);
static CANCEL: AtomicBool = AtomicBool::new(false);
static EXEC: AtomicBool = AtomicBool::new(false);
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Catches kept so far (mirrors the count inside `RAW`, for the waiting loop).
static STORED: AtomicUsize = AtomicUsize::new(0);
static RAW: Mutex<(usize, [Raw; MAX_RAW])> = Mutex::new((0, [Raw { rip: 0, thread: 0, regs: [0; 16], ret: 0 }; MAX_RAW]));
/// Keep one catch per value of this register (16: keep all), so a busy function fills the list with
/// different objects, not the same one 256 times.
static KEY: AtomicUsize = AtomicUsize::new(16);
/// The watch is ending: a hit clears the watchpoint in the context it returns to.
static STOPPING: AtomicBool = AtomicBool::new(false);
/// Only one watch at a time.
static BUSY: Mutex<()> = Mutex::new(());
/// Data watch kind: 1 = writes only, 3 = reads and writes.
static RW: AtomicU8 = AtomicU8::new(1);
/// Set on unload: no new watches, a running one ends now.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// A watch ended with DR1 maybe still armed on some thread, so its handler was left in: this DLL
/// must never unload (the next hit would land in freed code).
static STAY_LOADED: AtomicBool = AtomicBool::new(false);
/// The shared debug-register lock, made on first use (0: not yet).
static LOCK: AtomicUsize = AtomicUsize::new(0);

/// End the running watch and refuse new ones (the DLL is about to unload).
pub fn shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
    CANCEL.store(true, Ordering::SeqCst);
}

/// True when a watch could not prove DR1 is off on every thread: our handler is still in, so
/// unloading would crash the game on the next hit.
pub fn must_stay_loaded() -> bool {
    STAY_LOADED.load(Ordering::SeqCst)
}

/// What a watch found.
pub struct Watched {
    pub hits: Vec<WriteHit>,
    /// Every hit, also those not kept in the list.
    pub total: u32,
    /// It stopped before the time was up (the 256-hit list was full, or it was cancelled), so
    /// `total` covers only part of the time.
    pub ended_early: bool,
}

#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

/// Held while one thread's debug registers change; released when dropped.
struct DrLock(HANDLE);

impl DrLock {
    /// None (and a log line) when the lock cannot be made or stays busy past `LOCK_WAIT_MS`.
    fn take() -> Option<DrLock> {
        let mut h = LOCK.load(Ordering::Acquire) as HANDLE;
        if h.is_null() {
            let name: Vec<u16> = format!("Local\\aurora-hwbp-{}\0", std::process::id()).encode_utf16().collect();
            // SAFETY: a named mutex, not taken on creation; the name ends in NUL.
            let new = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if new.is_null() {
                log("debug-register lock: CreateMutexW failed, thread skipped");
                return None;
            }
            match LOCK.compare_exchange(0, new as usize, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => h = new,
                Err(old) => {
                    // SAFETY: our own, unused handle.
                    unsafe { CloseHandle(new) };
                    h = old as HANDLE;
                }
            }
        }
        // SAFETY: a mutex handle we keep for the life of the DLL.
        match unsafe { WaitForSingleObject(h, LOCK_WAIT_MS) } {
            // Abandoned: its last holder ended without letting go; it is ours now.
            WAIT_OBJECT_0 | WAIT_ABANDONED => Some(DrLock(h)),
            _ => {
                log(&format!("debug-register lock busy for {LOCK_WAIT_MS} ms, thread skipped"));
                None
            }
        }
    }
}

impl Drop for DrLock {
    fn drop(&mut self) {
        // SAFETY: we hold it (taken in `take`).
        unsafe { ReleaseMutex(self.0) };
    }
}

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: Windows hands us valid pointers for the duration of the call.
    unsafe {
        let rec = (*info).ExceptionRecord;
        let ctx = (*info).ContextRecord;
        if rec.is_null() || ctx.is_null() || (*rec).ExceptionCode != EXCEPTION_SINGLE_STEP {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // B1 in DR6 says our watchpoint fired (not the mod's DR0, not a single step).
        if (*ctx).Dr6 & DR6_B1 == 0 || WATCH_ADDR.load(Ordering::Relaxed) == 0 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Only our bit: the mod's B0 stays for its handler.
        (*ctx).Dr6 &= !DR6_B1;
        // An execution breakpoint fires before the instruction: set RF so it runs once.
        if EXEC.load(Ordering::Relaxed) {
            (*ctx).EFlags |= 0x10000;
        }
        // A thread cleared while it was in here gets its old DR7 back from this context when it
        // returns, and then hits with no handler left.
        if STOPPING.load(Ordering::Relaxed) {
            (*ctx).Dr1 = 0;
            (*ctx).Dr7 &= !DR1_BITS;
        }
        COUNT.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut raw) = RAW.try_lock() {
            let c = &*ctx;
            let regs = [
                c.Rax, c.Rbx, c.Rcx, c.Rdx, c.Rsi, c.Rdi, c.Rbp, c.Rsp, c.R8, c.R9, c.R10, c.R11, c.R12, c.R13, c.R14, c.R15,
            ];
            let key = KEY.load(Ordering::Relaxed);
            let (n, list) = &mut *raw;
            let seen = key < 16 && list[..*n].iter().any(|r| r.rip == c.Rip && r.regs[key] == regs[key]);
            if *n < MAX_RAW && !seen {
                // The word at rsp is the return address while the function has not pushed yet.
                let ret = if memory::readable(c.Rsp, 8) { std::ptr::read_unaligned(c.Rsp as *const u64) } else { 0 };
                list[*n] = Raw { rip: c.Rip, ret, thread: GetCurrentThreadId(), regs };
                *n += 1;
                STORED.store(*n, Ordering::Relaxed);
            }
        }
        // The mod's DR0 fired on the same instruction too: let its handler see it as well.
        if (*ctx).Dr6 & 1 != 0 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        EXCEPTION_CONTINUE_EXECUTION
    }
}

/// Every thread of this process except the caller.
pub(crate) fn other_threads() -> Vec<u32> {
    let me = unsafe { GetCurrentThreadId() };
    let pid = std::process::id();
    let mut out = Vec::new();
    // SAFETY: Toolhelp thread snapshot; entry zeroed and sized.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: THREADENTRY32 = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut e) != 0 {
            loop {
                if e.th32OwnerProcessID == pid && e.th32ThreadID != me {
                    out.push(e.th32ThreadID);
                }
                if Thread32Next(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// Suspend one thread, hand its debug registers to `f` (true: write them back), resume it, all
/// under the shared lock. `f` runs while the thread is stopped, so it must not allocate. False when
/// any step failed.
fn with_debug_regs(tid: u32, f: impl FnOnce(&mut CONTEXT) -> bool) -> bool {
    let Some(_lock) = DrLock::take() else { return false };
    // SAFETY: standard suspend / get context / set context / resume dance.
    unsafe {
        let h: HANDLE = OpenThread(THREAD_GET_CONTEXT | THREAD_SET_CONTEXT | THREAD_SUSPEND_RESUME, 0, tid);
        if h.is_null() {
            return false;
        }
        if SuspendThread(h) == u32::MAX {
            CloseHandle(h);
            return false;
        }
        let mut ctx = AlignedContext(std::mem::zeroed());
        ctx.0.ContextFlags = CONTEXT_DEBUG_REGISTERS_AMD64;
        let mut ok = GetThreadContext(h, &mut ctx.0) != 0;
        if ok && f(&mut ctx.0) {
            ok = SetThreadContext(h, &ctx.0) != 0;
        }
        ResumeThread(h);
        CloseHandle(h);
        ok
    }
}

/// Set (or clear, with `addr == 0`) DR1 on one thread. The thread is suspended meanwhile. `len ==
/// 0` means an execution breakpoint (fires when the code at `addr` runs). DR0, DR2, DR3, their DR7
/// bits and their DR6 bits are left as they are.
fn set_on_thread(tid: u32, addr: u64, len: u32) -> bool {
    let rw = u64::from(RW.load(Ordering::Relaxed));
    with_debug_regs(tid, |c| {
        if addr == 0 {
            c.Dr1 = 0;
            c.Dr7 &= !DR1_BITS;
        } else {
            let (rw_bits, len_bits): (u64, u64) = match len {
                0 => (0b00, 0b00), // Execute, 1 byte
                1 => (rw, 0b00),
                2 => (rw, 0b01),
                8 => (rw, 0b10),
                _ => (rw, 0b11), // 4 bytes
            };
            c.Dr1 = addr;
            // L1 enable, RW1 (write, access or execute), LEN1 as above.
            c.Dr7 = (c.Dr7 & !DR1_BITS) | (1 << 2) | (rw_bits << 20) | (len_bits << 22);
        }
        c.Dr6 &= !DR6_B1;
        true
    })
}

/// Is DR1 still armed on this thread (read back from DR7)? None when it could not be read.
fn dr1_armed(tid: u32) -> Option<bool> {
    let mut armed = false;
    let ok = with_debug_regs(tid, |c| {
        armed = c.Dr7 & DR1_ENABLE != 0;
        false
    });
    ok.then_some(armed)
}

/// Watch `addr` for `millis`: writes (`rw` 1), reads and writes (`rw` 3), or with `len == 0` who
/// runs the code at `addr`. Hits grouped by rip plus the register `key_reg` (so one instruction
/// that touches many objects lists each object once), most frequent first, at most `max`; also the
/// total number of hits. At most `MAX_RAW` (256) hits are kept, and the watch ends early once that
/// many are.
pub fn watch(addr: u64, len: u32, millis: u32, max: u32, key_reg: Option<usize>, rw: u8) -> Result<Watched, String> {
    CANCEL.store(true, Ordering::SeqCst);
    let guard = loop {
        if let Ok(g) = BUSY.try_lock() {
            break g;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    CANCEL.store(false, Ordering::SeqCst);
    let _guard = guard;
    if SHUTDOWN.load(Ordering::SeqCst) {
        return Err("shutting down".into());
    }
    if !memory::readable(addr, len.max(1) as usize) {
        return Err(format!("{addr:#x} is not readable"));
    }
    if len != 0 && !addr.is_multiple_of(u64::from(len)) {
        return Err("address must be aligned to the length".into());
    }
    COUNT.store(0, Ordering::SeqCst);
    STORED.store(0, Ordering::SeqCst);
    if let Ok(mut raw) = RAW.lock() {
        raw.0 = 0;
    }
    KEY.store(key_reg.unwrap_or(16).min(16), Ordering::SeqCst);
    STOPPING.store(false, Ordering::SeqCst);
    RW.store(rw, Ordering::SeqCst);
    EXEC.store(len == 0, Ordering::SeqCst);
    WATCH_ADDR.store(addr, Ordering::SeqCst);
    // SAFETY: handler is a valid extern "system" fn; first = 1 puts it ahead of the game's.
    let veh = unsafe { AddVectoredExceptionHandler(1, Some(handler)) };
    if veh.is_null() {
        WATCH_ADDR.store(0, Ordering::SeqCst);
        return Err("AddVectoredExceptionHandler failed".into());
    }
    let threads = other_threads();
    let mut armed = 0;
    for &t in &threads {
        if set_on_thread(t, addr, len) {
            armed += 1;
        }
    }
    log(&format!("watchpoint (DR1) on {addr:#x} ({len} bytes) armed on {armed}/{} threads", threads.len()));
    let deadline = std::time::Instant::now() + Duration::from_millis(u64::from(millis));
    let mut ended_early = false;
    while std::time::Instant::now() < deadline {
        let n = STORED.load(Ordering::Relaxed);
        if n >= MAX_RAW || (n > 0 && max == 1) || CANCEL.load(Ordering::Relaxed) || SHUTDOWN.load(Ordering::Relaxed) {
            ended_early = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Off on every thread before the handler stops answering, twice: a thread that was inside the
    // handler during the first pass puts its old DR7 back on return (the handler clears it too
    // while stopping).
    STOPPING.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        for &t in &threads {
            set_on_thread(t, 0, 0);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // Then read DR7 back on every thread still alive and clear again where DR1 is on (or could not
    // be read). Threads born during the watch never had DR1.
    let mut left = threads.clone();
    for round in 0..=CLEAR_RETRIES {
        let alive = other_threads();
        left.retain(|&t| alive.contains(&t) && dr1_armed(t) != Some(false));
        if left.is_empty() || round == CLEAR_RETRIES {
            break;
        }
        for &t in &left {
            set_on_thread(t, 0, 0);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if left.is_empty() {
        WATCH_ADDR.store(0, Ordering::SeqCst);
        // SAFETY: the handle AddVectoredExceptionHandler gave us.
        unsafe { RemoveVectoredExceptionHandler(veh) };
    } else {
        // Keep the handler in (it clears DR1 on the next hit, STOPPING stays on) and never unload:
        // removing it would let the next hit crash the game.
        STAY_LOADED.store(true, Ordering::SeqCst);
        log(&format!(
            "watchpoint: DR1 may still be on for {} threads ({left:?}): handler kept, this DLL stays loaded until the game closes",
            left.len()
        ));
    }
    let total = COUNT.load(Ordering::SeqCst);
    let raw: Vec<Raw> = RAW.lock().map(|r| r.1[..r.0].to_vec()).unwrap_or_default();
    log(&format!("watchpoint off, {total} hits, {} kept, ended early: {ended_early}", raw.len()));

    // Group by rip (and the key register), keep the first registers seen, count the rest.
    let mut hits: Vec<WriteHit> = Vec::new();
    let modules = memory::modules();
    for r in raw {
        let same = |h: &WriteHit| h.rip == r.rip && key_reg.is_none_or(|k| h.regs[k] == r.regs[k]);
        if let Some(h) = hits.iter_mut().find(|h| same(h)) {
            h.count += 1;
            continue;
        }
        if hits.len() >= max as usize {
            continue;
        }
        let (module, offset) = modules
            .iter()
            .find(|m| r.rip >= m.base && r.rip < m.base + m.size)
            .map(|m| (m.name.clone(), r.rip - m.base))
            .unwrap_or_default();
        let code = memory::read(r.rip.saturating_sub(16), 48).unwrap_or_default();
        hits.push(WriteHit { rip: r.rip, module, offset, thread: r.thread, code, regs: r.regs, count: 1, ret: r.ret });
    }
    hits.sort_by_key(|h| std::cmp::Reverse(h.count));
    Ok(Watched { hits, total: total as u32, ended_early })
}
