//! ESP drawn inside the game. The overlay window always showed up a frame or two after the game's
//! own picture, so its boxes trailed when the player turned. Here the game's "show this frame" call
//! (vkQueuePresentKHR) is hooked, the camera is read at that moment, and the boxes are copied into
//! the frame just before it goes out.
//!
//! Everything drawn is a filled rectangle: a stretched copy (blit) of a 1x1 coloured image. That
//! works whatever pixel format the game's screen uses, so nothing here has to know it. Lines are
//! thin rectangles, digits are made of small ones.
//!
//! Split: this file is the part that knows no graphics API (settings, the character scan, boxes,
//! bones, the menu pictures, the hook setting). `vulkan` draws through Vulkan (GRB_vulkan.exe),
//! `dx11` through DirectX 11 (GRB.exe). The game exe's own import table picks one, once (`api`).
//! The name `vkdraw` stayed so the menu needs no changes.

mod bonetrace;
mod dx11;
// P path: bones from each body's own current pose.
mod ppath;
mod vulkan;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aurora_core::proto::EspStyle;
use retour::RawDetour;
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible};
use windows_sys::core::BOOL;

use crate::entities::safe_read;
use crate::{entities, log, memory};
use bonetrace::{LayoutStep, Tried};

/// The graphics API the game draws with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Api {
    Vulkan,
    Dx11,
}

/// Picked once, from the names the game exe imports: GRB_vulkan.exe imports vulkan-1.dll, GRB.exe
/// imports d3d11.dll (grbstatic, build 25349072). Not by "is vulkan-1.dll loaded": an overlay can
/// load it into the DX11 game.
fn pick_api(imports: &[String]) -> Option<Api> {
    let has = |n: &str| imports.iter().any(|i| i.eq_ignore_ascii_case(n));
    if has("vulkan-1.dll") {
        Some(Api::Vulkan)
    } else if has("d3d11.dll") {
        Some(Api::Dx11)
    } else {
        None
    }
}

/// The dll names in a PE image's import table, read through `read` (address, buffer -> bytes read).
/// Empty when the headers don't look right.
fn import_names(read: &dyn Fn(u64, &mut [u8]) -> usize, base: u64) -> Vec<String> {
    let u32_at = |a: u64| {
        let mut b = [0u8; 4];
        (read(a, &mut b) == 4).then(|| u32::from_le_bytes(b))
    };
    let mut out = Vec::new();
    let Some(nt) = u32_at(base + 0x3c).map(|o| base + u64::from(o)) else { return out };
    // "PE\0\0", then the optional header 24 bytes on; 0x20b is a 64-bit one.
    let mut magic = [0u8; 2];
    if u32_at(nt) != Some(0x4550) || read(nt + 24, &mut magic) != 2 || u16::from_le_bytes(magic) != 0x20b {
        return out;
    }
    // Data directory 1 (imports) at optional header + 112 + 8.
    let Some(rva) = u32_at(nt + 24 + 120).filter(|r| *r != 0) else { return out };
    for i in 0..1024u64 {
        // Each import descriptor is 20 bytes; the dll name's address is at +12. An empty one ends
        // the list.
        let Some(name) = u32_at(base + u64::from(rva) + i * 20 + 12) else { break };
        if name == 0 {
            break;
        }
        let mut b = [0u8; 64];
        let got = read(base + u64::from(name), &mut b);
        let end = b[..got].iter().position(|&c| c == 0).unwrap_or(got);
        out.push(String::from_utf8_lossy(&b[..end]).into_owned());
    }
    out
}

fn api() -> Api {
    static API: std::sync::OnceLock<Api> = std::sync::OnceLock::new();
    *API.get_or_init(|| {
        let names = memory::find_module("").map(|m| import_names(&safe_read, m.base)).unwrap_or_default();
        match pick_api(&names) {
            Some(Api::Vulkan) => {
                log("draw: the game imports vulkan-1.dll, drawing through Vulkan");
                Api::Vulkan
            }
            Some(Api::Dx11) => {
                log("draw: the game imports d3d11.dll, drawing through DirectX 11");
                Api::Dx11
            }
            // As before.
            None => {
                log(&format!("draw: neither vulkan-1.dll nor d3d11.dll in the game's {} imports, drawing through Vulkan", names.len()));
                Api::Vulkan
            }
        }
    })
}

/// Camera, in the player root (`globals::CAM_ROOT`): position as f64 at +0x1c0, right / forward /
/// up unit vectors as f32 at +0x1e0 / +0x1f0 / +0x200, and the vertical field of view in radians at
/// +0x1b0 (a guess that fits: 1.13 = 65 degrees).
const CAM_POS: u64 = 0x1c0;
const CAM_RIGHT: u64 = 0x1e0;
const CAM_FORWARD: u64 = 0x1f0;
const CAM_UP: u64 = 0x200;
const CAM_FOV: u64 = 0x1b0;
/// Body position offset.
const POS: u64 = 0x20;
/// Body flags: bit 0x4000 is set once the NPC is dead.
const FLAGS: u64 = 0x108;
const DEAD: u32 = 0x4000;
/// The game's own distances are 1.5 times ours, so a world unit is not a metre on its HUD.
const GAME_METRES: f64 = 1.5;
/// How tall a box is, in metres above the body position.
const HEIGHT: f64 = 1.8;
const SIDE_ENEMY: i32 = 3;
const SIDE_FRIENDLY: i32 = 0;

/// Colours, in the order of the 1x1 images: enemy, friendly, NPC, player, target, shadow, drone.
fn colors(style: &EspStyle) -> [[f32; 4]; COLOURS] {
    let rgba = |c: [u8; 3]| [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, 1.0];
    let mut out = [[0.0, 0.0, 0.0, 1.0]; COLOURS];
    for (o, c) in out.iter_mut().zip(style.colors) {
        *o = rgba(c);
    }
    out[C_DRONE] = rgba(style.drone);
    out
}

/// How far boxes reach, in world units (the settings are in the game's metres).
fn radius(style: &EspStyle) -> f64 {
    (f64::from(style.range_m) / GAME_METRES).clamp(10.0, 1000.0)
}
const C_ENEMY: usize = 0;
const C_FRIENDLY: usize = 1;
const C_NPC: usize = 2;
const C_PLAYER: usize = 3;
const C_TARGET: usize = 4;
const C_SHADOW: usize = 5;
/// Drones.
const C_DRONE: usize = 6;
/// How many colours (1x1 images) there are.
const COLOURS: usize = 7;
/// The order the colours are drawn in: shadows first, so the colours land on top.
const DRAW_ORDER: [usize; COLOURS] = [C_SHADOW, C_ENEMY, C_FRIENDLY, C_NPC, C_PLAYER, C_TARGET, C_DRONE];

/// The in-game menu's picture can be at most this big (it is copied 1:1 into the frame).
const MENU_W: u32 = 2048;
const MENU_H: u32 = 1536;

/// The menu picture to copy into every frame, and where (top-left, in screen pixels).
pub struct MenuPic {
    pub generation: u64,
    pub frame: aurora_core::menuview::Frame,
    pub x: i32,
    pub y: i32,
}

/// Slot 0 is the menu, slot 1 the toast, slot 2 the typing box, slot 3 the mouse pointer, drawn
/// last so it sits on top.
pub const MENU: usize = 0;
pub const TOAST: usize = 1;
pub const INPUT: usize = 2;
pub const CURSOR: usize = 3;
pub const PIC_SLOTS: usize = 4;
static PICS: Mutex<[Option<std::sync::Arc<MenuPic>>; PIC_SLOTS]> = Mutex::new([None, None, None, None]);

/// The most a slot's picture may be. The pointer's is small, so its image costs next to nothing on
/// the graphics card.
pub(crate) fn slot_size(slot: usize) -> (u32, u32) {
    if slot == CURSOR { (128, 128) } else { (MENU_W, MENU_H) }
}
/// The last frame size we drew into.
static LAST_W: AtomicU32 = AtomicU32::new(0);
static LAST_H: AtomicU32 = AtomicU32::new(0);

/// Show (Some) or hide (None) the menu or the toast.
pub fn set_pic(slot: usize, pic: Option<std::sync::Arc<MenuPic>>) {
    if let Ok(mut m) = PICS.lock() {
        m[slot] = pic;
    }
}

/// The game's picture size: the last frame we drew into, else its window.
pub fn screen() -> Option<(i32, i32)> {
    let (w, h) = (LAST_W.load(Ordering::Relaxed), LAST_H.load(Ordering::Relaxed));
    if w > 0 && h > 0 {
        return Some((w as i32, h as i32));
    }
    let hwnd = find_window()?;
    // SAFETY: plain user32 query on a live window.
    let mut r: RECT = unsafe { std::mem::zeroed() };
    (unsafe { GetClientRect(hwnd, &mut r) } != 0 && r.right >= 64 && r.bottom >= 64).then_some((r.right, r.bottom))
}

pub fn config() -> Config {
    CONFIG.lock().map(|c| *c).unwrap_or_default()
}

/// What the ESP draws (set from the menu and profiles; defaults at every inject).
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub on: bool,
    pub hide_dead: bool,
    pub hostile: bool,
    pub friendly: bool,
    pub players: bool,
    /// Drones as diamonds.
    pub drones: bool,
    /// The picked target's body (Target > Pick), drawn white.
    pub target: u64,
    pub style: EspStyle,
}

struct Seen {
    body: u64,
    /// The character object that pointed at the body, and the class it must have: checked every
    /// frame, so a freed or reused one draws no ghost box.
    owner: u64,
    tag: u64,
    player: bool,
    side: Option<i32>,
    /// The object holding the body's bone poses (see `find_bones`); 0 when not found.
    bones: u64,
    /// P path: the body's animation owner (`ppath::anim_owner`); 0 when none.
    anim: u64,
}

static CONFIG: Mutex<Config> =
    Mutex::new(Config { on: false, hide_dead: true, hostile: true, friendly: true, players: true, drones: true, target: 0, style: EspStyle::DEFAULT });
static SEEN: Mutex<Vec<Seen>> = Mutex::new(Vec::new());
/// Drones from the world list, nearest first, set by the scan thread.
static DRONES: Mutex<Vec<entities::Drone>> = Mutex::new(Vec::new());
/// Diamonds drawn since the last heartbeat line.
static DRONES_DRAWN: AtomicU64 = AtomicU64::new(0);

/// Our detours, and (DirectX 11 only) each target's first bytes right after ours went in, to see at
/// unload whether something hooked on top of us.
struct Hooks(Vec<RawDetour>, Vec<(usize, [u8; 16])>);
// SAFETY: the detours are only enabled, disabled and dropped under the HOOKS lock.
unsafe impl Send for Hooks {}
static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);
/// Threads inside one of our hooks right now; unload waits for 0.
static IN_HOOK: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static SCANNER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static EXE: AtomicU64 = AtomicU64::new(0);

/// Presents seen since hooking, and when the hooks went in: no presents means the game calls a
/// present we did not hook, and the window tries again a few seconds later.
static PRESENTS: AtomicU64 = AtomicU64::new(0);
static HOOKED_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// Said once in the log that the menu's image could not be made.
static MENU_FAILED: AtomicBool = AtomicBool::new(false);
/// Frames not drawn because their swapchain lacks the transfer flag.
static NO_SAFE: AtomicU64 = AtomicU64::new(0);
/// Broken once: stop drawing instead of failing every frame.
static BROKEN: AtomicBool = AtomicBool::new(false);
/// Frames we drew into, for the heartbeat line.
static DRAWN: AtomicU64 = AtomicU64::new(0);
/// Why characters got no box (stale, no position, dead, too far, filtered, off screen), and the
/// last camera/me read, for the heartbeat.
static SKIPS: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
/// The player's T target in the last frame: 0 drawn, 1..6 skipped for SKIPS[n - 1], 9 not in the
/// scan list, for finding why a marked NPC has no ESP.
static TARGET_STATE: AtomicU64 = AtomicU64::new(9);
static NO_VIEW: AtomicU64 = AtomicU64::new(0);
/// Frames with boxes we still skipped: image unknown, slot busy.
static NO_IMAGE: AtomicU64 = AtomicU64::new(0);
static BUSY: AtomicU64 = AtomicU64::new(0);
/// Where the scanner is (1 waiting, 2 finding the player, 3 scanning, 4 sorting sides, 5 storing)
/// and when it last finished, for the heartbeat.
static STAGE: AtomicU32 = AtomicU32::new(0);
static LAST_SCAN: Mutex<Option<Instant>> = Mutex::new(None);
const QUIET_AFTER_JUMP: Duration = Duration::from_secs(10);

/// Turn the in-game ESP on or off, hooking the game on first use.
pub fn set(cfg: Config) -> Result<(), String> {
    if let Ok(mut c) = CONFIG.lock() {
        *c = cfg;
    }
    if !cfg.on {
        return Ok(());
    }
    ensure_hooks()
}

/// Where to hook the drawing: the `Hook:` line of the Launchpad's Settings.txt. Auto takes the
/// first function in the chain (the Steam overlay's when it is on, else the GPU driver's), Driver
/// skips every layer, Off never hooks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HookAt {
    Auto,
    Driver,
    Off,
}

/// Read once, at inject: a change in the launchpad needs a new inject.
fn hook_at() -> HookAt {
    static AT: std::sync::OnceLock<HookAt> = std::sync::OnceLock::new();
    *AT.get_or_init(|| {
        // The Launchpad's file: only read here, once.
        let text = std::fs::read_to_string(aurora_core::paths::launchpad_settings()).unwrap_or_default();
        let at = match aurora_core::line_value(&text, aurora_core::HOOK_LINE).map(str::to_ascii_lowercase).as_deref() {
            Some("driver") => HookAt::Driver,
            Some("off") => HookAt::Off,
            _ => HookAt::Auto,
        };
        log(&format!("vkdraw: hook setting {at:?}"));
        at
    })
}

/// One line in `%APPDATA%\Aurora\Bin\hook.txt` for the launchpad's Options: where we hooked.
fn hook_note(line: &str) {
    let p = aurora_core::paths::hook_note();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, line);
}

/// At inject: take the setting and say so in hook.txt (the hooks go in when the menu or the ESP
/// first shows), so an old line from the last inject never shows.
pub fn hook_start() {
    // Vulkan or DirectX 11, picked (and logged) now.
    api();
    hook_note(match hook_at() {
        HookAt::Off => "Off",
        _ => "Not yet (hooks when the menu or ESP first shows)",
    });
}

/// Hook the game's present call once (the ESP and the menu both draw through it).
pub fn ensure_hooks() -> Result<(), String> {
    if hook_at() == HookAt::Off {
        return Err("in-game drawing is off (launchpad > Options)".into());
    }
    if BROKEN.load(Ordering::SeqCst) {
        return Err("in-game drawing stopped after an error (see dll.log)".into());
    }
    let mut hooks = HOOKS.lock().map_err(|_| "hook lock poisoned")?;
    if hooks.is_none() {
        let made = match api() {
            Api::Vulkan => vulkan::install(),
            Api::Dx11 => {
                // The first failure is kept until the next inject: each try makes a new device and
                // swapchain inside the game (a hitch).
                static FAILED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
                if let Some(e) = FAILED.get() {
                    return Err(e.clone());
                }
                dx11::install().inspect_err(|e| {
                    let _ = FAILED.set(e.clone());
                })
            }
        };
        *hooks = Some(made.inspect_err(|e| hook_note(&format!("Failed: {e}")))?);
        if let Ok(mut t) = HOOKED_AT.lock() {
            *t = Some(Instant::now());
        }
        STOP.store(false, Ordering::SeqCst);
        if let Ok(mut s) = SCANNER.lock() {
            *s = Some(std::thread::spawn(scanner));
        }
    }
    drop(hooks);
    let since = HOOKED_AT.lock().ok().and_then(|t| *t).map(|t| t.elapsed()).unwrap_or_default();
    if since > Duration::from_secs(3) {
        if PRESENTS.load(Ordering::SeqCst) == 0 {
            return Err("no frames came through the hook".into());
        }
        if api() == Api::Vulkan && vulkan::DEVICE.load(Ordering::SeqCst) == 0 {
            return Err("frames came through, but the game's device was never seen".into());
        }
    }
    Ok(())
}

/// Something (an overlay) changed the start of a function we hooked after we did: taking our hook
/// out would cut it off, and its jump back would land in our freed code. DirectX 11 only; Vulkan
/// unloads as before.
fn hooked_over(hooks: &Hooks) -> Option<String> {
    hooks.1.iter().find_map(|&(at, ours)| {
        let mut now = [0u8; 16];
        (safe_read(at as u64, &mut now) != 16 || now != ours).then(|| module_of(at))
    })
}

/// May the mod unload now? Err says why not (another program hooked on top of ours).
pub fn can_unload() -> Result<(), String> {
    let hooks = HOOKS.lock().map_err(|_| "hook lock poisoned")?;
    match hooks.as_ref().and_then(hooked_over) {
        Some(at) => Err(format!("can't unload: another program hooked the game's present call after Aurora ({at}). Aurora stays loaded; close the game to remove it")),
        None => Ok(()),
    }
}

/// Unhook and free everything. Must run before the DLL unloads. False: something hooked on top of
/// ours, so the hooks stay in and the DLL must stay loaded.
pub fn shutdown() -> bool {
    if let Ok(mut c) = CONFIG.lock() {
        c.on = false;
    }
    set_pic(MENU, None);
    set_pic(TOAST, None);
    set_pic(CURSOR, None);
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = SCANNER.lock().ok().and_then(|mut s| s.take()) {
        let _ = h.join();
    }
    crate::posehook::off();
    let Some(hooks) = HOOKS.lock().ok().and_then(|mut h| h.take()) else { return true };
    if let Some(at) = hooked_over(&hooks) {
        log(&format!("draw: something hooked {at} after us; our hooks stay in and the DLL stays loaded"));
        if let Ok(mut h) = HOOKS.lock() {
            *h = Some(hooks);
        }
        return false;
    }
    for d in &hooks.0 {
        // SAFETY: our own detours, still alive.
        let _ = unsafe { d.disable() };
    }
    let t = Instant::now();
    while IN_HOOK.load(Ordering::SeqCst) != 0 && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // The last frames we touched are still on their way to the screen.
    std::thread::sleep(Duration::from_millis(150));
    match api() {
        Api::Vulkan => vulkan::release(),
        Api::Dx11 => dx11::release(),
    }
    drop(hooks);
    log(match api() {
        Api::Vulkan => "vkdraw: unhooked",
        Api::Dx11 => "dx11: unhooked",
    });
    true
}

/// The heartbeat, so a crash log shows whether frames still came and whether we drew. `tag` starts
/// the line ("vkdraw" or "dx11").
fn heartbeat(tag: &str, n: u64) {
    let sk: Vec<u64> = SKIPS.iter().map(|s| s.swap(0, Ordering::Relaxed)).collect();
    let seen = SEEN.lock().map(|s| s.len()).unwrap_or(0);
    // Bones drawn / boxes drawn / total, then the bone trace counts.
    let (ok, miss) = (BONES_OK.swap(0, Ordering::Relaxed), BONES_MISS.swap(0, Ordering::Relaxed));
    log(&format!(
        "{tag}: {n} frames, {} drawn, {} unsafe, {seen} seen, skipped {sk:?}, drones drawn {} (listed {}), {}{}, no view {}, no image {}, busy {}, scan stage {}, last scan {}{}",
        DRAWN.load(Ordering::Relaxed),
        NO_SAFE.swap(0, Ordering::Relaxed),
        DRONES_DRAWN.swap(0, Ordering::Relaxed),
        DRONES.lock().map(|d| d.len()).unwrap_or(0),
        bonetrace::bones_text(ok, miss),
        // P path: its counts right after the bones.
        ppath::heartbeat_text(),
        NO_VIEW.swap(0, Ordering::Relaxed),
        NO_IMAGE.swap(0, Ordering::Relaxed),
        BUSY.swap(0, Ordering::Relaxed),
        STAGE.load(Ordering::Relaxed),
        LAST_SCAN.lock().ok().and_then(|t| *t).map_or("never".into(), |t| format!("{} s ago", t.elapsed().as_secs())),
        bonetrace::heartbeat_tail()
    ));
    if crate::entities::target().is_some() {
        let why = ["drawn", "owner changed", "no position", "dead", "out of range", "side off", "off screen"];
        let st = TARGET_STATE.load(Ordering::Relaxed) as usize;
        log(&format!("target: {}", why.get(st).copied().unwrap_or("not in the scan list")));
    }
    if let Ok(mut w) = BONE_WHY.lock()
        && !w.is_empty()
    {
        log(&format!("bones missed: {:?}", std::mem::take(&mut *w)));
    }
}

fn module_of(addr: usize) -> String {
    memory::modules()
        .into_iter()
        .find(|m| (addr as u64) >= m.base && (addr as u64) < m.base + m.size)
        .map(|m| format!("{}+{:#x}", m.name, addr as u64 - m.base))
        .unwrap_or_else(|| format!("{addr:#x}"))
}

/// The dll an address is in.
fn module_name(addr: usize) -> Option<String> {
    memory::modules()
        .into_iter()
        .find(|m| (addr as u64) >= m.base && (addr as u64) < m.base + m.size)
        .map(|m| m.name)
}

/// The game's biggest visible window.
pub(crate) fn find_window() -> Option<HWND> {
    struct Best(HWND, i64);
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        // SAFETY: lp is the &mut Best passed below; user32 calls on a live handle.
        unsafe {
            let best = &mut *(lp as *mut Best);
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                let mut r: RECT = std::mem::zeroed();
                if GetClientRect(hwnd, &mut r) != 0 {
                    let area = i64::from(r.right) * i64::from(r.bottom);
                    if area > best.1 {
                        *best = Best(hwnd, area);
                    }
                }
            }
        }
        1
    }
    let mut best = Best(std::ptr::null_mut(), 0);
    // SAFETY: the callback only runs during this call and only touches `best`.
    unsafe { EnumWindows(Some(each), &mut best as *mut Best as LPARAM) };
    (!best.0.is_null()).then_some(best.0)
}

fn rd_u64(addr: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    (safe_read(addr, &mut b) == 8).then(|| u64::from_le_bytes(b))
}

fn rd_f64x3(addr: u64) -> Option<[f64; 3]> {
    let mut b = [0u8; 24];
    if safe_read(addr, &mut b) != 24 {
        return None;
    }
    let f = |i: usize| f64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let v = [f(0), f(8), f(16)];
    v.iter().all(|x| x.is_finite() && x.abs() < 1.0e5).then_some(v)
}

fn rd_f32x3(addr: u64) -> Option<[f64; 3]> {
    let mut b = [0u8; 12];
    if safe_read(addr, &mut b) != 12 {
        return None;
    }
    let f = |i: usize| f64::from(f32::from_le_bytes(b[i..i + 4].try_into().unwrap()));
    let v = [f(0), f(4), f(8)];
    v.iter().all(|x| x.is_finite()).then_some(v)
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Rectangles to fill, per colour, clipped to the screen.
fn boxes(cfg: &Config, w: i32, h: i32) -> [Vec<[i32; 4]>; COLOURS] {
    let mut out: [Vec<[i32; 4]>; COLOURS] = Default::default();
    let exe = EXE.load(Ordering::Relaxed);
    let Some(root) = crate::globals::CAM_ROOT.rva().and_then(|r| rd_u64(exe + r)).filter(|r| is_ptr(*r)) else {
        NO_VIEW.fetch_add(1, Ordering::Relaxed);
        return out;
    };
    let (Some(cam), Some(right), Some(fwd), Some(up)) =
        (rd_f64x3(root + CAM_POS), rd_f32x3(root + CAM_RIGHT), rd_f32x3(root + CAM_FORWARD), rd_f32x3(root + CAM_UP))
    else {
        return out;
    };
    let Some(me) = crate::globals::BODY_ROOT.rva().and_then(|r| rd_u64(exe + r)).filter(|b| is_ptr(*b)).and_then(|b| rd_f64x3(b + POS)) else {
        NO_VIEW.fetch_add(1, Ordering::Relaxed);
        return out;
    };
    let mut fb = [0u8; 4];
    let tan_half = (cam_fov(root) / 2.0).tan();
    let (wf, hf) = (f64::from(w), f64::from(h));
    let aspect = wf / hf;
    let project = |p: [f64; 3]| -> Option<(f64, f64)> {
        let d = [p[0] - cam[0], p[1] - cam[1], p[2] - cam[2]];
        let z = dot(d, fwd);
        if z < 0.5 {
            return None;
        }
        let x = dot(d, right) / z / (tan_half * aspect);
        let y = dot(d, up) / z / tan_half;
        Some((wf / 2.0 * (1.0 + x), hf / 2.0 * (1.0 - y)))
    };
    let mut push = |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| {
        let r = [(x0.round() as i32).clamp(0, w), (y0.round() as i32).clamp(0, h), (x1.round() as i32).clamp(0, w), (y1.round() as i32).clamp(0, h)];
        if r[2] > r[0] && r[3] > r[1] {
            out[c].push(r);
        }
    };
    // Lines scale a little with the screen (2 px at 1440p), unless the player set a width.
    let t = match cfg.style.width {
        0 => (hf / 720.0).max(1.0).round(),
        n => f64::from(n),
    };
    // The black edge.
    let edge = cfg.style.edge;
    let seen = SEEN.lock();
    let Ok(seen) = seen else { return Default::default() };
    if !seen.iter().any(|s| s.body == cfg.target) {
        TARGET_STATE.store(9, Ordering::Relaxed);
    }
    // Rectangles the head rings asked for this frame.
    let mut ring_rects = 0usize;
    // The heads drawn this frame, for Silent Aim.
    let mut heads: Vec<(u64, [f64; 3])> = Vec::new();
    for s in seen.iter() {
        if s.body == cfg.target {
            TARGET_STATE.store(0, Ordering::Relaxed);
        }
        // Ghost boxes after a teleport: the character object must still be the same kind and still
        // point at this body.
        let mut ob = [0u8; 16];
        if safe_read(s.owner, &mut ob) != 16
            || u64::from_le_bytes(ob[0..8].try_into().unwrap()) != s.tag
            || u64::from_le_bytes(ob[8..16].try_into().unwrap()) != s.body
        {
            SKIPS[0].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(1, Ordering::Relaxed);
            }
            continue;
        }
        let Some(p) = rd_f64x3(s.body + POS) else {
            SKIPS[1].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(2, Ordering::Relaxed);
            }
            continue;
        };
        if cfg.hide_dead && safe_read(s.body + FLAGS, &mut fb) == 4 && u32::from_le_bytes(fb) & DEAD != 0 {
            SKIPS[2].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(3, Ordering::Relaxed);
            }
            continue;
        }
        let dist = ((p[0] - me[0]).powi(2) + (p[1] - me[1]).powi(2) + (p[2] - me[2]).powi(2)).sqrt();
        if !(1.0..=radius(&cfg.style)).contains(&dist) {
            SKIPS[3].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(4, Ordering::Relaxed);
            }
            continue;
        }
        if (s.player && !cfg.players) || (s.side == Some(SIDE_ENEMY) && !cfg.hostile) || (s.side == Some(SIDE_FRIENDLY) && !cfg.friendly) {
            SKIPS[4].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(5, Ordering::Relaxed);
            }
            continue;
        }
        let (Some(feet), Some(head)) = (project(p), project([p[0], p[1], p[2] + HEIGHT])) else {
            SKIPS[5].fetch_add(1, Ordering::Relaxed);
            if s.body == cfg.target {
                TARGET_STATE.store(6, Ordering::Relaxed);
            }
            continue;
        };
        let c = if cfg.target == s.body {
            C_TARGET
        } else if s.player {
            C_PLAYER
        } else {
            match s.side {
                Some(SIDE_ENEMY) => C_ENEMY,
                Some(SIDE_FRIENDLY) => C_FRIENDLY,
                _ => C_NPC,
            }
        };
        let tall = (feet.1 - head.1).max(4.0);
        let half = tall * 0.25;
        let (x0, x1, y0, y1) = (head.0 - half, head.0 + half, head.1, feet.1);
        if x1 < 0.0 || x0 > wf || y1 < 0.0 || y0 > hf {
            continue;
        }
        // The ring's centre from the Head bone when the P path drew this body; None: placed as
        // before.
        let mut mid = None;
        // Bones where they can be read, else the box.
        let skeleton = if cfg.style.bones {
            // No bone object: the full rig's list instead.
            let mut pick = None;
            // P path: first the body's own current pose; this route only when that fails.
            let own = ppath::skeleton(s.anim, s.body, p, &mut pick, &mut mid);
            let sk = ppath::or_old_route(own, || if s.bones == 0 { rig_skeleton(p, s.body, &mut pick) } else { skeleton(s.bones, s.body, p) });
            // The bone trace notes the result.
            let sk = match sk {
                Ok(v) => {
                    bonetrace::frame(s.body, s.tag, p, dist, None, pick.as_ref());
                    Some(v)
                }
                Err(why) => {
                    bonetrace::frame(s.body, s.tag, p, dist, Some(&why), None);
                    bone_miss(s.tag, why, dist);
                    None
                }
            };
            if sk.is_some() { &BONES_OK } else { &BONES_MISS }.fetch_add(1, Ordering::Relaxed);
            sk
        } else {
            None
        };
        if let Some(sk) = skeleton {
            // Kept as the head's offset from the body.
            if let Some(m) = head_mid(sk[CHEST], sk[HEAD], mid) {
                heads.push((s.body, [m[0] - p[0], m[1] - p[1], m[2] - p[2]]));
            }
            // A ring around the head, same colour, lines and edge, while this frame's ring budget
            // lasts.
            let ring = if ring_rects < RING_RECTS_MAX { head_ring(&project, up, sk[CHEST], sk[HEAD], mid, (wf, hf)) } else { None };
            for (c2, g) in [(C_SHADOW, 1.0), (c, 0.0)] {
                if c2 == C_SHADOW && !edge {
                    continue;
                }
                for (a, b) in BONE_LINES {
                    if let (Some(pa), Some(pb)) = (project(sk[a]), project(sk[b])) {
                        // With a ring the chest -> head line stops at its edge, and is left out
                        // when the chest is inside the ring on screen.
                        let end = match ring {
                            Some(r) if (a, b) == (CHEST, HEAD) => r.edge_towards(pa),
                            _ => Some(pb),
                        };
                        if let Some(pb) = end {
                            line(&mut push, c2, pa, pb, t + 2.0 * g);
                        }
                    }
                }
                if let Some(r) = ring {
                    ring_rects += r.draw(&mut push, c2, t + 2.0 * g);
                }
            }
        } else {
            // A thin black edge around each line keeps them readable on bright ground.
            for (c2, g) in [(C_SHADOW, 1.0), (c, 0.0)] {
                if c2 == C_SHADOW && !edge {
                    continue;
                }
                push(c2, x0 - g, y0 - g, x1 + g, y0 + t + g);
                push(c2, x0 - g, y1 - t - g, x1 + g, y1 + g);
                push(c2, x0 - g, y0 - g, x0 + t + g, y1 + g);
                push(c2, x1 - t - g, y0 - g, x1 + g, y1 + g);
            }
        }
        if cfg.style.labels {
            label(&mut push, c, &format!("{:.0}m", dist * GAME_METRES), (x0 + x1) / 2.0, y1 + 2.0 * t, t, edge);
        }
    }
    drop(seen);
    if let Ok(mut h) = HEADS.lock() {
        *h = (Some(Instant::now()), heads);
    }
    // Drones: a diamond each, after the people.
    if cfg.drones {
        draw_drones(&mut push, &project, up, me, cfg, t, (wf, hf));
    }
    out
}

/// A label in the pixel font, centred on `cx` with its top at `top`, each font pixel `px` screen
/// pixels; with the black edge, a shadow one font pixel down and right.
fn label(push: &mut impl FnMut(usize, f64, f64, f64, f64), c: usize, text: &str, cx: f64, top: f64, px: f64, edge: bool) {
    let width: f64 = text.chars().map(|ch| f64::from(glyph(ch).0 + 1)).sum::<f64>() * px;
    let mut x = cx - width / 2.0;
    for ch in text.chars() {
        let (gw, rows) = glyph(ch);
        for (row, bits) in rows.iter().enumerate() {
            // Runs of lit pixels become one rectangle each.
            let mut col = 0;
            while col < gw {
                if bits & (1 << (gw - 1 - col)) == 0 {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < gw && bits & (1 << (gw - 1 - col)) != 0 {
                    col += 1;
                }
                let (rx0, ry0) = (x + f64::from(start) * px, top + row as f64 * px);
                let (rx1, ry1) = (x + f64::from(col) * px, ry0 + px);
                if edge {
                    push(C_SHADOW, rx0 + px, ry0 + px, rx1 + px, ry1 + px);
                }
                push(c, rx0, ry0, rx1, ry1);
            }
        }
        x += f64::from(gw + 1) * px;
    }
}

/// Drones: a diamond around each one in the Drones colour with the black edge, the distance under
/// it as for people. Nearest first, until `DIAMOND_RECTS_MAX`.
fn draw_drones(
    push: &mut impl FnMut(usize, f64, f64, f64, f64),
    project: &impl Fn([f64; 3]) -> Option<(f64, f64)>,
    up: [f64; 3],
    me: [f64; 3],
    cfg: &Config,
    t: f64,
    screen: (f64, f64),
) {
    let list = DRONES.lock().map(|d| d.clone()).unwrap_or_default();
    let (mut rects, mut drawn) = (0usize, 0u64);
    for d in &list {
        if rects >= DIAMOND_RECTS_MAX {
            break;
        }
        let Some(now) = entities::drone_at(d) else { continue };
        // Shot down: the game keeps it listed, marked dead.
        if cfg.hide_dead && now.dead {
            continue;
        }
        let p = now.mid;
        let dist = ((p[0] - me[0]).powi(2) + (p[1] - me[1]).powi(2) + (p[2] - me[2]).powi(2)).sqrt();
        if !(1.0..=radius(&cfg.style)).contains(&dist) {
            continue;
        }
        let Some(dm) = diamond(project, up, p, now.half, screen) else { continue };
        for (c2, g) in [(C_SHADOW, 1.0), (C_DRONE, 0.0)] {
            if c2 == C_SHADOW && !cfg.style.edge {
                continue;
            }
            rects += dm.draw(push, c2, t + 2.0 * g);
        }
        if cfg.style.labels {
            label(push, C_DRONE, &format!("{:.0}m", dist * GAME_METRES), dm.at.0, dm.at.1 + dm.r + 2.0 * t, t, cfg.style.edge);
        }
        drawn += 1;
    }
    DRONES_DRAWN.fetch_add(drawn, Ordering::Relaxed);
}

/// The smallest diamond on screen (middle to corner, pixels), so far drones still show.
const DIAMOND_MIN_PX: f64 = 5.0;
/// Rectangles the diamonds may add to one frame, both colours together.
const DIAMOND_RECTS_MAX: usize = 8192;

/// A drone's diamond on screen: its middle, and the distance from there to each corner (px).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Diamond {
    at: (f64, f64),
    r: f64,
}

/// Where a drone's diamond goes: the middle `mid` on screen, the corners `half` world units from it
/// (measured along the camera's up, as the head ring; `entities::DroneNow`), at least
/// `DIAMOND_MIN_PX`. None when it is behind the camera, off screen, bigger than half the screen, or
/// not a finite number.
fn diamond(project: &impl Fn([f64; 3]) -> Option<(f64, f64)>, up: [f64; 3], mid: [f64; 3], half: f64, (w, h): (f64, f64)) -> Option<Diamond> {
    let at = project(mid)?;
    let edge = project([0, 1, 2].map(|k| mid[k] + up[k] * half))?;
    let r = (edge.0 - at.0).hypot(edge.1 - at.1).max(DIAMOND_MIN_PX);
    if ![at.0, at.1, r].iter().all(|v| v.is_finite()) || r > h / 2.0 || at.0 + r < 0.0 || at.0 - r > w || at.1 + r < 0.0 || at.1 - r > h {
        return None;
    }
    Some(Diamond { at, r })
}

impl Diamond {
    /// The corners, from the top round to the left.
    fn corners(&self) -> [(f64, f64); 4] {
        let (x, y, r) = (self.at.0, self.at.1, self.r);
        [(x, y - r), (x + r, y), (x, y + r), (x - r, y)]
    }

    /// The outline as rectangles of thickness `t`, through `push`: each side half a pixel past its
    /// corners so they close, as the head ring's. Returns how many it pushed.
    fn draw(&self, push: &mut impl FnMut(usize, f64, f64, f64, f64), c: usize, t: f64) -> usize {
        let mut n = 0;
        let mut counted = |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| {
            n += 1;
            push(c, x0, y0, x1, y1);
        };
        let pts = self.corners();
        for (k, &a) in pts.iter().enumerate() {
            let b = pts[(k + 1) % pts.len()];
            let len = (b.0 - a.0).hypot(b.1 - a.1);
            let u = ((b.0 - a.0) / len * 0.5, (b.1 - a.1) / len * 0.5);
            line(&mut counted, c, (a.0 - u.0, a.1 - u.1), (b.0 + u.0, b.1 + u.1), t);
        }
        n
    }
}

/// Bones: the character object +0x60 -> +0x230 (on most NPCs; `find_bones` searches) is an object
/// whose +0x10 is the body; its +0x308 holds 44 bone poses, 0x30 each (position x y z w, rotation
/// quaternion, scale), in the body's own space: x right, y forward, z up, metres from the feet.
/// About 20 are in use; the rest stay at 0.
const BONE_COUNT: usize = 18;
const BONE_SIZE: usize = 0x30;
/// The body's facing: three rows of four floats at body+0x40 (a turn about the up axis).
const BODY_ROT: u64 = 0x40;
/// Which bones join, from that NPC's pose: pelvis 0, spine 9, chest 10, head 15, shoulders 13 14,
/// elbows 7 8, hands 3 4, hips 16 17, knees 5 6, ankles 1 2, toes 11 12. No feet: the player only
/// wants legs, arms, torso and head.
const BONE_LINES: [(usize, usize); 15] = [
    (0, 9), (9, 10), (10, 15),
    (10, 13), (13, 7), (7, 3),
    (10, 14), (14, 8), (8, 4),
    (0, 16), (16, 5), (5, 1),
    (0, 17), (17, 6), (6, 2),
];
/// The chest and the head in that list.
const CHEST: usize = 10;
const HEAD: usize = 15;

/// The ring around the head in Bones style. On a full rig (almost every skeleton drawn) bone 15 is
/// the top of the face block, the highest head point `rig_layout` finds, so the middle of the head
/// is below it. The ring's centre goes this far from bone 15 along the chest -> head line, so it
/// tilts with the head (world units, which are metres on the bodies; minus is back towards the
/// chest). From the game's Male 184 skeleton: the head joint is 16 cm under the crown, the middle
/// of the head 11 cm under it, so the face's top point is some 4 to 11 cm above the middle. The
/// rare 44-bone list's bone 15 may sit lower (not checked). To be tuned live. With bones by name
/// (`BONES_BY_NAME`) bone 15 is the top of the head, 0.14 above the Head joint: this puts the
/// centre about 6 cm above the joint, 1 cm behind it.
const HEAD_RING_OFFSET: f64 = -0.08;
/// The ring's radius in world units: about half a head's width, so it doesn't cover the whole head.
const HEAD_RING_RADIUS: f64 = 0.08;
/// A ring smaller than this on screen (radius, pixels) is drawn as a dot.
const RING_MIN_PX: f64 = 2.5;
/// Sides of the ring: few on far rings, more on big ones (see `ring_sides`).
const RING_SIDES_MIN: usize = 8;
const RING_SIDES_MAX: usize = 24;
/// Rectangles the rings may add to one frame, both colours together. Past it the rest of the
/// frame's skeletons (nearest come first) get no ring: rings are dropped, never bones.
const RING_RECTS_MAX: usize = 8192;

/// Kinds of character whose bones could not be read, and why, logged once each.
static BONE_MISSES: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

fn bone_miss(tag: u64, why: String, dist: f64) {
    // Why close bodies miss, by kind of reason, for the heartbeat line.
    if let Ok(mut c) = BONE_WHY.lock() {
        // The pelvis height itself says why (lying down, or another body's pose).
        let kind: String = if why.starts_with("pelvis") {
            why.clone()
        } else {
            why.chars().take_while(|ch| !ch.is_ascii_digit() && *ch != '-').collect()
        };
        // In the game's metres since (were world units labelled "<30m", "60m+").
        *c.entry(format!("{} {}", bonetrace::band(dist), kind.trim())).or_default() += 1;
    }
    let exe = EXE.load(Ordering::Relaxed);
    if let Ok(mut m) = BONE_MISSES.lock()
        && m.len() < 40
        && !m.iter().any(|e| e.0 == tag && e.1 == why)
    {
        log(&format!("bones: class exe+{:#x}: {why}", tag.wrapping_sub(exe)));
        m.push((tag, why));
    }
}

/// Bodies drawn with bones and with the box instead, for the heartbeat line.
static BONES_OK: AtomicU64 = AtomicU64::new(0);
static BONES_MISS: AtomicU64 = AtomicU64::new(0);
static BONE_WHY: Mutex<std::collections::BTreeMap<String, u32>> = Mutex::new(std::collections::BTreeMap::new());

/// Bone objects found per (character object, body), so each is looked for once.
type BoneObjs = std::collections::HashMap<(u64, u64), (u64, Instant)>;
static BONE_OBJS: Mutex<Option<BoneObjs>> = Mutex::new(None);

/// The object whose +0x10 is the body and +0x308 its poses. Most NPCs have it at character +0x60 ->
/// +0x230, but the +0x230 slot differs between NPCs, so two levels under the character object are
/// searched.
fn find_bones(owner: u64, body: u64) -> u64 {
    // Not found is tried again after 3 s: a body is often met before its bones are set up.
    if let Some(&(b, t)) = BONE_OBJS.lock().ok().as_ref().and_then(|m| m.as_ref()?.get(&(owner, body)))
        && (b != 0 || t.elapsed() < Duration::from_secs(3))
    {
        return b;
    }
    let fits = |x: u64| is_ptr(x) && x.checked_add(0x10).and_then(rd_u64) == Some(body) && x.checked_add(0x308).and_then(rd_u64).is_some_and(|p| is_ptr(p) && pose_like(p));
    let words = |at: u64, len: usize| {
        let mut b = vec![0u8; len];
        let got = safe_read(at, &mut b);
        b[..got].as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect::<Vec<u64>>()
    };
    let mut found = 0;
    'search: for p1 in words(owner, 0x200).into_iter().filter(|v| is_ptr(*v)) {
        for x in words(p1, 0x800) {
            if fits(x) {
                found = x;
                break 'search;
            }
        }
    }
    if let Ok(mut m) = BONE_OBJS.lock() {
        let m = m.get_or_insert_with(Default::default);
        if m.len() > 4000 {
            m.clear();
        }
        // Not found is remembered too, until the list is cleared.
        m.insert((owner, body), (found, Instant::now()));
    }
    found
}

/// Does a standing pose list start here: a pelvis 0.3 to 1.4 m up, a rotation of length 1 and a
/// scale of 1. (A second list of the same body starts with empty poses.)
fn pose_like(p: u64) -> bool {
    let mut b = [0u8; BONE_SIZE];
    if safe_read(p, &mut b) != BONE_SIZE {
        return false;
    }
    let f = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let q = (0..4).map(|k| f(0x10 + 4 * k).powi(2)).sum::<f32>();
    (0.3..1.4).contains(&f(8)) && (0.98..1.02).contains(&q) && (0..3).all(|k| (f(0x20 + 4 * k) - 1.0).abs() < 0.01)
}

/// The first bones of a body in the world, or why they can't be read or look wrong.
fn skeleton(obj: u64, body: u64, p: [f64; 3]) -> Result<[[f64; 3]; BONE_COUNT], String> {
    if obj == 0 {
        return Err("no bone object found".into());
    }
    if obj.checked_add(0x10).and_then(rd_u64) != Some(body) {
        return Err("bone object moved".into());
    }
    let poses = obj.checked_add(0x308).and_then(rd_u64).filter(|v| is_ptr(*v) && pose_like(*v)).ok_or("no poses")?;
    let mut b = [0u8; BONE_COUNT * BONE_SIZE];
    if safe_read(poses, &mut b) != b.len() {
        return Err("poses unreadable".into());
    }
    let f = |i: usize| f64::from(f32::from_le_bytes(b[i..i + 4].try_into().unwrap()));
    let mut local = [[0.0; 3]; BONE_COUNT];
    for (i, l) in local.iter_mut().enumerate() {
        *l = [f(i * BONE_SIZE), f(i * BONE_SIZE + 4), f(i * BONE_SIZE + 8)];
    }
    place(body, p, &local)
}

/// Full-body poses, for NPCs without the bone object: the game's pose function (`POSE_FN`) gets a
/// pose object in r8 (noted by `posehook`). The object starts with its world position (three f64,
/// the same as its body's to 3 cm) and holds at +0x248 its list: one entry per bone of the full rig
/// (431 and 456 on the NPCs checked, face included), 0x20 each (position x y z w, rotation), in
/// body space. So the objects are taken every few seconds and matched to bodies by position: no
/// guessing whose list it is (the earlier pointer search drew neighbours' and resting poses). A
/// body can have two: the final pose and a layer under it that stops when the NPC plays something
/// else; the one that changed last is drawn.
const POSE_FN: &str = "48 89 5C 24 08 57 48 83 EC 30 49 8B 80 40 02 00 00 49 8B B8 48 02 00 00 4D 8B 81 48 02 00 00";
/// The list pointer in the pose object.
const POSE_LIST: u64 = 0x248;
/// A pose object belongs to the body it stands on (metres; same spot to 3 cm when checked).
const POSE_NEAR: f64 = 0.5;
const RIG_SIZE: usize = 0x20;
/// Entries read when working out a rig's layout (the largest seen had 456).
const RIG_READ: usize = 640;
/// P path: false = old route only.
const P_PATH: bool = true;
/// Bones by name on the P path: each skeleton's 18 bones looked up by name in its bone table
/// (`ppath`), the head point the top of the head from the Head bone. False = by lengths
/// (`rig_layout`) for every skeleton.
const BONES_BY_NAME: bool = true;
/// The head ring's centre from the Head bone on the P path: the middle of the head in the Head
/// bone's own frame (`ppath`), so it turns with the head. False = from bone 15 as before
/// (`HEAD_RING_OFFSET`).
const HEAD_RING_FROM_HEAD: bool = true;

/// One caught pose: its object and list, which entry is each of the 18 bones, and when its pelvis,
/// hands or head last moved (`None`: not seen moving yet).
struct Rig {
    obj: u64,
    list: u64,
    idx: [usize; BONE_COUNT],
    last: [u8; 0x60],
    moved: Option<Instant>,
    /// Length of each limb bone when caught: bones never stretch, so a frame whose lengths differ
    /// was read mid-write or the list now holds something else.
    lens: [f64; LIMB_LINES.len()],
    /// The last frame that passed, drawn instead of a torn one; and torn frames in a row.
    good: Option<[[f64; 3]; BONE_COUNT]>,
    bad: u32,
}

/// Torn frames in a row before a pose is dropped (caught again on the next pass if real).
const RIG_BAD_MAX: u32 = 30;
/// How much a bone may seem to stretch before the frame counts as torn (metres).
const RIG_STRETCH: f64 = 0.04;

/// Upper arms, forearms, thighs and shins: one joint each, so their length never changes (the torso
/// lines span several joints and do change when bending).
const LIMB_LINES: [(usize, usize); 8] = [(13, 7), (7, 3), (14, 8), (8, 4), (16, 5), (5, 1), (17, 6), (6, 2)];

fn line_lens(pts: &[[f64; 3]; BONE_COUNT]) -> [f64; LIMB_LINES.len()] {
    LIMB_LINES.map(|(a, b)| (0..3).map(|k| (pts[a][k] - pts[b][k]).powi(2)).sum::<f64>().sqrt())
}

/// The 18 bones of a list, in body space.
fn rig_local(list: u64, idx: &[usize; BONE_COUNT]) -> Option<[[f64; 3]; BONE_COUNT]> {
    rig_local_with(&safe_read, list, idx)
}

/// The same through `read`.
fn rig_local_with(read: &dyn Fn(u64, &mut [u8]) -> usize, list: u64, idx: &[usize; BONE_COUNT]) -> Option<[[f64; 3]; BONE_COUNT]> {
    let mut local = [[0.0; 3]; BONE_COUNT];
    for (l, &i) in local.iter_mut().zip(idx) {
        let mut b = [0u8; 12];
        if read(list + (i * RIG_SIZE) as u64, &mut b) != 12 {
            return None;
        }
        *l = [0, 4, 8].map(|k| f64::from(f32::from_le_bytes(b[k..k + 4].try_into().unwrap())));
    }
    Some(local)
}

static POSES: Mutex<Vec<Rig>> = Mutex::new(Vec::new());
/// Where `POSE_FN` is, once found (0: not looked for, 1: not found).
static POSE_FN_AT: AtomicU64 = AtomicU64::new(0);

/// Which entry is which bone, worked out from one pose by bone lengths, since the order differs
/// between rigs (the right leg was entry 20 on one and 24 on the other): pelvis twice at 0 and 1,
/// left leg 2 to 6, the right hip found by matching the left leg's lengths, then a spine of four
/// short pieces leading to the left arm. The right arm is the left arm moved past the face: its
/// shoulder and elbow have the same repeated (twist) entries as the left's, the same distance
/// further on. The head is the top of the face block, the point farthest up the spine within 40 cm
/// of the chest. Err names the check that failed.
fn rig_layout(t: &[[f32; 3]]) -> Result<[usize; BONE_COUNT], LayoutStep> {
    let n = t.len();
    let d = |a: usize, b: usize| (0..3).map(|k| (t[a][k] - t[b][k]).powi(2)).sum::<f32>().sqrt();
    let same = |a: usize, b: usize| d(a, b) < 0.002;
    let seg = |a: usize, b: usize, lo: f32, hi: f32| (lo..hi).contains(&d(a, b));
    if n < 200 {
        return Err(LayoutStep::Short);
    }
    if !same(0, 1) || !(0.2..1.4).contains(&t[1][2]) {
        return Err(LayoutStep::Pelvis);
    }
    let (thigh, shin) = (d(2, 3), d(3, 4));
    if !(0.3..0.55).contains(&thigh) || !(0.3..0.55).contains(&shin) || !seg(1, 2, 0.04, 0.15) {
        return Err(LayoutStep::LegLengths);
    }
    let j = (7..80)
        .find(|&j| d(j, 2) > 0.1 && (d(j, 1) - d(2, 1)).abs() < 0.02 && (d(j, j + 1) - thigh).abs() < 0.02 && (d(j + 1, j + 2) - shin).abs() < 0.02)
        .ok_or(LayoutStep::RightLeg)?;
    let s = (j + 5..n - 7).find(|&s| {
        d(s, 1) < 0.2
            && seg(s, s + 1, 0.1, 0.25)
            && seg(s + 1, s + 2, 0.1, 0.25)
            && seg(s + 2, s + 3, 0.1, 0.25)
            && seg(s + 3, s + 4, 0.1, 0.25)
            && seg(s + 4, s + 5, 0.22, 0.38)
            && seg(s + 5, s + 6, 0.2, 0.34)
    })
    .ok_or(LayoutStep::Spine)?;
    let ts = (s + 7..n).find(|&k| same(k, s + 4)).ok_or(LayoutStep::Twist)?;
    let te = (s + 7..n).find(|&k| same(k, s + 5)).ok_or(LayoutStep::Twist)?;
    let r = (40..n)
        .find(|&r| {
            s + 6 + r < n && ts + r < n && te + r < n && same(s + 4 + r, ts + r) && same(s + 5 + r, te + r) && seg(s + 4 + r, s + 5 + r, 0.22, 0.38) && seg(s + 5 + r, s + 6 + r, 0.2, 0.34)
        })
        .ok_or(LayoutStep::RightArm)?;
    let up = [0, 1, 2].map(|k| t[s + 3][k] - t[s][k]);
    let len = up.iter().map(|v| v * v).sum::<f32>().sqrt().max(0.01);
    let along = |k: usize| (0..3).map(|c| (t[k][c] - t[s + 3][c]) * up[c] / len).sum::<f32>();
    let h = (s + 7..s + 3 + r).filter(|&k| d(k, s + 3) < 0.4).max_by(|&a, &b| along(a).total_cmp(&along(b))).ok_or(LayoutStep::Head)?;
    if along(h) < 0.12 {
        return Err(LayoutStep::Head);
    }
    // BONE_LINES order: pelvis, ankles, hands, knees, elbows, spine, chest, toes, shoulders, head,
    // hips.
    Ok([1, 4, j + 2, s + 6, s + 6 + r, 3, j + 1, s + 5, s + 5 + r, s + 1, s + 3, 6, j + 4, s + 4, s + 4 + r, h, 2, j])
}

/// Positions of the first `RIG_READ` entries of a list, as read now.
fn rig_points(list: u64) -> Vec<[f32; 3]> {
    let mut b = vec![0u8; RIG_READ * RIG_SIZE];
    let n = safe_read(list, &mut b) / RIG_SIZE;
    (0..n).map(|i| [0, 4, 8].map(|k| f32::from_le_bytes(b[i * RIG_SIZE + k..i * RIG_SIZE + k + 4].try_into().unwrap()))).collect()
}

/// Takes the pose objects the hook noted and keeps the full-body ones.
fn harvest_poses() {
    let at = match POSE_FN_AT.load(Ordering::Relaxed) {
        0 => {
            let found = (|| {
                let exe = crate::memory::find_module("")?;
                crate::memory::find_code(&exe, "pose function", POSE_FN).inspect_err(|e| log(&format!("poses: {e}"))).ok()
            })()
            .unwrap_or(1);
            POSE_FN_AT.store(found, Ordering::Relaxed);
            found
        }
        v => v,
    };
    if at == 1 {
        return;
    }
    if let Err(e) = crate::posehook::set(at, true) {
        log(&format!("poses: {e}"));
        POSE_FN_AT.store(1, Ordering::Relaxed);
        return;
    }
    let objs = crate::posehook::objects();
    let calls = crate::posehook::calls();
    let Ok(mut poses) = POSES.lock() else { return };
    // What this pass did, for the bone trace's harvest line.
    let mut h = bonetrace::Harvest::default();
    // Forget objects that no longer hold the same list (freed or reused).
    poses.retain(|r| {
        if r.bad >= RIG_BAD_MAX {
            h.torn += 1;
            return false;
        }
        if r.obj.checked_add(POSE_LIST).and_then(rd_u64) != Some(r.list) {
            h.moved += 1;
            return false;
        }
        true
    });
    for &obj in &objs {
        if !is_ptr(obj) {
            continue;
        }
        if poses.iter().any(|r| r.obj == obj) {
            h.tried.insert(obj, Tried::Rig);
            continue;
        }
        let Some(list) = obj.checked_add(POSE_LIST).and_then(rd_u64).filter(|v| is_ptr(*v)) else {
            h.tried.insert(obj, Tried::NoList);
            continue;
        };
        match rig_layout(&rig_points(list)) {
            Ok(idx) => match rig_local(list, &idx) {
                Some(pts) => {
                    poses.push(Rig { obj, list, idx, last: [0; 0x60], moved: None, lens: line_lens(&pts), good: None, bad: 0 });
                    h.added += 1;
                    h.tried.insert(obj, Tried::Rig);
                }
                None => {
                    h.tried.insert(obj, Tried::Unreadable);
                }
            },
            Err(step) => {
                h.tried.insert(obj, Tried::Fail(step));
            }
        }
    }
    let rigs = poses.len();
    drop(poses);
    bonetrace::harvested(&objs, calls, h, rigs);
}

/// The 18 bones from the pose standing on this body that moved last, in the world. `pick` gets the
/// rig chosen and when it last moved.
fn rig_skeleton(p: [f64; 3], body: u64, pick: &mut Option<bonetrace::Pick>) -> Result<[[f64; 3]; BONE_COUNT], String> {
    let mut poses = POSES.lock().map_err(|_| "pose lock")?;
    let mut best: Option<(Instant, usize)> = None;
    let mut near = false;
    for (n, r) in poses.iter_mut().enumerate() {
        let mut w = [0u8; 24];
        if safe_read(r.obj, &mut w) != 24 {
            continue;
        }
        let at = [0, 8, 16].map(|k| f64::from_le_bytes(w[k..k + 8].try_into().unwrap()));
        if (0..3).map(|k| (at[k] - p[k]).powi(2)).sum::<f64>().sqrt() > POSE_NEAR {
            continue;
        }
        near = true;
        // Pelvis, a hand and the head: a standing NPC can keep its pelvis still.
        let mut now = [0u8; 0x60];
        let ok = [0, 3, 15].iter().enumerate().all(|(n, &b)| safe_read(r.list + (r.idx[b] * RIG_SIZE) as u64, &mut now[n * 0x20..n * 0x20 + 0x20]) == 0x20);
        if ok && now != r.last {
            if r.last != [0; 0x60] {
                r.moved = Some(Instant::now());
            }
            r.last = now;
        }
        if let Some(m) = r.moved
            && best.is_none_or(|b| m > b.0)
        {
            best = Some((m, n));
        }
    }
    let (m, n) = best.ok_or(if near { "pose not moving" } else { "no pose caught" })?;
    let r = &mut poses[n];
    *pick = Some(bonetrace::Pick { obj: r.obj, moved: m });
    // Odd shapes now and then: a frame read while the game writes it, or a list taken over by
    // another rig. Bones never stretch; check that.
    let fresh = rig_local(r.list, &r.idx).filter(|l| line_lens(l).iter().zip(&r.lens).all(|(a, b)| (a - b).abs() < RIG_STRETCH));
    let local = match fresh {
        Some(l) => {
            r.good = Some(l);
            r.bad = 0;
            l
        }
        None => {
            r.bad += 1;
            r.good.ok_or("pose torn")?
        }
    };
    drop(poses);
    place(body, p, &local)
}

/// Body-space bones to world: the body's facing turns them, its position moves them.
fn place(body: u64, p: [f64; 3], local: &[[f64; 3]; BONE_COUNT]) -> Result<[[f64; 3]; BONE_COUNT], String> {
    place_with(body, p, local, None).map(|(w, _)| w)
}

/// Bones in the world, and the one more point placed with them (`place_with`).
type Placed = ([[f64; 3]; BONE_COUNT], Option<[f64; 3]>);

/// The same, with one more body-space point turned and moved like them. It comes back None when it
/// is not finite or over 2.5 m out.
fn place_with(body: u64, p: [f64; 3], local: &[[f64; 3]; BONE_COUNT], more: Option<[f64; 3]>) -> Result<Placed, String> {
    let mut r = [0u8; 48];
    if safe_read(body + BODY_ROT, &mut r) != 48 {
        return Err("facing unreadable".into());
    }
    let f = |buf: &[u8], i: usize| f64::from(f32::from_le_bytes(buf[i..i + 4].try_into().unwrap()));
    let row = |n: usize| [f(&r, n * 16), f(&r, n * 16 + 4), f(&r, n * 16 + 8)];
    place_rows([row(0), row(1), row(2)], p, local, more)
}

/// `place_with` once the facing's three rows are read.
fn place_rows(rows: [[f64; 3]; 3], p: [f64; 3], local: &[[f64; 3]; BONE_COUNT], more: Option<[f64; 3]>) -> Result<Placed, String> {
    if rows.iter().flatten().any(|v| !v.is_finite() || v.abs() > 1.01) {
        return Err("facing is not a turn".into());
    }
    let turn = |l: [f64; 3]| [0, 1, 2].map(|k| p[k] + l[0] * rows[0][k] + l[1] * rows[1][k] + l[2] * rows[2][k]);
    let mut out = [[0.0; 3]; BONE_COUNT];
    for (i, (o, &l)) in out.iter_mut().zip(local).enumerate() {
        if l.iter().any(|v| !v.is_finite() || v.abs() > 2.5) {
            return Err(format!("bone {i} is {:.2} {:.2} {:.2}", l[0], l[1], l[2]));
        }
        *o = turn(l);
    }
    // The pelvis sits near the body: low when prone or sitting, never metres off; anything else is
    // not this layout.
    let hip = out[0][2] - p[2];
    if !(-0.3..1.6).contains(&hip) {
        return Err(format!("pelvis {hip:.2} m up"));
    }
    Ok((out, more.filter(|l| l.iter().all(|v| v.is_finite() && v.abs() <= 2.5)).map(turn)))
}

/// A line of thickness `t` as screen rectangles: one per run of rows (or columns) at the same
/// place, so a steep line costs about as many rectangles as it is wide.
fn line(push: &mut impl FnMut(usize, f64, f64, f64, f64), c: usize, a: (f64, f64), b: (f64, f64), t: f64) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let half = t / 2.0;
    if dy.abs() >= dx.abs() {
        let (a, b) = if a.1 <= b.1 { (a, b) } else { (b, a) };
        let steps = (b.1 - a.1).ceil().max(1.0);
        let mut start = a.1;
        let mut x = a.0.round();
        let mut y = a.1;
        while y < b.1 {
            let ny = (y + 1.0).min(b.1);
            let nx = (a.0 + (b.0 - a.0) * ((ny - a.1) / steps.max(b.1 - a.1))).round();
            if nx != x || ny >= b.1 {
                push(c, x - half, start - half, x + half, ny + half);
                start = ny;
                x = nx;
            }
            y = ny;
        }
    } else {
        let (a, b) = if a.0 <= b.0 { (a, b) } else { (b, a) };
        let mut start = a.0;
        let mut y = a.1.round();
        let mut x = a.0;
        while x < b.0 {
            let nx = (x + 1.0).min(b.0);
            let ny = (a.1 + (b.1 - a.1) * ((nx - a.0) / (b.0 - a.0))).round();
            if ny != y || nx >= b.0 {
                push(c, start - half, y - half, nx + half, y + half);
                start = nx;
                y = ny;
            }
            x = nx;
        }
    }
}

/// The ring around a head in Bones style: its centre on screen and its radius, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ring {
    at: (f64, f64),
    r: f64,
}

/// The middle of a head: `centre` when the P path gave one, else `HEAD_RING_OFFSET` from the head
/// bone along the chest -> head line. None when the bones are not finite or too close.
fn head_mid(chest: [f64; 3], head: [f64; 3], centre: Option<[f64; 3]>) -> Option<[f64; 3]> {
    if let Some(c) = centre {
        return Some(c);
    }
    let d = [0, 1, 2].map(|k| head[k] - chest[k]);
    let len = dot(d, d).sqrt();
    if !len.is_finite() || len < 0.01 {
        return None;
    }
    Some([0, 1, 2].map(|k| head[k] + d[k] / len * HEAD_RING_OFFSET))
}

/// The heads of the bodies drawn with bones in the last frame: when, and each body's head middle
/// (`head_mid`) as an offset from its body position.
static HEADS: Mutex<Heads> = Mutex::new((None, Vec::new()));
type Heads = (Option<Instant>, Vec<(u64, [f64; 3])>);
/// A head offset older than this is not used (the pose may have changed: crouch, prone).
const HEAD_FRESH: Duration = Duration::from_millis(500);

/// Body `body`'s head middle minus its body position, from the last frame the ESP drew its bones,
/// if that frame is fresh (Silent Aim adds it to the body's position now; none with the ESP off or
/// in Box style). Was the head itself, up to 250 ms old, until a moving soldier's head was behind
/// where it is.
pub fn head_offset(body: u64) -> Option<[f64; 3]> {
    let h = HEADS.lock().ok()?;
    if h.0.is_none_or(|t| t.elapsed() > HEAD_FRESH) {
        return None;
    }
    h.1.iter().find(|(b, _)| *b == body).map(|(_, p)| *p).filter(|p| p.iter().all(|v| v.is_finite() && v.abs() < 3.0))
}

/// The camera's vertical field of view (radians) at `root`; 65 degrees when it reads wrong. Live,
/// so scoping in narrows it.
fn cam_fov(root: u64) -> f64 {
    let mut fb = [0u8; 4];
    let fov = if safe_read(root + CAM_FOV, &mut fb) == 4 { f64::from(f32::from_le_bytes(fb)) } else { 0.0 };
    if (0.01..2.5).contains(&fov) { fov } else { 65f64.to_radians() }
}

/// The Silent Aim circle's radius in pixels: a cone `aim_deg` wide (full angle) round the camera's
/// forward, on a picture `h` pixels tall whose vertical field of view is `cam_fov` radians. Pixels
/// are square here (x and y use the same scale), so it is a true circle; whatever is drawn inside
/// it is inside the cone Silent Aim picks from.
pub fn circle_px(aim_deg: f64, cam_fov: f64, h: f64) -> f64 {
    (aim_deg.to_radians() / 2.0).tan() / (cam_fov / 2.0).tan() * (h / 2.0)
}

/// Want the drawing hooks for the Silent Aim circle (even with the ESP off).
pub fn want_circle() {
    if let Err(e) = ensure_hooks() {
        log(&format!("silent aim circle: {e}"));
    }
}

/// The Silent Aim circle round the crosshair, when Silent Aim is on and its circle shown.
pub(crate) fn circle_rects(w: i32, h: i32) -> [Vec<[i32; 4]>; COLOURS] {
    let mut out: [Vec<[i32; 4]>; COLOURS] = Default::default();
    let Some(deg) = crate::silentaim::circle_deg() else { return out };
    let exe = EXE.load(Ordering::Relaxed);
    let Some(root) = crate::globals::CAM_ROOT.rva().and_then(|r| rd_u64(exe + r)).filter(|r| is_ptr(*r)) else {
        return out;
    };
    let (wf, hf) = (f64::from(w), f64::from(h));
    let r = circle_px(deg, cam_fov(root), hf);
    if !r.is_finite() || r < 2.0 || r > wf.max(hf) {
        return out;
    }
    let style = config().style;
    let edge = style.edge;
    let t = match style.width {
        0 => (hf / 720.0).max(1.0).round(),
        n => f64::from(n),
    };
    let mut push = |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| {
        let q = [(x0.round() as i32).clamp(0, w), (y0.round() as i32).clamp(0, h), (x1.round() as i32).clamp(0, w), (y1.round() as i32).clamp(0, h)];
        if q[2] > q[0] && q[3] > q[1] {
            out[c].push(q);
        }
    };
    for (c, g) in [(C_SHADOW, 1.0), (C_TARGET, 0.0)] {
        if c == C_SHADOW && !edge {
            continue;
        }
        for (a, b) in circle_points((wf / 2.0, hf / 2.0), r).windows(2).map(|p| (p[0], p[1])) {
            line(&mut push, c, a, b, t + 2.0 * g);
        }
    }
    out
}

/// Points round a circle, closed (the last is the first), enough for a smooth line.
fn circle_points(c: (f64, f64), r: f64) -> Vec<(f64, f64)> {
    let n = ((r / 3.0).round() as usize).clamp(32, 256);
    (0..=n).map(|i| {
        let a = std::f64::consts::TAU * i as f64 / n as f64;
        (c.0 + r * a.cos(), c.1 + r * a.sin())
    }).collect()
}

/// `rects` with the Silent Aim circle's added.
pub(crate) fn with_circle(mut rects: [Vec<[i32; 4]>; COLOURS], w: i32, h: i32) -> [Vec<[i32; 4]>; COLOURS] {
    for (all, more) in rects.iter_mut().zip(circle_rects(w, h)) {
        all.extend(more);
    }
    rects
}

/// Is there anything for the drawing hook to draw besides the menu (ESP or the circle)?
pub(crate) fn drawing(cfg: &Config) -> bool {
    cfg.on || crate::silentaim::circle_deg().is_some()
}

/// Where a head's ring goes. The centre is `centre` when the P path gave one (the middle of the
/// head from the Head bone, `HEAD_RING_FROM_HEAD`), else `HEAD_RING_OFFSET` from the head along the
/// chest -> head line; the size comes from a point `HEAD_RING_RADIUS` away along the camera's up,
/// so it shrinks with distance like the bones. None when the head is behind the camera, off screen,
/// taller than the screen, or anything is not a finite number.
fn head_ring(
    project: &impl Fn([f64; 3]) -> Option<(f64, f64)>,
    up: [f64; 3],
    chest: [f64; 3],
    head: [f64; 3],
    centre: Option<[f64; 3]>,
    (w, h): (f64, f64),
) -> Option<Ring> {
    let mid = head_mid(chest, head, centre)?;
    let at = project(mid)?;
    let edge = project([0, 1, 2].map(|k| mid[k] + up[k] * HEAD_RING_RADIUS))?;
    let r = (edge.0 - at.0).hypot(edge.1 - at.1);
    if ![at.0, at.1, r].iter().all(|v| v.is_finite()) || r > h / 2.0 || at.0 + r < 0.0 || at.0 - r > w || at.1 + r < 0.0 || at.1 - r > h {
        return None;
    }
    Some(Ring { at, r })
}

impl Ring {
    /// Where a line from `from` towards the centre meets the ring; None when `from` is inside it.
    fn edge_towards(&self, from: (f64, f64)) -> Option<(f64, f64)> {
        let (dx, dy) = (self.at.0 - from.0, self.at.1 - from.1);
        let d = dx.hypot(dy);
        (d > self.r).then(|| (self.at.0 - dx / d * self.r, self.at.1 - dy / d * self.r))
    }

    /// The ring as rectangles of thickness `t`, through `push`: a closed outline of `ring_sides`
    /// lines, each half a pixel past its corners, or one dot when it is tiny. Returns how many it
    /// pushed.
    fn draw(&self, push: &mut impl FnMut(usize, f64, f64, f64, f64), c: usize, t: f64) -> usize {
        let mut n = 0;
        let mut counted = |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| {
            n += 1;
            push(c, x0, y0, x1, y1);
        };
        if self.r < RING_MIN_PX {
            let s = self.r + t / 2.0;
            counted(c, self.at.0 - s, self.at.1 - s, self.at.0 + s, self.at.1 + s);
        } else {
            let pts = ring_points(self.at, self.r);
            for (k, &a) in pts.iter().enumerate() {
                // Each side half a pixel longer at both ends, so the sides overlap at the corners:
                // 1 px rings had holes there.
                let b = pts[(k + 1) % pts.len()];
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                let u = ((b.0 - a.0) / len * 0.5, (b.1 - a.1) / len * 0.5);
                line(&mut counted, c, (a.0 - u.0, a.1 - u.1), (b.0 + u.0, b.1 + u.1), t);
            }
        }
        n
    }
}

/// The ring's corners: `ring_sides(r)` points evenly around the centre, the first straight up.
/// Joined in order, and the last back to the first, they close the ring.
fn ring_points(at: (f64, f64), r: f64) -> Vec<(f64, f64)> {
    let n = ring_sides(r);
    (0..n)
        .map(|k| {
            let a = std::f64::consts::TAU * k as f64 / n as f64;
            (at.0 + r * a.sin(), at.1 - r * a.cos())
        })
        .collect()
}

/// More sides on bigger rings, so the corners stay within about half a pixel of a true circle up to
/// 60 px (one pixel at 120 px), and few on far ones: 8 to 24, a multiple of 4 so the ring looks the
/// same from every side.
fn ring_sides(r: f64) -> usize {
    // NaN and negative radii count as 0, huge ones end on the most.
    let n = (std::f64::consts::PI * r.max(0.0).sqrt()).ceil();
    (n.clamp(RING_SIDES_MIN as f64, RING_SIDES_MAX as f64) as usize).div_ceil(4) * 4
}

/// A tiny pixel font: width and five rows, bit set = lit, leftmost pixel highest.
fn glyph(c: char) -> (u32, [u8; 5]) {
    match c {
        '0' => (3, [0b111, 0b101, 0b101, 0b101, 0b111]),
        '1' => (3, [0b010, 0b110, 0b010, 0b010, 0b111]),
        '2' => (3, [0b111, 0b001, 0b111, 0b100, 0b111]),
        '3' => (3, [0b111, 0b001, 0b111, 0b001, 0b111]),
        '4' => (3, [0b101, 0b101, 0b111, 0b001, 0b001]),
        '5' => (3, [0b111, 0b100, 0b111, 0b001, 0b111]),
        '6' => (3, [0b111, 0b100, 0b111, 0b101, 0b111]),
        '7' => (3, [0b111, 0b001, 0b001, 0b001, 0b001]),
        '8' => (3, [0b111, 0b101, 0b111, 0b101, 0b111]),
        '9' => (3, [0b111, 0b101, 0b111, 0b001, 0b111]),
        'm' => (5, [0b00000, 0b11110, 0b10101, 0b10101, 0b10101]),
        _ => (2, [0; 5]),
    }
}

/// Who is around, kept going: a panic in one scan must not end ESP for the session.
fn scanner() {
    while !STOP.load(Ordering::SeqCst) {
        if std::panic::catch_unwind(scan_loop).is_err() {
            log("vkdraw: the character scan crashed, starting it again");
            // The drones too.
            forget_seen();
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Drop the list when it can't be refreshed: stale entries only hide real ones.
fn forget_seen() {
    if let Ok(mut s) = SEEN.lock()
        && !s.is_empty()
    {
        s.clear();
    }
    if let Ok(mut d) = DRONES.lock() {
        d.clear();
    }
}

/// Instant ESP: how often the game's world list is read. A new body shows within about this, plus
/// one frame.
const WORLD_EVERY: Duration = Duration::from_millis(200);
/// While the world list works, the marker scan still runs this often while a character has no
/// marker from the rule (the scan's marker, and so a side, is taken for it), and a full one every
/// `SLOW_FULL` (a check: it logs bodies the list did not give). Before: every second, a full one
/// every 10 s.
const SLOW_SCAN: Duration = Duration::from_secs(5);
const SLOW_FULL: Duration = Duration::from_secs(30);
/// A body's side, bone object and animation owner are read again this often (each scan did it,
/// every second, before), and at once for a new body.
const REFRESH: Duration = Duration::from_secs(1);

/// A line about the world list going on or off: at most 20 per run of the scan, so a list that
/// flips all the time can't fill the log.
fn world_line(lines: &mut u32, line: &str) {
    if *lines < 20 {
        *lines += 1;
        log(line);
        if *lines == 20 {
            log("vkdraw: the world list went on and off too often, no more lines about it");
        }
    }
}

/// Kept per (body, marker) between passes.
struct Extra {
    side: Option<i32>,
    bones: u64,
    anim: u64,
    at: Instant,
    cfg_bones: bool,
}

/// The ESP's list from the bodies found (the marker in `owner`), with the class check as before.
/// Side, bone object and animation owner come from `extras`; new or older than `REFRESH`: read now.
fn to_seen(list: &[entities::Body], extras: &mut std::collections::HashMap<(u64, u64), Extra>) -> Vec<Seen> {
    let cfg_bones = config().style.bones;
    let classes = [entities::marker_class(false), entities::marker_class(true)];
    let seen = list
        .iter()
        .filter_map(|b| {
            // The class it must have, not whatever it holds now: freed objects had their new first
            // bytes taken as the class and passed every frame.
            let tag = classes[usize::from(b.player)]?;
            // Its class and its body, like every frame checks.
            let mut ob = [0u8; 16];
            if b.owner == 0
                || safe_read(b.owner, &mut ob) != 16
                || u64::from_le_bytes(ob[0..8].try_into().unwrap()) != tag
                || u64::from_le_bytes(ob[8..16].try_into().unwrap()) != b.body
            {
                return None;
            }
            let key = (b.body, b.owner);
            if extras.get(&key).is_none_or(|x| x.at.elapsed() >= REFRESH || x.cfg_bones != cfg_bones) {
                let x = Extra {
                    side: if b.player { None } else { side_of(b.owner) },
                    bones: if cfg_bones { find_bones(b.owner, b.body) } else { 0 },
                    // P path: found here, checked again every frame.
                    anim: if cfg_bones && P_PATH { ppath::anim_owner(&safe_read, b.body).unwrap_or(0) } else { 0 },
                    at: Instant::now(),
                    cfg_bones,
                };
                extras.insert(key, x);
            }
            let x = extras.get(&key)?;
            Some(Seen { body: b.body, owner: b.owner, tag, player: b.player, side: x.side, bones: x.bones, anim: x.anim })
        })
        .collect();
    let keep: std::collections::HashSet<(u64, u64)> = list.iter().map(|b| (b.body, b.owner)).collect();
    extras.retain(|k, _| keep.contains(k));
    seen
}

/// The character scan every second (full every 10 s), with each one's side. Instant ESP: with
/// `entities::WORLD_LIST` the bodies come from the game's world list every 200 ms. This scan is
/// then the fallback (list unreadable, empty or without characters), and else runs slowly (see
/// `SLOW_SCAN`).
fn scan_loop() {
    let mut said = String::new();
    let mut last_poses = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(60);
    let mut was: Option<([f64; 3], Instant)> = None;
    let mut tps = crate::tp::jumps();
    let mut quiet_until: Option<Instant> = None;
    // Instant ESP.
    let mut world = entities::WorldList::default();
    let mut last_world = Instant::now() - Duration::from_secs(60);
    let mut world_ok = false;
    let mut world_said = String::new();
    // The slow marker scan's bodies and markers, for characters the rule could not link.
    let mut scanned: std::collections::HashMap<u64, (u64, bool)> = std::collections::HashMap::new();
    let mut last_slow = Instant::now() - Duration::from_secs(60);
    let mut last_slow_full = Instant::now();
    let mut extras = std::collections::HashMap::new();
    let mut world_lines = 0u32;
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
        // The bone trace's game reads and lines.
        bonetrace::drain();
        STAGE.store(1, Ordering::Relaxed);
        let (on, reach) = CONFIG.lock().map(|c| (c.on, radius(&c.style))).unwrap_or((false, 0.0));
        if !on {
            continue;
        }
        // A teleport or fast travel: the list belongs to the old place. Drop it and search
        // everything again at once.
        STAGE.store(2, Ordering::Relaxed);
        let me = entities::player_pos();
        // Our own teleports say so. Else by speed, not distance: one pass can take a second or two
        // (pose catch, scan), and moving fast covered 50 m in that and blanked the ESP for 10 s.
        // Never while flying: fly goes up to 800 m/s.
        let tp_now = crate::tp::jumps();
        let jumped = tp_now != tps
            || (!crate::fly::is_on()
                && matches!((was, me), (Some((a, t)), Some(b)) if {
                    let d = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                    d > 50.0 && d / t.elapsed().as_secs_f64().max(0.05) > 150.0
                }));
        tps = tp_now;
        if let Some(b) = me {
            was = Some((b, Instant::now()));
        }
        // The game reloads around the player's right after a jump; every GPU reset seen so far came
        // then. Scan nothing (and so draw no boxes) until it has settled.
        if jumped {
            // The drones too: their diamonds kept drawing through the quiet time.
            forget_seen();
            // P path: the old place's poses and skeletons are gone too.
            ppath::forget();
            extras.clear();
            scanned.clear();
            quiet_until = Some(Instant::now() + QUIET_AFTER_JUMP);
            log("vkdraw: jump seen, quiet while the game loads");
            continue;
        }
        if me.is_none() {
            forget_seen();
            continue;
        }
        if quiet_until.is_some_and(|t| Instant::now() < t) {
            continue;
        }
        let settled = quiet_until.take().is_some();
        // Instant ESP: the game's world list, every 200 ms.
        let mut from_world = None;
        if entities::WORLD_LIST && (settled || last_world.elapsed() >= WORLD_EVERY) {
            last_world = Instant::now();
            STAGE.store(6, Ordering::Relaxed);
            match world.pass(reach, 120) {
                Ok(l) => {
                    if !world_ok {
                        world_line(&mut world_lines, &format!("vkdraw: bodies come from the game's world list ({} entries, {} characters)", world.last.entries, world.last.chars));
                    }
                    world_ok = true;
                    world_said.clear();
                    from_world = Some(l);
                }
                Err(e) => {
                    if world_ok || e != world_said {
                        world_line(&mut world_lines, &format!("vkdraw: world list not used ({e}), the marker scan finds the bodies"));
                    }
                    world_ok = false;
                    world_said = e;
                }
            }
            // Drones: this pass's, none while the list can't be used.
            if let Ok(mut d) = DRONES.lock() {
                d.clear();
                if world_ok {
                    d.extend_from_slice(world.drones());
                }
            }
            if let Some(line) = world.report() {
                log(&format!("vkdraw: {line}"));
            }
        }
        let beat = settled || last.elapsed() >= Duration::from_secs(1);
        if beat {
            last = Instant::now();
            // The animation level table: logged once, and again when a world load changes it.
            bonetrace::lod_table();
        }
        // While the world list works, only its pass gives the list (every 200 ms); else today's
        // scan does, every second.
        if from_world.is_none() && (world_ok || !beat) {
            continue;
        }
        let list = match from_world {
            Some(mut l) => {
                // The marker scan, slow now: every 5 s while a character has no marker from the
                // rule (the scan's marker for that body is then taken), a full one every 30 s.
                let unlinked = l.iter().any(|b| b.owner == 0);
                let full = last_slow_full.elapsed() >= SLOW_FULL;
                if full || (unlinked && last_slow.elapsed() >= SLOW_SCAN) {
                    last_slow = Instant::now();
                    if full {
                        last_slow_full = Instant::now();
                        entities::mark_full();
                    }
                    STAGE.store(3, Ordering::Relaxed);
                    if let Ok(found) = entities::near(reach, 120, full, true) {
                        // A check for the game tests: bodies the scan has and the list not.
                        let missing = found.iter().filter(|f| !l.iter().any(|b| b.body == f.body)).count();
                        if missing > 0 && l.len() < 120 && found.len() < 120 {
                            log(&format!("vkdraw: the marker scan found {missing} of its {} bodies that the world list did not give", found.len()));
                        }
                        scanned = found.into_iter().map(|b| (b.body, (b.owner, b.player))).collect();
                    }
                }
                // Characters the marker rule could not link: the marker the scan found for that
                // body, else no box (as before: no marker, no box).
                for b in l.iter_mut().filter(|b| b.owner == 0) {
                    if let Some(&(owner, player)) = scanned.get(&b.body) {
                        (b.owner, b.player) = (owner, player);
                    }
                }
                l.retain(|b| b.owner != 0);
                l
            }
            // The fallback: today's scan, every second, a full one every 10 s.
            None => {
                let full = settled || entities::full_due();
                if full {
                    entities::mark_full();
                }
                STAGE.store(3, Ordering::Relaxed);
                match entities::near(reach, 120, full, true) {
                    Ok(l) => l,
                    Err(e) => {
                        if e != said {
                            log(&format!("vkdraw: character scan failed: {e}"));
                            said = e;
                        }
                        forget_seen();
                        continue;
                    }
                }
            }
        };
        said.clear();
        STAGE.store(4, Ordering::Relaxed);
        let cfg_bones = config().style.bones;
        let seen = to_seen(&list, &mut extras);
        // Full-body poses for characters without the bone object, every 2 s.
        if !cfg_bones {
            crate::posehook::off();
        } else if last_poses.elapsed() > Duration::from_secs(2) {
            harvest_poses();
            last_poses = Instant::now();
        }
        STAGE.store(5, Ordering::Relaxed);
        if let Ok(mut s) = SEEN.lock() {
            *s = seen;
        }
        if let Ok(mut t) = LAST_SCAN.lock() {
            *t = Some(Instant::now());
        }
    }
}

/// A value that can be a user-space address. Garbage read from a freed object (huge numbers) made
/// `+ offset` overflow and the scan restart over and over.
fn is_ptr(v: u64) -> bool {
    v > 0x10000 && v < 0x7fff_ffff_0000
}

/// Character +0x60 -> +0x160 -> +0x104: 3 enemy, 0 friendly.
fn side_of(owner: u64) -> Option<i32> {
    let l1 = rd_u64(owner.checked_add(0x60)?).filter(|v| is_ptr(*v))?;
    let l2 = rd_u64(l1 + 0x160).filter(|v| is_ptr(*v))?;
    let mut b = [0u8; 4];
    (safe_read(l2 + 0x104, &mut b) == 4).then(|| i32::from_le_bytes(b)).filter(|v| (0..64).contains(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn api_from_imports() {
        // The two Steam exes' imports (grbstatic, build 25349072): both have d3d9 and AGS.
        let vulkan = names(&["KERNEL32.dll", "d3d9.dll", "amd_ags_x64.dll", "VULKAN-1.dll", "dinput8.dll"]);
        let dx11 = names(&["KERNEL32.dll", "d3d11.dll", "dxgi.dll", "d3d9.dll", "amd_ags_x64.dll", "dinput8.dll"]);
        assert_eq!(pick_api(&vulkan), Some(Api::Vulkan));
        assert_eq!(pick_api(&dx11), Some(Api::Dx11));
        assert_eq!(pick_api(&names(&["KERNEL32.dll", "d3d9.dll"])), None);
        assert_eq!(pick_api(&[]), None);
    }

    /// A tiny 64-bit PE image in a buffer with two imports.
    fn image() -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        b[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x98..0x9a].copy_from_slice(&0x20bu16.to_le_bytes());
        // Import directory at 0x200 (optional header 0x98 + 120).
        b[0x98 + 120..0x98 + 124].copy_from_slice(&0x200u32.to_le_bytes());
        for (i, (at, name)) in [(0x300usize, &b"d3d11.dll\0"[..]), (0x320, &b"KERNEL32.dll\0"[..])].into_iter().enumerate() {
            let d = 0x200 + i * 20;
            b[d + 12..d + 16].copy_from_slice(&(at as u32).to_le_bytes());
            b[at..at + name.len()].copy_from_slice(name);
        }
        b
    }

    fn reader(img: &[u8]) -> impl Fn(u64, &mut [u8]) -> usize + '_ {
        move |a, buf| {
            let a = a as usize;
            let n = buf.len().min(img.len().saturating_sub(a));
            buf[..n].copy_from_slice(&img[a..a + n]);
            n
        }
    }

    #[test]
    fn imports_read_from_pe() {
        let img = image();
        assert_eq!(import_names(&reader(&img), 0), vec!["d3d11.dll".to_string(), "KERNEL32.dll".to_string()]);
        assert_eq!(pick_api(&import_names(&reader(&img), 0)), Some(Api::Dx11));
        // Not a PE, or a 32-bit one: nothing.
        let mut bad = img.clone();
        bad[0x80] = b'X';
        assert!(import_names(&reader(&bad), 0).is_empty());
        let mut bad = img.clone();
        bad[0x98] = 0x0b;
        bad[0x99] = 0x01;
        assert!(import_names(&reader(&bad), 0).is_empty());
    }

    // The head ring.

    /// A camera at the origin looking along +y (x right, z up) on a 1000 x 1000 screen, `f` pixels
    /// per unit at 1 unit away; the same sums as the frame's `project`.
    fn cam(f: f64) -> impl Fn([f64; 3]) -> Option<(f64, f64)> {
        move |p| (p[1] >= 0.5).then(|| (500.0 + f * p[0] / p[1], 500.0 - f * p[2] / p[1]))
    }
    const UP: [f64; 3] = [0.0, 0.0, 1.0];
    const SCREEN: (f64, f64) = (1000.0, 1000.0);

    /// Rectangles as (colour, x0, y0, x1, y1).
    type Rects = Vec<(usize, f64, f64, f64, f64)>;

    /// Every rectangle a ring draws, and the count it returned.
    fn drawn(ring: Ring, t: f64) -> (usize, Rects) {
        let mut got = Vec::new();
        let n = ring.draw(&mut |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| got.push((c, x0, y0, x1, y1)), 4, t);
        (n, got)
    }

    /// Drones: the diamond sits on the drone with its corners up, right, down and left, shrinks far
    /// away but never under the minimum, and none is drawn behind the camera, off screen or bigger
    /// than half the screen.
    #[test]
    fn drone_diamond_sits_on_the_drone() {
        // A camera at 0 looking along +y with +z up: 1000 px per unit at depth 1.
        let project = |p: [f64; 3]| (p[1] >= 0.5).then(|| (960.0 + p[0] / p[1] * 1000.0, 540.0 - p[2] / p[1] * 1000.0));
        let (up, screen) = ([0.0, 0.0, 1.0], (1920.0, 1080.0));
        let near = diamond(&project, up, [0.0, 10.0, 0.0], 0.9, screen).unwrap();
        assert_eq!(near.at, (960.0, 540.0));
        assert!((near.r - 90.0).abs() < 1e-9, "{}", near.r);
        let r = near.r;
        assert_eq!(near.corners(), [(960.0, 540.0 - r), (960.0 + r, 540.0), (960.0, 540.0 + r), (960.0 - r, 540.0)]);
        let far = diamond(&project, up, [0.0, 1000.0, 0.0], 0.9, screen).unwrap();
        assert_eq!(far.r, DIAMOND_MIN_PX);
        assert!(diamond(&project, up, [0.0, -5.0, 0.0], 0.9, screen).is_none(), "behind");
        assert!(diamond(&project, up, [100.0, 10.0, 0.0], 0.9, screen).is_none(), "off screen");
        assert!(diamond(&project, up, [0.0, 0.6, 0.0], 0.9, screen).is_none(), "bigger than half the screen");
        assert!(diamond(&project, up, [f64::NAN, 10.0, 0.0], 0.9, screen).is_none(), "not a number");
        // A bigger drone, a bigger diamond.
        let big = diamond(&project, up, [0.0, 10.0, 0.0], 2.0, screen).unwrap();
        assert!((big.r - 200.0).abs() < 1e-9, "{}", big.r);
    }

    /// The diamond's outline closes at its four corners and costs about one rectangle per row of
    /// each side, so a frame of drones stays cheap.
    #[test]
    fn drone_diamond_outline_is_closed_and_cheap() {
        let d = Diamond { at: (100.0, 100.0), r: 20.0 };
        let mut rects = Vec::new();
        let n = d.draw(&mut |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| rects.push((c, x0, y0, x1, y1)), C_DRONE, 2.0);
        assert_eq!(n, rects.len());
        assert!(rects.iter().all(|r| r.0 == C_DRONE));
        for (cx, cy) in d.corners() {
            assert!(rects.iter().any(|&(_, x0, y0, x1, y1)| x0 - 0.5 <= cx && cx <= x1 + 0.5 && y0 - 0.5 <= cy && cy <= y1 + 0.5), "corner {cx} {cy}");
        }
        assert!(n <= 4 * (d.r as usize + 3), "{n}");
    }

    /// The distance label is the same as before it moved into `label`: centred, under its top, with
    /// the shadow a font pixel down and right.
    #[test]
    fn label_is_centred_with_its_shadow() {
        let mut rects = Vec::new();
        label(&mut |c: usize, x0: f64, y0: f64, x1: f64, y1: f64| rects.push((c, x0, y0, x1, y1)), C_DRONE, "1m", 100.0, 50.0, 2.0, true);
        // "1" is 3 wide, "m" 5, each plus a gap: 10 font pixels, 20 px, so it starts at 90.
        let lit: Vec<_> = rects.iter().filter(|r| r.0 == C_DRONE).collect();
        let shadow: Vec<_> = rects.iter().filter(|r| r.0 == C_SHADOW).collect();
        assert_eq!(lit.len(), shadow.len());
        assert_eq!(lit.iter().map(|r| r.1).fold(f64::MAX, f64::min), 90.0);
        assert_eq!(lit.iter().map(|r| r.2).fold(f64::MAX, f64::min), 50.0);
        assert_eq!(lit.iter().map(|r| r.4).fold(f64::MIN, f64::max), 60.0);
        for (a, b) in lit.iter().zip(&shadow) {
            assert_eq!((b.1, b.2, b.3, b.4), (a.1 + 2.0, a.2 + 2.0, a.3 + 2.0, a.4 + 2.0));
        }
        // The drones' colour has its own image, drawn last, on top.
        assert_eq!(DRAW_ORDER[0], C_SHADOW);
        assert_eq!(*DRAW_ORDER.last().unwrap(), C_DRONE);
        let c = colors(&EspStyle::DEFAULT);
        assert_eq!(c[C_DRONE], [160.0 / 255.0, 90.0 / 255.0, 1.0, 1.0]);
        assert_eq!(c[C_SHADOW], [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn ring_points_close_evenly_around_the_centre() {
        for r in [2.5, 7.0, 30.0, 120.0] {
            let at = (300.0, 200.0);
            let p = ring_points(at, r);
            assert_eq!(p.len(), ring_sides(r));
            // Every corner on the circle, the first straight up.
            assert!(p.iter().all(|q| ((q.0 - at.0).hypot(q.1 - at.1) - r).abs() < 1e-9));
            assert!((p[0].0 - at.0).abs() < 1e-9 && (p[0].1 - (at.1 - r)).abs() < 1e-9);
            // All sides the same length, the last one back to the first too: closed.
            let side = |i: usize| (p[i].0 - p[(i + 1) % p.len()].0).hypot(p[i].1 - p[(i + 1) % p.len()].1);
            assert!((0..p.len()).all(|i| (side(i) - side(0)).abs() < 1e-9));
            // The corners' middle is the centre.
            let n = p.len() as f64;
            let (mx, my) = p.iter().fold((0.0, 0.0), |s, q| (s.0 + q.0 / n, s.1 + q.1 / n));
            assert!((mx - at.0).abs() < 1e-9 && (my - at.1).abs() < 1e-9);
        }
    }

    /// Whether a fill from the ring's centre gets out, so the ring has a gap. Pixels are lit as the
    /// backends light them (corners rounded, right and bottom edges left out) and the fill goes
    /// four ways, so a gap one pixel wide lets it through. None when the centre pixel itself is
    /// lit.
    fn leaks(ring: Ring, t: f64, got: &Rects) -> Option<bool> {
        let lim = (ring.r + t).ceil() as i64 + 4;
        let (cx, cy) = (ring.at.0.floor() as i64, ring.at.1.floor() as i64);
        let side = 2 * lim + 1;
        let cell = |x: i64, y: i64| ((y - cy + lim) * side + x - cx + lim) as usize;
        let px = |v: f64| v.round() as i64;
        let mut lit = vec![false; (side * side) as usize];
        for g in got {
            for y in px(g.2).max(cy - lim)..px(g.4).min(cy + lim + 1) {
                for x in px(g.1).max(cx - lim)..px(g.3).min(cx + lim + 1) {
                    lit[cell(x, y)] = true;
                }
            }
        }
        if lit[cell(cx, cy)] {
            return None;
        }
        lit[cell(cx, cy)] = true;
        let mut todo = vec![(cx, cy)];
        while let Some((x, y)) = todo.pop() {
            if (x - cx).abs() == lim || (y - cy).abs() == lim {
                return Some(true);
            }
            for (nx, ny) in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
                if !lit[cell(nx, ny)] {
                    lit[cell(nx, ny)] = true;
                    todo.push((nx, ny));
                }
            }
        }
        Some(false)
    }

    #[test]
    fn a_drawn_ring_has_no_gaps() {
        // A fill from the centre must stay inside. Lines of 1 to 3 px and their black edges (3 to 5
        // px), rings from the smallest outline up, centres all over a pixel.
        let (mut gaps, mut tested) = (Vec::new(), 0);
        for t in [1.0, 2.0, 3.0, 4.0, 5.0] {
            for r in [2.5, 2.7, 3.0, 3.5, 4.0, 5.0, 6.5, 8.0, 10.0, 13.6, 17.0, 22.0, 27.0, 40.0, 58.0, 90.0, 135.0] {
                for fx in [0.0, 0.13, 0.25, 0.5, 0.71, 0.9] {
                    for fy in [0.0, 0.31, 0.5, 0.77] {
                        let ring = Ring { at: (500.0 + fx, 400.0 + fy), r };
                        let (n, got) = drawn(ring, t);
                        assert_eq!(n, got.len());
                        assert!(got.iter().all(|g| g.0 == 4));
                        // Nothing away from the ring.
                        let reach = r + t + 1.0;
                        assert!(got.iter().all(|g| g.1 >= ring.at.0 - reach && g.3 <= ring.at.0 + reach && g.2 >= ring.at.1 - reach && g.4 <= ring.at.1 + reach));
                        match leaks(ring, t, &got) {
                            Some(out) => {
                                tested += 1;
                                if out {
                                    gaps.push((t, r, fx, fy));
                                }
                            }
                            // Only a small ring with thick lines may fill its own middle.
                            None => assert!(r < 2.0 * t + 3.0, "r {r} t {t}: middle filled"),
                        }
                    }
                }
            }
        }
        assert!(gaps.is_empty(), "{} of {tested} rings have a gap, e.g. (t, r, x, y) {:?}", gaps.len(), &gaps[..gaps.len().min(6)]);
    }

    #[test]
    fn ring_sides_are_bounded() {
        let mut last = 0;
        for r in [0.0, 0.5, 2.5, 4.0, 9.0, 16.0, 30.0, 58.0, 60.0, 200.0, 5000.0, 1e300] {
            let n = ring_sides(r);
            assert!((RING_SIDES_MIN..=RING_SIDES_MAX).contains(&n) && n.is_multiple_of(4), "{r}: {n}");
            assert!(n >= last, "{r}: fewer sides on a bigger ring");
            last = n;
        }
        assert_eq!((ring_sides(0.0), ring_sides(1e300)), (RING_SIDES_MIN, RING_SIDES_MAX));
        for r in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -5.0] {
            assert!((RING_SIDES_MIN..=RING_SIDES_MAX).contains(&ring_sides(r)), "{r}");
        }
        // The corners stay within half a pixel of a true circle while the sides last.
        for r in [2.5, 5.0, 10.0, 20.0, 40.0, 58.0] {
            assert!(r * (1.0 - (std::f64::consts::PI / ring_sides(r) as f64).cos()) <= 0.5, "{r}");
        }
    }

    #[test]
    fn head_ring_shrinks_with_distance_and_tilts_with_the_head() {
        let cam = cam(500.0);
        // Standing straight 10 and 20 units ahead: chest 1.45 up, head point 1.75 up.
        let at = |y: f64| head_ring(&cam, UP, [0.0, y, 1.45], [0.0, y, 1.75], None, SCREEN).unwrap();
        let (near, far) = (at(10.0), at(20.0));
        assert!((near.r - 500.0 * HEAD_RING_RADIUS / 10.0).abs() < 1e-9);
        assert!((near.r / far.r - 2.0).abs() < 1e-9);
        // The centre is HEAD_RING_OFFSET from the head along the chest -> head line.
        let want = cam([0.0, 10.0, 1.75 + HEAD_RING_OFFSET]).unwrap();
        assert!((near.at.0 - want.0).abs() < 1e-9 && (near.at.1 - want.1).abs() < 1e-9);
        // Leaning 45 degrees to the side: the centre moves along the lean.
        let lean = head_ring(&cam, UP, [0.0, 10.0, 1.45], [0.3, 10.0, 1.75], None, SCREEN).unwrap();
        let k = HEAD_RING_OFFSET / 0.3f64.hypot(0.3);
        let want = cam([0.3 + 0.3 * k, 10.0, 1.75 + 0.3 * k]).unwrap();
        assert!((lean.at.0 - want.0).abs() < 1e-9 && (lean.at.1 - want.1).abs() < 1e-9);
        assert!((lean.r - near.r).abs() < 1e-9);
    }

    #[test]
    fn no_ring_behind_the_camera_off_screen_or_broken() {
        let cam5 = cam(500.0);
        let ring = |chest: [f64; 3], head: [f64; 3]| head_ring(&cam5, UP, chest, head, None, SCREEN);
        assert!(ring([0.0, 10.0, 1.45], [0.0, 10.0, 1.75]).is_some());
        // Behind the camera, or so far to the side it is off screen.
        assert_eq!(ring([0.0, -10.0, 1.45], [0.0, -10.0, 1.75]), None);
        assert_eq!(ring([50.0, 10.0, 1.45], [50.0, 10.0, 1.75]), None);
        // Chest and head on one spot, or not numbers.
        assert_eq!(ring([0.0, 10.0, 1.75], [0.0, 10.0, 1.75]), None);
        assert_eq!(ring([0.0, 10.0, f64::NAN], [0.0, 10.0, 1.75]), None);
        assert_eq!(ring([0.0, 10.0, 1.45], [0.0, f64::INFINITY, 1.75]), None);
        assert_eq!(head_ring(&|_: [f64; 3]| Some((f64::NAN, 1.0)), UP, [0.0, 10.0, 1.45], [0.0, 10.0, 1.75], None, SCREEN), None);
        // Zoomed in on a close head: taller than the screen, no ring; further off it fits.
        let scope = cam(5000.0);
        // So close that the ring's radius is 750 px, over half the screen, whatever the radius.
        let y = 5000.0 * HEAD_RING_RADIUS / 750.0;
        assert_eq!(head_ring(&scope, UP, [0.0, y, -0.3], [0.0, y, 0.0], None, SCREEN), None);
        assert!(head_ring(&scope, UP, [0.0, 3.0, -0.3], [0.0, 3.0, 0.0], None, SCREEN).is_some());
    }

    #[test]
    fn tiny_rings_are_one_dot() {
        // Far away the ring is under RING_MIN_PX: one square dot on the ring's centre.
        let far = head_ring(&cam(500.0), UP, [0.0, 30.0, 1.45], [0.0, 30.0, 1.75], None, SCREEN).unwrap();
        assert!(far.r < RING_MIN_PX);
        assert_eq!(drawn(far, 2.0).0, 1);
        let (n, got) = drawn(Ring { at: (100.0, 50.0), r: 1.0 }, 2.0);
        assert_eq!((n, got), (1, vec![(4, 98.0, 48.0, 102.0, 52.0)]));
        // From RING_MIN_PX on it is an outline of lines.
        assert!(drawn(Ring { at: (100.0, 50.0), r: RING_MIN_PX }, 2.0).0 >= RING_SIDES_MIN);
    }

    #[test]
    fn the_neck_line_stops_at_the_ring() {
        let ring = Ring { at: (100.0, 100.0), r: 20.0 };
        assert_eq!(ring.edge_towards((100.0, 200.0)), Some((100.0, 120.0)));
        // From the side: on the ring, on the line to the centre.
        let e = ring.edge_towards((160.0, 180.0)).unwrap();
        assert!(((e.0 - 100.0).hypot(e.1 - 100.0) - 20.0).abs() < 1e-9);
        assert!(((e.0 - 100.0) * 80.0 - (e.1 - 100.0) * 60.0).abs() < 1e-9);
        // A chest inside the ring on screen: no line at all.
        assert_eq!(ring.edge_towards((105.0, 110.0)), None);
        assert_eq!(ring.edge_towards((100.0, 100.0)), None);
        // A real head: the line from the chest ends on the ring, below its centre.
        let head = head_ring(&cam(500.0), UP, [0.0, 10.0, 1.45], [0.0, 10.0, 1.75], None, SCREEN).unwrap();
        let chest = cam(500.0)([0.0, 10.0, 1.45]).unwrap();
        let end = head.edge_towards(chest).unwrap();
        assert!((end.0 - head.at.0).abs() < 1e-9 && (end.1 - (head.at.1 + head.r)).abs() < 1e-9);
    }

    #[test]
    fn head_ring_on_the_centre_it_is_given() {
        // The P path's middle of the head: the ring sits on it, its size from there; the chest and
        // head point don't move it.
        let cam = cam(500.0);
        let mid = [0.02, 10.0, 1.62];
        for head in [[0.0, 10.0, 1.75], [0.3, 10.0, 1.6], [0.0, 10.0, 1.45]] {
            let r = head_ring(&cam, UP, [0.0, 10.0, 1.45], head, Some(mid), SCREEN).unwrap();
            let want = cam(mid).unwrap();
            assert!((r.at.0 - want.0).abs() < 1e-9 && (r.at.1 - want.1).abs() < 1e-9);
            assert!((r.r - 500.0 * HEAD_RING_RADIUS / 10.0).abs() < 1e-9);
        }
        // The neck line from the chest ends on the ring, on the way to that centre.
        let r = head_ring(&cam, UP, [0.0, 10.0, 1.45], [0.0, 10.0, 1.75], Some(mid), SCREEN).unwrap();
        let chest = cam([0.0, 10.0, 1.45]).unwrap();
        let end = r.edge_towards(chest).unwrap();
        assert!(((end.0 - r.at.0).hypot(end.1 - r.at.1) - r.r).abs() < 1e-9);
        assert!(((end.0 - chest.0) * (r.at.1 - chest.1) - (end.1 - chest.1) * (r.at.0 - chest.0)).abs() < 1e-6);
        // Behind the camera or not a number: no ring, as before.
        assert_eq!(head_ring(&cam, UP, [0.0, 10.0, 1.45], [0.0, 10.0, 1.75], Some([0.0, -10.0, 1.6]), SCREEN), None);
        assert_eq!(head_ring(&cam, UP, [0.0, 10.0, 1.45], [0.0, 10.0, 1.75], Some([f64::NAN, 10.0, 1.6]), SCREEN), None);
    }

    #[test]
    fn the_ring_centre_is_placed_with_the_bones() {
        // A body at (100, 200, 10) turned a quarter left: its x (right) is the world's y, its y
        // (forward) the world's -x.
        let rows = [[0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let p = [100.0, 200.0, 10.0];
        let near = |a: [f64; 3], b: [f64; 3]| (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9);
        let mut local = [[0.0, 0.0, 1.0]; BONE_COUNT];
        local[HEAD] = [0.1, 0.2, 1.7];
        let (w, mid) = place_rows(rows, p, &local, Some([0.1, 0.2, 1.6])).unwrap();
        assert!(near(w[HEAD], [99.8, 200.1, 11.7]));
        assert!(near(mid.unwrap(), [99.8, 200.1, 11.6]));
        // Each bone as before (place's sums, in its order).
        for (o, l) in w.iter().zip(&local) {
            assert_eq!(*o, [0, 1, 2].map(|k| p[k] + l[0] * rows[0][k] + l[1] * rows[1][k] + l[2] * rows[2][k]));
        }
        // No centre, or one not a number or over 2.5 m out: the bones the same, no centre.
        for more in [None, Some([f64::NAN, 0.0, 1.6]), Some([0.0, 0.0, 2.6])] {
            assert_eq!(place_rows(rows, p, &local, more), Ok((w, None)));
        }
        // Bones that fail fail as before, whatever the centre.
        let mut low = local;
        low[0] = [0.0, 0.0, -0.5];
        assert_eq!(place_rows(rows, p, &low, Some([0.1, 0.2, 1.6])), Err("pelvis -0.50 m up".to_string()));
        assert_eq!(place_rows([[2.0, 0.0, 0.0], rows[1], rows[2]], p, &local, None), Err("facing is not a turn".to_string()));
    }

    #[test]
    fn ring_costs_are_bounded() {
        // Rectangles per ring, one colour: a dot far away, tens close by.
        assert_eq!(drawn(Ring { at: (1920.0, 1080.0), r: 1.0 }, 2.0).0, 1);
        assert!(drawn(Ring { at: (1920.0, 1080.0), r: 10.0 }, 2.0).0 < 60);
        // The biggest ring drawn (half a 4K screen high), with its black edge, stays a few
        // thousand, so a frame's rings stay under RING_RECTS_MAX plus one such ring.
        let big = drawn(Ring { at: (1920.0, 1080.0), r: 1080.0 }, 3.0).0 + drawn(Ring { at: (1920.0, 1080.0), r: 1080.0 }, 5.0).0;
        assert!(big < 6000, "{big}");
    }
}
