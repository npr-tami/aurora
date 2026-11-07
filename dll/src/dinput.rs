//! Keeping the menu's keys from the game. The game reads the keyboard through DirectInput 8, past
//! its window messages, so the device's GetDeviceState and GetDeviceData are wrapped: while the
//! menu is open the menu's keys read as up, and their presses are taken out of the event list.
//! Releases still pass, so no key stays stuck down in the game. The game reads the mouse here too:
//! while the menu is open with the game in front, the same two reads give the game a still mouse
//! and hand the moves, wheel and buttons to the menu's pointer instead.

use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use retour::RawDetour;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW;
use windows_sys::core::GUID;

use crate::log;

/// While the menu is open.
pub static OPEN: AtomicBool = AtomicBool::new(false);
/// While a number is being typed: the digits are the menu's too.
pub static EDITING: AtomicBool = AtomicBool::new(false);
/// Set while a new hotkey is being picked in the menu: then every key is kept from the game (and
/// from our own hotkeys and fly).
pub static BINDING: AtomicBool = AtomicBool::new(false);
/// The menu is open and the game in front: the mouse is the menu's. The game gets no moves, presses
/// or wheel; the menu's pointer gets them. Off the moment the menu closes, the game goes behind, or
/// Aurora unloads.
static MOUSE: AtomicBool = AtomicBool::new(false);
/// When the menu last said it has the mouse (ms since `CLOCK` started). The menu says so every tick
/// (12 ms); if it stops saying it for `MOUSE_LAPSE` the mouse is the game's again by itself, so it
/// never stays blocked.
static MOUSE_AT: AtomicU64 = AtomicU64::new(0);
static CLOCK: OnceLock<Instant> = OnceLock::new();
const MOUSE_LAPSE: u64 = 500;

fn now_ms() -> u64 {
    CLOCK.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// The menu has the mouse right now (and said so lately).
pub fn menu_has_mouse() -> bool {
    still_said(MOUSE.load(Ordering::Relaxed), MOUSE_AT.load(Ordering::Relaxed), now_ms())
}

/// The menu said it has the mouse (`on`, last at `at`), and not too long ago (`now`).
fn still_said(on: bool, at: u64, now: u64) -> bool {
    on && now.saturating_sub(at) < MOUSE_LAPSE
}

/// What the game's mouse reads carried while `MOUSE` is on, kept for the menu: moves and wheel
/// added up per read (GetDeviceState and GetDeviceData apart, so a game that uses both is not
/// counted twice), and how many reads there were.
struct Moves {
    x: AtomicI32,
    y: AtomicI32,
    wheel: AtomicI32,
    reads: AtomicU32,
}
static FROM_STATE: Moves = Moves { x: AtomicI32::new(0), y: AtomicI32::new(0), wheel: AtomicI32::new(0), reads: AtomicU32::new(0) };
static FROM_DATA: Moves = Moves { x: AtomicI32::new(0), y: AtomicI32::new(0), wheel: AtomicI32::new(0), reads: AtomicU32::new(0) };
/// Bit n: mouse button n is down (0 left, 1 right, 2 middle, 3 and 4 the side ones).
static BUTTONS: AtomicU32 = AtomicU32::new(0);
static SAID_MOUSE: AtomicBool = AtomicBool::new(false);

/// The mouse the game read since the last call, for the menu's pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseRead {
    pub dx: i32,
    pub dy: i32,
    /// In Windows' wheel units: 120 per notch, up is more.
    pub wheel: i32,
    pub buttons: u32,
    /// False: the game did not read its mouse since the last call (then the menu asks Windows).
    pub fresh: bool,
}

/// Take what the game's mouse reads carried. The buffered reads win when they moved, or when the
/// state reads did not move either; a game that moves through GetDeviceState and only polls
/// GetDeviceData for buttons still moves the pointer.
pub fn take_mouse() -> MouseRead {
    let take = |m: &Moves| (m.x.swap(0, Ordering::SeqCst), m.y.swap(0, Ordering::SeqCst), m.wheel.swap(0, Ordering::SeqCst), m.reads.swap(0, Ordering::SeqCst));
    let (d, s) = (take(&FROM_DATA), take(&FROM_STATE));
    let moved = |m: &(i32, i32, i32, u32)| m.0 != 0 || m.1 != 0 || m.2 != 0;
    let (dx, dy, wheel, _) = if d.3 > 0 && (moved(&d) || !moved(&s)) { d } else { s };
    MouseRead { dx, dy, wheel, buttons: BUTTONS.load(Ordering::SeqCst), fresh: d.3 + s.3 > 0 }
}

/// The menu takes (true) or gives back (false) the mouse. Each change starts clean: no old moves,
/// and no button left marked down from a release the menu never saw.
pub fn set_mouse(on: bool) {
    if on {
        MOUSE_AT.store(now_ms(), Ordering::SeqCst);
    }
    if MOUSE.swap(on, Ordering::SeqCst) != on {
        BUTTONS.store(0, Ordering::SeqCst);
        take_mouse();
    }
}

fn add(m: &Moves, x: i32, y: i32, wheel: i32) {
    m.x.fetch_add(x, Ordering::SeqCst);
    m.y.fetch_add(y, Ordering::SeqCst);
    m.wheel.fetch_add(wheel, Ordering::SeqCst);
    m.reads.fetch_add(1, Ordering::SeqCst);
}

fn said_mouse(how: &str) {
    if !SAID_MOUSE.swap(true, Ordering::Relaxed) {
        log(&format!("dinput: the menu took the mouse from the game ({how})"));
    }
}

/// Enter (both), Backspace, the arrows, Right Shift, Right Ctrl, the number pad keys the menu uses
/// (8 2 4 6 5 0), O and Delete: the menu's `NAV` keys. Left Shift and Left Ctrl stay the game's.
const MENU_KEYS: [u8; 17] = [0x1C, 0x9C, 0x0E, 0xC8, 0xD0, 0xCB, 0xCD, 0x36, 0x9D, 0x48, 0x50, 0x4B, 0x4D, 0x4C, 0x52, 0x18, 0xD3];
/// 1 to 0 on the top row, the full stop, and the rest of the number pad with its dot.
const DIGIT_KEYS: [u8; 23] = [
    0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x34, 0x47, 0x49, 0x4F, 0x51, 0x53, // Digits, dot
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, // A to F, for hex colours
    0x00,
];

/// The command box is open.
fn typing() -> bool {
    OPEN.load(Ordering::Relaxed) && EDITING.load(Ordering::Relaxed)
}

/// The game must not get the mouse: the command box is open (as before), or the menu has it.
fn mouse_ours() -> bool {
    typing() || menu_has_mouse()
}

/// Keep only the events `keep(offset, pressed)` says yes to.
///
/// # Safety
/// `buf` holds `*count` events of `size` (at least 8) bytes.
unsafe fn keep_events(buf: *mut u8, count: *mut u32, size: u32, keep: impl Fn(u32, bool) -> bool) {
    unsafe {
        let mut kept = 0usize;
        for i in 0..*count as usize {
            let ev = buf.add(i * size as usize);
            let ofs = (ev as *const u32).read_unaligned();
            let down = (ev.add(4) as *const u32).read_unaligned() & 0x80 != 0;
            if !keep(ofs, down) {
                continue;
            }
            if kept != i {
                std::ptr::copy(ev, buf.add(kept * size as usize), size as usize);
            }
            kept += 1;
        }
        *count = kept as u32;
    }
}

/// Keys that are a Aurora hotkey, default or the player's own: the game never gets them, menu open
/// or not. By Windows key number and by DirectInput key number.
static HOTKEY_VK: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static HOTKEY_DIK: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];

pub fn set_hotkeys(vks: &[u32]) {
    let (mut vk_on, mut dik_on) = ([false; 256], [false; 256]);
    for &vk in vks.iter().filter(|&&v| v < 256) {
        vk_on[vk as usize] = true;
        if let Some(d) = dik_of(vk) {
            dik_on[d as usize] = true;
        }
    }
    for (slot, on) in HOTKEY_VK.iter().zip(vk_on).chain(HOTKEY_DIK.iter().zip(dik_on)) {
        slot.store(on, Ordering::Relaxed);
    }
}

/// Fly's keys as DirectInput's (0 = none): kept from the game while Fly is on. Down first; since
/// night all of them (forward, back, left, right, up, down, sprint), so the player's body stands
/// still while the player flies instead of walking or sprinting on the spot.
pub const FLY_KEY_COUNT: usize = 7;
static FLY_DIK: [AtomicU32; FLY_KEY_COUNT] = [const { AtomicU32::new(0) }; FLY_KEY_COUNT];
static FLY_VK: [AtomicU32; FLY_KEY_COUNT] = [const { AtomicU32::new(0) }; FLY_KEY_COUNT];

/// Fly tells us its keys (Windows key numbers) while it runs.
pub fn set_fly_keys(vks: [u32; FLY_KEY_COUNT]) {
    for (i, vk) in vks.into_iter().enumerate() {
        if FLY_VK[i].swap(vk, Ordering::Relaxed) != vk {
            FLY_DIK[i].store(dik_of(vk).map_or(0, u32::from), Ordering::Relaxed);
        }
    }
}

fn fly_key(dik: u32) -> bool {
    FLY_DIK.iter().any(|d| d.load(Ordering::Relaxed) == dik)
}

pub fn is_hotkey(vk: u32) -> bool {
    vk < 256 && HOTKEY_VK[vk as usize].load(Ordering::Relaxed)
}

/// A Windows key number as DirectInput's: the scan code, extended keys with 0x80 added. Mouse
/// buttons have none.
fn dik_of(vk: u32) -> Option<u8> {
    // SAFETY: a plain lookup; 4 is MAPVK_VK_TO_VSC_EX (0xE0xx for extended keys).
    let sc = unsafe { MapVirtualKeyW(vk, 4) };
    match sc {
        0 => None,
        s if s & 0xFF00 == 0xE000 && s & 0xFF < 0x80 => Some((s & 0xFF) as u8 | 0x80),
        s if s < 0x80 => Some(s as u8),
        _ => None,
    }
}

fn blocked(dik: u32) -> bool {
    if dik == 0 || dik > 0xFF {
        return false;
    }
    if HOTKEY_DIK[dik as usize].load(Ordering::Relaxed) {
        return true;
    }
    if fly_key(dik) && crate::fly::is_on() {
        return true;
    }
    if !OPEN.load(Ordering::Relaxed) {
        return false;
    }
    let k = dik as u8;
    BINDING.load(Ordering::Relaxed) || MENU_KEYS.contains(&k) || (EDITING.load(Ordering::Relaxed) && DIGIT_KEYS.contains(&k))
}

const IID_DI8W: GUID = GUID::from_u128(0xBF798031_483A_4DA2_AA99_5D64ED369700);
const IID_DI8A: GUID = GUID::from_u128(0xBF798030_483A_4DA2_AA99_5D64ED369700);
const SYS_KEYBOARD: GUID = GUID::from_u128(0x6F1D2B61_D5A0_11CF_BFC7_444553540000);
const DI8DEVTYPE_MOUSE: u8 = 0x12;
const DI8DEVTYPE_KEYBOARD: u8 = 0x13;
/// GetDeviceData's "only look, leave the events there" flag: not counted, they come again.
const DIGDD_PEEK: u32 = 1;

type CreateFn = unsafe extern "system" fn(usize, u32, *const GUID, *mut usize, usize) -> i32;
type CreateDeviceFn = unsafe extern "system" fn(usize, *const GUID, *mut usize, usize) -> i32;
type ReleaseFn = unsafe extern "system" fn(usize) -> u32;
type StateFn = unsafe extern "system" fn(usize, u32, *mut u8) -> i32;
type DataFn = unsafe extern "system" fn(usize, u32, *mut u8, *mut u32, u32) -> i32;
type CapsFn = unsafe extern "system" fn(usize, *mut DevCaps) -> i32;

#[repr(C)]
struct DevCaps {
    size: u32,
    flags: u32,
    dev_type: u32,
    rest: [u32; 8],
}

// One pair of originals per interface flavour (W and A may be separate functions).
static ORIG_STATE: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static ORIG_DATA: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static CAPS: AtomicUsize = AtomicUsize::new(0);
static IN_HOOK: AtomicU32 = AtomicU32::new(0);
static SAID: AtomicBool = AtomicBool::new(false);

struct Hooks(Vec<RawDetour>);
// SAFETY: the detours are only enabled, disabled and dropped under the HOOKS lock.
unsafe impl Send for Hooks {}
static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);
static TRIED: AtomicBool = AtomicBool::new(false);
/// The device type (keyboard, mouse...), per device seen.
static KINDS: Mutex<Vec<(usize, u8)>> = Mutex::new(Vec::new());

fn said() {
    if !SAID.swap(true, Ordering::Relaxed) {
        log("dinput: a menu key was kept from the game");
    }
}

fn kind(dev: usize) -> u8 {
    if let Some(k) = KINDS.lock().ok().and_then(|k| k.iter().find(|d| d.0 == dev).map(|d| d.1)) {
        return k;
    }
    let caps = CAPS.load(Ordering::SeqCst);
    let mut c = DevCaps { size: std::mem::size_of::<DevCaps>() as u32, flags: 0, dev_type: 0, rest: [0; 8] };
    // SAFETY: GetCapabilities of the same device class, read from our own device's table.
    let ok = caps != 0 && unsafe { std::mem::transmute::<usize, CapsFn>(caps)(dev, &mut c) } >= 0;
    let t = if ok { (c.dev_type & 0xFF) as u8 } else { 0 };
    if let Ok(mut k) = KINDS.lock() {
        if k.len() > 64 {
            k.clear();
        }
        k.push((dev, t));
    }
    t
}

fn is_keyboard(dev: usize) -> bool {
    kind(dev) == DI8DEVTYPE_KEYBOARD
}

/// A mouse's events in a list: moves and wheel added up for the menu, buttons noted.
///
/// # Safety
/// `buf` holds `count` events of `size` (at least 8) bytes.
unsafe fn note_mouse_events(buf: *const u8, count: u32, size: u32) {
    let (mut x, mut y, mut wheel) = (0i32, 0i32, 0i32);
    for i in 0..count as usize {
        // SAFETY: event i of `count`: its offset, then its value.
        let (ofs, val) = unsafe {
            let ev = buf.add(i * size as usize);
            ((ev as *const u32).read_unaligned(), (ev.add(4) as *const u32).read_unaligned())
        };
        match ofs {
            0 => x = x.wrapping_add(val as i32),
            4 => y = y.wrapping_add(val as i32),
            8 => wheel = wheel.wrapping_add(val as i32),
            12..=19 => {
                let bit = 1u32 << (ofs - 12);
                if val & 0x80 != 0 {
                    BUTTONS.fetch_or(bit, Ordering::SeqCst);
                } else {
                    BUTTONS.fetch_and(!bit, Ordering::SeqCst);
                }
            }
            _ => {}
        }
    }
    add(&FROM_DATA, x, y, wheel);
}

unsafe fn state(n: usize, dev: usize, len: u32, data: *mut u8) -> i32 {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the trampoline runs the original with the game's arguments.
    let r = unsafe { std::mem::transmute::<usize, StateFn>(ORIG_STATE[n].load(Ordering::SeqCst))(dev, len, data) };
    // A keyboard's state is 256 bytes, one per key; nothing else reads that size. While the command
    // box is open, or the menu has the mouse, the mouse is ours (as Stand's): a mouse's state (16
    // or 20 bytes) reads as still, nothing pressed. With the menu, its moves, wheel and buttons go
    // to the pointer first.
    if r >= 0 && (len == 16 || len == 20) && !data.is_null() && mouse_ours() {
        if menu_has_mouse() {
            // SAFETY: DIMOUSESTATE(2) in the game's buffer: x, y, wheel as i32, then 4 or 8
            // Button bytes.
            unsafe {
                let v = |i: usize| (data.add(i * 4) as *const i32).read_unaligned();
                let mut b = 0u32;
                for n in 0..(len as usize - 12) {
                    if *data.add(12 + n) & 0x80 != 0 {
                        b |= 1 << n;
                    }
                }
                BUTTONS.store(b, Ordering::SeqCst);
                add(&FROM_STATE, v(0), v(1), v(2));
            }
            said_mouse("GetDeviceState");
        }
        // SAFETY: the game's buffer of `len` bytes.
        unsafe { std::ptr::write_bytes(data, 0, len as usize) };
    }
    if r >= 0 && len == 256 && !data.is_null() {
        for k in 1..256u32 {
            // SAFETY: inside the game's 256-byte buffer.
            if blocked(k) && unsafe { *data.add(k as usize) } & 0x80 != 0 {
                unsafe { *data.add(k as usize) = 0 };
                said();
            }
        }
    }
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

unsafe fn data(n: usize, dev: usize, size: u32, buf: *mut u8, count: *mut u32, flags: u32) -> i32 {
    IN_HOOK.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the trampoline runs the original with the game's arguments.
    let r = unsafe { std::mem::transmute::<usize, DataFn>(ORIG_DATA[n].load(Ordering::SeqCst))(dev, size, buf, count, flags) };
    // Each event starts with the key (dwOfs) and its state (dwData); presses go, releases stay. The
    // mouse while the command box is open (any device but the keyboard, as before), or while the
    // menu has it (the mouse only): moves, presses and the wheel go, releases stay (a button held
    // when it opened is let go normally). Buttons are offsets 12 to 19.
    let menu_mouse = menu_has_mouse() && kind(dev) == DI8DEVTYPE_MOUSE;
    if r >= 0 && !buf.is_null() && !count.is_null() && size >= 8 && ((typing() && !is_keyboard(dev)) || menu_mouse) {
        if menu_mouse && flags & DIGDD_PEEK == 0 {
            // SAFETY: `count` events of `size` bytes, as the original just filled them.
            unsafe { note_mouse_events(buf, *count, size) };
            said_mouse("GetDeviceData");
        }
        // SAFETY: as below.
        unsafe { keep_events(buf, count, size, |ofs, down| (12..20).contains(&ofs) && !down) };
    }
    if r >= 0 && !buf.is_null() && !count.is_null() && size >= 8 && is_keyboard(dev) {
        // SAFETY: `count` events of `size` bytes, as the original just filled them.
        unsafe {
            keep_events(buf, count, size, |ofs, down| {
                let go = down && blocked(ofs);
                if go {
                    said();
                }
                !go
            })
        };
    }
    IN_HOOK.fetch_sub(1, Ordering::SeqCst);
    r
}

unsafe extern "system" fn hk_state_w(d: usize, l: u32, p: *mut u8) -> i32 {
    unsafe { state(0, d, l, p) }
}
unsafe extern "system" fn hk_state_a(d: usize, l: u32, p: *mut u8) -> i32 {
    unsafe { state(1, d, l, p) }
}
unsafe extern "system" fn hk_data_w(d: usize, s: u32, b: *mut u8, c: *mut u32, f: u32) -> i32 {
    unsafe { data(0, d, s, b, c, f) }
}
unsafe extern "system" fn hk_data_a(d: usize, s: u32, b: *mut u8, c: *mut u32, f: u32) -> i32 {
    unsafe { data(1, d, s, b, c, f) }
}

/// A keyboard device of our own, just to read its function table. Released again.
unsafe fn table(iid: &GUID) -> Result<(usize, usize, usize), String> {
    unsafe {
        let m = GetModuleHandleA(c"dinput8.dll".as_ptr() as _);
        if m.is_null() {
            return Err("the game has no DirectInput".into());
        }
        let create = GetProcAddress(m, c"DirectInput8Create".as_ptr() as _).ok_or("DirectInput8Create missing")?;
        let create: CreateFn = std::mem::transmute(create);
        let mut di = 0usize;
        let hinst = GetModuleHandleA(std::ptr::null()) as usize;
        let r = create(hinst, 0x0800, iid, &mut di, 0);
        if r < 0 || di == 0 {
            return Err(format!("DirectInput8Create failed: {r:#x}"));
        }
        let di_vt = *(di as *const *const usize);
        let mut dev = 0usize;
        let r = std::mem::transmute::<usize, CreateDeviceFn>(*di_vt.add(3))(di, &SYS_KEYBOARD, &mut dev, 0);
        let out = if r < 0 || dev == 0 {
            Err(format!("CreateDevice failed: {r:#x}"))
        } else {
            let vt = *(dev as *const *const usize);
            let got = (*vt.add(9), *vt.add(10), *vt.add(3));
            std::mem::transmute::<usize, ReleaseFn>(*vt.add(2))(dev);
            Ok(got)
        };
        std::mem::transmute::<usize, ReleaseFn>(*di_vt.add(2))(di);
        out
    }
}

/// Wrap the keyboard reads, once. Errors are logged; the menu still works without it.
pub fn ensure() {
    if TRIED.swap(true, Ordering::SeqCst) {
        return;
    }
    match install() {
        Ok(h) => {
            if let Ok(mut g) = HOOKS.lock() {
                *g = Some(h);
            }
            log("dinput: menu keys are kept from the game while the menu is open");
        }
        Err(e) => log(&format!("dinput: keys reach the game too: {e}")),
    }
}

fn install() -> Result<Hooks, String> {
    // SAFETY: the targets are dinput8's own GetDeviceState and GetDeviceData, with exactly
    // These signatures; each trampoline is stored before its detour is enabled.
    unsafe {
        let (sw, dw, caps) = table(&IID_DI8W)?;
        let a = table(&IID_DI8A).ok();
        CAPS.store(caps, Ordering::SeqCst);
        let mut hooks = Vec::new();
        let mut done: Vec<usize> = Vec::new();
        let mut pairs: Vec<(usize, *const (), &AtomicUsize)> =
            vec![(sw, hk_state_w as *const (), &ORIG_STATE[0]), (dw, hk_data_w as *const (), &ORIG_DATA[0])];
        if let Some((sa, da, _)) = a {
            pairs.push((sa, hk_state_a as *const (), &ORIG_STATE[1]));
            pairs.push((da, hk_data_a as *const (), &ORIG_DATA[1]));
        }
        for (target, hook, orig) in pairs {
            // A and W may share one function: wrap it once.
            if target == 0 || done.contains(&target) {
                continue;
            }
            done.push(target);
            let d = RawDetour::new(target as *const (), hook).map_err(|e| format!("hook: {e}"))?;
            orig.store(d.trampoline() as *const () as usize, Ordering::SeqCst);
            hooks.push(d);
        }
        for d in &hooks {
            d.enable().map_err(|e| format!("enable hook: {e}"))?;
        }
        Ok(Hooks(hooks))
    }
}

pub fn shutdown() {
    // The mouse is the game's again before anything else.
    MOUSE.store(false, Ordering::SeqCst);
    OPEN.store(false, Ordering::SeqCst);
    EDITING.store(false, Ordering::SeqCst);
    BINDING.store(false, Ordering::SeqCst);
    let Some(hooks) = HOOKS.lock().ok().and_then(|mut h| h.take()) else { return };
    for d in &hooks.0 {
        // SAFETY: our own detours, still alive.
        let _ = unsafe { d.disable() };
    }
    let t = Instant::now();
    while IN_HOOK.load(Ordering::SeqCst) != 0 && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
    drop(hooks);
    log("dinput: unhooked");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One buffered event as the game gets it (DIDEVICEOBJECTDATA, 24 bytes on 64-bit).
    fn ev(ofs: u32, data: u32) -> [u8; 24] {
        let mut e = [0u8; 24];
        e[0..4].copy_from_slice(&ofs.to_le_bytes());
        e[4..8].copy_from_slice(&data.to_le_bytes());
        e
    }

    /// The menu's pointer gets the moves, wheel and buttons of the game's buffered mouse reads, and
    /// the game keeps only the releases.
    #[test]
    fn mouse_events_go_to_the_menu() {
        let list = [ev(0, 5), ev(4, (-3i32) as u32), ev(8, 120), ev(12, 0x80), ev(0, 2), ev(13, 0)];
        let mut buf: Vec<u8> = list.iter().flatten().copied().collect();
        set_mouse(true);
        // SAFETY: six 24-byte events.
        unsafe { note_mouse_events(buf.as_ptr(), 6, 24) };
        // A state read in the same tick does not count twice.
        add(&FROM_STATE, 100, 100, 0);
        let got = take_mouse();
        assert_eq!(got, MouseRead { dx: 7, dy: -3, wheel: 120, buttons: 1, fresh: true });
        assert_eq!(take_mouse(), MouseRead { buttons: 1, ..Default::default() }, "taken once");
        // What the game is left with: the right button's release only.
        let mut count = 6u32;
        // SAFETY: as above.
        unsafe { keep_events(buf.as_mut_ptr(), &mut count, 24, |ofs, down| (12..20).contains(&ofs) && !down) };
        assert_eq!((count, &buf[..24]), (1, &ev(13, 0)[..]));
        // Buffered reads with no moves (buttons only): the state reads' moves are used.
        // SAFETY: no events are read.
        unsafe { note_mouse_events(buf.as_ptr(), 0, 24) };
        add(&FROM_STATE, 4, -2, 0);
        assert_eq!(take_mouse(), MouseRead { dx: 4, dy: -2, wheel: 0, buttons: 1, fresh: true });
        // Given back: nothing held over to the next time.
        set_mouse(false);
        assert!(!menu_has_mouse());
        assert_eq!(take_mouse(), MouseRead::default());
    }

    /// If the menu stops saying it has the mouse, the game gets it back by itself in half a second
    /// (never a stuck mouse).
    #[test]
    fn the_mouse_lapses_back() {
        assert!(still_said(true, 1000, 1000));
        assert!(still_said(true, 1000, 1499));
        assert!(!still_said(true, 1000, 1500));
        assert!(!still_said(false, 1000, 1000));
    }
}
