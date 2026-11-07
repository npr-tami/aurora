//! Hardware watchpoint: "who writes this address?". Sets DR0 on every game thread, catches the
//! single-step trap in a vectored exception handler, records rip and the registers, lets the game
//! carry on. Cleared after the time is up. Each thread's suspend..resume runs under one named lock
//! (`Local\aurora-hwbp-<pid>`) that the research DLL's DR1 watch takes too, so neither writes back
//! a stale copy of the other's debug registers.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use aurora_core::proto::WriteHit;
use windows_sys::Win32::Foundation::{
    CloseHandle, EXCEPTION_SINGLE_STEP, HANDLE, INVALID_HANDLE_VALUE, WAIT_ABANDONED, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, CONTEXT, CONTEXT_DEBUG_REGISTERS_AMD64, EXCEPTION_CONTINUE_EXECUTION,
    EXCEPTION_POINTERS, GetThreadContext, RemoveVectoredExceptionHandler, SetThreadContext,
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
const MAX_RAW: usize = 256;
/// DR6 bit B0: DR0 fired. The research DLL's B1 is left for its own handler.
const DR6_B0: u64 = 0x1;
/// How long to wait for the shared debug-register lock before skipping a thread. It is only ever
/// held for one thread's suspend..resume.
const LOCK_WAIT_MS: u32 = 250;
/// The shared debug-register lock, made on first use (0: not yet).
static LOCK: AtomicUsize = AtomicUsize::new(0);

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

/// One raw catch, filled inside the exception handler (no allocation there).
#[derive(Clone, Copy, Default)]
struct Raw {
    rip: u64,
    thread: u32,
    regs: [u64; 16],
    ret: u64,
}

static WATCH_ADDR: AtomicU64 = AtomicU64::new(0);
static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// One more at every cancel (the freeze guard, a new watch taking over), for callers that watch
/// again in a loop: they stop once it moved.
static CANCELS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn cancels() -> u32 {
    CANCELS.load(Ordering::SeqCst)
}
static EXEC: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// Catches kept so far (mirrors the count inside `RAW`, for the waiting loop).
static STORED: AtomicUsize = AtomicUsize::new(0);
static RAW: Mutex<(usize, [Raw; MAX_RAW])> =
    Mutex::new((0, [Raw { rip: 0, thread: 0, regs: [0; 16], ret: 0 }; MAX_RAW]));
/// Keep one catch per value of this register (16: keep all), so a busy function fills the list with
/// different objects, not the same one 256 times.
static KEY: AtomicUsize = AtomicUsize::new(16);
/// The watch is ending: a hit clears the watchpoint in the context it returns to.
static STOPPING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Only one watch at a time.
static BUSY: Mutex<()> = Mutex::new(());
/// A return address that ends the watch soon after it is first seen (0: none), and whether it was
/// (`find_writers_until`).
static FINISH_RET: AtomicU64 = AtomicU64::new(0);
static FINISH_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Set on Bye: no new watches, running ones end now.
static SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The address being watched right now, 0 when none.
pub fn watching() -> u64 {
    WATCH_ADDR.load(Ordering::Relaxed)
}

/// The last `find_writers_until` saw its awaited reader.
pub fn finish_seen() -> bool {
    FINISH_SEEN.load(Ordering::SeqCst)
}

/// End the running watch early.
pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
    CANCELS.fetch_add(1, Ordering::SeqCst);
}

/// End the running watch and refuse new ones (the DLL is about to unload).
pub fn shutdown() {
    SHUTDOWN.store(true, Ordering::SeqCst);
    CANCEL.store(true, Ordering::SeqCst);
    CANCELS.fetch_add(1, Ordering::SeqCst);
}

#[repr(C, align(16))]
struct AlignedContext(CONTEXT);

unsafe extern "system" fn handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: Windows hands us valid pointers for the duration of the call.
    unsafe {
        let rec = (*info).ExceptionRecord;
        let ctx = (*info).ContextRecord;
        if rec.is_null() || ctx.is_null() || (*rec).ExceptionCode != EXCEPTION_SINGLE_STEP {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // B0 in DR6 says our watchpoint fired (not someone else's single step).
        if (*ctx).Dr6 & DR6_B0 == 0 || WATCH_ADDR.load(Ordering::Relaxed) == 0 {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        // Only our bit: the research DLL's B1 stays for its handler.
        (*ctx).Dr6 &= !DR6_B0;
        // An execution breakpoint fires before the instruction: set RF so it runs once.
        if EXEC.load(Ordering::Relaxed) {
            (*ctx).EFlags |= 0x10000;
        }
        // A thread cleared while it was in here gets its old DR7 back from this context when it
        // returns, and then hits with no handler left: the game crashed so.
        if STOPPING.load(Ordering::Relaxed) {
            (*ctx).Dr0 = 0;
            (*ctx).Dr7 &= !0x000F_0003;
        }
        COUNT.fetch_add(1, Ordering::Relaxed);
        // Every hit, kept or not (one per object is kept, so the caller's own read may not be).
        let finish = FINISH_RET.load(Ordering::Relaxed);
        if finish != 0
            && !FINISH_SEEN.load(Ordering::Relaxed)
            && memory::readable((*ctx).Rsp, 8)
            && std::ptr::read_unaligned((*ctx).Rsp as *const u64) == finish
        {
            FINISH_SEEN.store(true, Ordering::Relaxed);
        }
        if let Ok(mut raw) = RAW.try_lock() {
            let c = &*ctx;
            let regs = [
                c.Rax, c.Rbx, c.Rcx, c.Rdx, c.Rsi, c.Rdi, c.Rbp, c.Rsp, c.R8, c.R9, c.R10, c.R11, c.R12, c.R13, c.R14,
                c.R15,
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

/// Set (or clear, with `addr == 0`) DR0 on one thread. The thread is suspended meanwhile. `len ==
/// 0` means an execution breakpoint (fires when the code at `addr` runs). Under the shared
/// debug-register lock; false (thread skipped) when it stays busy.
fn set_on_thread(tid: u32, addr: u64, len: u32) -> bool {
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
        if ok {
            if addr == 0 {
                ctx.0.Dr0 = 0;
                ctx.0.Dr7 &= !0x000F_0003;
            } else {
                // RW 01: writes only.
                let (rw_bits, len_bits): (u64, u64) = match len {
                    0 => (0b00, 0b00), // Execute, 1 byte
                    1 => (0b01, 0b00),
                    2 => (0b01, 0b01),
                    8 => (0b01, 0b10),
                    _ => (0b01, 0b11), // 4 bytes
                };
                ctx.0.Dr0 = addr;
                // L0 enable, RW0 (write or execute), LEN0 as above.
                ctx.0.Dr7 = (ctx.0.Dr7 & !0x000F_0003) | 1 | (rw_bits << 16) | (len_bits << 18);
            }
            ctx.0.Dr6 &= !DR6_B0;
            ok = SetThreadContext(h, &ctx.0) != 0;
        }
        ResumeThread(h);
        CloseHandle(h);
        ok
    }
}

/// Watch `addr` for `millis`, then report the writers, most frequent first. With `len == 0` it is
/// an execution catch: who runs the code at `addr`, with registers.
pub fn find_writers(addr: u64, len: u32, millis: u32, max: u32) -> Result<(Vec<WriteHit>, u32), String> {
    find_writers_by(addr, len, millis, max, None)
}

/// Same, grouping hits by rip plus the register `key_reg` (so one instruction that touches many
/// objects lists each object once).
pub fn find_writers_by(addr: u64, len: u32, millis: u32, max: u32, key_reg: Option<usize>) -> Result<(Vec<WriteHit>, u32), String> {
    find_writers_until(addr, len, millis, max, key_reg, None)
}

/// Same, ending `after` ms once a hit returns to `ret` (`finish`: (ret, after)), so a caller
/// waiting for one reader gets what came with it soon after.
pub fn find_writers_until(addr: u64, len: u32, millis: u32, max: u32, key_reg: Option<usize>, finish: Option<(u64, u32)>) -> Result<(Vec<WriteHit>, u32), String> {
    // A long pre-arm may be waiting; a new watch cancels it and takes over.
    CANCEL.store(true, Ordering::SeqCst);
    CANCELS.fetch_add(1, Ordering::SeqCst);
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
    EXEC.store(len == 0, Ordering::SeqCst);
    FINISH_SEEN.store(false, Ordering::SeqCst);
    FINISH_RET.store(finish.map_or(0, |f| f.0), Ordering::SeqCst);
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
    log(&format!("watchpoint on {addr:#x} ({len} bytes) armed on {armed}/{} threads", threads.len()));
    let mut deadline = std::time::Instant::now() + Duration::from_millis(u64::from(millis));
    let mut finishing = false;
    while std::time::Instant::now() < deadline {
        if let Some((_, after)) = finish
            && !finishing
            && FINISH_SEEN.load(Ordering::Relaxed)
        {
            finishing = true;
            deadline = deadline.min(std::time::Instant::now() + Duration::from_millis(u64::from(after)));
            log("watch: the awaited reader was seen, ending soon");
        }
        let n = STORED.load(Ordering::Relaxed);
        if n >= MAX_RAW || (n > 0 && max == 1) || CANCEL.load(Ordering::Relaxed) || SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Off on every thread before the handler stops answering: a thread that hit the watchpoint in
    // between had no handler and the game crashed. Twice: a thread that was inside the handler
    // during the first pass puts its old DR7 back on return (the handler clears it too while
    // stopping).
    STOPPING.store(true, Ordering::SeqCst);
    for _ in 0..2 {
        for &t in &threads {
            set_on_thread(t, 0, 0);
        }
        // Threads born during the watch never had DR0; nothing to clear there. A hit raised just
        // before its thread was cleared may still be on its way to the handler.
        std::thread::sleep(Duration::from_millis(50));
    }
    WATCH_ADDR.store(0, Ordering::SeqCst);
    unsafe { RemoveVectoredExceptionHandler(veh) };
    FINISH_RET.store(0, Ordering::SeqCst);
    let total = COUNT.load(Ordering::SeqCst);
    let raw: Vec<Raw> = RAW.lock().map(|r| r.1[..r.0].to_vec()).unwrap_or_default();
    log(&format!("watchpoint off, {total} hits, {} kept", raw.len()));

    // Group by rip, keep the first registers seen, count the rest.
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
    Ok((hits, total as u32))
}
