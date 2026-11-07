//! Fly / no-clip v1: hold the body position and move it with the numpad. The character controller
//! cannot pull the player down while we rewrite the position every few milliseconds. Map axes, not
//! camera-relative (v2). The game also stops moving the player while Fly is on (`noclip`): before,
//! its collision pushed the player out of slopes every frame and the player got stuck.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

use crate::{log, memory};

/// `[globals::BODY_ROOT] + 0x20`: x y z doubles of the player's physics body.
const BODY_POS: u64 = 0x20;

static ON: AtomicBool = AtomicBool::new(false);

/// Fly loop rounds since load, for the freeze guard's report.
static TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

static STOP: AtomicBool = AtomicBool::new(false);
/// Fly Speed and Sprint are multipliers, as Stand's Levitation: 1.00 is BASE m/s, the old default.
/// Sprint is its own speed in the same units: while the fast key is held the player flies at Sprint
/// x BASE instead of Fly Speed x BASE, so Sprint below Fly Speed is slower and the same is the
/// same. Both from MIN to MAX.
pub const BASE: f64 = 8.0;
pub const MIN: f64 = 0.01;
pub const MAX: f64 = 10_000.0;
pub const SPEED_DEFAULT: f64 = 1.0;
/// 4.00, 32 m/s: the old 4x of Fly Speed 1.00 while Space is held.
pub const SPRINT_DEFAULT: f64 = 4.0;
static SPEED: Mutex<f64> = Mutex::new(SPEED_DEFAULT);
static SPRINT: Mutex<f64> = Mutex::new(SPRINT_DEFAULT);
static WORKER: Mutex<Option<std::thread::JoinHandle<Option<Handoff>>>> = Mutex::new(None);

/// Water fixes to try, in Aurora > Experiments, untested, off by default. Wait For Calm: at Fly off
/// Stand While Flying goes first and Fly Through keeps holding the player's until the game stops
/// moving the player (`after`). Snap Guard: a throw or pull by the game right after Fly off puts
/// the player back. The third, Let Game Fall, is Stand While Flying's (`airwalk::set_let_fall`).
static CALM: AtomicBool = AtomicBool::new(false);
static GUARD: AtomicBool = AtomicBool::new(false);

pub fn calm() -> bool {
    CALM.load(Ordering::Relaxed)
}

pub fn set_calm(on: bool) {
    CALM.store(on, Ordering::SeqCst);
}

pub fn guard() -> bool {
    GUARD.load(Ordering::Relaxed)
}

pub fn set_guard(on: bool) {
    GUARD.store(on, Ordering::SeqCst);
}

pub fn is_on() -> bool {
    ON.load(Ordering::Relaxed)
}

pub fn speed() -> f64 {
    SPEED.lock().map(|s| *s).unwrap_or(SPEED_DEFAULT)
}

pub fn sprint() -> f64 {
    SPRINT.lock().map(|s| *s).unwrap_or(SPRINT_DEFAULT)
}

/// A multiplier kept to two decimals, inside MIN..MAX (a NaN gives the lowest).
pub fn clamp(v: f64) -> f64 {
    if v.is_nan() { MIN } else { ((v * 100.0).round() / 100.0).clamp(MIN, MAX) }
}

pub fn set_sprint(v: f64) {
    if let Ok(mut s) = SPRINT.lock() {
        *s = clamp(v);
    }
}

pub fn set_speed(v: f64) {
    if let Ok(mut s) = SPEED.lock() {
        *s = clamp(v);
    }
}

/// Metres per second for a Fly Speed, a Sprint and whether the fast key is held: Sprint takes Fly
/// Speed's place while it is held.
pub fn metres_per_second(speed: f64, sprint: f64, fast: bool) -> f64 {
    BASE * if fast { sprint } else { speed }
}

/// Shift, Ctrl and Alt fly with the left key only: the right ones are left to the game.
pub fn left_only(vk: u32) -> u32 {
    match vk {
        0x10 => 0xA0, // Shift -> Left Shift
        0x11 => 0xA2, // Ctrl -> Left Ctrl
        0x12 => 0xA4, // Alt -> Left Alt
        v => v,
    }
}

fn down(vk: u32) -> bool {
    let vk = left_only(vk);
    // SAFETY: plain Win32 query.
    vk != 0
        && !crate::dinput::BINDING.load(Ordering::Relaxed)
        && !crate::dinput::EDITING.load(Ordering::Relaxed)
        && crate::keys::game_focused()
        && unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

/// Where the body's x y z live right now, if the path resolves.
pub fn body_addr() -> Option<u64> {
    let exe = memory::find_module("")?;
    let root = exe.base + crate::globals::BODY_ROOT.rva()?;
    let body = memory::read(root, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))?;
    if body < 0x10000 {
        return None;
    }
    let pos = body + BODY_POS;
    memory::readable(pos, 24).then_some(pos)
}

fn read_pos(addr: u64) -> Option<[f64; 3]> {
    let b = memory::read(addr, 24).ok()?;
    Some([
        f64::from_le_bytes(b[0..8].try_into().unwrap()),
        f64::from_le_bytes(b[8..16].try_into().unwrap()),
        f64::from_le_bytes(b[16..24].try_into().unwrap()),
    ])
}

fn write_pos(addr: u64, p: [f64; 3]) -> bool {
    let mut b = Vec::with_capacity(24);
    for v in p {
        b.extend_from_slice(&v.to_le_bytes());
    }
    memory::write(addr, &b).is_ok()
}

/// Turn fly on (true) or off (false). Idempotent.
pub fn set(on: bool) -> Result<(), String> {
    if on == ON.load(Ordering::SeqCst) {
        // Fly is off already: Fly off's after phase, if it still holds the player, lets go now (the
        // freeze guard, a second Fly off).
        if !on {
            end_after(END);
        }
        return Ok(());
    }
    if on {
        let Some(addr) = body_addr() else {
            return Err("body not found (be in the world)".into());
        };
        let Some(start) = read_pos(addr) else {
            return Err("could not read the body".into());
        };
        // Fly off's after phase ends first: Fly holds the player from here on.
        end_after(FLY_AGAIN);
        STOP.store(false, Ordering::SeqCst);
        if let Ok(mut j) = JUMP.lock() {
            *j = None;
        }
        // A worker that stopped by itself is finished: collect it before starting anew.
        if let Some(h) = WORKER.lock().ok().and_then(|mut w| w.take()) {
            let _ = h.join();
        }
        ON.store(true, Ordering::SeqCst);
        safe_start();
        // The hooks go in before the worker starts, so a worker that stops by itself at once always
        // takes them out again after.
        follow_fly(true);
        let handle = std::thread::spawn(move || worker(start, addr));
        if let Ok(mut w) = WORKER.lock() {
            *w = Some(handle);
        }
        log(&format!("fly on at {start:.1?}"));
    } else {
        STOP.store(true, Ordering::SeqCst);
        let handoff = WORKER.lock().ok().and_then(|mut w| w.take()).and_then(|h| h.join().ok()).flatten();
        ON.store(false, Ordering::SeqCst);
        let quitting = SAFE_QUIT.load(Ordering::SeqCst);
        // Wait For Calm only when the game was moving the player's lately (water, the ground): any
        // other Fly off lets go at once, as before.
        let settle = !quitting && calm() && handoff.as_ref().is_some_and(|h| h.copy.busy(BUSY));
        if settle {
            // Stand While Flying first, so the game may change how the player moves; Fly Through
            // keeps holding the player's until the game is calm (`after`).
            if let Err(e) = crate::airwalk::set(false) {
                log(&format!("fly: stand while flying off: {e}"));
            }
        } else {
            follow_fly(false);
        }
        log("fly off");
        match handoff {
            Some(h) if !quitting && (settle || guard() || h.copy.busy(BUSY)) => start_after(h, settle),
            _ => safe_end(),
        }
    }
    Ok(())
}

pub fn shutdown() {
    SAFE_QUIT.store(true, Ordering::SeqCst);
    let _ = set(false);
    // set(false) does nothing when fly already stopped by itself: its thread still has to be
    // collected before the DLL goes away.
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = WORKER.lock().ok().and_then(|mut w| w.take()) {
        let _ = h.join();
    }
    end_after(END);
    join_landing();
}

/// A teleport moved the player: fly holds the player at the new spot from now on.
static JUMP: Mutex<Option<[f64; 3]>> = Mutex::new(None);

pub fn jump(pos: [f64; 3]) {
    if ON.load(Ordering::SeqCst) {
        if let Ok(mut j) = JUMP.lock() {
            *j = Some(pos);
        }
    } else {
        // Fly is off: a teleport wins over Fly off's after phase, which lets go of the player at
        // once.
        let _ = AFTER_STOP.compare_exchange(RUN, END, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// God mode while flying and until the player has landed, so neither shots nor the fall hurt. Only
/// turns god off again if fly turned it on.
static SAFE: AtomicBool = AtomicBool::new(true);
/// Fly turned god mode on and still owns it.
static GOD_BY_FLY: AtomicBool = AtomicBool::new(false);
static SAFE_QUIT: AtomicBool = AtomicBool::new(false);
static LANDING: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

pub fn safe() -> bool {
    SAFE.load(Ordering::Relaxed)
}

pub fn set_safe(on: bool) {
    SAFE.store(on, Ordering::SeqCst);
}

/// God mode was switched by hand: it is the player's now, fly leaves it alone.
pub fn forget_god() {
    GOD_BY_FLY.store(false, Ordering::SeqCst);
}

fn safe_start() {
    // Fly's god mode went away under it (a world change drops the hold): the flag is stale, so fly
    // turns god on again instead of flying without it.
    if GOD_BY_FLY.load(Ordering::SeqCst) && !crate::godmode::is_on() {
        GOD_BY_FLY.store(false, Ordering::SeqCst);
    }
    if !SAFE.load(Ordering::Relaxed) || GOD_BY_FLY.load(Ordering::SeqCst) || crate::godmode::is_on() {
        return;
    }
    if crate::godmode::on_if_known() {
        GOD_BY_FLY.store(true, Ordering::SeqCst);
        log("fly safe: god mode on");
    } else {
        log("fly safe: god mode not learned yet (take one hit first)");
    }
}

fn join_landing() {
    if let Some(h) = LANDING.lock().ok().and_then(|mut l| l.take()) {
        let _ = h.join();
    }
}

/// Fly is off: once the player's height has stayed put for a second (landed), god goes off again.
fn safe_end() {
    if !GOD_BY_FLY.load(Ordering::SeqCst) {
        return;
    }
    // Unloading: no waiting for the player to land (that held up the unload for up to 90 s).
    if SAFE_QUIT.load(Ordering::SeqCst) {
        if GOD_BY_FLY.swap(false, Ordering::SeqCst) {
            let _ = crate::godmode::off();
        }
        return;
    }
    join_landing();
    let h = std::thread::spawn(|| {
        let start = Instant::now();
        let mut last_z = f64::NAN;
        let mut still_since: Option<Instant> = None;
        while start.elapsed() < Duration::from_secs(90) {
            std::thread::sleep(Duration::from_millis(100));
            if SAFE_QUIT.load(Ordering::SeqCst) || ON.load(Ordering::SeqCst) || !GOD_BY_FLY.load(Ordering::SeqCst) {
                return;
            }
            let Some(z) = body_addr().and_then(read_pos).map(|p| p[2]) else { continue };
            if (z - last_z).abs() < 0.02 {
                if still_since.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(1) {
                    break;
                }
            } else {
                still_since = None;
            }
            last_z = z;
        }
        if GOD_BY_FLY.swap(false, Ordering::SeqCst) && !ON.load(Ordering::SeqCst) {
            let _ = crate::godmode::off();
            log("fly safe: landed, god mode off");
        }
    });
    if let Ok(mut l) = LANDING.lock() {
        *l = Some(h);
    }
}

/// In the player root (`globals::CAM_ROOT`): two camera unit vectors at +0x1e0 and +0x1f0.
const RIGHT_OFF: u64 = 0x1e0;
const FORWARD_OFF: u64 = 0x1f0;

fn vec2(addr: u64) -> Option<(f64, f64)> {
    let b = memory::read(addr, 8).ok()?;
    let x = f64::from(f32::from_le_bytes(b[0..4].try_into().unwrap()));
    let y = f64::from(f32::from_le_bytes(b[4..8].try_into().unwrap()));
    let n = (x * x + y * y).sqrt();
    (n > 0.5 && n < 1.5).then(|| (x / n, y / n))
}

/// Forward and right unit vectors on the map (fx fy rx ry), from the camera. Map north / east when
/// the root cannot be read.
fn facing() -> (f64, f64, f64, f64) {
    let fallback = (0.0, 1.0, 1.0, 0.0);
    let Some(exe) = memory::find_module("") else { return fallback };
    let Some(cam) = crate::globals::CAM_ROOT.rva() else { return fallback };
    let Some(root) = memory::read(exe.base + cam, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { return fallback };
    if root < 0x10000 {
        return fallback;
    }
    heading(vec2(root + RIGHT_OFF), vec2(root + FORWARD_OFF)).unwrap_or(fallback)
}

/// Forward from the right arrow: the right arrow stays level at any pitch, so forward on the map is
/// (-ry, rx). The forward arrow's flat part shrinks as the player looks down or up and was dropped
/// past about 60 degrees, which flew the player's north / east (backwards when facing south). It is
/// only the backup now.
fn heading(right: Option<(f64, f64)>, forward: Option<(f64, f64)>) -> Option<(f64, f64, f64, f64)> {
    match (right, forward) {
        (Some((rx, ry)), _) => Some((-ry, rx, rx, ry)),
        (None, Some((fx, fy))) => Some((fx, fy, fy, -fx)),
        (None, None) => None,
    }
}

/// Hop Up and Hop Height: the first press of the up key (Shift by default) while the player stands
/// on the ground lifts the player this much at once, so the game's ground check lets go of the
/// player. Never in the air. The row shows the HUD's metres: world units x 1.5.
pub const HUD_PER_UNIT: f64 = 1.5;
/// The hop before 2.5 world units, so 3.75 m on the HUD.
const HOP_UNITS_OLD: f64 = 2.5;
pub const HOP_DEFAULT_M: f64 = HOP_UNITS_OLD * HUD_PER_UNIT;
/// 0.50 m is a small step up; 15.00 m (10 units) is a few floors of a building.
pub const HOP_MIN_M: f64 = 0.5;
pub const HOP_MAX_M: f64 = 15.0;
static HOP_UP: AtomicBool = AtomicBool::new(true);
static HOP_M: Mutex<f64> = Mutex::new(HOP_DEFAULT_M);

pub fn hop_up() -> bool {
    HOP_UP.load(Ordering::Relaxed)
}

pub fn set_hop_up(on: bool) {
    HOP_UP.store(on, Ordering::SeqCst);
}

/// Hop Height in the HUD's metres.
pub fn hop_height() -> f64 {
    HOP_M.lock().map(|h| *h).unwrap_or(HOP_DEFAULT_M)
}

pub fn set_hop_height(m: f64) {
    if let Ok(mut h) = HOP_M.lock() {
        *h = clamp_hop(m);
    }
}

/// A Hop Height kept to two decimals, inside HOP_MIN_M..HOP_MAX_M (a NaN gives the lowest).
pub fn clamp_hop(m: f64) -> f64 {
    if m.is_nan() { HOP_MIN_M } else { ((m * 100.0).round() / 100.0).clamp(HOP_MIN_M, HOP_MAX_M) }
}

/// The HUD's metres in world units, as Fly moves the player.
pub fn hop_units(m: f64) -> f64 {
    m / HUD_PER_UNIT
}

/// Does this tick hop? Only with Hop Up on, Fly on, the player's standing on the ground, and a
/// fresh press of the up key (held from before does not count). Never while the player is in the
/// air.
pub fn hop_now(hop_up: bool, fly_on: bool, on_ground: bool, up: bool, up_before: bool) -> bool {
    hop_up && fly_on && on_ground && up && !up_before
}

/// How far the player may be from where Fly found the player and still stand there (world units).
const STILL: f64 = 0.05;

/// Fly's own idea of "on the ground": Fly never looks at the ground itself, so the player stands on
/// it only while the player is still on the spot where Fly found the player's standing. Any move (W
/// A S D, up, down, a teleport) is flying.
pub fn on_ground(stood: bool, stand: [f64; 3], target: [f64; 3]) -> bool {
    stood && (0..3).all(|i| (target[i] - stand[i]).abs() <= STILL)
}

/// For this long after Fly comes on, Fly adds up how far the game still moves the player's height
/// between its writes, up or down: then the player was in the air (falling or jumping), not
/// standing.
const FALL_CHECK: Duration = Duration::from_millis(100);
/// More than this in all over FALL_CHECK (world units) is "in the air". It is a sum, so it does not
/// depend on the frame rate: about 0.5 units a second.
const AIR_SUM: f64 = 0.05;

/// How far the game moved the player's height since Fly last wrote it, up or down (0 if
/// unreadable).
pub fn game_moved(written_z: f64, read_z: f64) -> f64 {
    let d = (written_z - read_z).abs();
    if d.is_finite() { d } else { 0.0 }
}

/// The game moved the player this much in all since Fly came on: the player is in the air.
pub fn in_air(moved_sum: f64) -> bool {
    moved_sum > AIR_SUM
}

/// Fly Through's hold after this tick's write: `Some(true)` holds the player, `Some(false)` lets
/// go, `None` leaves it as it is, so the log has only the changes. The player is held once the fall
/// check is over and only while Fly could write the player's body: on a respawn or a world change
/// the game clears the player's body first, and another body could get its old address.
pub fn hold_change(holding: bool, checked: bool, wrote: bool) -> Option<bool> {
    let hold = checked && wrote;
    (hold != holding).then_some(hold)
}

/// The most one fly step may move the player, in world units. 50 units is 75 m on the HUD. A step
/// comes every 4 to 16 ms, so the player still goes 3,000 to 12,000 units a second: across the
/// island in a few seconds, but one step can never take the player's more than 50 units, never off
/// the map. Normal speeds never reach it: Sprint 4.00 is 32 units a second, at most 3.2 units in
/// one step even after a 0.1 s hitch. With one key it kicks in only above 500 units a second; with
/// a diagonal and up together (up to 1.73 times as long) from about 289.
pub const STEP_CAP: f64 = 50.0;

/// One step's move, no longer than STEP_CAP (same direction), and whether it was cut.
pub fn capped(d: [f64; 3]) -> ([f64; 3], bool) {
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len <= STEP_CAP {
        return (d, false);
    }
    if !len.is_finite() {
        return ([0.0; 3], true);
    }
    let k = STEP_CAP / len;
    ([d[0] * k, d[1] * k, d[2] * k], true)
}

/// The cap's log line is said once per Fly on.
static CAP_SAID: AtomicBool = AtomicBool::new(false);

/// Fly stopped by itself (the player's body went away or changed): off, and the landing check still
/// runs so god mode that fly turned on goes off again.
fn stop_by_itself(why: &str) {
    log(&format!("fly: {why}, stopping"));
    ON.store(false, Ordering::SeqCst);
    follow_fly(false);
    safe_end();
}

/// Stand While Flying is always on while Fly is on: its hooks go in with Fly and come out with it.
/// If they can't go in, Fly still flies; the log says why. Fly Through Everything the same.
fn follow_fly(on: bool) {
    if let Err(e) = crate::airwalk::set(on) {
        log(&format!("fly: stand while flying {}: {e}", if on { "on" } else { "off" }));
    }
    if let Err(e) = crate::noclip::set(on) {
        log(&format!("fly: fly through {}: {e}", if on { "on" } else { "off" }));
    }
}

fn worker(mut target: [f64; 3], body: u64) -> Option<Handoff> {
    TICKS.store(0, Ordering::Relaxed);
    CAP_SAID.store(false, Ordering::Relaxed);
    // Falls Stand While Flying kept from the player before this flight, and the last second of it
    // (for Fly off's trace).
    let blocked0 = crate::airwalk::blocked();
    let mut before: VecDeque<Row> = VecDeque::new();
    let started = Instant::now();
    let mut last = started;
    let mut misses = 0;
    let mut space_was = false;
    // Where Fly found the player, and whether the player stood there (for the hop).
    let stand = target;
    let mut stood = true;
    // How far the game moved the player's height in all while Fly checks (see FALL_CHECK).
    let mut moved_sum = 0.0;
    // The game leaves the player's alone (Fly Through Everything) once the fall check is over,
    // while the player's body is there (`hold_change`).
    let mut holding = false;
    // The game's own copy of the player's place, kept on the player while the player is held
    // (`noclip::keep_copy`).
    let mut copy = crate::noclip::Copy::default();
    loop {
        std::thread::sleep(Duration::from_millis(4));
        if STOP.load(Ordering::Relaxed) {
            if !holding {
                return None;
            }
            // Once more right before Fly lets go: the game puts the player where its copy is.
            crate::noclip::keep_copy(&mut copy, body - BODY_POS, target);
            copy.said();
            let blocked = crate::airwalk::blocked().saturating_sub(blocked0);
            return Some(Handoff { addr: body, target, copy, blocked, before });
        }
        TICKS.fetch_add(1, Ordering::Relaxed);
        if let Some(p) = JUMP.lock().ok().and_then(|mut j| j.take()) {
            target = p;
            stood = false;
        }
        // Only while the player is still on Fly's first spot: once the player moves or hops, the
        // game pulling at the player's says nothing about how the player was when Fly came on.
        if on_ground(stood, stand, target)
            && started.elapsed() < FALL_CHECK
            && let Some(p) = read_pos(body)
        {
            moved_sum += game_moved(target[2], p[2]);
            if in_air(moved_sum) {
                stood = false;
                log("fly: the player was in the air when Fly came on, so no hop this time");
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(last).as_secs_f64().min(0.1);
        last = now;
        let k = crate::keys::get();
        crate::dinput::set_fly_keys([k.down, k.forward, k.back, k.left, k.right, k.up, k.fast].map(left_only));
        let step = metres_per_second(speed(), sprint(), down(k.fast)) * dt;
        // Forward / right on the map, turned by the camera yaw when we know it.
        let (fx, fy, rx, ry) = facing();
        let mut d = [0.0; 3];
        if down(k.forward) {
            d[0] += fx * step;
            d[1] += fy * step;
        }
        if down(k.back) {
            d[0] -= fx * step;
            d[1] -= fy * step;
        }
        if down(k.right) {
            d[0] += rx * step;
            d[1] += ry * step;
        }
        if down(k.left) {
            d[0] -= rx * step;
            d[1] -= ry * step;
        }
        let space = down(k.up);
        if space {
            d[2] += step;
        }
        if down(k.down) {
            d[2] -= step;
        }
        let (d, cut) = capped(d);
        if cut && !CAP_SAID.swap(true, Ordering::Relaxed) {
            log(&format!("fly: a step was held to {STEP_CAP} units (Fly Speed {:.2} / Sprint {:.2} is very fast)", speed(), sprint()));
        }
        // The hop, only from the ground (looked at before this tick's move).
        if hop_now(hop_up(), ON.load(Ordering::Relaxed), on_ground(stood, stand, target), space, space_was) {
            target[2] += hop_units(hop_height());
            // The player is in the air now: no second hop, and no "in the air" log for the hop
            // itself.
            stood = false;
        }
        space_was = space;
        for (t, m) in target.iter_mut().zip(d) {
            *t += m;
        }
        let wrote = match body_addr() {
            // A new body (world change, respawn): the old spot means nothing there.
            Some(addr) if addr != body => {
                if holding {
                    copy.said();
                }
                stop_by_itself("the player's body changed");
                return None;
            }
            Some(addr) => write_pos(addr, target),
            None => false,
        };
        if wrote {
            misses = 0;
        } else {
            misses += 1;
            if misses > 100 {
                stop_by_itself("body went away");
                return None;
            }
        }
        // Let go at the first miss, not only when Fly stops (about 0.4 s later): the game clears
        // the player's body first, and a body put at its address meanwhile would be held instead.
        if let Some(hold) = hold_change(holding, started.elapsed() >= FALL_CHECK, wrote) {
            holding = hold;
            if hold {
                copy = crate::noclip::Copy::default();
                crate::noclip::hold(body - BODY_POS);
            } else {
                crate::noclip::let_go();
                copy.said();
            }
        }
        // While the player is held the game's own copy of the player's place follows the player, so
        // Fly off leaves the player where they are.
        if holding {
            crate::noclip::keep_copy(&mut copy, body - BODY_POS, target);
            let now = Instant::now();
            before.push_back(Row { at: now, phase: "fly", place: Some(target), copy: copy.seen(), moved: copy.delta() });
            while before.front().is_some_and(|r| now.duration_since(r.at) > TRACE_BEFORE) {
                before.pop_front();
            }
        }
    }
}

/// What Fly hands to its after phase at Fly off: the player's body's place address, where Fly held
/// the player, the game's copy as Fly kept it, the falls Stand While Flying kept from the player
/// this flight, and the last second of the flight.
struct Handoff {
    addr: u64,
    target: [f64; 3],
    copy: crate::noclip::Copy,
    blocked: u64,
    before: VecDeque<Row>,
}

/// One tick for the trace: when, what Fly was doing, the player's place, the game's copy of it, and
/// how far the game moved the copy since Fly's last write.
struct Row {
    at: Instant,
    phase: &'static str,
    place: Option<[f64; 3]>,
    copy: Option<[f64; 3]>,
    moved: Option<[f32; 3]>,
}

/// The after phase's ticks.
const TICK: Duration = Duration::from_millis(4);
/// The game moved its copy of the player within this long before Fly off: Wait For Calm waits, and
/// the trace is kept.
const BUSY: Duration = Duration::from_millis(500);
/// Calm: no move of the copy but a plain fall for this long (`noclip::foreign`).
const CALM_FOR: Duration = Duration::from_millis(150);
/// Wait For Calm and each Snap Guard hold end after this at the latest.
const CALM_MAX: Duration = Duration::from_millis(1500);
/// How long the after phase watches the player's once Fly let go.
const WATCH: Duration = Duration::from_secs(2);
const GUARD_TRIES: u32 = 3;
/// The trace keeps the flight's last second.
const TRACE_BEFORE: Duration = Duration::from_secs(1);
/// A throw: one step of more than this (world units)...
const JUMP_MIN: f64 = 1.0;
/// ...and much more than the step before (a fall grows step by step, even at a low frame rate where
/// one frame's step of a fast fall is over a unit).
const JUMP_RATIO: f64 = 3.0;
const JUMP_SLACK: f64 = 0.5;
/// A pull: in the first second after the let go, more than this far sideways from the spot...
const PULL_MIN: f64 = 1.0;
const PULL_FOR: Duration = Duration::from_secs(1);
/// ...while going sideways faster than this.
const PULL_SPEED: f64 = 8.0;
/// Sideways speed over at least this long.
const SPEED_SPAN: Duration = Duration::from_millis(40);
/// flytrace.log stops growing here.
const TRACE_MAX: u64 = 10 * 1024 * 1024;

/// The after phase runs, or Fly came on again, or everything ends (a teleport, the freeze guard,
/// unload, a second Fly off).
const RUN: u8 = 0;
const FLY_AGAIN: u8 = 1;
const END: u8 = 2;
static AFTER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static AFTER_STOP: AtomicU8 = AtomicU8::new(RUN);

fn start_after(h: Handoff, settle: bool) {
    end_after(END);
    AFTER_STOP.store(RUN, Ordering::SeqCst);
    let t = std::thread::spawn(move || after(h, settle));
    if let Ok(mut a) = AFTER.lock() {
        *a = Some(t);
    }
}

/// The after phase ends now (`why`), and its thread is collected.
fn end_after(why: u8) {
    let Some(t) = AFTER.lock().ok().and_then(|mut a| a.take()) else { return };
    AFTER_STOP.store(why, Ordering::SeqCst);
    let _ = t.join();
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn side(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// The game threw the player: a step (between two different places) of more than JUMP_MIN and much
/// more than the step before.
pub fn jumped(step: f64, before: f64) -> bool {
    step.is_finite() && step > JUMP_MIN && step > JUMP_RATIO * before + JUMP_SLACK
}

/// The game pulls the player's sideways from where Fly let go.
pub fn pulled(spot: [f64; 3], p: [f64; 3], speed: f64, since: Duration) -> bool {
    since <= PULL_FOR && side(spot, p) > PULL_MIN && speed > PULL_SPEED
}

/// Sideways speed to `p` from the newest point at least SPEED_SPAN old (the oldest kept if none
/// is); 0 while there is too little to say.
fn side_speed(recent: &VecDeque<([f64; 3], Instant)>, p: [f64; 3], now: Instant) -> f64 {
    let Some(&(q, t)) = recent.iter().rev().find(|r| now.duration_since(r.1) >= SPEED_SPAN).or(recent.front()) else { return 0.0 };
    let dt = now.duration_since(t).as_secs_f64();
    if dt < 0.01 { 0.0 } else { side(q, p) / dt }
}

/// How a hold of the after phase ended.
enum Held {
    /// No move but a plain fall for CALM_FOR: after this many ms.
    Calm(u128),
    /// The game kept moving the player (this many foreign moves) until CALM_MAX.
    Busy(u32),
    Stop(u8),
    Gone,
    Failed(String),
}

impl Held {
    fn why(&self) -> u8 {
        match self {
            Held::Stop(w) => *w,
            Held::Gone => END,
            _ => RUN,
        }
    }

    fn say(&self) -> String {
        match self {
            Held::Calm(ms) => format!("calm after {ms} ms"),
            Held::Busy(n) => format!("the game kept moving the player ({n} moves in {:.1} s), let go anyway", CALM_MAX.as_secs_f64()),
            Held::Stop(FLY_AGAIN) => "Fly came on again".into(),
            Held::Stop(_) => "stopped".into(),
            Held::Gone => "the player's body went away".into(),
            Held::Failed(e) => format!("could not hold the player: {e}"),
        }
    }
}

/// Hold the player at `spot` (the player's body and the game's copy, Fly Through's hook in) until
/// the game is calm or CALM_MAX has passed.
fn hold_calm(h: &mut Handoff, spot: [f64; 3], tr: &mut Trace, phase: &'static str) -> Held {
    let start = Instant::now();
    let mut quiet = start;
    let mut moves = 0;
    loop {
        std::thread::sleep(TICK);
        let why = AFTER_STOP.load(Ordering::SeqCst);
        if why != RUN {
            return Held::Stop(why);
        }
        if body_addr() != Some(h.addr) {
            return Held::Gone;
        }
        let _ = write_pos(h.addr, spot);
        crate::noclip::keep_copy(&mut h.copy, h.addr - BODY_POS, spot);
        let now = Instant::now();
        let d = h.copy.delta();
        tr.row(phase, Some(spot), h.copy.seen(), d);
        if d.is_some_and(crate::noclip::foreign) {
            moves += 1;
            quiet = now;
        }
        if now.duration_since(quiet) >= CALM_FOR {
            return Held::Calm(now.duration_since(start).as_millis());
        }
        if now.duration_since(start) >= CALM_MAX {
            return Held::Busy(moves);
        }
    }
}

/// Fly off's after phase. With Wait For Calm, first the hold until calm (Stand While Flying is out
/// already), then Fly Through lets go. Then WATCH of watching the player: the trace, and with Snap
/// Guard on, a throw or pull puts the player back and holds the player's until calm, at most
/// GUARD_TRIES times. God mode's landing check starts at the end.
fn after(mut h: Handoff, settle: bool) {
    let mut tr = Trace::new(&h, settle);
    let mut notes = vec![format!(
        "Stand While Flying kept {} falls from the player this flight; the game moved its copy {} times",
        h.blocked,
        h.copy.moves()
    )];
    let mut why = RUN;
    if settle {
        let spot = h.target;
        let r = hold_calm(&mut h, spot, &mut tr, "calm");
        notes.push(format!("wait for calm: {}", r.say()));
        why = r.why();
        crate::noclip::let_go_because("Fly off waited for calm");
        if let Err(e) = crate::noclip::set(false) {
            log(&format!("fly: fly through off: {e}"));
        }
    }
    if why == RUN {
        why = watch(&mut h, &mut tr, &mut notes);
    }
    tr.finish(&notes);
    if why != FLY_AGAIN {
        safe_end();
    }
}

/// WATCH of watching the player after the let go (see `after`).
fn watch(h: &mut Handoff, tr: &mut Trace, notes: &mut Vec<String>) -> u8 {
    let mut spot = h.target;
    let mut freed = Instant::now();
    let mut recent: VecDeque<([f64; 3], Instant)> = VecDeque::new();
    // The player's last place that differed, and the step that led there (for throws).
    let mut q = spot;
    let mut last_step = 0.0;
    let mut tries = 0;
    let mut told = false;
    loop {
        std::thread::sleep(TICK);
        let why = AFTER_STOP.load(Ordering::SeqCst);
        if why != RUN {
            return why;
        }
        if body_addr() != Some(h.addr) {
            notes.push("the player's body went away".into());
            return END;
        }
        let Some(p) = read_pos(h.addr) else { continue };
        let now = Instant::now();
        tr.row("free", Some(p), h.copy.now(), None);
        let since = now.duration_since(freed);
        let step = dist(q, p);
        let moved = step > 1e-4;
        let jump = moved && jumped(step, last_step);
        let pull = !jump && pulled(spot, p, side_speed(&recent, p, now), since);
        if (jump || pull) && !told {
            let what = if jump { format!("threw the player {step:.1} units") } else { format!("pulled the player {:.1} units sideways", side(spot, p)) };
            if guard() && tries < GUARD_TRIES {
                tries += 1;
                let back = if jump { q } else { spot };
                log(&format!("snap guard: the game {what} {} ms after the let go; putting the player back (try {tries} of {GUARD_TRIES})", since.as_millis()));
                let r = rehold(h, back, tr);
                notes.push(format!("snap guard {tries}: the game {what}; {}", r.say()));
                let why = r.why();
                if why != RUN {
                    return why;
                }
                spot = back;
                freed = Instant::now();
                recent.clear();
                q = back;
                last_step = 0.0;
                continue;
            }
            notes.push(format!("the game {what} {} ms after the let go", since.as_millis()));
            told = true;
        }
        if moved {
            q = p;
            last_step = step;
        }
        recent.push_back((p, now));
        while recent.len() > 2 && recent.front().is_some_and(|r| now.duration_since(r.1) > SPEED_SPAN * 3) {
            recent.pop_front();
        }
        if since >= WATCH {
            notes.push(format!("{:.1} s after the let go: {:.1} units sideways, {:+.1} height", since.as_secs_f64(), side(spot, p), p[2] - spot[2]));
            return RUN;
        }
    }
}

/// Snap Guard: Fly Through back in, the player's body and the game's copy at `back`, held until
/// calm, then let go again.
fn rehold(h: &mut Handoff, back: [f64; 3], tr: &mut Trace) -> Held {
    if let Err(e) = crate::noclip::set(true) {
        return Held::Failed(e);
    }
    crate::noclip::hold(h.addr - BODY_POS);
    let _ = write_pos(h.addr, back);
    let r = hold_calm(h, back, tr, "guard");
    crate::noclip::let_go_because("the snap guard let go");
    if let Err(e) = crate::noclip::set(false) {
        log(&format!("fly: fly through off: {e}"));
    }
    r
}

/// Fly off's trace: the flight's last second and the after phase, a line a tick, appended to
/// `flytrace.log` next to dll.log when the game was moving the player (at most TRACE_MAX bytes).
/// One summary line goes to dll.log.
struct Trace {
    t0: Instant,
    lines: Vec<String>,
    keep: bool,
}

fn xyz(v: Option<[f64; 3]>) -> String {
    v.map(|v| format!("{:.3} {:.3} {:.3}", v[0], v[1], v[2])).unwrap_or_else(|| "-".into())
}

impl Trace {
    fn new(h: &Handoff, settle: bool) -> Self {
        let t0 = Instant::now();
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let on = |b: bool| if b { "on" } else { "off" };
        let mut tr = Trace {
            t0,
            lines: vec![format!(
                "== fly off at unix {unix}: wait for calm {}, snap guard {}, let game fall {}; falls kept from the player {}; copy moves {}",
                on(settle),
                on(guard()),
                on(crate::airwalk::let_fall()),
                h.blocked,
                h.copy.moves()
            )],
            keep: settle || h.copy.busy(BUSY),
        };
        tr.lines.push("ms phase | place x y z | the game's copy x y z | the game moved the copy by".into());
        for r in &h.before {
            let ms = -(t0.duration_since(r.at).as_millis() as i64);
            tr.line(ms, r.phase, r.place, r.copy, r.moved);
        }
        tr
    }

    fn line(&mut self, ms: i64, phase: &str, place: Option<[f64; 3]>, copy: Option<[f64; 3]>, moved: Option<[f32; 3]>) {
        let m = moved.map(|d| format!("{:.3} {:.3} {:.3}", d[0], d[1], d[2])).unwrap_or_else(|| "-".into());
        self.lines.push(format!("{ms:+6} {phase} | {} | {} | {m}", xyz(place), xyz(copy)));
    }

    fn row(&mut self, phase: &str, place: Option<[f64; 3]>, copy: Option<[f64; 3]>, moved: Option<[f32; 3]>) {
        if moved.is_some_and(crate::noclip::foreign) {
            self.keep = true;
        }
        let ms = self.t0.elapsed().as_millis() as i64;
        self.line(ms, phase, place, copy, moved);
    }

    fn finish(mut self, notes: &[String]) {
        let summary = format!("fly off after: {}", notes.join("; "));
        log(&summary);
        if !self.keep && notes.len() < 3 {
            return;
        }
        self.lines.push(summary);
        // Never in unit tests (crate::dumper_for_writing).
        let Some(dir) = crate::dumper_for_writing() else { return };
        let path = dir.join("flytrace.log");
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > TRACE_MAX {
            return;
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all((self.lines.join("\n") + "\n").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_is_a_multiplier() {
        // Fly Speed 1.00 is the old 8 m/s; Sprint is its own speed while the fast key is held.
        assert_eq!(metres_per_second(SPEED_DEFAULT, SPRINT_DEFAULT, false), 8.0);
        assert_eq!(metres_per_second(SPEED_DEFAULT, SPRINT_DEFAULT, true), 32.0);
        assert_eq!(metres_per_second(2.0, 0.5, true), 4.0, "Sprint below Fly Speed: slower");
        assert_eq!(metres_per_second(2.0, 2.0, true), metres_per_second(2.0, 2.0, false), "the same: the same");
        assert_eq!(metres_per_second(2.0, 0.5, false), 16.0, "not held: Fly Speed");
        assert_eq!([clamp(0.0), clamp(0.004), clamp(2.104), clamp(1e9), clamp(f64::NAN)], [MIN, MIN, 2.1, MAX, MIN]);
    }

    #[test]
    fn one_step_is_capped() {
        // Normal speeds are left alone: Sprint 4 after a 0.1 s hitch is 3.2 units.
        let normal = metres_per_second(SPEED_DEFAULT, SPRINT_DEFAULT, true) * 0.1;
        assert_eq!(capped([normal, 0.0, normal]), ([normal, 0.0, normal], false));
        assert_eq!(capped([0.0; 3]), ([0.0; 3], false));
        assert_eq!(capped([30.0, 40.0, 0.0]), ([30.0, 40.0, 0.0], false), "exactly 50 is allowed");
        // The highest Sprint (10000) in one 0.1 s step: held to 50 units, same way.
        let huge = metres_per_second(MAX, MAX, true) * 0.1;
        let (d, cut) = capped([huge, huge, -huge]);
        let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        assert!(cut && (len - STEP_CAP).abs() < 1e-9, "{d:?}");
        assert!(d[0] > 0.0 && (d[0] - d[1]).abs() < 1e-9 && (d[0] + d[2]).abs() < 1e-9, "{d:?}");
        let (d, cut) = capped([300.0, 400.0, 0.0]);
        assert!(cut && (d[0] - 30.0).abs() < 1e-9 && (d[1] - 40.0).abs() < 1e-9 && d[2] == 0.0, "{d:?}");
        // Never a jump to nowhere.
        assert_eq!(capped([f64::INFINITY, 0.0, 0.0]), ([0.0; 3], true));
        // One key: only above 500 units a second can a 0.1 s step reach the cap.
        assert!(!capped([499.0 * 0.1, 0.0, 0.0]).1 && capped([501.0 * 0.1, 0.0, 0.0]).1);
        // A diagonal and up together: from about 289 units a second.
        assert!(!capped([288.0 * 0.1; 3]).1 && capped([290.0 * 0.1; 3]).1);
    }

    #[test]
    fn hop_only_from_the_ground() {
        // A fresh press of up, Fly on, standing, Hop Up on: the hop.
        assert!(hop_now(true, true, true, true, false));
        // Never in the air, never held from before, never with Hop Up or Fly off, never without up.
        assert!(!hop_now(true, true, false, true, false));
        assert!(!hop_now(true, true, true, true, true));
        assert!(!hop_now(false, true, true, true, false));
        assert!(!hop_now(true, false, true, true, false));
        assert!(!hop_now(true, true, true, false, false));
        // On the ground only on the spot where Fly found the player's standing.
        let stand = [100.0, 200.0, 30.0];
        assert!(on_ground(true, stand, stand));
        assert!(on_ground(true, stand, [100.01, 200.0, 30.04]));
        assert!(!on_ground(true, stand, [100.0, 200.0, 30.0 + hop_units(HOP_DEFAULT_M)]), "after the hop the player is in the air");
        assert!(!on_ground(true, stand, [100.0, 200.0, 30.1]), "flying up");
        assert!(!on_ground(true, stand, [100.0, 200.0, 29.9]), "flying down");
        assert!(!on_ground(true, stand, [101.0, 200.0, 30.0]), "flying along");
        assert!(!on_ground(false, stand, stand), "the player was falling or teleported");
        // In the air: the game moved the player's height, up or down, between Fly's writes. Summed
        // over the check, so the frame rate does not matter: 1 unit a second for 0.1 s is caught at
        // 30 and at 144 frames a second, standing still is not.
        assert_eq!(game_moved(30.0, 29.9), game_moved(30.0, 30.1));
        assert_eq!(game_moved(30.0, f64::NAN), 0.0);
        for fps in [30.0, 60.0, 144.0, 240.0] {
            let frames = (0.1 * fps) as usize;
            let falling: f64 = (0..frames).map(|_| game_moved(30.0, 30.0 - 1.0 / fps)).sum();
            let rising: f64 = (0..frames).map(|_| game_moved(30.0, 30.0 + 1.0 / fps)).sum();
            assert!(in_air(falling) && in_air(rising), "{fps} fps: {falling} {rising}");
        }
        assert!(!in_air(0.0) && !in_air(0.04));
    }

    #[test]
    fn fly_through_lets_go_at_the_first_miss() {
        // Not during the fall check; then held once, not again every tick.
        assert_eq!(hold_change(false, false, true), None);
        assert_eq!(hold_change(false, true, true), Some(true));
        assert_eq!(hold_change(true, true, true), None);
        // The player's body went away (a respawn, a world change): let go at the first miss, once.
        assert_eq!(hold_change(true, true, false), Some(false));
        assert_eq!(hold_change(false, true, false), None);
        // Back and written again: held again.
        assert_eq!(hold_change(false, true, true), Some(true));
        // 50 ticks without the player's body, then 50 with it: two changes in all, so two log
        // lines.
        let mut holding = true;
        let mut changes = Vec::new();
        for wrote in (0..100).map(|i| i >= 50) {
            if let Some(h) = hold_change(holding, true, wrote) {
                holding = h;
                changes.push(h);
            }
        }
        assert_eq!(changes, [false, true]);
    }

    #[test]
    fn snap_guard_tells_throws_and_pulls_from_falls() {
        // Live from standing still, 5.7 units down to the water in one frame.
        assert!(jumped(5.73, 0.0));
        // A fall from rest at 135 frames a second grows step by step: never a throw.
        let mut before = 0.0;
        for i in 1..400 {
            let step = 9.5 * f64::from(i) / 135.0 / 135.0;
            assert!(!jumped(step, before), "frame {i}");
            before = step;
        }
        // A fast fall at 30 frames a second: over a unit a frame, like the frame before.
        assert!(!jumped(1.25, 1.23));
        // A small step is never a throw, nor a broken read; a throw during a slow fall is.
        assert!(!jumped(0.9, 0.0));
        assert!(!jumped(f64::NAN, 0.0));
        assert!(jumped(3.0, 0.2));

        // Live pulled back to the shore at constant height, from where Fly let go.
        let spot = [10615.782, 11628.666, 72.588];
        let t0 = Instant::now();
        let seen = [
            (0.051, [10615.206, 11628.515, 72.588]),
            (0.102, [10614.762, 11628.397, 72.588]),
            (0.152, [10614.254, 11628.264, 72.588]),
            (0.204, [10613.613, 11628.096, 72.588]),
        ];
        let mut recent = VecDeque::from([(spot, t0)]);
        let mut caught = None;
        for (t, p) in seen {
            let now = t0 + Duration::from_secs_f64(t);
            if pulled(spot, p, side_speed(&recent, p, now), now.duration_since(t0)) {
                caught.get_or_insert(t);
            }
            recent.push_back((p, now));
        }
        assert_eq!(caught, Some(0.102), "a unit out at 9 units a second");
        // Running off at 5 units a second for a second: never a pull.
        let mut recent = VecDeque::from([(spot, t0)]);
        for i in 1..=250 {
            let t = f64::from(i) * 0.004;
            let now = t0 + Duration::from_secs_f64(t);
            let p = [spot[0] + 5.0 * t, spot[1], spot[2]];
            assert!(!pulled(spot, p, side_speed(&recent, p, now), now.duration_since(t0)), "{t}");
            recent.push_back((p, now));
        }
        // After the first second it is not a pull any more; too soon to say gives no speed.
        assert!(!pulled(spot, [spot[0] + 5.0, spot[1], spot[2]], 20.0, Duration::from_millis(1100)));
        let one = VecDeque::from([(spot, t0)]);
        assert_eq!(side_speed(&one, [spot[0] + 1.0, spot[1], spot[2]], t0 + Duration::from_millis(5)), 0.0);
    }

    #[test]
    fn hop_height_in_hud_metres() {
        // Today's 2.5 units are 3.75 m on the HUD, and back.
        assert_eq!(HOP_DEFAULT_M, 3.75);
        assert!((hop_units(HOP_DEFAULT_M) - 2.5).abs() < 1e-12);
        assert!((hop_units(15.0) - 10.0).abs() < 1e-12);
        // Two decimals, inside 0.50 to 15.00 m.
        assert_eq!([clamp_hop(0.0), clamp_hop(-3.0), clamp_hop(3.754), clamp_hop(99.0), clamp_hop(f64::NAN)], [HOP_MIN_M, HOP_MIN_M, 3.75, HOP_MAX_M, HOP_MIN_M]);
        assert_eq!([HOP_MIN_M, HOP_MAX_M], [0.5, 15.0]);
    }

    #[test]
    fn forward_comes_from_the_right_arrow() {
        // The player's game: right (0.741, 0.672), forward (-0.640, 0.706) at 17.6 degrees down
        // (flat length 0.953). Forward from the right arrow points the same way.
        let n = (0.741f64 * 0.741 + 0.672 * 0.672).sqrt();
        let (rx, ry) = (0.741 / n, 0.672 / n);
        let (fx, fy, gx, gy) = heading(Some((rx, ry)), None).unwrap();
        let (wx, wy) = (-0.640 / 0.953, 0.706 / 0.953);
        assert!((fx - wx).abs() < 0.01 && (fy - wy).abs() < 0.01, "{fx} {fy}");
        assert_eq!((gx, gy), (rx, ry));
        // Looking 75 degrees down while facing south: the forward arrow's flat part is too short
        // (dropped), the right arrow is west -> forward stays south, never north.
        let (fx, fy, _, _) = heading(Some((-1.0, 0.0)), None).unwrap();
        assert!(fy < -0.99 && fx.abs() < 1e-9, "{fx} {fy}");
        // No right arrow: the forward arrow, with right a quarter turn clockwise of it.
        let (fx, fy, rx, ry) = heading(None, Some((0.0, -1.0))).unwrap();
        assert_eq!((fx, fy), (0.0, -1.0));
        assert_eq!((rx, ry), (-1.0, -0.0));
        assert_eq!(heading(None, None), None);
    }
}
