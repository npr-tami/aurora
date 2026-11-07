//! The target process from the outside, read-only: find it, open it, list modules and memory
//! regions, and read memory with a typed result. The handle only has PROCESS_QUERY_INFORMATION |
//! PROCESS_VM_READ. Nothing here writes, changes protection or makes threads in the target.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
    Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_FREE, MEM_IMAGE, MEM_MAPPED, MEM_PRIVATE, MEM_RESERVE, MEMORY_BASIC_INFORMATION,
    PAGE_EXECUTE, PAGE_EXECUTE_READ, PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD,
    PAGE_NOCACHE, PAGE_READONLY, PAGE_READWRITE, PAGE_WRITECOMBINE, PAGE_WRITECOPY,
    VirtualQueryEx,
};
use windows_sys::Win32::System::ProcessStatus::{PSAPI_WORKING_SET_EX_INFORMATION, QueryWorkingSetEx};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    QueryFullProcessImageNameW,
};

/// The only rights we ever ask for. No write, no operation, no thread rights.
pub const ACCESS: u32 = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;
/// Below this is the null page area: never a real object.
pub const USER_MIN: u64 = 0x10000;
/// Highest user-mode address. Anything above is kernel or non-canonical.
pub const USER_MAX: u64 = 0x7FFF_FFFF_FFFF;
pub const PAGE: u64 = 0x1000;
const STILL_ACTIVE: u32 = 259;

/// Why a read did not give every byte. A failed read never turns into zeros.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    /// Null page or not a user-mode (canonical) address.
    InvalidAddress,
    /// Nothing allocated there.
    Free,
    /// Address space reserved but not committed.
    Reserved,
    /// PAGE_NOACCESS, PAGE_GUARD, or the read was refused.
    NoAccess,
    /// Only the first n bytes could be read.
    PartialRead(usize),
    /// The process has ended.
    ProcessGone,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::InvalidAddress => write!(f, "InvalidAddress (null page or not a user address)"),
            ReadError::Free => write!(f, "Free (nothing allocated there)"),
            ReadError::Reserved => write!(f, "Reserved (not committed)"),
            ReadError::NoAccess => write!(f, "NoAccess (no-access or guard page)"),
            ReadError::PartialRead(n) => write!(f, "PartialRead({n} bytes)"),
            ReadError::ProcessGone => write!(f, "ProcessGone (the process has ended)"),
        }
    }
}

/// One VirtualQueryEx answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub size: u64,
    pub alloc_base: u64,
    pub state: u32,
    pub protect: u32,
    pub kind: u32,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.size)
    }

    pub fn committed(&self) -> bool {
        self.state == MEM_COMMIT
    }

    pub fn guard(&self) -> bool {
        self.protect & PAGE_GUARD != 0
    }

    /// GPU-style memory (no-cache / write-combine): reading it is slow and pointless.
    pub fn gpu(&self) -> bool {
        self.protect & (PAGE_NOCACHE | PAGE_WRITECOMBINE) != 0
    }

    /// Committed, not guard, not no-access.
    pub fn readable(&self) -> bool {
        self.committed() && !self.guard() && prot_rwx(self.protect).0
    }

    pub fn writable(&self) -> bool {
        self.committed() && prot_rwx(self.protect).1
    }

    pub fn executable(&self) -> bool {
        self.committed() && prot_rwx(self.protect).2
    }

    pub fn state_str(&self) -> &'static str {
        match self.state {
            MEM_COMMIT => "commit",
            MEM_RESERVE => "reserve",
            MEM_FREE => "free",
            _ => "?",
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self.kind {
            MEM_PRIVATE => "private",
            MEM_MAPPED => "mapped",
            MEM_IMAGE => "image",
            _ => "-",
        }
    }

    /// Like "rw-", plus "+guard" / "+nocache".
    pub fn prot_str(&self) -> String {
        if !self.committed() {
            return "---".into();
        }
        let (r, w, x) = prot_rwx(self.protect);
        let mut s = format!(
            "{}{}{}",
            if r { 'r' } else { '-' },
            if w { 'w' } else { '-' },
            if x { 'x' } else { '-' }
        );
        if self.guard() {
            s.push_str("+guard");
        }
        if self.gpu() {
            s.push_str("+gpu");
        }
        s
    }

    /// Short note for something that is not inside a module, e.g. "heap rw-".
    pub fn note(&self) -> String {
        let kind = match self.kind {
            MEM_PRIVATE => "heap",
            MEM_MAPPED => "mapped",
            MEM_IMAGE => "image",
            _ => "unknown",
        };
        format!("{kind} {}", self.prot_str())
    }
}

/// Read, write and execute bits of a protection value (guard ignored).
pub fn prot_rwx(protect: u32) -> (bool, bool, bool) {
    match protect & 0xFF {
        PAGE_READONLY => (true, false, false),
        PAGE_READWRITE | PAGE_WRITECOPY => (true, true, false),
        PAGE_EXECUTE => (false, false, true),
        PAGE_EXECUTE_READ => (true, false, true),
        PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => (true, true, true),
        _ => (false, false, false),
    }
}

/// What can be said about a read before trying it, from the address and its region alone.
pub fn precheck(addr: u64, region: Option<&Region>) -> Result<(), ReadError> {
    if !(USER_MIN..=USER_MAX).contains(&addr) {
        return Err(ReadError::InvalidAddress);
    }
    let Some(r) = region else {
        return Err(ReadError::InvalidAddress);
    };
    match r.state {
        MEM_FREE => Err(ReadError::Free),
        MEM_RESERVE => Err(ReadError::Reserved),
        MEM_COMMIT if r.guard() || !prot_rwx(r.protect).0 => Err(ReadError::NoAccess),
        MEM_COMMIT => Ok(()),
        _ => Err(ReadError::InvalidAddress),
    }
}

/// Is this a plausible pointer value (user range, 8-aligned)? Same test as the mod's ESP.
pub fn is_ptr(v: u64) -> bool {
    v > USER_MIN && v < 0x7FFF_FFFF_0000 && v.is_multiple_of(8)
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub base: u64,
    pub size: u64,
    pub path: String,
}

impl Module {
    pub fn contains(&self, addr: u64) -> bool {
        addr >= self.base && addr < self.base + self.size
    }
}

/// File version and PE timestamp of the exe: together they name a game build.
#[derive(Debug, Clone)]
pub struct Build {
    pub file_version: String,
    pub pe_timestamp: Option<u32>,
}

impl Build {
    /// Short build key, e.g. "1.2.3.4-5F3A1B2C".
    pub fn key(&self) -> String {
        match self.pe_timestamp {
            Some(t) => format!("{}-{t:08X}", self.file_version),
            None => self.file_version.clone(),
        }
    }
}

fn utf16_str(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Every running process as (pid, exe name).
pub fn list_processes() -> Vec<(u32, String)> {
    let mut out = Vec::new();
    // SAFETY: Toolhelp process snapshot; struct zeroed and sized.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                out.push((entry.th32ProcessID, utf16_str(&entry.szExeFile)));
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    out
}

/// An opened, read-only target process.
pub struct Target {
    handle: HANDLE,
    pub pid: u32,
    pub exe: String,
    pub path: String,
    pub modules: Vec<Module>,
}

impl Drop for Target {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Target {
    pub fn open(pid: u32) -> Result<Target, String> {
        // SAFETY: plain Win32; read-only rights.
        let handle = unsafe { OpenProcess(ACCESS, 0, pid) };
        if handle.is_null() {
            return Err(format!("OpenProcess({pid}) failed (error {})", unsafe { GetLastError() }));
        }
        let mut t = Target { handle, pid, exe: String::new(), path: String::new(), modules: Vec::new() };
        t.path = t.image_path();
        t.exe = t.path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        t.refresh_modules();
        Ok(t)
    }

    fn image_path(&self) -> String {
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        // SAFETY: buffer and length are ours.
        let ok = unsafe { QueryFullProcessImageNameW(self.handle, 0, buf.as_mut_ptr(), &mut len) };
        if ok == 0 { String::new() } else { utf16_str(&buf[..len as usize]) }
    }

    /// Reload the module list (main exe first).
    pub fn refresh_modules(&mut self) {
        let mut out = Vec::new();
        // SAFETY: Toolhelp module snapshot of another process; may need a retry while it loads.
        unsafe {
            let mut snap = INVALID_HANDLE_VALUE;
            for _ in 0..5 {
                snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, self.pid);
                if snap != INVALID_HANDLE_VALUE {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if snap != INVALID_HANDLE_VALUE {
                let mut entry: MODULEENTRY32W = std::mem::zeroed();
                entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
                if Module32FirstW(snap, &mut entry) != 0 {
                    loop {
                        out.push(Module {
                            name: utf16_str(&entry.szModule),
                            base: entry.modBaseAddr as u64,
                            size: entry.modBaseSize as u64,
                            path: utf16_str(&entry.szExePath),
                        });
                        if Module32NextW(snap, &mut entry) == 0 {
                            break;
                        }
                    }
                }
                CloseHandle(snap);
            }
        }
        if let Some(i) = out.iter().position(|m| m.name.eq_ignore_ascii_case(&self.exe)) {
            let m = out.remove(i);
            out.insert(0, m);
        }
        self.modules = out;
    }

    /// Still running? (GetExitCodeProcess.)
    pub fn alive(&self) -> bool {
        let mut code = 0u32;
        // SAFETY: our handle has query rights.
        let ok = unsafe { GetExitCodeProcess(self.handle, &mut code) };
        ok != 0 && code == STILL_ACTIVE
    }

    pub fn exe_module(&self) -> Option<&Module> {
        self.modules.first()
    }

    /// "exe" is the main module; otherwise a case-insensitive name, with or without ".dll"/".exe".
    pub fn module_by_name(&self, name: &str) -> Option<&Module> {
        if name.eq_ignore_ascii_case("exe") {
            return self.exe_module();
        }
        self.modules.iter().find(|m| {
            m.name.eq_ignore_ascii_case(name)
                || m.name.rsplit_once('.').is_some_and(|(stem, _)| stem.eq_ignore_ascii_case(name))
        })
    }

    pub fn module_at(&self, addr: u64) -> Option<&Module> {
        self.modules.iter().find(|m| m.contains(addr))
    }

    pub fn query(&self, addr: u64) -> Option<Region> {
        // SAFETY: plain query into our own struct.
        unsafe {
            let mut mbi: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            let n = VirtualQueryEx(
                self.handle,
                addr as *const c_void,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            );
            if n == 0 {
                return None;
            }
            Some(Region {
                base: mbi.BaseAddress as u64,
                size: mbi.RegionSize as u64,
                alloc_base: mbi.AllocationBase as u64,
                state: mbi.State,
                protect: mbi.Protect,
                kind: mbi.Type,
            })
        }
    }

    /// Every region from `from` up to `to`, in order.
    pub fn regions(&self, from: u64, to: u64) -> Vec<Region> {
        let mut out = Vec::new();
        let mut a = from & !(PAGE - 1);
        while a < to && a <= USER_MAX {
            let Some(r) = self.query(a) else { break };
            let next = r.end();
            out.push(r);
            if next <= a {
                break;
            }
            a = next;
        }
        out
    }

    /// Plain ReadProcessMemory: bytes read, 0 on failure. Callers check the region first.
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) -> usize {
        let mut got = 0usize;
        // SAFETY: the buffer is ours and sized; the kernel validates the remote range.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut got,
            )
        };
        if ok == 0 { 0 } else { got }
    }

    /// Read as much as possible, region by region. Returns the bytes that were read and, if it
    /// stopped early, why. Failed bytes are never filled with zeros.
    pub fn read_some(&self, addr: u64, len: usize) -> (Vec<u8>, Option<ReadError>) {
        let mut out: Vec<u8> = Vec::with_capacity(len.min(1 << 24));
        let short = |out: &Vec<u8>, e: ReadError| {
            if out.is_empty() || e == ReadError::ProcessGone { e } else { ReadError::PartialRead(out.len()) }
        };
        if addr.checked_add(len as u64).is_none() {
            return (out, Some(ReadError::InvalidAddress));
        }
        while out.len() < len {
            let cur = addr + out.len() as u64;
            let region = if (USER_MIN..=USER_MAX).contains(&cur) { self.query(cur) } else { None };
            if region.is_none() && !self.alive() {
                return (out.clone(), Some(ReadError::ProcessGone));
            }
            if let Err(e) = precheck(cur, region.as_ref()) {
                let e = short(&out, e);
                return (out, Some(e));
            }
            let r = region.unwrap_or_else(|| unreachable!());
            let n = ((r.end() - cur) as usize).min(len - out.len());
            let start = out.len();
            out.resize(start + n, 0);
            let got = self.read_raw(cur, &mut out[start..]);
            if got == n {
                continue;
            }
            // The whole span failed (pages can change under us): fall back to pages.
            out.truncate(start);
            let mut p = cur;
            while p < cur + n as u64 {
                let page_end = ((p & !(PAGE - 1)) + PAGE).min(cur + n as u64);
                let mut buf = vec![0u8; (page_end - p) as usize];
                if self.read_raw(p, &mut buf) != buf.len() {
                    break;
                }
                out.extend_from_slice(&buf);
                p = page_end;
            }
            if !self.alive() {
                return (out, Some(ReadError::ProcessGone));
            }
            let e = match precheck(p, self.query(p).as_ref()) {
                Err(e) => e,
                Ok(()) => ReadError::NoAccess,
            };
            let e = short(&out, e);
            return (out, Some(e));
        }
        (out, None)
    }

    /// Read exactly `len` bytes or say why not.
    pub fn read(&self, addr: u64, len: usize) -> Result<Vec<u8>, ReadError> {
        match self.read_some(addr, len) {
            (b, None) => Ok(b),
            (_, Some(e)) => Err(e),
        }
    }

    pub fn read_u64(&self, addr: u64) -> Result<u64, ReadError> {
        let b = self.read(addr, 8)?;
        Ok(u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])))
    }

    /// Which of `pages` pages starting at `addr` (page-aligned) are in RAM now. Reading only those
    /// does not page anything in or grow the game.
    pub fn resident(&self, addr: u64, pages: usize) -> Option<Vec<bool>> {
        let mut info: Vec<PSAPI_WORKING_SET_EX_INFORMATION> = (0..pages)
            .map(|i| PSAPI_WORKING_SET_EX_INFORMATION {
                VirtualAddress: (addr + i as u64 * PAGE) as *mut c_void,
                ..Default::default()
            })
            .collect();
        let cb = (pages * std::mem::size_of::<PSAPI_WORKING_SET_EX_INFORMATION>()) as u32;
        // SAFETY: array sized to cb; query rights only.
        let ok = unsafe { QueryWorkingSetEx(self.handle, info.as_mut_ptr() as *mut c_void, cb) };
        if ok == 0 {
            return None;
        }
        // SAFETY: Flags is the plain-integer view of the union.
        Some(info.iter().map(|i| unsafe { i.VirtualAttributes.Flags } & 1 != 0).collect())
    }

    /// Static form of an address: "module+0xRVA", or a region note like "heap rw- (region 0x.
    /// +0x..)".
    pub fn describe(&self, addr: u64) -> String {
        if let Some(m) = self.module_at(addr) {
            return format!("{}+0x{:X}", m.name, addr - m.base);
        }
        if !(USER_MIN..=USER_MAX).contains(&addr) {
            return "not a user address".into();
        }
        match self.query(addr) {
            Some(r) if r.committed() => {
                format!("{} (alloc 0x{:X} +0x{:X})", r.note(), r.alloc_base, addr - r.alloc_base)
            }
            Some(r) => format!("{} (no data)", r.state_str()),
            None => "not mapped".into(),
        }
    }

    /// Where a value points, if it points into committed memory: "exe+0x..", "heap rw-", else None.
    pub fn points_to(&self, v: u64) -> Option<String> {
        if !(USER_MIN..=USER_MAX).contains(&v) {
            return None;
        }
        if let Some(m) = self.module_at(v) {
            return Some(format!("{}+0x{:X}", m.name, v - m.base));
        }
        match self.query(v) {
            Some(r) if r.committed() => Some(r.note()),
            _ => None,
        }
    }

    /// File version of the exe file and the PE header timestamp read from the image.
    pub fn build(&self) -> Build {
        let file_version = file_version(&self.path).unwrap_or_else(|| "unknown".into());
        let pe_timestamp = self.exe_module().and_then(|m| {
            let dos = self.read(m.base, 0x40).ok()?;
            if &dos[..2] != b"MZ" {
                return None;
            }
            let lfanew = u32::from_le_bytes(dos[0x3C..0x40].try_into().ok()?) as u64;
            let nt = self.read(m.base + lfanew, 12).ok()?;
            if &nt[..4] != b"PE\0\0" {
                return None;
            }
            Some(u32::from_le_bytes(nt[8..12].try_into().ok()?))
        });
        Build { file_version, pe_timestamp }
    }
}

/// "a.b.c.d" from the file's version resource.
fn file_version(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let wpath = wide(path);
    // SAFETY: version API on a file path; buffers sized by the API's own answer.
    unsafe {
        let mut dummy = 0u32;
        let size = GetFileVersionInfoSizeW(wpath.as_ptr(), &mut dummy);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        if GetFileVersionInfoW(wpath.as_ptr(), 0, size, data.as_mut_ptr() as *mut c_void) == 0 {
            return None;
        }
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let root = wide("\\");
        if VerQueryValueW(data.as_ptr() as *const c_void, root.as_ptr(), &mut ptr, &mut len) == 0
            || ptr.is_null()
            || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let fi = &*(ptr as *const VS_FIXEDFILEINFO);
        Some(format!(
            "{}.{}.{}.{}",
            fi.dwFileVersionMS >> 16,
            fi.dwFileVersionMS & 0xFFFF,
            fi.dwFileVersionLS >> 16,
            fi.dwFileVersionLS & 0xFFFF
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::PAGE_NOACCESS;

    fn region(state: u32, protect: u32) -> Region {
        Region { base: 0x10_0000, size: 0x1000, alloc_base: 0x10_0000, state, protect, kind: MEM_PRIVATE }
    }

    #[test]
    fn classifies_before_reading() {
        let ok = region(MEM_COMMIT, PAGE_READWRITE);
        assert_eq!(precheck(0x10_0000, Some(&ok)), Ok(()));
        assert_eq!(precheck(0, Some(&ok)), Err(ReadError::InvalidAddress));
        assert_eq!(precheck(0xFFFF, Some(&ok)), Err(ReadError::InvalidAddress));
        assert_eq!(precheck(0x8000_0000_0000, Some(&ok)), Err(ReadError::InvalidAddress));
        assert_eq!(precheck(0xFFFF_F800_0000_0000, Some(&ok)), Err(ReadError::InvalidAddress));
        assert_eq!(precheck(0x10_0000, None), Err(ReadError::InvalidAddress));
        assert_eq!(precheck(0x10_0000, Some(&region(MEM_FREE, 0))), Err(ReadError::Free));
        assert_eq!(precheck(0x10_0000, Some(&region(MEM_RESERVE, 0))), Err(ReadError::Reserved));
        assert_eq!(precheck(0x10_0000, Some(&region(MEM_COMMIT, PAGE_NOACCESS))), Err(ReadError::NoAccess));
        assert_eq!(
            precheck(0x10_0000, Some(&region(MEM_COMMIT, PAGE_READWRITE | PAGE_GUARD))),
            Err(ReadError::NoAccess)
        );
        assert_eq!(precheck(0x10_0000, Some(&region(MEM_COMMIT, PAGE_EXECUTE))), Err(ReadError::NoAccess));
        assert_eq!(precheck(0x10_0000, Some(&region(MEM_COMMIT, PAGE_EXECUTE_READ))), Ok(()));
    }

    #[test]
    fn prot_strings() {
        assert_eq!(region(MEM_COMMIT, PAGE_READWRITE).prot_str(), "rw-");
        assert_eq!(region(MEM_COMMIT, PAGE_EXECUTE_READ).prot_str(), "r-x");
        assert_eq!(region(MEM_COMMIT, PAGE_READWRITE | PAGE_GUARD).prot_str(), "rw-+guard");
        assert_eq!(region(MEM_RESERVE, 0).prot_str(), "---");
        assert_eq!(region(MEM_COMMIT, PAGE_READWRITE).note(), "heap rw-");
        assert!(is_ptr(0x1_0000_0008));
        assert!(!is_ptr(0x1_0000_0004));
        assert!(!is_ptr(0x100));
    }
}
