//! The game process from the outside: find it, open it, read it, inject the DLL.

use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;

use aurora_core::proto::ModuleInfo;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM, RECT};
use windows_sys::Win32::System::Diagnostics::Debug::WriteProcessMemory;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, PROCESSENTRY32W,
    Process32FirstW, Process32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
};
use windows_sys::Win32::System::Threading::{
    CreateRemoteThread, OpenProcess, PROCESS_CREATE_THREAD, PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION,
    PROCESS_SYNCHRONIZE, PROCESS_VM_READ, PROCESS_VM_WRITE, WaitForSingleObject,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};

use crate::platform::GAME_EXES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Game {
    pub pid: u32,
    pub exe: String,
    /// The exe's full path, when Windows tells us.
    pub path: Option<std::path::PathBuf>,
}

impl Game {
    /// The Vulkan build (GRB_vulkan.exe); else DirectX 11 (GRB.exe). The mod draws on both since
    /// and picks its own way in the game; this is only for the status line, the log and the hook
    /// options.
    pub fn vulkan(&self) -> bool {
        self.exe.to_ascii_lowercase().ends_with("_vulkan.exe")
    }

    /// "Vulkan" or "DirectX 11", for the status line and the log.
    pub fn api(&self) -> &'static str {
        if self.vulkan() { "Vulkan" } else { "DirectX 11" }
    }
}

/// The exe's full path (needs only limited query rights).
fn image_path(pid: u32) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::System::Threading::{PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    // SAFETY: plain Win32; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return None;
    }
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer is as long as we say.
    let ok = unsafe { QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) } != 0;
    // SAFETY: opened above.
    unsafe { CloseHandle(h) };
    ok.then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

fn utf16_str(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The running game, if any. With two (its start-up restart overlaps), the one in the picked
/// store's folder, then the one with a game-sized window, wins.
pub fn find_game(dir: Option<&Path>) -> Option<Game> {
    let mut all = Vec::new();
    // SAFETY: Toolhelp process snapshot; struct zeroed and sized.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let name = utf16_str(&entry.szExeFile);
                if GAME_EXES.iter().any(|g| g.eq_ignore_ascii_case(&name)) {
                    all.push(Game { pid: entry.th32ProcessID, exe: name, path: image_path(entry.th32ProcessID) });
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    if all.len() <= 1 {
        return all.pop();
    }
    let low = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    let in_dir = |g: &Game| dir.is_some_and(|d| g.path.as_ref().and_then(|p| p.parent()).is_some_and(|p| low(p) == low(d)));
    all.sort_by_key(|g| (!in_dir(g), window_size(g.pid).is_none()));
    all.into_iter().next()
}

/// The client size of the game's biggest visible window, in pixels.
pub fn window_size(pid: u32) -> Option<(i32, i32)> {
    struct Search {
        pid: u32,
        best: Option<(i32, i32)>,
    }
    unsafe extern "system" fn each_window(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: lparam is the &mut Search handed to EnumWindows below, alive for the call.
        let s = unsafe { &mut *(lparam as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: plain Win32 queries on a window handle EnumWindows gave us.
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == s.pid && IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
                let mut r: RECT = std::mem::zeroed();
                if GetClientRect(hwnd, &mut r) != 0 {
                    let (w, h) = (r.right - r.left, r.bottom - r.top);
                    let area = |(w, h): (i32, i32)| w as i64 * h as i64;
                    if area((w, h)) > s.best.map(area).unwrap_or(0) {
                        s.best = Some((w, h));
                    }
                }
            }
        }
        1
    }
    let mut s = Search { pid, best: None };
    // SAFETY: the callback only touches `s`, which outlives the call.
    unsafe { EnumWindows(Some(each_window), &mut s as *mut Search as LPARAM) };
    s.best
}

/// An open process handle. Closed on drop.
pub struct Proc {
    handle: HANDLE,
    pub pid: u32,
}

// SAFETY: a process handle is a kernel object; using it from several threads is fine.
unsafe impl Send for Proc {}
unsafe impl Sync for Proc {}

impl Drop for Proc {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe { CloseHandle(self.handle) };
    }
}

impl Proc {
    pub fn open(pid: u32) -> Result<Arc<Proc>, String> {
        // Only what loading a DLL needs.
        let access = PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ | PROCESS_SYNCHRONIZE;
        // SAFETY: plain Win32.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "OpenProcess failed (error {}). Is the game running as admin?",
                unsafe { GetLastError() }
            ));
        }
        Ok(Arc::new(Proc { handle, pid }))
    }

    /// Modules loaded in the game, main exe first.
    pub fn modules(&self) -> Vec<ModuleInfo> {
        self.module_files().into_iter().map(|(m, _)| m).collect()
    }

    /// Modules loaded in the game with their file paths, from one snapshot.
    pub fn module_files(&self) -> Vec<(ModuleInfo, String)> {
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
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut entry: MODULEENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;
            if Module32FirstW(snap, &mut entry) != 0 {
                loop {
                    out.push((
                        ModuleInfo {
                            name: utf16_str(&entry.szModule),
                            base: entry.modBaseAddr as u64,
                            size: entry.modBaseSize as u64,
                        },
                        utf16_str(&entry.szExePath),
                    ));
                    if Module32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out
    }

    pub fn has_module(&self, name: &str) -> bool {
        self.modules()
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(name))
    }

    /// Where the game has the module called `name`, when it does.
    pub fn module_base(&self, name: &str) -> Option<u64> {
        self.modules().iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| m.base)
    }

    /// `len` bytes of the game's memory at `addr` (the after-load check reads the loaded DLL's
    /// headers and card there).
    pub fn read_memory(&self, addr: u64, len: usize) -> Result<Vec<u8>, String> {
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        let mut out = vec![0u8; len];
        let mut read = 0usize;
        // SAFETY: a buffer of the size we say; a bad address only fails the call.
        let ok = unsafe { ReadProcessMemory(self.handle, addr as *const c_void, out.as_mut_ptr() as *mut c_void, len, &mut read) };
        if ok == 0 || read != len {
            return Err(format!("ReadProcessMemory at {addr:#x} failed (error {})", unsafe { GetLastError() }));
        }
        Ok(out)
    }

    /// Run a function the game already has (an export of the loaded DLL, at `addr`) on a new thread
    /// in the game, with `buf` written into the game's memory as its one argument; the bytes read
    /// back after it returned, and its return value. The same way as [`Proc::inject`] runs
    /// LoadLibraryW. A function still running after `wait` is left alone, with its memory.
    pub fn call_export(&self, addr: u64, buf: &[u8], wait: std::time::Duration) -> Result<(Vec<u8>, u32), String> {
        use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
        use windows_sys::Win32::System::Threading::GetExitCodeThread;
        let bytes = buf.len().max(1);
        // SAFETY: standard remote-thread call; every handle and pointer is checked.
        unsafe {
            let remote = VirtualAllocEx(self.handle, std::ptr::null(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() {
                return Err(format!("VirtualAllocEx failed (error {})", GetLastError()));
            }
            let mut written = 0usize;
            if !buf.is_empty() && (WriteProcessMemory(self.handle, remote, buf.as_ptr() as *const c_void, buf.len(), &mut written) == 0 || written != buf.len()) {
                VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
                return Err(format!("WriteProcessMemory failed (error {})", GetLastError()));
            }
            let start: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(addr as usize);
            let thread = CreateRemoteThread(self.handle, std::ptr::null(), 0, Some(start), remote, 0, std::ptr::null_mut());
            if thread.is_null() {
                VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
                return Err(format!("CreateRemoteThread failed (error {})", GetLastError()));
            }
            let waited = WaitForSingleObject(thread, wait.as_millis().min(u32::MAX as u128) as u32);
            if waited != 0 {
                CloseHandle(thread);
                return Err(format!("the call did not return within {} s", wait.as_secs()));
            }
            let mut code = 0u32;
            GetExitCodeThread(thread, &mut code);
            CloseHandle(thread);
            let mut back = vec![0u8; buf.len()];
            let mut read = 0usize;
            let ok = buf.is_empty() || (ReadProcessMemory(self.handle, remote, back.as_mut_ptr() as *mut c_void, buf.len(), &mut read) != 0 && read == buf.len());
            VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
            if !ok {
                return Err(format!("ReadProcessMemory failed (error {})", GetLastError()));
            }
            Ok((back, code))
        }
    }

    /// What [`Proc::inject`] says when LoadLibraryW is still running after 15 s: it may still open
    /// and load the file later.
    pub const TIMED_OUT: &'static str = "LoadLibraryW did not return within 15 s";

    /// Load `dll` into the game: write the path into its memory, start a thread on `LoadLibraryW`.
    /// The classic way; Steam's overlay does the same.
    pub fn inject(&self, dll: &Path) -> Result<(), String> {
        let path = wide(&dll.to_string_lossy());
        let bytes = path.len() * 2;
        // SAFETY: standard remote-thread injection; every handle and pointer is checked.
        unsafe {
            let kernel = GetModuleHandleW(wide("kernel32.dll").as_ptr());
            if kernel.is_null() {
                return Err("kernel32 not found".into());
            }
            let Some(load_library) = GetProcAddress(kernel, c"LoadLibraryW".as_ptr() as *const u8)
            else {
                return Err("LoadLibraryW not found".into());
            };
            let remote = VirtualAllocEx(
                self.handle,
                std::ptr::null(),
                bytes,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            if remote.is_null() {
                return Err(format!("VirtualAllocEx failed (error {})", GetLastError()));
            }
            let mut written = 0usize;
            let ok = WriteProcessMemory(
                self.handle,
                remote,
                path.as_ptr() as *const c_void,
                bytes,
                &mut written,
            );
            if ok == 0 || written != bytes {
                VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
                return Err(format!(
                    "WriteProcessMemory failed (error {})",
                    GetLastError()
                ));
            }
            // LoadLibraryW(path) has the shape a thread start routine wants on x64.
            let start: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(load_library);
            let thread = CreateRemoteThread(
                self.handle,
                std::ptr::null(),
                0,
                Some(start),
                remote,
                0,
                std::ptr::null_mut(),
            );
            if thread.is_null() {
                VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
                return Err(format!(
                    "CreateRemoteThread failed (error {})",
                    GetLastError()
                ));
            }
            let waited = WaitForSingleObject(thread, 15_000);
            CloseHandle(thread);
            if waited != 0 {
                // LoadLibraryW may still be reading the path: leave its memory alone (a few bytes
                // kept) rather than free it under the game's feet.
                return Err(Self::TIMED_OUT.into());
            }
            VirtualFreeEx(self.handle, remote, 0, MEM_RELEASE);
        }
        // The thread's exit code is only the low 32 bits of the module handle (zero for a module
        // loaded at a 4 GB boundary), so the module list says whether it loaded.
        let name = dll.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if self.has_module(&name) {
            Ok(())
        } else {
            Err(format!("{name} did not load (see the mod's dll.log; is the file a 64-bit DLL?)"))
        }
    }
}

