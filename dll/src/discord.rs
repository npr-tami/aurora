//! Aurora > Settings > Discord: the Discord status, through Discord's local pipe (`\\.\pipe\discord-ipc-N`).
//!
//! A frame is two little-endian u32s (opcode, length) and that many bytes of JSON. Opcode 0 is the
//! handshake with the application id, 1 a command (`SET_ACTIVITY`), 2 close. Discord answers every
//! frame; answers are read with a time limit, so a silent Discord never holds a thread.
//!
//! What is shown:
//! - Line 1: "Ghost Recon Breakpoint".
//! - Line 2: "Free roam · Erewhon" in the hub, else "Free roam · In the field · Solo" or
//!   "Free roam · In the field · Co-op · N players".
//!   Players come from the game's world list: in the open world only the squad is there (at most 4
//!   with the player), in the hub everyone in it, so more than 3 other players means the hub.
//! - Time: since the game started.
//! - Picture: the game's art with the Aurora logo in its corner, the logo alone, or none (art
//!   assets `breakpoint` and `aurora_a` of the Discord application).

use std::fs::File;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use crate::{entities, log};

const APP_ID: &str = "1575298778028835018";
/// Discord takes 5 updates per 20 s; one every 15 s at most, and only when something changed.
const EVERY: Duration = Duration::from_secs(15);
/// No Discord running: try again this often.
const RETRY: Duration = Duration::from_secs(30);
/// How long to wait for Discord's answer to a frame.
const ANSWER_WAIT: Duration = Duration::from_secs(3);
/// A frame bigger than this is not Discord's.
const FRAME_MAX: u32 = 64 * 1024;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;

const ASSET_GAME: &str = "breakpoint";
const ASSET_A: &str = "aurora_a";

/// Squad size in the open world, the player included.
const SQUAD_MAX: usize = 4;
/// Far enough to take in the whole loaded world.
const WORLD_RADIUS: f64 = 100_000.0;
const WORLD_MAX: usize = 400;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Picture {
    None = 0,
    /// The game's art, the Aurora logo in its corner.
    GameWithA = 1,
    /// The Aurora logo alone.
    AOnly = 2,
}

static ON: AtomicBool = AtomicBool::new(false);
static PICTURE: AtomicU8 = AtomicU8::new(Picture::None as u8);
/// Something the status shows changed: send at the next chance, not after `EVERY`.
static DIRTY: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

pub fn is_on() -> bool {
    ON.load(Ordering::SeqCst)
}

pub fn picture() -> Picture {
    match PICTURE.load(Ordering::SeqCst) {
        1 => Picture::GameWithA,
        2 => Picture::AOnly,
        _ => Picture::None,
    }
}

pub fn set_picture(p: Picture) {
    PICTURE.store(p as u8, Ordering::SeqCst);
    DIRTY.store(true, Ordering::SeqCst);
}

/// On: the worker starts and keeps the status up to date. Off: the status is cleared and the pipe
/// closed.
pub fn set(on: bool) -> Result<(), String> {
    if on == is_on() {
        return Ok(());
    }
    // Unit tests never talk to the Discord running on the machine.
    if cfg!(test) {
        ON.store(on, Ordering::SeqCst);
        return Ok(());
    }
    if on {
        if crate::stopping() {
            return Err("unloading".into());
        }
        stop_worker();
        STOP.store(false, Ordering::SeqCst);
        ON.store(true, Ordering::SeqCst);
        DIRTY.store(true, Ordering::SeqCst);
        let h = std::thread::Builder::new().name("aurora discord".into()).spawn(worker).map_err(|e| format!("discord thread: {e}"))?;
        if let Ok(mut w) = WORKER.lock() {
            *w = Some(h);
        }
    } else {
        ON.store(false, Ordering::SeqCst);
        stop_worker();
    }
    Ok(())
}

/// Unload: clear the status, close the pipe, wait for the thread.
pub fn shutdown() {
    ON.store(false, Ordering::SeqCst);
    stop_worker();
}

fn stop_worker() {
    STOP.store(true, Ordering::SeqCst);
    let h = WORKER.lock().ok().and_then(|mut w| w.take());
    if let Some(h) = h {
        crate::join_for(h, "discord", Duration::from_secs(10));
    }
}

fn stopped() -> bool {
    STOP.load(Ordering::SeqCst) || crate::stopping()
}

/// Sleep up to `d` in short steps; false once told to stop.
fn wait(d: Duration, wake_on_dirty: bool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stopped() {
            return false;
        }
        if wake_on_dirty && DIRTY.load(Ordering::SeqCst) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !stopped()
}

fn worker() {
    let start = game_start();
    let mut world = entities::WorldList::default();
    let mut pipe: Option<File> = None;
    let mut sent = String::new();
    let mut said = String::new();
    let mut last_send: Option<Instant> = None;
    while !stopped() {
        if pipe.is_none() {
            match connect() {
                Ok(p) => {
                    log("discord: connected");
                    said.clear();
                    sent.clear();
                    pipe = Some(p);
                }
                Err(e) => {
                    if e != said {
                        log(&format!("discord: {e}; trying again every {} s", RETRY.as_secs()));
                        said = e;
                    }
                    if !wait(RETRY, false) {
                        break;
                    }
                    continue;
                }
            }
        }
        // Not more often than Discord takes.
        if let Some(t) = last_send
            && t.elapsed() < EVERY
            && !wait(EVERY - t.elapsed(), false)
        {
            break;
        }
        DIRTY.store(false, Ordering::SeqCst);
        let others = other_players(&mut world);
        let activity = activity_json(&status(others), start, picture());
        if activity != sent {
            let Some(p) = pipe.as_mut() else { continue };
            match set_activity(p, &activity) {
                Ok(()) => {
                    sent = activity;
                    last_send = Some(Instant::now());
                }
                Err(e) => {
                    log(&format!("discord: {e}; connecting again"));
                    pipe = None;
                    continue;
                }
            }
        }
        if !wait(EVERY, true) {
            break;
        }
    }
    if let Some(mut p) = pipe {
        let _ = set_activity(&mut p, "null");
        let _ = write_frame(&mut p, OP_CLOSE, "{}");
        log("discord: status cleared");
    }
}

/// Other players in the player's world; None while the player is in no world (menus, loading).
fn other_players(world: &mut entities::WorldList) -> Option<usize> {
    world.pass(WORLD_RADIUS, WORLD_MAX).ok().map(|l| l.iter().filter(|b| b.player).count())
}

/// The two lines: (details, state).
fn status(others: Option<usize>) -> (String, String) {
    let details = "Ghost Recon Breakpoint".to_string();
    let state = match others {
        None => "In the menus".to_string(),
        Some(n) if n >= SQUAD_MAX => "Free roam · Erewhon".to_string(),
        Some(0) => "Free roam · In the field · Solo".to_string(),
        Some(n) => format!("Free roam · In the field · Co-op · {} players", n + 1),
    };
    (details, state)
}

/// When the game started, in Unix seconds; now if it can't be read.
fn game_start() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        let (mut created, mut exited, mut kernel, mut user) = (z, z, z, z);
        // SAFETY: the current process's pseudo handle and four FILETIMEs of ours.
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut created, &mut exited, &mut kernel, &mut user) };
        if ok != 0 {
            let t = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
            // FILETIME counts 100 ns steps since 1601.
            let unix = (t / 10_000_000).saturating_sub(11_644_473_600);
            if unix > 0 && unix <= now {
                return unix;
            }
        }
    }
    now
}

fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The activity as Discord's `SET_ACTIVITY` wants it.
fn activity_json((details, state): &(String, String), start: u64, pic: Picture) -> String {
    let assets = match pic {
        Picture::None => String::new(),
        Picture::GameWithA => format!(r#","assets":{{"large_image":"{ASSET_GAME}","large_text":"Ghost Recon Breakpoint","small_image":"{ASSET_A}","small_text":"Aurora"}}"#),
        Picture::AOnly => format!(r#","assets":{{"large_image":"{ASSET_A}","large_text":"Aurora"}}"#),
    };
    format!(r#"{{"details":{},"state":{},"timestamps":{{"start":{start}}}{assets}}}"#, json_str(details), json_str(state))
}

fn command_json(activity: &str, pid: u32, nonce: u64) -> String {
    format!(r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{pid},"activity":{activity}}},"nonce":"{nonce}"}}"#)
}

fn frame(op: u32, json: &str) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + json.len());
    v.extend_from_slice(&op.to_le_bytes());
    v.extend_from_slice(&(json.len() as u32).to_le_bytes());
    v.extend_from_slice(json.as_bytes());
    v
}

/// A frame's header: (opcode, length). Err when it can't be Discord's.
fn header(h: [u8; 8]) -> Result<(u32, u32), String> {
    let op = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
    let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]);
    if op > 4 || len > FRAME_MAX {
        return Err(format!("odd frame from Discord (op {op}, {len} bytes)"));
    }
    Ok((op, len))
}

/// Discord's answer is an error: its message.
fn answer_error(op: u32, body: &str) -> Option<String> {
    if op == OP_CLOSE {
        return Some(format!("Discord closed the link: {body}"));
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    (v.get("evt").and_then(|e| e.as_str()) == Some("ERROR")).then(|| {
        let msg = v.pointer("/data/message").and_then(|m| m.as_str()).unwrap_or("unknown error");
        format!("Discord said: {msg}")
    })
}

fn write_frame(p: &mut File, op: u32, json: &str) -> Result<(), String> {
    p.write_all(&frame(op, json)).map_err(|e| format!("pipe write: {e}"))
}

/// Bytes waiting in the pipe, without blocking.
fn waiting(p: &File) -> Result<u32, String> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        let mut avail = 0u32;
        // SAFETY: the pipe's own handle; only the count is asked for.
        let ok = unsafe { windows_sys::Win32::System::Pipes::PeekNamedPipe(p.as_raw_handle() as _, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) };
        if ok == 0 {
            return Err("the pipe closed".into());
        }
        Ok(avail)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        Ok(0)
    }
}

fn read_exact_within(p: &mut File, buf: &mut [u8], until: Instant) -> Result<(), String> {
    let mut got = 0;
    while got < buf.len() {
        let avail = waiting(p)? as usize;
        if avail == 0 {
            if Instant::now() >= until || stopped() {
                return Err("no answer from Discord".into());
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let want = (buf.len() - got).min(avail);
        let n = p.read(&mut buf[got..got + want]).map_err(|e| format!("pipe read: {e}"))?;
        if n == 0 {
            return Err("the pipe closed".into());
        }
        got += n;
    }
    Ok(())
}

/// Discord's answer to the last frame: Err if it is an error or does not come.
fn read_answer(p: &mut File) -> Result<(), String> {
    let until = Instant::now() + ANSWER_WAIT;
    let mut h = [0u8; 8];
    read_exact_within(p, &mut h, until)?;
    let (op, len) = header(h)?;
    let mut body = vec![0u8; len as usize];
    read_exact_within(p, &mut body, until)?;
    match answer_error(op, &String::from_utf8_lossy(&body)) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The first Discord pipe that takes the handshake.
fn connect() -> Result<File, String> {
    let mut last = "Discord is not running".to_string();
    for i in 0..10 {
        let name = format!(r"\\.\pipe\discord-ipc-{i}");
        let Ok(mut p) = std::fs::OpenOptions::new().read(true).write(true).open(&name) else { continue };
        let hello = format!(r#"{{"v":1,"client_id":"{APP_ID}"}}"#);
        match write_frame(&mut p, OP_HANDSHAKE, &hello).and_then(|()| read_answer(&mut p)) {
            Ok(()) => return Ok(p),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn set_activity(p: &mut File, activity: &str) -> Result<(), String> {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let cmd = command_json(activity, std::process::id(), NONCE.fetch_add(1, Ordering::Relaxed));
    write_frame(p, OP_FRAME, &cmd)?;
    read_answer(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_for_solo_coop_and_the_hub() {
        assert_eq!(status(Some(0)).1, "Free roam · In the field · Solo");
        assert_eq!(status(Some(1)).1, "Free roam · In the field · Co-op · 2 players");
        assert_eq!(status(Some(3)).1, "Free roam · In the field · Co-op · 4 players");
        // More players than a squad holds: the hub.
        assert_eq!(status(Some(4)).1, "Free roam · Erewhon");
        assert_eq!(status(Some(14)).1, "Free roam · Erewhon");
        assert_eq!(status(None).1, "In the menus");
        assert_eq!(status(Some(0)).0, "Ghost Recon Breakpoint");
    }

    #[test]
    fn activity_json_is_valid_for_each_picture() {
        let lines = status(Some(1));
        for (pic, large, small) in [(Picture::None, None, None), (Picture::GameWithA, Some(ASSET_GAME), Some(ASSET_A)), (Picture::AOnly, Some(ASSET_A), None)] {
            let v: serde_json::Value = serde_json::from_str(&activity_json(&lines, 1_700_000_000, pic)).unwrap();
            assert_eq!(v["details"], "Ghost Recon Breakpoint");
            assert_eq!(v["state"], "Free roam · In the field · Co-op · 2 players");
            assert_eq!(v["timestamps"]["start"], 1_700_000_000u64);
            assert_eq!(v.pointer("/assets/large_image").and_then(|x| x.as_str()), large);
            assert_eq!(v.pointer("/assets/small_image").and_then(|x| x.as_str()), small);
        }
    }

    #[test]
    fn command_wraps_the_activity() {
        let v: serde_json::Value = serde_json::from_str(&command_json("null", 42, 7)).unwrap();
        assert_eq!(v["cmd"], "SET_ACTIVITY");
        assert_eq!(v["args"]["pid"], 42);
        assert!(v["args"]["activity"].is_null());
        assert_eq!(v["nonce"], "7");
    }

    #[test]
    fn frames_round_trip() {
        let f = frame(OP_FRAME, "{}");
        assert_eq!(f.len(), 10);
        assert_eq!(header(f[..8].try_into().unwrap()), Ok((OP_FRAME, 2)));
        assert!(header([9, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(header([1, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F]).is_err());
    }

    #[test]
    fn errors_from_discord_are_seen() {
        assert_eq!(answer_error(OP_FRAME, r#"{"cmd":"SET_ACTIVITY","evt":null,"data":{}}"#), None);
        assert_eq!(answer_error(OP_FRAME, r#"{"evt":"ERROR","data":{"code":4000,"message":"bad"}}"#).as_deref(), Some("Discord said: bad"));
        assert!(answer_error(OP_CLOSE, r#"{"code":4000}"#).is_some());
    }

    #[test]
    fn pictures_round_trip() {
        for p in [Picture::GameWithA, Picture::AOnly, Picture::None] {
            set_picture(p);
            assert_eq!(picture(), p);
        }
    }
}
