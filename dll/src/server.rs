//! Named-pipe server. One client at a time: the Launchpad (only `Ping`, to see the mod is in) or
//! the mod's own unloader (`Bye`). Lines of JSON in, lines out.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;

use aurora_core::proto::{self, Request, Response};
use aurora_core::{PIPE_NAME, PROTO_VERSION};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe,
    PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::log;


fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Unload from inside, with no Launchpad: send `Bye` to our own pipe on a thread of its own (the
/// shutdown it starts joins the menu's thread, so the menu cannot wait for it), then the start
/// thread frees the DLL (see `lib.rs`).
pub fn unload_self() -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if crate::SELF_UNLOAD.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let h = std::thread::spawn(|| {
        // The pipe can be busy for a moment (the Launchpad pinging) or between two clients (not
        // made again yet): try for up to 3 s.
        let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(PIPE_NAME);
        for _ in 0..30 {
            if pipe.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            pipe = std::fs::OpenOptions::new().read(true).write(true).open(PIPE_NAME);
        }
        match pipe {
            Ok(mut p) => {
                let _ = p.write_all(proto::encode(&Request::Bye).as_bytes());
                let mut answer = String::new();
                let _ = BufReader::new(p).read_line(&mut answer);
                log(&format!("own bye answered: {}", answer.trim()));
            }
            Err(e) => {
                log(&format!("own unload: pipe not reachable: {e}"));
                crate::SELF_UNLOAD.store(false, Ordering::SeqCst);
            }
        }
    });
    if let Ok(mut u) = crate::UNLOADER.lock() {
        *u = Some(h);
    }
    Ok(())
}

/// Runs until a client says `Bye`. Each disconnect goes back to waiting.
pub fn run() {
    let name = wide(PIPE_NAME);
    loop {
        // SAFETY: plain Win32; the name is NUL-terminated.
        let pipe = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 * 1024,
                64 * 1024,
                0,
                std::ptr::null(),
            )
        };
        if pipe == INVALID_HANDLE_VALUE {
            log(&format!("CreateNamedPipe failed: {}", unsafe { GetLastError() }));
            return;
        }
        log("waiting for the window");
        // SAFETY: valid pipe handle. A client that connected between create and this call
        // Makes ConnectNamedPipe return 0 with ERROR_PIPE_CONNECTED (535); that's fine.
        let connected = unsafe { ConnectNamedPipe(pipe, std::ptr::null_mut()) } != 0
            || unsafe { GetLastError() } == 535;
        if !connected {
            log(&format!("ConnectNamedPipe failed: {}", unsafe { GetLastError() }));
            unsafe { CloseHandle(pipe) };
            continue;
        }
        log("window connected");
        // SAFETY: we own the handle; File closes it on drop.
        let file = unsafe { File::from_raw_handle(pipe as _) };
        let bye = serve(file);
        // File is dropped (handle closed) by serve. Nothing else to do for the pipe.
        if bye {
            log("bye received");
            return;
        }
    }
}

/// Handle one client until it disconnects. Returns true when it asked us to stop.
fn serve(file: File) -> bool {
    let mut writer = match file.try_clone() {
        Ok(f) => f,
        Err(e) => {
            log(&format!("pipe clone failed: {e}"));
            return false;
        }
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // The mod keeps everything it holds: the Launchpad only launches and injects, and
                // closing it must not turn god mode or the gear score off.
                log("Launchpad disconnected (the mod keeps running)");
                return false;
            }
            Ok(_) => {}
            Err(e) => {
                log(&format!("pipe read error: {e}"));
                return false;
            }
        }
        let req: Request = match proto::decode(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = writer.write_all(proto::encode(&Response::err(format!("bad request: {e}"))).as_bytes());
                continue;
            }
        };
        let is_bye = matches!(req, Request::Bye);
        crate::watchdog::note(&format!("{req:?}"));
        if is_bye {
            // DirectX 11: another program hooked the present call on top of ours, so taking ours
            // out would crash the game. Say no and keep running.
            if let Err(e) = crate::vkdraw::can_unload() {
                log(&e);
                crate::menu::toast(e.clone());
                crate::SELF_UNLOAD.store(false, std::sync::atomic::Ordering::SeqCst);
                if writer.write_all(proto::encode(&Response::err(e)).as_bytes()).is_err() {
                    return false;
                }
                continue;
            }
            // From here the unload goes ahead: the warm-up and god mode's wait stop.
            crate::begin_stop();
            let t = std::time::Instant::now();
            // Each step in the log, so a step that hangs shows.
            let step = |what: &str| log(&format!("unload: {what}"));
            // First: the menu's hold on the game window's messages, then the hook in the game's
            // present call; either left behind would crash the game once we unload.
            step("menu");
            crate::menu::shutdown();
            step("drawing");
            if !crate::vkdraw::shutdown() {
                // Hooked over between the check above and now: the hooks stay in, so the DLL must
                // not be freed.
                crate::KEEP_LOADED.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            // The warm-up before the parts it prepares, so nothing of theirs is made or switched on
            // after their shutdown.
            step("warm-up");
            crate::join_warm();
            step("watchdog");
            crate::watchdog::shutdown();
            step("fly");
            crate::fly::shutdown();
            step("switches");
            let _ = crate::unseen::set(false);
            let _ = crate::inbox::set(false);
            let _ = crate::nvgzone::set(false);
            step("silent aim");
            crate::silentaim::shutdown();
            step("weapon");
            crate::weapon::shutdown();
            step("nvg");
            crate::nvg::shutdown();
            step("airwalk");
            crate::airwalk::shutdown();
            step("fly through");
            crate::noclip::shutdown();
            step("credits");
            crate::credits::shutdown();
            step("gear score");
            crate::gearscore::shutdown();
            step("god mode");
            let _ = crate::godmode::off();
            crate::godmode::shutdown();
            // After god mode's shutdown cancelled the watchpoints.
            step("credits watch");
            crate::credits::join_watch();
            step("holds");
            crate::freeze::shutdown();
            log(&format!("unload: all parts stopped in {} ms", t.elapsed().as_millis()));
        }
        let resp = handle(req);
        if writer.write_all(proto::encode(&resp).as_bytes()).is_err() {
            log("pipe write failed");
            return false;
        }
        if is_bye {
            // Let the client read Ok before the handle goes away.
            let _ = writer.flush();
            // SAFETY: the raw handle is still ours until the File drops.
            unsafe {
                use std::os::windows::io::AsRawHandle;
                DisconnectNamedPipe(reader.get_ref().as_raw_handle() as _);
            }
            return true;
        }
    }
}

fn handle(req: Request) -> Response {
    match req {
        Request::Ping => Response::Pong {
            pid: std::process::id(),
            proto: PROTO_VERSION,
            dll_version: crate::DLL_VERSION.to_string(),
        },
        Request::Bye => Response::Ok,
    }
}
