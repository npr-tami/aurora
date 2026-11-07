//! Looking at our own process: modules, readable ranges, pattern scans.

use std::ffi::c_void;

use aurora_core::pattern::Pattern;
use aurora_core::proto::ModuleInfo;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::Debug::{ReadProcessMemory, WriteProcessMemory};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, VirtualQuery,
};

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

/// The module called `name` (case-insensitive), or the main exe when `name` is empty. The main exe
/// never moves, so it is looked up once: fly and the ESP ask hundreds of times a second, and each
/// module list is a Toolhelp snapshot of the whole process.
pub fn find_module(name: &str) -> Option<ModuleInfo> {
    static EXE: std::sync::OnceLock<ModuleInfo> = std::sync::OnceLock::new();
    if name.is_empty() {
        if let Some(exe) = EXE.get() {
            return Some(exe.clone());
        }
        let exe = modules().into_iter().next()?;
        return Some(EXE.get_or_init(|| exe).clone());
    }
    modules().into_iter().find(|m| m.name.eq_ignore_ascii_case(name))
}

/// Is `[addr, addr+len)` committed and readable right now?
pub fn readable(addr: u64, len: usize) -> bool {
    let mut cur = addr;
    let end = addr.saturating_add(len as u64);
    while cur < end {
        // SAFETY: VirtualQuery only inspects, never touches, the address.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe {
            VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>())
        };
        if got == 0 || info.State != MEM_COMMIT || info.Protect & (PAGE_NOACCESS | PAGE_GUARD) != 0 {
            return false;
        }
        cur = info.BaseAddress as u64 + info.RegionSize as u64;
    }
    true
}

/// Copy `len` bytes out of our own address space, if readable.
pub fn read(addr: u64, len: usize) -> Result<Vec<u8>, String> {
    if len > 16 * 1024 * 1024 {
        return Err("read too large (max 16 MB)".into());
    }
    if !readable(addr, len) {
        return Err(format!("{addr:#x}+{len} is not readable"));
    }
    let mut buf = vec![0u8; len];
    // Through the kernel, not a plain copy: memory freed between the check above and the copy (a
    // world change, joining a friend) fails the call instead of crashing the game. The plain copy
    // crashed the game, most likely this way.
    let mut got = 0usize;
    // SAFETY: the buffer is ours and sized; the kernel checks the source range.
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const c_void, buf.as_mut_ptr() as *mut c_void, len, &mut got) };
    if ok == 0 || got != len {
        return Err(format!("{addr:#x}+{len} went away while reading"));
    }
    Ok(buf)
}

/// Write bytes into our own address space. Refuses unreadable ranges; the page must already be
/// writable (code pages are handled in a later step with VirtualProtect).
pub fn write(addr: u64, bytes: &[u8]) -> Result<(), String> {
    if !readable(addr, bytes.len()) {
        return Err(format!("{addr:#x}+{} is not mapped", bytes.len()));
    }
    // SAFETY: range is committed; a read-only page would fault, so check protection first.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    unsafe {
        VirtualQuery(addr as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>())
    };
    const WRITABLE: u32 = 0x04 | 0x08 | 0x40 | 0x80; // RW, WRITECOPY, EXECUTE_RW, EXECUTE_WRITECOPY
    if info.Protect & WRITABLE == 0 {
        return Err(format!("{addr:#x} is not writable (protect {:#x})", info.Protect));
    }
    // Through the kernel too (see `read`).
    let mut put = 0usize;
    // SAFETY: the source is our slice; the kernel checks the target range.
    let ok = unsafe { WriteProcessMemory(GetCurrentProcess(), addr as *const c_void, bytes.as_ptr() as *const c_void, bytes.len(), &mut put) };
    if ok == 0 || put != bytes.len() {
        return Err(format!("{addr:#x}+{} went away while writing", bytes.len()));
    }
    Ok(())
}

/// One code swap at a time: two at once on the same page would each save the other's "writable" as
/// the protection to put back, or put back read-only under the other's store.
static SWAP: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Swap one aligned 8-byte word of game code in a single store, so no thread can run a half-written
/// instruction. Returns the word that was there.
pub fn swap_code_u64(addr: u64, new: u64) -> Result<u64, String> {
    if !addr.is_multiple_of(8) || !readable(addr, 8) {
        return Err(format!("{addr:#x}: not an aligned readable code word"));
    }
    let _one = SWAP.lock().unwrap_or_else(|p| p.into_inner());
    swap_word(addr, new).ok_or_else(|| format!("{addr:#x}: could not make the code writable"))
}

/// The swap itself: no lock, no allocation (`swap_code_u64_paused` runs it while the game's threads
/// are stopped). None when the code could not be made writable.
fn swap_word(addr: u64, new: u64) -> Option<u64> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
    use windows_sys::Win32::System::Memory::{PAGE_EXECUTE_READWRITE, VirtualProtect};
    let mut prev = 0u32;
    // SAFETY: protection change on a committed code word the caller checked.
    if unsafe { VirtualProtect(addr as *const c_void, 8, PAGE_EXECUTE_READWRITE, &mut prev) } == 0 {
        return None;
    }
    // SAFETY: aligned, committed and now writable; one atomic store.
    let old = unsafe { (*(addr as *const AtomicU64)).swap(new, Ordering::SeqCst) };
    let mut ignored = 0u32;
    // SAFETY: put the protection back and drop stale decoded code.
    unsafe {
        VirtualProtect(addr as *const c_void, 8, prev, &mut ignored);
        FlushInstructionCache(GetCurrentProcess(), addr as *const c_void, 8);
    }
    Some(old)
}

/// `swap_code_u64`, done while every other thread is stopped and none of them is about to run code
/// inside `avoid`. Needed when the 5-byte jump covers more than one instruction: a thread stopped
/// between them would, once the jump is in, run the jump's last bytes as code. Tries again for
/// about a second while a thread is in the way, then leaves the code alone.
pub fn swap_code_u64_paused(addr: u64, new: u64, avoid: std::ops::Range<u64>) -> Result<u64, String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Diagnostics::Debug::{CONTEXT, CONTEXT_CONTROL_AMD64, GetThreadContext};
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, SuspendThread, THREAD_GET_CONTEXT, THREAD_SUSPEND_RESUME};
    #[repr(C, align(16))]
    struct Ctx(CONTEXT);
    if !addr.is_multiple_of(8) || !readable(addr, 8) {
        return Err(format!("{addr:#x}: not an aligned readable code word"));
    }
    let _one = SWAP.lock().unwrap_or_else(|p| p.into_inner());
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
        let swapped = if clear { swap_word(addr, new) } else { None };
        for &h in &stopped {
            // SAFETY: stopped by us above.
            unsafe { ResumeThread(h) };
        }
        for h in threads {
            // SAFETY: opened above.
            unsafe { CloseHandle(h) };
        }
        if clear {
            return swapped.ok_or_else(|| format!("{addr:#x}: could not make the code writable"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Err(format!("{addr:#x}: a game thread kept sitting in the code to change; left alone"))
}

/// Executable memory within a jump (2 GB) below `near`, for a few bytes of our own code. Never
/// freed: a game thread may still be in it after the jump to it is removed.
pub fn alloc_code_near(near: u64, len: usize) -> Option<u64> {
    use windows_sys::Win32::System::Memory::{MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
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

/// Which build of the game this is: the exe's link time (PE header) and size. Saved offsets belong
/// to one build; a game update gives a new id and they are found again.
fn build_id(exe: &ModuleInfo) -> Option<String> {
    let lfanew = u32::from_le_bytes(read(exe.base + 0x3c, 4).ok()?.try_into().ok()?);
    let stamp = u32::from_le_bytes(read(exe.base + u64::from(lfanew) + 8, 4).ok()?.try_into().ok()?);
    Some(format!("{stamp:08x}-{:x}", exe.size))
}

/// `Bin\Cache\Offsets.bin`: where each code search found its code in this game build, so the next
/// inject skips the seconds-long scans. Each entry is checked against its pattern before use, so a
/// wrong one costs one scan.
#[derive(Default)]
struct SavedOffsets {
    build: String,
    /// Search name -> (its pattern, exe offset).
    found: std::collections::BTreeMap<String, (String, u64)>,
}

/// The file's first bytes and its layout's number. A new layout gets a new number: an older file is
/// then just ignored and the searches run once.
const OFFSETS_MAGIC: &[u8; 8] = b"AURORAOF";
const OFFSETS_VERSION: u32 = 1;
/// More entries than this is a damaged file, not a real one (there are about 30 searches).
const OFFSETS_MAX: u32 = 10_000;

/// FNV-1a: a cheap check that the file is whole, not a guard against anyone.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3))
}

impl SavedOffsets {
    /// The file: magic, version (u32), build id, entry count (u32), then each entry's name, pattern
    /// (text with a u16 length) and exe offset (u64); last a check sum of all before it.
    /// Little-endian throughout.
    fn encode(&self) -> Vec<u8> {
        fn text(out: &mut Vec<u8>, s: &str) {
            let b = &s.as_bytes()[..s.len().min(u16::MAX as usize)];
            out.extend_from_slice(&(b.len() as u16).to_le_bytes());
            out.extend_from_slice(b);
        }
        let mut out = Vec::with_capacity(64 + self.found.len() * 96);
        out.extend_from_slice(OFFSETS_MAGIC);
        out.extend_from_slice(&OFFSETS_VERSION.to_le_bytes());
        text(&mut out, &self.build);
        out.extend_from_slice(&(self.found.len() as u32).to_le_bytes());
        for (name, (pattern, off)) in &self.found {
            text(&mut out, name);
            text(&mut out, pattern);
            out.extend_from_slice(&off.to_le_bytes());
        }
        let sum = fnv1a(&out);
        out.extend_from_slice(&sum.to_le_bytes());
        out
    }

    /// The file's entries, only when it is whole, in this layout, and for `build`: anything else
    /// (another game build, an old layout, a cut-off or changed file) gives None, and the searches
    /// run again.
    fn decode(bytes: &[u8], build: &str) -> Option<std::collections::BTreeMap<String, (String, u64)>> {
        struct Reader<'a>(&'a [u8]);
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                (self.0.len() >= n).then(|| {
                    let (a, b) = self.0.split_at(n);
                    self.0 = b;
                    a
                })
            }
            fn u16(&mut self) -> Option<u16> {
                self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
            }
            fn u32(&mut self) -> Option<u32> {
                self.take(4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
            }
            fn u64(&mut self) -> Option<u64> {
                self.take(8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes)
            }
            fn text(&mut self) -> Option<String> {
                let n = self.u16()?;
                String::from_utf8(self.take(usize::from(n))?.to_vec()).ok()
            }
        }
        if build.is_empty() || bytes.len() < 8 {
            return None;
        }
        let (body, sum) = bytes.split_at(bytes.len() - 8);
        if fnv1a(body) != u64::from_le_bytes(sum.try_into().ok()?) {
            return None;
        }
        let mut r = Reader(body);
        if r.take(8)? != OFFSETS_MAGIC || r.u32()? != OFFSETS_VERSION || r.text()? != build {
            return None;
        }
        let n = r.u32()?;
        if n > OFFSETS_MAX {
            return None;
        }
        let mut found = std::collections::BTreeMap::new();
        for _ in 0..n {
            let (name, pattern, off) = (r.text()?, r.text()?, r.u64()?);
            found.insert(name, (pattern, off));
        }
        // Bytes left over: not a file this wrote.
        r.0.is_empty().then_some(found)
    }

    /// Into `Bin\Cache` (made here when missing, but only inside an `Aurora` that is there:
    /// `write_file` refuses otherwise), through `Bin\Temp` as every save.
    fn save(&self) {
        // Never in unit tests.
        if cfg!(test) {
            return;
        }
        let path = aurora_core::paths::offsets_cache();
        if aurora_core::paths::root().is_dir()
            && let Some(dir) = path.parent()
        {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = aurora_core::paths::write_file(&path, self.encode()) {
            crate::log(&format!("offsets: {} not saved: {e}", path.display()));
        }
    }
}

static OFFSETS: std::sync::Mutex<Option<SavedOffsets>> = std::sync::Mutex::new(None);

/// Find the one place `pattern` matches in the exe: from `Offsets.bin` when it still matches there,
/// else by scanning (and then saved). `name` is the search's name in that file and in errors, like
/// "unseen sight".
pub fn find_code(exe: &ModuleInfo, name: &str, pattern: &str) -> Result<u64, String> {
    let pat = Pattern::parse(pattern).map_err(|e| format!("{name}: {e}"))?;
    let build = build_id(exe).unwrap_or_default();
    // Without a build id the saved file can't be matched to this game build: an earlier session
    // that also had none would look like the same build. Scan, and keep nothing.
    if build.is_empty() {
        let hits = scan_module(exe, &pat, 2);
        if hits.len() != 1 {
            return Err(format!("{name}: pattern found {} times, want 1 (game updated?)", hits.len()));
        }
        crate::log(&format!("{name}: found at exe+{:#x} (build id unreadable, not saved)", hits[0] - exe.base));
        return Ok(hits[0]);
    }
    let mut slot = OFFSETS.lock().unwrap_or_else(|p| p.into_inner());
    let saved = slot.get_or_insert_with(|| {
        let found = match std::fs::read(aurora_core::paths::offsets_cache()) {
            Ok(bytes) => SavedOffsets::decode(&bytes, &build).unwrap_or_else(|| {
                crate::log("offsets: Offsets.bin is for another game build, or damaged: searching again");
                Default::default()
            }),
            Err(_) => Default::default(),
        };
        SavedOffsets { build: build.clone(), found }
    });
    if let Some((p, off)) = saved.found.get(name)
        && p == pattern
        && read(exe.base + off, pat.len()).is_ok_and(|b| pat.matches_at(&b))
    {
        return Ok(exe.base + off);
    }
    let hits = scan_module(exe, &pat, 2);
    if hits.len() != 1 {
        return Err(format!("{name}: pattern found {} times, want 1 (game updated?)", hits.len()));
    }
    saved.found.insert(name.to_string(), (pattern.to_string(), hits[0] - exe.base));
    saved.save();
    crate::log(&format!("{name}: found at exe+{:#x} (saved)", hits[0] - exe.base));
    Ok(hits[0])
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
        let got = unsafe {
            VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>())
        };
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

#[cfg(test)]
mod tests {
    use super::SavedOffsets;

    fn sample() -> SavedOffsets {
        let mut s = SavedOffsets { build: "5f1a2b3c-7a3e000".into(), ..Default::default() };
        s.found.insert("unseen sight".into(), ("48 8B ?? 05".into(), 0x1234_5678));
        s.found.insert("global LEVEL_ROOT".into(), ("4C 8D 05 ?? ?? ?? ??".into(), 0x63A0D20));
        s
    }

    #[test]
    fn offsets_cache_reads_back() {
        let s = sample();
        let bytes = s.encode();
        assert_eq!(&bytes[..8], b"AURORAOF");
        assert_eq!(SavedOffsets::decode(&bytes, &s.build), Some(s.found.clone()));
        // An empty cache is a file too.
        let empty = SavedOffsets { build: s.build.clone(), ..Default::default() };
        assert_eq!(SavedOffsets::decode(&empty.encode(), &s.build), Some(Default::default()));
    }

    #[test]
    fn offsets_cache_refuses_other_builds_and_damage() {
        let s = sample();
        let bytes = s.encode();
        // Another game build, or none: not trusted.
        assert_eq!(SavedOffsets::decode(&bytes, "5f1a2b3c-7a3f000"), None);
        assert_eq!(SavedOffsets::decode(&bytes, ""), None);
        // Every cut-off file, and every single changed byte, is refused.
        for n in 0..bytes.len() {
            assert_eq!(SavedOffsets::decode(&bytes[..n], &s.build), None, "cut at {n}");
            let mut b = bytes.clone();
            b[n] ^= 0x41;
            assert_eq!(SavedOffsets::decode(&b, &s.build), None, "byte {n} changed");
        }
        // Bytes added at the end, an old JSON cache, another layout number: refused.
        let mut longer = bytes.clone();
        longer.insert(bytes.len() - 8, 0);
        assert_eq!(SavedOffsets::decode(&longer, &s.build), None);
        assert_eq!(SavedOffsets::decode(br#"{"build":"5f1a2b3c-7a3e000","found":{}}"#, &s.build), None);
        let mut v2 = bytes[..bytes.len() - 8].to_vec();
        v2[8..12].copy_from_slice(&2u32.to_le_bytes());
        let sum = super::fnv1a(&v2);
        v2.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(SavedOffsets::decode(&v2, &s.build), None);
    }
}
