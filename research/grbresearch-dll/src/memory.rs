//! Looking at our own process: modules, readable ranges, pattern scans, and the few code changes
//! the `regs` recorder makes. A copy of the mod's helpers, so this DLL never depends on the mod.

use std::ffi::c_void;

use aurora_core::pattern::Pattern;
use aurora_core::proto::ModuleInfo;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{FlushInstructionCache, ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD, PAGE_NOACCESS, VirtualAlloc,
    VirtualProtect, VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// Every module in this process, main exe first.
pub fn modules() -> Vec<ModuleInfo> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp snapshot of our own process; the entry struct is zeroed and sized.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
        if Module32FirstW(snap, &mut entry) != 0 {
            loop {
                let len = entry.szModule.iter().position(|&c| c == 0).unwrap_or(entry.szModule.len());
                out.push(ModuleInfo {
                    name: String::from_utf16_lossy(&entry.szModule[..len]),
                    base: entry.modBaseAddr as u64,
                    size: entry.modBaseSize as u64,
                });
                if Module32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// The module called `name` (case-insensitive), or the main exe when `name` is empty.
pub fn find_module(name: &str) -> Option<ModuleInfo> {
    let mods = modules();
    if name.is_empty() {
        return mods.into_iter().next();
    }
    mods.into_iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

/// Is `[addr, addr+len)` committed and readable right now?
pub fn readable(addr: u64, len: usize) -> bool {
    let mut cur = addr;
    let end = addr.saturating_add(len as u64);
    while cur < end {
        // SAFETY: VirtualQuery only inspects, never touches, the address.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
            return false;
        }
        cur = info.BaseAddress as u64 + info.RegionSize as u64;
    }
    true
}

/// Copy `len` bytes out of our own address space, if readable. Through the kernel, so memory freed
/// between the check and the copy fails the call instead of crashing the game.
pub fn read(addr: u64, len: usize) -> Result<Vec<u8>, String> {
    if len > 16 * 1024 * 1024 {
        return Err("read too large (max 16 MB)".into());
    }
    if !readable(addr, len) {
        return Err(format!("{addr:#x}+{len} is not readable"));
    }
    let mut buf = vec![0u8; len];
    let mut got = 0usize;
    // SAFETY: the buffer is ours and sized; the kernel checks the source range.
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const c_void, buf.as_mut_ptr() as *mut c_void, len, &mut got) };
    if ok == 0 || got != len {
        return Err(format!("{addr:#x}+{len} went away while reading"));
    }
    Ok(buf)
}

pub fn read_u64(addr: u64) -> Option<u64> {
    read(addr, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// Write bytes into our own memory (only used for our own code caves, which are writable).
pub fn write(addr: u64, bytes: &[u8]) -> Result<(), String> {
    if !readable(addr, bytes.len()) {
        return Err(format!("{addr:#x}+{} is not mapped", bytes.len()));
    }
    let mut put = 0usize;
    // SAFETY: the source is our slice; the kernel checks the target range.
    let ok = unsafe { WriteProcessMemory(GetCurrentProcess(), addr as *const c_void, bytes.as_ptr() as *const c_void, bytes.len(), &mut put) };
    if ok == 0 || put != bytes.len() {
        return Err(format!("{addr:#x}+{} went away while writing", bytes.len()));
    }
    Ok(())
}

/// Put `new` in one aligned 8-byte word of game code, only if it still holds `expect`, in a single
/// store, so no thread can run a half-written instruction and a word someone else changed meanwhile
/// (the mod's own hooks) is left alone. Ok(true) when swapped.
pub fn replace_code_u64(addr: u64, expect: u64, new: u64) -> Result<bool, String> {
    if !addr.is_multiple_of(8) || !readable(addr, 8) {
        return Err(format!("{addr:#x}: not an aligned readable code word"));
    }
    replace_word(addr, expect, new).ok_or_else(|| format!("{addr:#x}: could not make the code writable"))
}

/// The compare-and-swap itself: no allocation (`replace_code_u64_paused` runs it while the game's
/// threads are stopped). None when the code could not be made writable.
fn replace_word(addr: u64, expect: u64, new: u64) -> Option<bool> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let mut prev = 0u32;
    // SAFETY: protection change on a committed code word the caller checked.
    if unsafe { VirtualProtect(addr as *const c_void, 8, PAGE_EXECUTE_READWRITE, &mut prev) } == 0 {
        return None;
    }
    // SAFETY: aligned, committed and now writable; one atomic compare-and-swap.
    let done = unsafe { (*(addr as *const AtomicU64)).compare_exchange(expect, new, Ordering::SeqCst, Ordering::SeqCst) }.is_ok();
    let mut ignored = 0u32;
    // SAFETY: put the protection back and drop stale decoded code.
    unsafe {
        VirtualProtect(addr as *const c_void, 8, prev, &mut ignored);
        FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, 8);
    }
    Some(done)
}

/// `replace_code_u64`, done while every other thread is stopped and none of them is about to run
/// code inside `avoid`. A 5-byte jump over more than one instruction needs it: a thread stopped
/// between them would, once the jump is in, run the jump's last bytes as code. Tries again for
/// about a second while a thread is in the way.
pub fn replace_code_u64_paused(addr: u64, expect: u64, new: u64, avoid: std::ops::Range<u64>) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, CONTEXT_CONTROL_AMD64, GetThreadContext};
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME};
    #[repr(C, align(16))]
    struct Ctx(CONTEXT);
    if !addr.is_multiple_of(8) || !readable(addr, 8) {
        return Err(format!("{addr:#x}: not an aligned readable code word"));
    }
    for _ in 0..50 {
        // Everything is opened and allocated first: a stopped thread may hold the heap's lock, so
        // nothing between the first stop and the last resume may allocate (or log).
        // SAFETY: plain thread handles of our own process.
        let threads: Vec<HANDLE> = crate::hwbp::other_threads()
            .into_iter()
            .map(|tid| unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, 0, tid) })
            .filter(|h| !h.is_null())
            .collect();
        let mut stopped: Vec<HANDLE> = Vec::with_capacity(threads.len());
        // SAFETY: a plain C struct; zero is a valid start.
        let mut ctx = Ctx(unsafe { std::mem::zeroed() });
        let mut clear = true;
        for &h in &threads {
            // SAFETY: our handle; every stopped thread is resumed below.
            if unsafe { SuspendThread(h) } == u32::MAX {
                continue; // Gone already
            }
            stopped.push(h); // Within capacity: no allocation
            ctx.0.ContextFlags = CONTEXT_CONTROL_AMD64;
            // Reading the context also waits until the stop has really happened.
            // SAFETY: stopped thread, aligned context.
            if unsafe { GetThreadContext(h, &mut ctx.0) } == 0 || avoid.contains(&ctx.0.Rip) {
                clear = false;
                break;
            }
        }
        let done = if clear { replace_word(addr, expect, new) } else { None };
        for &h in &stopped {
            // SAFETY: stopped by us above.
            unsafe { ResumeThread(h) };
        }
        for h in threads {
            // SAFETY: opened above.
            unsafe { CloseHandle(h) };
        }
        if clear {
            return done.ok_or_else(|| format!("{addr:#x}: could not make the code writable"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(format!("{addr:#x}: a game thread kept sitting in the code to change; left alone"))
}

/// Executable memory within a jump (2 GB) below `near`, for a few bytes of our own code. Never
/// freed: a game thread may still be in it after the jump to it is removed, and it stays valid
/// after this DLL unloads.
pub fn alloc_code_near(near: u64, len: usize) -> Option<u64> {
    let mut at = (near & !0xffff).checked_sub(0x10000)?;
    let floor = near.saturating_sub(0x7000_0000);
    while at > floor {
        // SAFETY: asking for fresh pages at a hint; fails if taken.
        let p = unsafe { VirtualAlloc(at as *const c_void, len, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) };
        if !p.is_null() {
            return Some(p as u64);
        }
        at -= 0x10000;
    }
    None
}

/// Scan a module for a pattern, region by region (Denuvo leaves holes, so we never read the module
/// as one block). Returns absolute addresses.
pub fn scan_module(module: &ModuleInfo, pattern: &Pattern, limit: usize) -> Vec<u64> {
    let mut hits = Vec::new();
    let end = module.base + module.size;
    let mut cur = module.base;
    while cur < end {
        // SAFETY: inspection only.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            break;
        }
        let region_start = info.BaseAddress as u64;
        let region_end = region_start.saturating_add(info.RegionSize as u64).min(end);
        let ok = info.State == MEM_COMMIT && info.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0;
        if ok && region_end > cur {
            let len = (region_end - cur) as usize;
            // SAFETY: committed and readable per VirtualQuery; we copy nothing, only borrow.
            let slice = unsafe { std::slice::from_raw_parts(cur as *const u8, len) };
            for off in pattern.find_all(slice, limit.saturating_sub(hits.len())) {
                hits.push(cur + off as u64);
            }
            if limit != 0 && hits.len() >= limit {
                break;
            }
        }
        cur = region_end;
    }
    hits
}
