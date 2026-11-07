//! Silent Aim. **Built** Self > Weapons > Silent Aim, Silent Aim FOV,
//! Silent Aim Range. Off at inject, like every switch.
//!
//! PvE only: the player's shots go towards an AI soldier or a drone near the player's crosshair,
//! never towards a player. Only the player's own shots change: a co-op partner's are the game's
//! own. Co-op with friends is fine; there is no player gate any more. Never PvP (Ghost War): the
//! player's rule, kept by the player; the mod can't tell a PvP match apart.
//!
//! How (research\docs\shot-trace.md): every bullet is
//! a shot object, and its origin and direction go in at one function, `0x3652430(shot, float4*
//! origin, float4* direction)` in Steam build 25349072. Its first three instructions (`mov r11,
//! rsp; push rbx; push rsi`, five bytes) become a jump to our code (the cave). The cave:
//! - leaves everything alone unless the "on" word is set, the shot's shooter (`[shot+0x90]`, a
//!   handle: its entity when bit 31 of its +0xC is set) is the player's body
//!   (`[globals::BODY_ROOT]`, read on every call), and the target is more than 1 unit from the
//!   origin;
//! - else calls the function with the direction `normalize(target - origin)` from a frame of its
//!   own (the game's direction is never written to), and returns to the game.
//!
//! The target point is picked by a worker thread here every `TICK_MS`: the nearest to the player's
//! crosshair inside the FOV cone and the range, of the AI soldiers (type & 7 == 2, not dead, with a
//! soldier's character and a side that is not friendly: animals have neither, 6 Nov
//! 2026) and the drones (type & 7 == 5, not dead). The FOV is the cone's full width: a point is in
//!       when it is at most FOV / 2 off the crosshair, which is exactly inside the circle drawn on
//!       screen (`vkdraw::circle_px`). The point: the head when the ESP's bones know it (its offset
//!       from the body, `vkdraw::head_offset`, added to the body's live position), else the chest;
//!       a drone's middle. Raised for drop by the median of what the player's last shots fell. No
//!       target: the "on" word is 0 and the player's shots are the game's own.
//!
//! The cave also keeps the last shot it steered (shot, origin, the game's direction, ours, a count)
//! so the worker can log each one.
//!
//! The jump covers three instructions, so it goes in only while every other thread is stopped and
//! none sits between them (`memory::swap_code_u64_paused`). Taking it out needs no pause. The cave
//! never calls into the DLL and is never freed, so a thread inside it at unload is fine.
//!
//! Not known yet:
//! - whether the direction given here is the one the bullet flies (the path points are made before
//!   this call, and moved into the world here with this direction: likely, not seen);
//! - bullet drop: the direction is straight at the point, with no lead for drop or a moving target;
//! - walls are not checked: a target behind cover takes the player's shot into the cover.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::{entities, globals, log, memory};

/// The shot-build entry: `mov r11, rsp; push rbx; push rsi; push r12; push r15; sub rsp, 0x108...`.
const SHOT_BUILD: &str = "4C 8B DC 53 56 41 54 41 57 48 81 EC 08";
/// The three instructions our code runs instead (five bytes).
const MOVED: [u8; 5] = [0x4C, 0x8B, 0xDC, 0x53, 0x56];
/// In a shot object: its shooter's handle.
const SHOT_SHOOTER: u32 = 0x90;
/// In a handle: the word whose bit 31 says its entity is there.
const HANDLE_LIVE: u8 = 0x0C;

/// Our block: the code (under 0x280 bytes), then the target (x y z 0, f32), the mask that clears w,
/// the "on" word and the least squared distance from the origin to the target. 16-aligned where SSE reads. After them
/// what the cave saw of the last shot it steered: the shot, its origin, the game's direction, ours,
/// and a count of steered shots (for the log and the drop measure).
const TARGET_AT: u64 = 0x280;
const MASK_AT: u64 = 0x290;
const ON_AT: u64 = 0x2A0;
const MIN_AT: u64 = 0x2A4;
const SHOT_AT: u64 = 0x2A8;
const ORIGIN_AT: u64 = 0x2B0;
const OLD_AT: u64 = 0x2C0;
const NEW_AT: u64 = 0x2D0;
/// Steered shots, the feature word (`FEAT_*`), all the player's shots.
const COUNT_AT: u64 = 0x2E0;
const FEAT_AT: u64 = 0x2E4;
const ALL_AT: u64 = 0x2E8;
const BLOCK: usize = 0x300;
/// The feature word's bits (the worker sets them every tick; 0 when off or a player is in the way).
const FEAT_MAGIC: u32 = 1;
const FEAT_RANGE: u32 = 2;
const FEAT_INSTANT: u32 = 4;
/// No Bullet Drop: before the game's call, the gravity curve is taken out of the shot's local path
/// points (their up axis follows the aim's pitch instead), see `flatten_code`. All the player's
/// shots, steered or not.
const FEAT_NODROP: u32 = 8;
/// What the features write (f32). Magic Bullets: the shot's wall budget (`+0x378`, a candidate:
/// walls subtract from it and the sweep stops at 0 or less). No Range Loss: its range (`+0x158`,
/// live 50 for the VHSD2). Instant Hit: its speed (`+0xC`, live 950) and speed / 3 (`+0x8`).
const SHOT_ENERGY: u64 = 0x378;
const SHOT_SPEED3: u64 = 0x8;
const MAGIC_ENERGY: f32 = 1.0e6;
const BIG_RANGE: f32 = 10_000.0;
const INSTANT_SPEED: f32 = 100_000.0;
/// Read for the log: lifetime, distance travelled, path points (or their pointer) and count.
const SHOT_LIFE: u64 = 0xC8;
const SHOT_TRAVELLED: u64 = 0x154;
const SHOT_PATH: u64 = 0x160;
const SHOT_PATH_COUNT: u64 = 0x176;
/// The local path points (before the build turns them into the world): a pointer at `+0x160` to
/// `float3`s (horizontal distance along, height, side; 12 bytes each), their count at `+0x16a`.
/// The game builds them before it calls the shot build, the aim's pitch and gravity both in the
/// height; the shot build turns them by the yaw only, and the shot then only follows them (the
/// mover adds no gravity of its own). Seen live: with the heights all made equal, shots flew level
/// wherever the aim pointed, so the height holds the pitch.
const SHOT_LOCAL_PATH: u32 = 0x160;
const SHOT_LOCAL_COUNT: u32 = 0x16a;
/// A point's height sits this far into it.
const POINT_UP: u8 = 4;
const POINT_SIZE: u8 = 12;
/// Magic Bullets skips a shot when a player is this close to the line from the player to the target
/// (world units); the others skip while a player is this near the crosshair (degrees).
const PLAYER_LINE_UNITS: f64 = 2.0;
const PLAYER_AIM_DEG: f64 = 10.0;
/// In a shot object (research\docs\shot-trace.md): its origin as it keeps it, and its
/// position this frame (float4 each); its speed and range (f32), read for the log.
const SHOT_ORIGIN: u64 = 0x140;
const SHOT_POS: u64 = 0x190;
const SHOT_SPEED: u64 = 0xC;
const SHOT_RANGE: u64 = 0x158;
/// The target must be at least this far from the shot's origin (world units, squared).
const MIN_LEN2: f32 = 1.0;

/// How often the target is picked again.
const TICK_MS: u64 = 25;
/// At most this many `silent aim shot` lines every 10 s (a full mag would fill the log).
const SHOT_LINES: u32 = 30;
/// How often the world list is read again (characters, drones, players).
const SCAN_EVERY: Duration = Duration::from_millis(100);
/// While the world list can't be used, the marker scan (slow) instead, at most this often.
const FALLBACK_EVERY: Duration = Duration::from_secs(2);
/// The game's distances are 1.5 times ours (`vkdraw::GAME_METRES`); the Range row is in the player's.
const GAME_METRES: f64 = 1.5;
/// A soldier's chest above its body position (world units), when the ESP knows no head: its box is
/// 1.8 tall (`vkdraw::HEIGHT`).
const CHEST_UP: f64 = 1.3;
/// In an entity: its type word. & 7: 1 player, 2 NPC, 5 drone; bit 0x4000 dead.
const E_TYPE: u64 = 0x108;
const TYPE_DEAD: u32 = 0x4000;
const KIND_PLAYER: u32 = 1;
const KIND_NPC: u32 = 2;
/// A side read 0 is friendly (`vkdraw::side_of`: 3 enemy, 0 friendly).
const SIDE_FRIENDLY: i32 = 0;

/// The FOV is the cone's full width (the circle's), degrees. 20 at inject: the same cone as the old
/// 10, which was measured from the middle.
pub const FOV_MIN: f64 = 1.0;
pub const FOV_MAX: f64 = 90.0;
pub const FOV_DEFAULT: f64 = 20.0;
pub const RANGE_MIN: f64 = 10.0;
pub const RANGE_MAX: f64 = 1000.0;
pub const RANGE_DEFAULT: f64 = 300.0;

struct Hook {
    block: u64,
    word: u64,
    old: u64,
    new: u64,
    at: u64,
    on: bool,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
/// The code search or a swap went wrong: off for this session (see the log).
static BAD: AtomicBool = AtomicBool::new(false);
/// The worker runs while this is set.
static RUN: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
/// One switch at a time (the menu, a hotkey, a profile and the worker's own turn-off).
static SWITCH: Mutex<()> = Mutex::new(());
static FOV: AtomicU32 = AtomicU32::new(0);
static RANGE: AtomicU32 = AtomicU32::new(0);
/// The jump is in (a copy of `Hook::on` the drawing reads every frame without the lock).
static ON: AtomicBool = AtomicBool::new(false);
/// Silent Aim Circle: shown while Silent Aim is on. Shown at inject.
static CIRCLE_HIDDEN: AtomicBool = AtomicBool::new(false);
/// Magic Bullets, No Range Loss, Instant Hit: the player's shots only, while Silent Aim is on (they
/// ride its hook). Off at inject.
static MAGIC: AtomicBool = AtomicBool::new(false);
static NO_RANGE: AtomicBool = AtomicBool::new(false);
static INSTANT: AtomicBool = AtomicBool::new(false);
/// No Bullet Drop: all the player's shots fly straight, Silent Aim on or off. It needs the jump in,
/// so it keeps the hook in while Silent Aim is off (no worker then: the feature word holds its bit
/// alone). Off at inject.
static NO_DROP: AtomicBool = AtomicBool::new(false);
/// Silent Aim is on (the switch, not the jump: `ON` is the jump, which No Bullet Drop shares).
static AIM: AtomicBool = AtomicBool::new(false);
/// Silent Aim Priority: 0 nearest the crosshair (at inject), 1 nearest to the player, 2 farthest
/// from the player.
static PRIORITY: AtomicU32 = AtomicU32::new(0);

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

/// Silent Aim FOV, degrees (1 to 90, 20 at inject).
pub fn fov() -> f64 {
    match FOV.load(Ordering::Relaxed) {
        0 => FOV_DEFAULT,
        b => f64::from(f32::from_bits(b)),
    }
}

pub fn set_fov(v: f64) {
    FOV.store((fov_kept(v) as f32).to_bits(), Ordering::Relaxed);
}

/// What `set_fov` keeps of `v`: whole degrees, 1 to 90. Apart so its test changes no setting: the
/// pictures test reads the FOV at the same time.
fn fov_kept(v: f64) -> f64 {
    v.round().clamp(FOV_MIN, FOV_MAX)
}

/// Silent Aim Range, metres as the player's HUD shows them (10 to 1000, 300 at inject).
pub fn range_m() -> f64 {
    match RANGE.load(Ordering::Relaxed) {
        0 => RANGE_DEFAULT,
        b => f64::from(f32::from_bits(b)),
    }
}

pub fn set_range_m(v: f64) {
    RANGE.store((range_kept(v) as f32).to_bits(), Ordering::Relaxed);
}

/// What `set_range_m` keeps of `v`: whole metres, 10 to 1000 (apart, as `fov_kept`).
fn range_kept(v: f64) -> f64 {
    v.round().clamp(RANGE_MIN, RANGE_MAX)
}

pub fn is_on() -> bool {
    AIM.load(Ordering::SeqCst)
}

/// Silent Aim Circle: shown (while Silent Aim is on).
pub fn circle() -> bool {
    !CIRCLE_HIDDEN.load(Ordering::Relaxed)
}

pub fn set_circle(on: bool) {
    CIRCLE_HIDDEN.store(!on, Ordering::Relaxed);
    if on && AIM.load(Ordering::Relaxed) {
        crate::vkdraw::want_circle();
    }
}

pub fn magic() -> bool {
    MAGIC.load(Ordering::Relaxed)
}

pub fn set_magic(on: bool) {
    MAGIC.store(on, Ordering::Relaxed);
    log(&format!("magic bullets {}", if on { "on" } else { "off" }));
}

pub fn no_range() -> bool {
    NO_RANGE.load(Ordering::Relaxed)
}

pub fn set_no_range(on: bool) {
    NO_RANGE.store(on, Ordering::Relaxed);
    log(&format!("no range loss {}", if on { "on" } else { "off" }));
}

pub fn instant() -> bool {
    INSTANT.load(Ordering::Relaxed)
}

pub fn set_instant(on: bool) {
    INSTANT.store(on, Ordering::Relaxed);
    log(&format!("instant hit {}", if on { "on" } else { "off" }));
}

pub fn no_drop() -> bool {
    NO_DROP.load(Ordering::Relaxed)
}

/// No Bullet Drop on or off. On: the jump goes in (if Silent Aim has not put it in already) and,
/// with no worker running, the feature word gets its bit. Off: the bit goes, and the jump too
/// unless Silent Aim still wants it.
pub fn set_no_drop(on: bool) -> Result<(), String> {
    let _one = SWITCH.lock().map_err(|_| "silent aim lock")?;
    if on {
        if BAD.load(Ordering::Relaxed) {
            return Err("No Bullet Drop is off for this session (see the log)".into());
        }
        let block = jump_in()?;
        NO_DROP.store(true, Ordering::SeqCst);
        if !is_on() {
            write_feat(block, FEAT_NODROP);
        }
        log("no bullet drop on");
    } else {
        NO_DROP.store(false, Ordering::SeqCst);
        let Ok(mut slot) = HOOK.lock() else { return Err("silent aim lock".into()) };
        if let Some(h) = slot.as_mut()
            && !is_on()
        {
            write_feat(h.block, 0);
            if h.on && let Err(e) = swap(h, false) {
                log(&format!("silent aim: {e}"));
            }
        }
        log("no bullet drop off");
    }
    Ok(())
}

/// Is a player within `r` of the line from `a` to `b` (Magic Bullets would send the player's shot
/// through walls along it)?
pub fn player_near_line(a: [f64; 3], b: [f64; 3], players: &[[f64; 3]], r: f64) -> bool {
    let ab = sub(b, a);
    let l2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    players.iter().any(|p| {
        let ap = sub(*p, a);
        let t = if l2 > 1e-9 { ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / l2).clamp(0.0, 1.0) } else { 0.0 };
        let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
        let d = len(sub(*p, q));
        d.is_nan() || d <= r
    })
}

/// Is a player within `deg` of the player's crosshair (from `eye` along `fwd`)?
pub fn player_near_aim(eye: [f64; 3], fwd: [f64; 3], players: &[[f64; 3]], deg: f64) -> bool {
    players.iter().any(|p| {
        let a = angle_deg(eye, fwd, *p);
        a.is_nan() || a <= deg
    })
}

/// The feature word for this tick: each switch on, and nobody in the way. Magic Bullets only with a
/// target and no player near the line to it; the others not while a player is near the player's
/// crosshair. Never anything for a shot that could reach a player.
pub fn features(target: Option<[f64; 3]>, eye: [f64; 3], fwd: [f64; 3], players: &[[f64; 3]], on: (bool, bool, bool)) -> u32 {
    let mut f = 0;
    if on.0
        && let Some(t) = target
        && !player_near_line(eye, t, players, PLAYER_LINE_UNITS)
    {
        f |= FEAT_MAGIC;
    }
    let clear = !player_near_aim(eye, fwd, players, PLAYER_AIM_DEG);
    if on.1 && clear {
        f |= FEAT_RANGE;
    }
    if on.2 && clear {
        f |= FEAT_INSTANT;
    }
    f
}

/// The circle to draw now: the FOV in degrees, when Silent Aim is on and its circle shown.
pub fn circle_deg() -> Option<f64> {
    (AIM.load(Ordering::Relaxed) && circle()).then(fov)
}

/// Which target wins when several are inside the circle and the range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// The one nearest the crosshair (at inject).
    Crosshair,
    /// The one nearest to the player.
    Nearest,
    /// The one farthest from the player.
    Farthest,
}

impl Priority {
    pub const ALL: [Priority; 3] = [Priority::Crosshair, Priority::Nearest, Priority::Farthest];

    /// As the menu row shows it.
    pub fn name(self) -> &'static str {
        match self {
            Priority::Crosshair => "Crosshair",
            Priority::Nearest => "Nearest",
            Priority::Farthest => "Farthest",
        }
    }

    /// From a typed or saved word (any case; the old long names too).
    pub fn parse(s: &str) -> Option<Priority> {
        let s: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        match s.as_str() {
            "crosshair" | "closesttocrosshair" | "0" => Some(Priority::Crosshair),
            "nearest" | "nearestdistance" | "closest" | "1" => Some(Priority::Nearest),
            "farthest" | "farthestdistance" | "furthest" | "2" => Some(Priority::Farthest),
            _ => None,
        }
    }
}

pub fn priority() -> Priority {
    priority_at(PRIORITY.load(Ordering::Relaxed))
}

pub fn set_priority(p: Priority) {
    PRIORITY.store(priority_slot(p), Ordering::Relaxed);
}

/// The number `set_priority` keeps for `p`; `priority_at` reads it back. Apart so their test
/// changes no setting: the pictures test reads Priority at the same time.
fn priority_slot(p: Priority) -> u32 {
    Priority::ALL.iter().position(|q| *q == p).unwrap_or(0) as u32
}

fn priority_at(slot: u32) -> Priority {
    Priority::ALL.get(slot as usize).copied().unwrap_or(Priority::Crosshair)
}

// ---------------------------------------------------------------------------------------------
// Target choice (plain maths, tested without the game)
// ---------------------------------------------------------------------------------------------

/// What a candidate is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum What {
    Soldier,
    Drone,
    /// Another player: never a target.
    Player,
}

/// A possible target as the worker sees it this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cand {
    pub what: What,
    /// The point the player's shot would go to.
    pub point: [f64; 3],
    pub dead: bool,
    /// Its side when known (`SIDE_FRIENDLY` is left out).
    pub side: Option<i32>,
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn len(a: [f64; 3]) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}

/// Degrees between the crosshair ray (from `eye` along the unit `fwd`) and the line to `p`.
pub fn angle_deg(eye: [f64; 3], fwd: [f64; 3], p: [f64; 3]) -> f64 {
    let d = sub(p, eye);
    let n = len(d);
    if !n.is_finite() || n < 1e-6 {
        return 180.0;
    }
    let c = (d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2]) / n;
    if !c.is_finite() {
        return 180.0;
    }
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// May this one be a target at all: a living soldier with a known side that is not friendly, or a
/// living drone. Never a player. A "soldier" whose side can't be read is left out: animals pass the
/// type word (& 7 == 2) but have no soldier's character to read a side from.
pub fn allowed(c: &Cand) -> bool {
    match c.what {
        What::Player => false,
        What::Soldier => !c.dead && matches!(c.side, Some(s) if s != SIDE_FRIENDLY),
        What::Drone => !c.dead,
    }
}

/// Is a point `off_deg` off the crosshair inside a cone `fov_deg` wide (the circle on screen)?
pub fn in_cone(off_deg: f64, fov_deg: f64) -> bool {
    off_deg <= fov_deg / 2.0
}

/// Of the candidates inside the cone `fov_deg` wide (`in_cone`, the circle) and within `range`
/// world units of `me` (the player's body), the one `prio` puts first; its index. Ties: nearer the
/// crosshair, then nearer to the player.
pub fn pick(eye: [f64; 3], fwd: [f64; 3], me: [f64; 3], cands: &[Cand], fov_deg: f64, range: f64, prio: Priority) -> Option<usize> {
    let cmp = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    cands
        .iter()
        .enumerate()
        .filter(|(_, c)| allowed(c) && c.point.iter().all(|v| v.is_finite()))
        .map(|(i, c)| (i, angle_deg(eye, fwd, c.point), len(sub(c.point, me))))
        .filter(|&(_, a, d)| in_cone(a, fov_deg) && d <= range)
        .min_by(|a, b| {
            let first = match prio {
                Priority::Crosshair => cmp(a.1, b.1),
                Priority::Nearest => cmp(a.2, b.2),
                Priority::Farthest => cmp(b.2, a.2),
            };
            first.then(cmp(a.1, b.1)).then(cmp(a.2, b.2))
        })
        .map(|(i, _, _)| i)
}

/// Bullet drop. The raise comes from physics: `fall = g/2 * (distance / speed)^2`, with the speed
/// the player's last shot had (`+0xC`, 950 for the VHSD2 in the player's log). The player's test,
/// b313a91: the drop learned from the player's shots (a median of measured falls) came out 30 times
/// too big (k 1.70e-4: 1.48 units over the head at 93 units; gravity gives 0.05) and the player's
/// shots went over the head. Raised shots measured more fall the more they were raised, so the
/// learning fed itself. The measured falls are still kept, for the log only.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Drop {
    /// The player's last shot's speed (world units per second); 0 until one was seen.
    pub speed: f64,
    /// Measured: the median `k` of the last `DROP_KEEP` shots (log only), and how many.
    pub measured: f64,
    pub shots: u32,
    last: [f64; DROP_KEEP],
}

const DROP_KEEP: usize = 15;
/// g/2 in world units (the player's unraised shots: 0.054 units over 105 at 950 units/s, as this
/// gives).
const HALF_G: f64 = 4.905;
/// The speed until one of the player's shots was read (the VHSD2's, from the player's log).
const SPEED_DEFAULT: f64 = 950.0;
/// Speeds believed from a shot (below: a misread; above: no drop to speak of).
const SPEED_MIN: f64 = 100.0;
const SPEED_MAX: f64 = 1.0e5;
/// Largest believable measured `k`: a bullet at 100 units/s.
const DROP_K_MAX: f64 = HALF_G / (SPEED_MIN * SPEED_MIN);
/// A shot is measured only once it flew this far (world units).
const DROP_MIN_DIST: f64 = 25.0;

impl Drop {
    /// `k` in `fall = k * distance^2`, from the speed.
    pub fn k(&self) -> f64 {
        let v = if self.speed > 0.0 { self.speed } else { SPEED_DEFAULT };
        HALF_G / (v * v)
    }

    /// The speed of a shot the player just fired; a misread changes nothing.
    pub fn set_speed(&mut self, v: f64) {
        if v.is_finite() && (SPEED_MIN..=SPEED_MAX).contains(&v) {
            self.speed = v;
        }
    }

    /// One measured shot (log only): it flew `along` units along its direction and fell `fell`
    /// units below that line. Nothing changes for a short or unbelievable one.
    pub fn add(&mut self, along: f64, fell: f64) -> bool {
        if !(along.is_finite() && fell.is_finite()) || along < DROP_MIN_DIST {
            return false;
        }
        let k = fell / (along * along);
        if k.abs() > DROP_K_MAX {
            return false;
        }
        self.last[self.shots as usize % DROP_KEEP] = k;
        self.shots += 1;
        let n = (self.shots as usize).min(DROP_KEEP);
        let mut v = self.last[..n].to_vec();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.measured = if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 };
        true
    }

    /// Where to aim so a shot from `from` falls onto `p`.
    pub fn raise(&self, from: [f64; 3], p: [f64; 3]) -> [f64; 3] {
        let d = len(sub(p, from));
        [p[0], p[1], p[2] + self.k() * d * d]
    }
}

/// How far a shot at `pos` flew from `origin` along the unit `dir`, and how far it fell below that
/// line (positive: below).
pub fn flown(origin: [f64; 3], dir: [f64; 3], pos: [f64; 3]) -> (f64, f64) {
    let v = sub(pos, origin);
    let along = v[0] * dir[0] + v[1] * dir[1] + v[2] * dir[2];
    let on_line = [origin[0] + dir[0] * along, origin[1] + dir[1] * along, origin[2] + dir[2] * along];
    (along, on_line[2] - pos[2])
}

/// The Range row (HUD metres) in world units.
pub fn range_units(m: f64) -> f64 {
    m.clamp(RANGE_MIN, RANGE_MAX) / GAME_METRES
}

// --------------------------------------------------------------------------------------------- The
// cave
// ---------------------------------------------------------------------------------------------

/// Our code at `cave` for the shot-build entry at `at`; `body_root` holds the player's body.
///
/// Every shot: when neither the "on" word nor the feature word is set, or the shot is not the player's,
/// straight on into the game's function (GO). Own shot: the shot, its origin and the game's direction
/// are kept for the log and a count goes up; then, in a frame of our own, the game's function is
/// called, with our direction (to the target) when "on" is set and the target is far enough from
/// the origin, else with the game's own. After it returns, the feature word's writes go into the
/// player's shot (Magic Bullets only on a steered shot): they come after the game's own set-up, so
/// it can't overwrite them. The game's return value (rax) is kept.
fn cave_code(cave: u64, at: u64, body_root: u64) -> Result<Vec<u8>, String> {
    let mut c: Vec<u8> = Vec::new();
    // Near jumps (0F 8x rel32) to GO and to NOSTEER, patched at the end.
    let mut to_go: Vec<usize> = Vec::new();
    let mut to_nosteer: Vec<usize> = Vec::new();
    let jcc = |c: &mut Vec<u8>, list: &mut Vec<usize>, op: u8| {
        c.extend_from_slice(&[0x0F, op, 0, 0, 0, 0]);
        list.push(c.len() - 4);
    };
    let rip = |c: &Vec<u8>, to: u64| rel(cave + c.len() as u64 + 4, to);
    // Mov eax, [rip+on]; or eax, [rip+feat]; jz GO
    c.extend_from_slice(&[0x8B, 0x05]);
    let r = rip(&c, cave + ON_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x0B, 0x05]);
    let r = rip(&c, cave + FEAT_AT)?;
    c.extend_from_slice(&r);
    jcc(&mut c, &mut to_go, 0x84);
    // Test rdx, rdx; jz GO; test r8, r8; jz GO
    c.extend_from_slice(&[0x48, 0x85, 0xD2]);
    jcc(&mut c, &mut to_go, 0x84);
    c.extend_from_slice(&[0x4D, 0x85, 0xC0]);
    jcc(&mut c, &mut to_go, 0x84);
    // Mov rax, [rcx+SHOT_SHOOTER]; test rax, rax; jz GO
    c.extend_from_slice(&[0x48, 0x8B, 0x81]);
    c.extend_from_slice(&SHOT_SHOOTER.to_le_bytes());
    c.extend_from_slice(&[0x48, 0x85, 0xC0]);
    jcc(&mut c, &mut to_go, 0x84);
    // Cmp dword [rax+0xC], 0; jge GO (bit 31 clear: no entity)
    c.extend_from_slice(&[0x83, 0x78, HANDLE_LIVE, 0x00]);
    jcc(&mut c, &mut to_go, 0x8D);
    // Mov rax, [rax]; test rax, rax; jz GO
    c.extend_from_slice(&[0x48, 0x8B, 0x00, 0x48, 0x85, 0xC0]);
    jcc(&mut c, &mut to_go, 0x84);
    // Mov r10, body_root; cmp rax, [r10]; jne GO
    c.extend_from_slice(&[0x49, 0xBA]);
    c.extend_from_slice(&body_root.to_le_bytes());
    c.extend_from_slice(&[0x49, 0x3B, 0x02]);
    jcc(&mut c, &mut to_go, 0x85);
    // Own shot. For the log: the shot, its origin, the game's direction, and a count.
    c.extend_from_slice(&[0x48, 0x89, 0x0D]); // Mov [rip+shot], rcx
    let r = rip(&c, cave + SHOT_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x0F, 0x10, 0x0A]); // Movups xmm1, [rdx]
    c.extend_from_slice(&[0x0F, 0x11, 0x0D]); // Movups [rip+origin], xmm1
    let r = rip(&c, cave + ORIGIN_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x41, 0x0F, 0x10, 0x18]); // Movups xmm3, [r8]
    c.extend_from_slice(&[0x0F, 0x11, 0x1D]); // Movups [rip+old], xmm3
    let r = rip(&c, cave + OLD_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0xF0, 0xFF, 0x05]); // Lock inc dword [rip+all]
    let r = rip(&c, cave + ALL_AT)?;
    c.extend_from_slice(&r);
    // A frame of our own: home space, the player's stack slots 5 to 7 copied (should the function
    // take more than its three), our direction at +0x40, the shot at +0x50, "steered" at +0x58.
    c.extend_from_slice(&[0x48, 0x83, 0xEC, FRAME]); // Sub rsp, FRAME
    for (from, to) in [(u32::from(FRAME) + 0x28, 0x20u8), (u32::from(FRAME) + 0x30, 0x28), (u32::from(FRAME) + 0x38, 0x30)] {
        c.extend_from_slice(&[0x48, 0x8B, 0x84, 0x24]); // Mov rax, [rsp+from]
        c.extend_from_slice(&from.to_le_bytes());
        c.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, to]); // Mov [rsp+to], rax
    }
    c.extend_from_slice(&[0x48, 0x89, 0x4C, 0x24, 0x50]); // Mov [rsp+0x50], rcx
    c.extend_from_slice(&[0xC6, 0x44, 0x24, 0x58, 0x00]); // Mov byte [rsp+0x58], 0
    // Steer? Mov eax, [rip+on]; test eax, eax; jz NOSTEER
    c.extend_from_slice(&[0x8B, 0x05]);
    let r = rip(&c, cave + ON_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x85, 0xC0]);
    jcc(&mut c, &mut to_nosteer, 0x84);
    // xmm0 = (target - origin) with w cleared (xmm1 still holds the origin).
    c.extend_from_slice(&[0x0F, 0x10, 0x05]); // Movups xmm0, [rip+target]
    let r = rip(&c, cave + TARGET_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x0F, 0x5C, 0xC1]); // Subps xmm0, xmm1
    c.extend_from_slice(&[0x0F, 0x54, 0x05]); // Andps xmm0, [rip+mask]
    let r = rip(&c, cave + MASK_AT)?;
    c.extend_from_slice(&r);
    // xmm1 = x*x + y*y + z*z
    c.extend_from_slice(&[0x0F, 0x28, 0xC8]); // Movaps xmm1, xmm0
    c.extend_from_slice(&[0x0F, 0x59, 0xC8]); // Mulps xmm1, xmm0
    c.extend_from_slice(&[0x0F, 0x28, 0xD1]); // Movaps xmm2, xmm1
    c.extend_from_slice(&[0x0F, 0xC6, 0xD2, 0x55]); // Shufps xmm2, xmm2, 0x55 (y*y)
    c.extend_from_slice(&[0x0F, 0x28, 0xD9]); // Movaps xmm3, xmm1
    c.extend_from_slice(&[0x0F, 0xC6, 0xDB, 0xAA]); // Shufps xmm3, xmm3, 0xAA (z*z)
    c.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xCA]); // Addss xmm1, xmm2
    c.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xCB]); // Addss xmm1, xmm3
    // Comiss xmm1, [rip+min]; jbe NOSTEER (too near, or not a number: unordered takes it too)
    c.extend_from_slice(&[0x0F, 0x2F, 0x0D]);
    let r = rip(&c, cave + MIN_AT)?;
    c.extend_from_slice(&r);
    jcc(&mut c, &mut to_nosteer, 0x86);
    c.extend_from_slice(&[0xF3, 0x0F, 0x51, 0xC9]); // Sqrtss xmm1, xmm1
    c.extend_from_slice(&[0x0F, 0xC6, 0xC9, 0x00]); // Shufps xmm1, xmm1, 0
    c.extend_from_slice(&[0x0F, 0x5E, 0xC1]); // Divps xmm0, xmm1: the unit direction, w 0
    c.extend_from_slice(&[0x0F, 0x11, 0x05]); // Movups [rip+new], xmm0 (for the log)
    let r = rip(&c, cave + NEW_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0xF0, 0xFF, 0x05]); // Lock inc dword [rip+count]
    let r = rip(&c, cave + COUNT_AT)?;
    c.extend_from_slice(&r);
    c.extend_from_slice(&[0x0F, 0x11, 0x44, 0x24, 0x40]); // Movups [rsp+0x40], xmm0
    c.extend_from_slice(&[0x4C, 0x8D, 0x44, 0x24, 0x40]); // Lea r8, [rsp+0x40]
    c.extend_from_slice(&[0xC6, 0x44, 0x24, 0x58, 0x01]); // Mov byte [rsp+0x58], 1
    // NOSTEER: No Bullet Drop first (the gravity curve out of the local path points, before the game
    // turns them into the world; the direction at r8 gives the pitch to keep), then call TRAMP (rcx,
    // rdx untouched; r8 ours or the game's).
    let nosteer = c.len();
    // The feature word's rel32 sits 3 bytes into the flatten code, so it counts from there + 4.
    let r = rel(cave + c.len() as u64 + 7, cave + FEAT_AT)?;
    c.extend_from_slice(&flatten_code(r));
    c.push(0xE8);
    let call_at = c.len();
    c.extend_from_slice(&[0; 4]);
    // After: the player's shot again, the feature word in r10d (rax keeps the game's return value).
    c.extend_from_slice(&[0x48, 0x8B, 0x4C, 0x24, 0x50]); // Mov rcx, [rsp+0x50]
    c.extend_from_slice(&[0x44, 0x8B, 0x15]); // Mov r10d, [rip+feat]
    let r = rip(&c, cave + FEAT_AT)?;
    c.extend_from_slice(&r);
    // Each feature: test r10d, bit; jz past; (Magic: cmp byte [rsp+0x58], 0; je past;) mov dword
    // [rcx+off], value.
    for (bit, writes, steered_only) in feature_writes() {
        let mut body: Vec<u8> = Vec::new();
        if steered_only {
            body.extend_from_slice(&[0x80, 0x7C, 0x24, 0x58, 0x00]); // Cmp byte [rsp+0x58], 0
            let skip: usize = writes.iter().map(|(off, _)| if *off < 0x80 { 7 } else { 10 }).sum();
            body.extend_from_slice(&[0x74, skip as u8]); // Je past
        }
        for (off, val) in writes {
            if off < 0x80 {
                body.extend_from_slice(&[0xC7, 0x41, off as u8]); // Mov dword [rcx+off8], imm32
            } else {
                body.extend_from_slice(&[0xC7, 0x81]); // Mov dword [rcx+off32], imm32
                body.extend_from_slice(&off.to_le_bytes());
            }
            body.extend_from_slice(&val.to_le_bytes());
        }
        c.extend_from_slice(&[0x41, 0xF7, 0xC2]); // Test r10d, imm32
        c.extend_from_slice(&bit.to_le_bytes());
        c.extend_from_slice(&[0x74, body.len() as u8]); // Jz past
        c.extend_from_slice(&body);
    }
    c.extend_from_slice(&[0x48, 0x83, 0xC4, FRAME]); // Add rsp, FRAME
    c.push(0xC3); // ret
    // GO / TRAMP: the moved instructions, then on into the game's function.
    let go = c.len();
    c.extend_from_slice(&MOVED);
    c.push(0xE9);
    let r = rip(&c, at + MOVED.len() as u64)?;
    c.extend_from_slice(&r);
    let call = rel(cave + call_at as u64 + 4, cave + go as u64)?;
    c[call_at..call_at + 4].copy_from_slice(&call);
    for (list, to) in [(&to_go, go), (&to_nosteer, nosteer)] {
        for &from in list {
            let r = rel(cave + from as u64 + 4, cave + to as u64)?;
            c[from..from + 4].copy_from_slice(&r);
        }
    }
    if c.len() as u64 > TARGET_AT {
        return Err("silent aim code too long".into());
    }
    Ok(c)
}

/// Our frame: 0x68 more, so the stack is 16-aligned at the call (entry is 8 off).
const FRAME: u8 = 0x68;

/// No Bullet Drop, in the cave before the game's call: when the feature word has its bit, every
/// local path point's height becomes the first point's plus its distance along times the aim's
/// rise over run (`dz / sqrt(dx² + dy²)` of the direction at `r8`: ours when steered, else the
/// game's). That keeps the pitch and takes out the gravity curve, which is all the game added.
/// Nothing is done with fewer than two points, no pointer, or a direction straight up or down (no
/// horizontal run). `feat` is the rel32 to the feature word from the end of the `mov r10d,
/// [rip+feat]`. Uses rax, r10 and xmm0 to xmm4 (all free here: the shot's shooter is checked, our
/// direction is in the frame, and the game's function is about to be called).
///
/// ```text
///   mov r10d, [rip+feat]; test r10d, FEAT_NODROP; jz CALL
///   mov rax, [rcx+0x160]; test rax, rax; jz CALL          the points
///   movzx r10d, word [rcx+0x16a]; cmp r10d, 2; jb CALL    their count
///   movss xmm1, [r8]; mulss xmm1, xmm1                    dx²
///   movss xmm2, [r8+4]; mulss xmm2, xmm2; addss xmm1, xmm2; sqrtss xmm1, xmm1   run
///   xorps xmm3, xmm3; ucomiss xmm1, xmm3; jbe CALL        no run (or not a number)
///   movss xmm2, [r8+8]; divss xmm2, xmm1                  k = rise / run
///   movss xmm0, [rax+4]; movss xmm3, [rax]                the first point's height and distance
/// LOOP: add rax, 12
///   movss xmm4, [rax]; subss xmm4, xmm3; mulss xmm4, xmm2; addss xmm4, xmm0
///   movss [rax+4], xmm4
///   dec r10d; cmp r10d, 1; ja LOOP
/// CALL:
/// ```
fn flatten_code(feat: [u8; 4]) -> Vec<u8> {
    let mut c: Vec<u8> = Vec::new();
    let mut loop_body: Vec<u8> = vec![0x48, 0x83, 0xC0, POINT_SIZE]; // Add rax, 12
    loop_body.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x20]); // Movss xmm4, [rax]
    loop_body.extend_from_slice(&[0xF3, 0x0F, 0x5C, 0xE3]); // Subss xmm4, xmm3
    loop_body.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xE2]); // Mulss xmm4, xmm2
    loop_body.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xE0]); // Addss xmm4, xmm0
    loop_body.extend_from_slice(&[0xF3, 0x0F, 0x11, 0x60, POINT_UP]); // Movss [rax+4], xmm4
    loop_body.extend_from_slice(&[0x41, 0xFF, 0xCA]); // Dec r10d
    loop_body.extend_from_slice(&[0x41, 0x83, 0xFA, 0x01]); // Cmp r10d, 1
    let back = -(loop_body.len() as i8 + 2);
    loop_body.extend_from_slice(&[0x77, back as u8]); // Ja LOOP
    // After the run check: k, the first point, the loop.
    let mut slope: Vec<u8> = vec![0xF3, 0x41, 0x0F, 0x10, 0x50, 0x08]; // Movss xmm2, [r8+8]
    slope.extend_from_slice(&[0xF3, 0x0F, 0x5E, 0xD1]); // Divss xmm2, xmm1
    slope.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x40, POINT_UP]); // Movss xmm0, [rax+4]
    slope.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x18]); // Movss xmm3, [rax]
    slope.extend_from_slice(&loop_body);
    // After the count check: the run, its check, then the rest.
    let mut run: Vec<u8> = vec![0xF3, 0x41, 0x0F, 0x10, 0x08]; // Movss xmm1, [r8]
    run.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xC9]); // Mulss xmm1, xmm1
    run.extend_from_slice(&[0xF3, 0x41, 0x0F, 0x10, 0x50, 0x04]); // Movss xmm2, [r8+4]
    run.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xD2]); // Mulss xmm2, xmm2
    run.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xCA]); // Addss xmm1, xmm2
    run.extend_from_slice(&[0xF3, 0x0F, 0x51, 0xC9]); // Sqrtss xmm1, xmm1
    run.extend_from_slice(&[0x0F, 0x57, 0xDB]); // Xorps xmm3, xmm3
    run.extend_from_slice(&[0x0F, 0x2E, 0xCB]); // Ucomiss xmm1, xmm3
    run.extend_from_slice(&[0x76, slope.len() as u8]); // Jbe CALL
    run.extend_from_slice(&slope);
    let mut count: Vec<u8> = vec![0x44, 0x0F, 0xB7, 0x91]; // Movzx r10d, word [rcx+0x16a]
    count.extend_from_slice(&SHOT_LOCAL_COUNT.to_le_bytes());
    count.extend_from_slice(&[0x41, 0x83, 0xFA, 0x02]); // Cmp r10d, 2
    count.extend_from_slice(&[0x72, run.len() as u8]); // Jb CALL
    count.extend_from_slice(&run);
    let mut points: Vec<u8> = vec![0x48, 0x8B, 0x81]; // Mov rax, [rcx+0x160]
    points.extend_from_slice(&SHOT_LOCAL_PATH.to_le_bytes());
    points.extend_from_slice(&[0x48, 0x85, 0xC0]); // Test rax, rax
    points.extend_from_slice(&[0x74, count.len() as u8]); // Jz CALL
    points.extend_from_slice(&count);
    c.extend_from_slice(&[0x44, 0x8B, 0x15]); // Mov r10d, [rip+feat]
    c.extend_from_slice(&feat);
    c.extend_from_slice(&[0x41, 0xF7, 0xC2]); // Test r10d, imm32
    c.extend_from_slice(&FEAT_NODROP.to_le_bytes());
    c.extend_from_slice(&[0x74, points.len() as u8]); // Jz CALL
    c.extend_from_slice(&points);
    c
}

/// The feature word's bits, and what each writes into the player's shot after the game set it up:
/// (bit, [(offset, f32 bits)], only on a steered shot).
type FeatureWrite = (u32, Vec<(u32, u32)>, bool);

fn feature_writes() -> [FeatureWrite; 3] {
    [
        (FEAT_MAGIC, vec![(SHOT_ENERGY as u32, MAGIC_ENERGY.to_bits())], true),
        (FEAT_RANGE, vec![(SHOT_RANGE as u32, BIG_RANGE.to_bits())], false),
        (FEAT_INSTANT, vec![(SHOT_SPEED as u32, INSTANT_SPEED.to_bits()), (SHOT_SPEED3 as u32, (INSTANT_SPEED / 3.0).to_bits())], false),
    ]
}

/// What `flatten_code` does to the local points, in plain numbers (its unit test checks the
/// bytes against this): each point's height after the first becomes the first's plus the distance
/// along from the first times the aim's rise over run. `dir` is the direction (x, y, z up).
#[cfg(test)]
pub fn flattened(points: &[[f32; 3]], dir: [f32; 3]) -> Vec<[f32; 3]> {
    let mut out = points.to_vec();
    let run = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
    if out.len() < 2 || run.is_nan() || run <= 0.0 {
        return out;
    }
    let k = dir[2] / run;
    let (x0, y0) = (out[0][0], out[0][1]);
    for p in out.iter_mut().skip(1) {
        p[1] = (p[0] - x0) * k + y0;
    }
    out
}

fn prepare() -> Result<Hook, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let body_root = exe.base + globals::BODY_ROOT.rva().ok_or("the player's body's place was not found")?;
    let at = memory::find_code(&exe, "silent aim shot build", SHOT_BUILD)?;
    if memory::read(at, MOVED.len())? != MOVED {
        return Err("the shot-build start is not as expected".into());
    }
    let word = at & !7;
    if at + 5 > word + 8 {
        return Err("the shot-build start crosses a code word".into());
    }
    let block = memory::alloc_code_near(exe.base, BLOCK).ok_or("no room for the silent aim code")?;
    let mut data = vec![0u8; 16];
    for _ in 0..3 {
        data.extend_from_slice(&u32::MAX.to_le_bytes());
    }
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes()); // on: 0
    data.extend_from_slice(&MIN_LEN2.to_le_bytes());
    data.resize(BLOCK - TARGET_AT as usize, 0); // The shot record and the count start at 0
    memory::write(block + TARGET_AT, &data)?;
    memory::write(block, &cave_code(block, at, body_root)?)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (at - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(at + 5, block)?);
    log(&format!("silent aim ready: shot build exe+{:#x}, code at {block:#x}", at - exe.base));
    Ok(Hook { block, word, old, new: u64::from_le_bytes(new_bytes.try_into().unwrap()), at, on: false })
}

/// The code search at load, so the first switch-on is quick. Nothing goes in.
pub fn warm() {
    let mut slot = match HOOK.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if slot.is_some() || BAD.load(Ordering::Relaxed) {
        return;
    }
    match prepare() {
        Ok(h) => *slot = Some(h),
        Err(e) => {
            log(&format!("silent aim: {e}"));
            BAD.store(true, Ordering::SeqCst);
        }
    }
}

/// The target point into the block, or none (`on` 0: the player's shots are the game's own).
fn write_target(block: u64, p: Option<[f64; 3]>) {
    match p {
        Some(p) => {
            let mut b = Vec::with_capacity(16);
            for v in p {
                b.extend_from_slice(&(v as f32).to_le_bytes());
            }
            b.extend_from_slice(&0f32.to_le_bytes());
            let _ = memory::write(block + TARGET_AT, &b);
            let _ = memory::write(block + ON_AT, &1u32.to_le_bytes());
        }
        None => {
            let _ = memory::write(block + ON_AT, &0u32.to_le_bytes());
        }
    }
}

fn write_feat(block: u64, f: u32) {
    let _ = memory::write(block + FEAT_AT, &f.to_le_bytes());
}

/// Swap the jump in or out. Called with the `SWITCH` lock held.
fn swap(h: &mut Hook, on: bool) -> Result<(), String> {
    if h.on == on {
        return Ok(());
    }
    let (want, put) = if on { (h.old, h.new) } else { (h.new, h.old) };
    // In: no thread may sit inside the three moved instructions. Out: none can be there.
    let was = if on { memory::swap_code_u64_paused(h.word, put, h.at + 1..h.at + 5) } else { memory::swap_code_u64(h.word, put) };
    match was {
        Ok(was) if was == want => {
            h.on = on;
            ON.store(on, Ordering::SeqCst);
            Ok(())
        }
        Ok(was) => {
            // Someone else changed this code: put back what was there and stop.
            let _ = memory::swap_code_u64(h.word, was);
            BAD.store(true, Ordering::SeqCst);
            Err(format!("silent aim code word was {was:#x}, expected {want:#x}; left alone"))
        }
        Err(e) => Err(e),
    }
}

/// Turn Silent Aim on or off (the menu row, its hotkey, a profile line).
pub fn set(on: bool) -> Result<(), String> {
    if on {
        switch_on()
    } else {
        switch_off("off");
        stop_worker();
        Ok(())
    }
}

fn switch_on() -> Result<(), String> {
    if is_on() {
        return Ok(());
    }
    // A worker left from an earlier turn-off (by itself) ends within a tick; waited for before the
    // switch lock, which that worker may still want.
    stop_worker();
    let _one = SWITCH.lock().map_err(|_| "silent aim lock")?;
    if is_on() {
        return Ok(());
    }
    if BAD.load(Ordering::Relaxed) {
        return Err("Silent Aim is off for this session (see the log)".into());
    }
    let world = entities::WorldList::default();
    let block = jump_in()?;
    AIM.store(true, Ordering::SeqCst);
    RUN.store(true, Ordering::SeqCst);
    let w = std::thread::spawn(move || worker(block, world));
    if let Ok(mut s) = WORKER.lock() {
        *s = Some(w);
    }
    if circle() {
        crate::vkdraw::want_circle();
    }
    log(&format!(
        "silent aim on (fov {:.0} deg wide, range {:.0} m, priority {}, circle {})",
        fov(),
        range_m(),
        priority().name(),
        if circle() { "shown" } else { "hidden" }
    ));
    Ok(())
}

/// The jump in, with the block ready (the code search first if it has not run): the block's
/// address. Called with the `SWITCH` lock held. The target is cleared first, so nothing is steered
/// until the worker says so.
fn jump_in() -> Result<u64, String> {
    if HOOK.lock().map_err(|_| "silent aim lock")?.is_none() {
        warm();
    }
    let mut slot = HOOK.lock().map_err(|_| "silent aim lock")?;
    let Some(h) = slot.as_mut() else { return Err("Silent Aim could not start (see the log)".into()) };
    if !h.on {
        write_target(h.block, None);
    }
    if let Err(e) = swap(h, true) {
        log(&format!("silent aim: {e}"));
        return Err(e);
    }
    Ok(h.block)
}

/// Silent Aim off: no target, the worker told to stop (not waited for: the worker calls this
/// itself), and the jump out, unless No Bullet Drop still needs it (then the feature word keeps
/// that bit alone).
fn switch_off(why: &str) {
    let Ok(_one) = SWITCH.lock() else { return };
    RUN.store(false, Ordering::SeqCst);
    AIM.store(false, Ordering::SeqCst);
    let Ok(mut slot) = HOOK.lock() else { return };
    let Some(h) = slot.as_mut() else { return };
    write_target(h.block, None);
    if no_drop() {
        write_feat(h.block, FEAT_NODROP);
        if h.on {
            log(&format!("silent aim {why}; no bullet drop keeps the hook"));
        }
        return;
    }
    write_feat(h.block, 0);
    if !h.on {
        return;
    }
    match swap(h, false) {
        Ok(()) => log(&format!("silent aim {why}")),
        Err(e) => log(&format!("silent aim: {e}")),
    }
}

fn stop_worker() {
    RUN.store(false, Ordering::SeqCst);
    let h = WORKER.lock().ok().and_then(|mut w| w.take());
    if let Some(h) = h
        && h.thread().id() != std::thread::current().id()
    {
        crate::join_for(h, "silent aim worker", Duration::from_secs(2));
    }
}

/// All off (unload): the jump out and the worker ended.
pub fn shutdown() {
    NO_DROP.store(false, Ordering::SeqCst);
    switch_off("off (unload)");
    stop_worker();
}

/// A soldier or drone the last scan saw, re-read every tick.
#[derive(Clone, Copy)]
enum Seen {
    Soldier { body: u64, side: Option<i32> },
    Drone(entities::Drone),
}

fn read_u32(addr: u64) -> Option<u32> {
    memory::read(addr, 4).ok().map(|b| u32::from_le_bytes(b.try_into().unwrap()))
}

fn read_u64(addr: u64) -> Option<u64> {
    memory::read(addr, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// A soldier's side, as the ESP reads it (`vkdraw::side_of`): character +0x60 -> +0x160 -> +0x104.
fn side_of(owner: u64) -> Option<i32> {
    let ptr = |v: u64| v > 0x10000 && v < 0x7fff_ffff_0000;
    let l1 = read_u64(owner.checked_add(0x60)?).filter(|v| ptr(*v))?;
    let l2 = read_u64(l1 + 0x160).filter(|v| ptr(*v))?;
    read_u32(l2 + 0x104).map(|v| v as i32).filter(|v| (0..64).contains(v))
}

/// Where on a target the shot goes, for the log.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Head,
    Chest,
    Middle,
}

/// The aim point on a body at `pos`: its head (`head` is the head's offset from the body, from the
/// ESP's last bones) else the chest.
fn aim_point(pos: [f64; 3], head: Option<[f64; 3]>) -> ([f64; 3], Part) {
    match head {
        Some(o) => ([pos[0] + o[0], pos[1] + o[1], pos[2] + o[2]], Part::Head),
        None => ([pos[0], pos[1], pos[2] + CHEST_UP], Part::Chest),
    }
}

/// A target as the worker keeps it for the log: its id (body or drone), what and which part, its
/// type word and side, and the point (before the drop raise).
#[derive(Clone, Copy, Debug)]
struct Aimed {
    id: u64,
    what: What,
    part: Part,
    typ: u32,
    side: Option<i32>,
    point: [f64; 3],
}

/// What a seen one is now, re-read.
fn now(s: &Seen) -> Option<(Cand, Aimed)> {
    match *s {
        Seen::Soldier { body, side } => {
            let typ = read_u32(body + E_TYPE)?;
            let what = match typ & 7 {
                KIND_NPC => What::Soldier,
                KIND_PLAYER => What::Player,
                _ => return None,
            };
            let pos = entities::pos_of(body)?;
            let (point, part) = aim_point(pos, crate::vkdraw::head_offset(body));
            Some((Cand { what, point, dead: typ & TYPE_DEAD != 0, side }, Aimed { id: body, what, part, typ, side, point }))
        }
        Seen::Drone(d) => entities::drone_at(&d).map(|n| {
            let typ = read_u32(d.ent + E_TYPE).unwrap_or(0);
            (Cand { what: What::Drone, point: n.mid, dead: n.dead, side: None }, Aimed { id: d.ent, what: What::Drone, part: Part::Middle, typ, side: None, point: n.mid })
        }),
    }
}

/// Degrees between two directions (any length); not a number when one is 0.
fn between_deg(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (la, lb) = (len(a), len(b));
    if !(la > 1e-9 && lb > 1e-9) {
        return f64::NAN;
    }
    ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)).clamp(-1.0, 1.0).acos().to_degrees()
}

fn read_f32x3(addr: u64) -> Option<[f64; 3]> {
    let b = memory::read(addr, 12).ok()?;
    let f = |i: usize| f64::from(f32::from_le_bytes(b[i..i + 4].try_into().unwrap()));
    let v = [f(0), f(4), f(8)];
    v.iter().all(|x| x.is_finite() && x.abs() < 1.0e6).then_some(v)
}

fn f3(v: [f64; 3]) -> String {
    format!("{:.2} {:.2} {:.2}", v[0], v[1], v[2])
}

/// The last steered shot, as the cave kept it, logged with the target it was meant for: where it
/// started, how far ours turned it from the game's, how far off the target's point now it is
/// (left/right and up/down), the target's type word and side, and the shot's speed and range.
fn log_shot(block: u64, n: u32, aimed: Option<Aimed>, now_point: Option<[f64; 3]>, drop: Drop) {
    let (Some(origin), Some(old), Some(new)) = (read_f32x3(block + ORIGIN_AT), read_f32x3(block + OLD_AT), read_f32x3(block + NEW_AT)) else { return };
    let shot = read_u64(block + SHOT_AT).unwrap_or(0);
    let speed = read_u32(shot + SHOT_SPEED).map(f32::from_bits).unwrap_or(f32::NAN);
    let range = read_u32(shot + SHOT_RANGE).map(f32::from_bits).unwrap_or(f32::NAN);
    let eye = entities::camera_pos();
    let mut line = format!(
        "silent aim shot {n}: origin {}{}, game dir {}, ours {} ({:.2} deg turned), speed {speed:.0}, range {range:.0}, drop k {:.2e} from the speed (measured {:.2e}, median of {} shots)",
        f3(origin),
        eye.map(|e| format!(" (camera {}, {:.2} off)", f3(e), len(sub(e, origin)))).unwrap_or_default(),
        f3(old),
        f3(new),
        between_deg(old, new),
        drop.k(),
        drop.measured,
        drop.shots
    );
    match aimed {
        Some(a) => {
            let p = now_point.unwrap_or(a.point);
            let to = sub(p, origin);
            let d = len(to);
            // Where ours passes at the target's distance, against the point: up/down and across.
            let at = [origin[0] + new[0] * d, origin[1] + new[1] * d, origin[2] + new[2] * d];
            line += &format!(
                ", target {:?} {:#x} type {:#x} side {} {:?} at {} ({d:.1} units), ours passes it {:.2} up, {:.2} across ({:.2} deg)",
                a.what,
                a.id,
                a.typ,
                a.side.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
                a.part,
                f3(p),
                at[2] - p[2],
                ((at[0] - p[0]).powi(2) + (at[1] - p[1]).powi(2)).sqrt(),
                between_deg(new, to)
            );
        }
        None => line += ", no target now",
    }
    log(&line);
}

/// One of the player's shots being watched as it flies: its drop (a steered one), and what Magic
/// Bullets, No Range Loss and Instant Hit rest on: the wall budget `+0x378` over the flight, speed,
/// range, lifetime, path points, and how fast it really went.
struct Track {
    shot: u64,
    steered: bool,
    /// Its origin as the shot keeps it (its own frame), and its direction (ours or the game's).
    origin: [f64; 3],
    dir: [f64; 3],
    since: Instant,
    along: f64,
    fell: f64,
    energy: (f32, f32, f32),
    travelled: f32,
    /// Read at the start, after our writes: speed, range, lifetime, and the path's first word,
    /// count and first points (as they sit, and through the first word if it is a pointer).
    at_start: String,
}

fn read_f32(addr: u64) -> f32 {
    read_u32(addr).map(f32::from_bits).unwrap_or(f32::NAN)
}

impl Track {
    fn start(block: u64, steered: bool) -> Option<Track> {
        let shot = read_u64(block + SHOT_AT).filter(|s| *s > 0x10000)?;
        let origin = read_f32x3(shot + SHOT_ORIGIN)?;
        let dir = read_f32x3(block + if steered { NEW_AT } else { OLD_AT }).filter(|d| (len(*d) - 1.0).abs() < 0.01)?;
        let e = read_f32(shot + SHOT_ENERGY);
        let path = read_u64(shot + SHOT_PATH).unwrap_or(0);
        let count = memory::read(shot + SHOT_PATH_COUNT, 2).ok().map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
        let pts = |at: u64| (0..3).filter_map(|i| read_f32x3(at + i * 12)).map(f3).collect::<Vec<_>>().join(" | ");
        let via = if path > 0x10000 && path < 0x7fff_ffff_0000 { format!(", through it {}", pts(path)) } else { String::new() };
        let at_start = format!(
            "speed +0xC {:.0}, +0x8 {:.1}, range +0x158 {:.0}, lifetime +0xC8 {:.3e}, path +0x160 word {path:#x} count +0x176 {count}, points as they sit {}{via}",
            read_f32(shot + SHOT_SPEED),
            read_f32(shot + SHOT_SPEED3),
            read_f32(shot + SHOT_RANGE),
            read_f32(shot + SHOT_LIFE),
            pts(shot + SHOT_PATH)
        );
        Some(Track { shot, steered, origin, dir, since: Instant::now(), along: 0.0, fell: 0.0, energy: (e, e, e), travelled: 0.0, at_start })
    }

    /// Read where it is now. False: it is gone (freed, reused, or flew long enough).
    fn step(&mut self) -> bool {
        if self.since.elapsed() > Duration::from_secs(3) {
            return false;
        }
        let same = read_f32x3(self.shot + SHOT_ORIGIN).is_some_and(|o| len(sub(o, self.origin)) < 0.01);
        let Some(pos) = read_f32x3(self.shot + SHOT_POS).filter(|_| same) else { return false };
        let (along, fell) = flown(self.origin, self.dir, pos);
        if along > self.along && along < 1.0e6 {
            (self.along, self.fell) = (along, fell);
        }
        let e = read_f32(self.shot + SHOT_ENERGY);
        if e.is_finite() {
            self.energy.1 = self.energy.1.min(e);
            self.energy.2 = e;
        }
        let t = read_f32(self.shot + SHOT_TRAVELLED);
        if t.is_finite() {
            self.travelled = t;
        }
        true
    }

    fn report(&self, feat: u32) -> String {
        let ms = self.since.elapsed().as_millis().max(1);
        format!(
            "silent aim track: shot {:#x} ({}, features {feat}): flew {:.1} units in about {ms} ms ({:.0} units/s seen), fell {:.3}, wall budget +0x378 {} -> {} (lowest {}), travelled +0x154 {:.1}; {}",
            self.shot,
            if self.steered { "steered" } else { "not steered" },
            self.along,
            self.along * 1000.0 / ms as f64,
            self.fell,
            self.energy.0,
            self.energy.2,
            self.energy.1,
            self.travelled,
            self.at_start
        )
    }
}

fn worker(block: u64, mut world: entities::WorldList) {
    let mut seen: Vec<Seen> = Vec::new();
    let mut last_scan = Instant::now() - SCAN_EVERY;
    let mut last_fallback = Instant::now() - FALLBACK_EVERY;
    let mut said = String::new();
    // Ticks with a target and in all, for a log line every 10 s.
    let (mut with, mut ticks, mut counted) = (0u32, 0u32, Instant::now());
    // Steered shots seen, and how many were logged in this 10 s (at most `SHOT_LINES`).
    let mut shots_seen = read_u32(block + COUNT_AT).unwrap_or(0);
    let mut shot_lines = 0u32;
    let mut aimed: Option<Aimed> = None;
    let mut last_id = 0u64;
    let mut drop = Drop::default();
    let mut track: Option<Track> = None;
    // All the player's shots seen; other players' bodies (never targets: kept only to keep the
    // features off near them); the feature word last written.
    let mut all_seen = read_u32(block + ALL_AT).unwrap_or(0);
    let mut players: Vec<u64> = Vec::new();
    let mut feat_was = 0u32;
    let mut track_lines = 0u32;
    while RUN.load(Ordering::SeqCst) && !crate::stopping() {
        std::thread::sleep(Duration::from_millis(TICK_MS));
        if last_scan.elapsed() >= SCAN_EVERY {
            last_scan = Instant::now();
            // A little past the range: a body walking in is known by the next tick.
            let reach = range_units(range_m()) + 50.0;
            let list = match world.pass(reach, 500) {
                Ok(l) => Some(l),
                Err(e) => {
                    if e != said {
                        log(&format!("silent aim: world list not used ({e})"));
                        said = e;
                    }
                    if last_fallback.elapsed() >= FALLBACK_EVERY {
                        last_fallback = Instant::now();
                        entities::near(reach, 500, false, false).ok()
                    } else {
                        None
                    }
                }
            };
            match list {
                Some(l) => {
                    said.clear();
                    // Players are never kept as candidates. No soldier's character (animals): no
                    // side, so `allowed` leaves it out.
                    seen = l.iter().filter(|b| !b.player).map(|b| Seen::Soldier { body: b.body, side: if b.owner != 0 { side_of(b.owner) } else { None } }).collect();
                    seen.extend(world.drones().iter().map(|d| Seen::Drone(*d)));
                    players = l.iter().filter(|b| b.player).map(|b| b.body).collect();
                }
                None => {
                    // The lists can't be read: no target until they can, and no features (a player
                    // could be anywhere).
                    seen.clear();
                    players.clear();
                }
            }
        }
        let view = (|| {
            let fwd = entities::camera_forward()?;
            let me = entities::player_pos()?;
            Some((fwd, me, entities::camera_pos().unwrap_or([me[0], me[1], me[2] + 1.6])))
        })();
        let picked = (|| {
            let (fwd, me, eye) = view?;
            // A body re-read as a player comes back as `What::Player`, which `pick` never takes.
            let now: Vec<(Cand, Aimed)> = seen.iter().filter_map(now).collect();
            let cands: Vec<Cand> = now.iter().map(|n| n.0).collect();
            pick(eye, fwd, me, &cands, fov(), range_units(range_m()), priority()).map(|i| (now[i].1, me))
        })();
        // Raised for drop by the median of the player's last shots (`Drop`), from the player's body
        // (the shot starts at the camera, near it).
        let target = picked.map(|(a, me)| if no_drop() { a.point } else { drop.raise(me, a.point) });
        let picked = picked.map(|p| p.0);
        if let Some(a) = picked
            && a.id != last_id
        {
            last_id = a.id;
            log(&format!(
                "silent aim target: {:?} {:#x} type {:#x} side {} {:?}",
                a.what,
                a.id,
                a.typ,
                a.side.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
                a.part
            ));
        }
        // The feature word: nothing while the lists can't be read or a player is in the way.
        let lists_ok = !seen.is_empty() || !players.is_empty();
        // No Bullet Drop aims at nobody, so it needs no player check.
        let nodrop = u32::from(no_drop()) * FEAT_NODROP;
        let feat = match view {
            Some((fwd, _, eye)) if lists_ok => {
                let pl: Vec<[f64; 3]> = players.iter().filter_map(|b| entities::pos_of(*b)).map(|p| [p[0], p[1], p[2] + CHEST_UP]).collect();
                features(target, eye, fwd, &pl, (magic(), no_range(), instant())) | nodrop
            }
            _ => nodrop,
        };
        if feat != feat_was {
            let wanted = (u32::from(magic() && target.is_some()) * FEAT_MAGIC) | (u32::from(no_range()) * FEAT_RANGE) | (u32::from(instant()) * FEAT_INSTANT) | nodrop;
            log(&format!("silent aim features {feat} (wanted {wanted}{})", if feat != wanted { ", held back: a player in the way or no lists" } else { "" }));
            feat_was = feat;
        }
        write_feat(block, feat);
        // The shot being watched: how far it fell and what its fields did, once it is gone.
        if let Some(t) = track.as_mut()
            && !t.step()
        {
            let (along, fell, steered) = (t.along, t.fell, t.steered);
            if track_lines < SHOT_LINES {
                track_lines += 1;
                log(&t.report(feat));
            }
            track = None;
            if steered && drop.add(along, fell) {
                log(&format!("silent aim drop: a shot fell {fell:.3} units over {along:.1}: measured k {:.2e} ({} shots), aimed with {:.2e} from the speed {:.0}", drop.measured, drop.shots, drop.k(), if drop.speed > 0.0 { drop.speed } else { SPEED_DEFAULT }));
            }
        }
        // A shot steered since the last tick: logged against the target it went for, with that
        // target's point now (moved since?), and watched if none is.
        let count = read_u32(block + COUNT_AT).unwrap_or(shots_seen);
        if count != shots_seen {
            shots_seen = count;
            if let Some(shot) = read_u64(block + SHOT_AT).filter(|s| *s > 0x10000) {
                drop.set_speed(f64::from(read_u32(shot + SHOT_SPEED).map(f32::from_bits).unwrap_or(f32::NAN)));
            }
            if shot_lines < SHOT_LINES {
                shot_lines += 1;
                let now_point = aimed.and_then(|a| picked.filter(|p| p.id == a.id).map(|p| p.point));
                log_shot(block, count, aimed, now_point, drop);
            }
            if track.is_none() {
                track = Track::start(block, true);
            }
        }
        // Any other own shot (not steered): watched too while a feature is on.
        let all = read_u32(block + ALL_AT).unwrap_or(all_seen);
        if all != all_seen {
            all_seen = all;
            if track.is_none() && feat != 0 {
                track = Track::start(block, false);
            }
        }
        aimed = picked;
        write_target(block, target);
        ticks += 1;
        with += u32::from(target.is_some());
        if counted.elapsed() >= Duration::from_secs(10) {
            log(&format!("silent aim: a target in {with} of {ticks} ticks, {} soldiers and drones in reach, {shots_seen} shots steered", seen.len()));
            (with, ticks, counted, shot_lines, track_lines) = (0, 0, Instant::now(), 0, 0);
        }
    }
    write_target(block, None);
    // The worker ends with Silent Aim; No Bullet Drop keeps its bit.
    write_feat(block, u32::from(no_drop()) * FEAT_NODROP);
}

#[cfg(test)]
mod tests {
    use super::*;

    const EYE: [f64; 3] = [0.0, 0.0, 1.6];
    const FWD: [f64; 3] = [0.0, 1.0, 0.0];
    const ME: [f64; 3] = [0.0, 0.0, 0.0];

    fn soldier(p: [f64; 3]) -> Cand {
        Cand { what: What::Soldier, point: p, dead: false, side: Some(3) }
    }

    #[test]
    fn angle_is_degrees_off_the_crosshair() {
        assert!(angle_deg(EYE, FWD, [0.0, 100.0, 1.6]).abs() < 1e-9);
        assert!((angle_deg(EYE, FWD, [100.0, 100.0, 1.6]) - 45.0).abs() < 1e-9);
        assert!((angle_deg(EYE, FWD, [0.0, -10.0, 1.6]) - 180.0).abs() < 1e-9);
        assert!((angle_deg(EYE, FWD, [10.0, 0.0, 1.6]) - 90.0).abs() < 1e-9);
        // On the eye itself or not a number: as far off as can be.
        assert_eq!(angle_deg(EYE, FWD, EYE), 180.0);
        assert_eq!(angle_deg(EYE, FWD, [f64::NAN, 1.0, 1.0]), 180.0);
    }

    #[test]
    fn pick_takes_the_nearest_to_the_crosshair_inside_fov_and_range() {
        let tan = |deg: f64| deg.to_radians().tan();
        let c = [
            soldier([100.0 * tan(8.0), 100.0, 1.6]), // 8 deg
            soldier([50.0 * tan(3.0), 50.0, 1.6]),   // 3 deg
            soldier([0.0, 400.0, 1.6]),              // 0 deg but too far
            soldier([200.0 * tan(20.0), 200.0, 1.6]), // Outside 10 deg
        ];
        // The FOV is the full width: 20 wide takes up to 10 off the middle.
        assert_eq!(pick(EYE, FWD, ME, &c, 20.0, 300.0, Priority::Crosshair), Some(1));
        // Wider range: the dead-centre one wins.
        assert_eq!(pick(EYE, FWD, ME, &c, 20.0, 500.0, Priority::Crosshair), Some(2));
        // Narrow FOV: nobody inside 1 deg within 300.
        assert_eq!(pick(EYE, FWD, ME, &c, 2.0, 300.0, Priority::Crosshair), None);
        // 10 wide is 5 off the middle: the 8 deg one is out, the 3 deg one in.
        assert_eq!(pick(EYE, FWD, ME, &c[..1], 10.0, 300.0, Priority::Crosshair), None);
        assert_eq!(pick(EYE, FWD, ME, &c[..2], 10.0, 300.0, Priority::Crosshair), Some(1));
        // Wide FOV takes the 20 deg one too, but the 3 deg one is still nearest the crosshair.
        assert_eq!(pick(EYE, FWD, ME, &c[3..], 90.0, 300.0, Priority::Crosshair), Some(0));
        assert_eq!(pick(EYE, FWD, ME, &[], 90.0, 1000.0, Priority::Crosshair), None);
    }

    #[test]
    fn players_are_never_targets() {
        let p = Cand { what: What::Player, point: [0.0, 50.0, 1.6], dead: false, side: None };
        assert!(!allowed(&p));
        // Right on the crosshair, alone: still nothing.
        assert_eq!(pick(EYE, FWD, ME, &[p], 90.0, 1000.0, Priority::Crosshair), None);
        // With a soldier off to the side: the soldier.
        let s = soldier([20.0, 50.0, 1.6]);
        assert_eq!(pick(EYE, FWD, ME, &[p, s], 90.0, 1000.0, Priority::Crosshair), Some(1));
    }

    /// Co-op: a co-op partner standing right on the crosshair, nearer than any soldier, dead
    /// centre, is never picked; the soldier a few degrees off is. A friend alone, or a dead or
    /// friendly-side "player", gives no target at all.
    #[test]
    fn coop_friend_on_the_crosshair_is_never_picked() {
        let friend = Cand { what: What::Player, point: [0.0, 5.0, 1.6], dead: false, side: None };
        let soldier_off = soldier([5.0, 100.0, 1.6]);
        for fov in [1.0, 10.0, 90.0] {
            assert_eq!(pick(EYE, FWD, ME, &[friend], fov, 1000.0, Priority::Crosshair), None, "fov {fov}");
        }
        assert_eq!(pick(EYE, FWD, ME, &[friend, soldier_off], 20.0, 1000.0, Priority::Crosshair), Some(1));
        assert_eq!(pick(EYE, FWD, ME, &[soldier_off, friend], 20.0, 1000.0, Priority::Crosshair), Some(0));
        // Every priority: still never the friend.
        for prio in Priority::ALL {
            assert_eq!(pick(EYE, FWD, ME, &[friend, soldier_off], 20.0, 1000.0, prio), Some(1), "{prio:?}");
            assert_eq!(pick(EYE, FWD, ME, &[friend], 90.0, 1000.0, prio), None, "{prio:?}");
        }
        // Whatever else is set on it, a player stays out.
        for side in [None, Some(0), Some(3)] {
            for dead in [false, true] {
                assert!(!allowed(&Cand { what: What::Player, point: [0.0, 5.0, 1.6], dead, side }));
            }
        }
        // Many friends round the crosshair and no soldier: nothing.
        let friends: Vec<Cand> = (0..8).map(|i| Cand { point: [f64::from(i) * 0.1, 10.0, 1.6], ..friend }).collect();
        assert_eq!(pick(EYE, FWD, ME, &friends, 90.0, 1000.0, Priority::Crosshair), None);
    }

    #[test]
    fn dead_and_friendly_are_left_out_drones_are_in() {
        let dead = Cand { dead: true, ..soldier([0.0, 50.0, 1.6]) };
        let friend = Cand { side: Some(SIDE_FRIENDLY), ..soldier([0.0, 60.0, 1.6]) };
        let other_side = Cand { side: Some(5), ..soldier([1.0, 70.0, 1.6]) };
        let drone = Cand { what: What::Drone, point: [0.0, 80.0, 3.0], dead: false, side: None };
        let dead_drone = Cand { dead: true, ..drone };
        assert!(!allowed(&dead) && !allowed(&friend) && !allowed(&dead_drone));
        assert!(allowed(&other_side) && allowed(&drone));
        assert_eq!(pick(EYE, FWD, ME, &[dead, friend, dead_drone, drone], 20.0, 300.0, Priority::Crosshair), Some(3));
    }

    /// Animals: type & 7 is 2 like a soldier, but no soldier's character, so no side. Never picked,
    /// even dead centre and nearest; the soldier off to the side is.
    #[test]
    fn animals_are_never_targets() {
        let animal = Cand { side: None, ..soldier([0.0, 8.0, 0.6]) };
        assert!(!allowed(&animal));
        let s = soldier([3.0, 60.0, 1.6]);
        for prio in Priority::ALL {
            assert_eq!(pick(EYE, FWD, ME, &[animal], 90.0, 1000.0, prio), None, "{prio:?}");
            assert_eq!(pick(EYE, FWD, ME, &[animal, s], 90.0, 1000.0, prio), Some(1), "{prio:?}");
        }
    }

    /// The limits, on what the setters keep (`fov_kept`, `range_kept`): no setting changes, so this
    /// can't clash with `pages_match_her_approved_pictures`, which reads them.
    #[test]
    fn range_and_fov_limits() {
        assert_eq!(range_units(300.0), 200.0);
        assert_eq!(range_units(5.0), RANGE_MIN / 1.5);
        assert_eq!(range_units(5000.0), RANGE_MAX / 1.5);
        assert_eq!(fov_kept(0.0), FOV_MIN);
        assert_eq!(fov_kept(200.0), FOV_MAX);
        assert_eq!(fov_kept(19.6), FOV_DEFAULT);
        assert_eq!(range_kept(9999.0), RANGE_MAX);
        assert_eq!(range_kept(0.0), RANGE_MIN);
        assert_eq!(range_kept(299.5), RANGE_DEFAULT);
    }

    /// Every near jump lands on GO (the moved instructions) or on the call, the call goes to GO,
    /// the rip-relative reads hit their slots, each feature's jz skips exactly its writes, and the
    /// last jump goes back into the game's function.
    #[test]
    fn cave_jumps_and_slots() {
        let (cave, at, root) = (0x1_4000_0000u64, 0x1_4365_2430u64, 0x1_463e_beb0u64);
        let c = cave_code(cave, at, root).unwrap();
        assert!(c.len() as u64 <= TARGET_AT, "{} bytes", c.len());
        let go = c.len() - 5 - MOVED.len();
        assert_eq!(&c[go..go + MOVED.len()], &MOVED);
        assert_eq!(c[go + MOVED.len()], 0xE9);
        let tail = i32::from_le_bytes(c[c.len() - 4..].try_into().unwrap());
        assert_eq!((cave + c.len() as u64).wrapping_add_signed(i64::from(tail)), at + 5);
        // After the steer block: the No Bullet Drop code (reads the feature word), then the call
        // to GO.
        let lea = c.windows(5).position(|w| w == [0x4C, 0x8D, 0x44, 0x24, 0x40]).unwrap();
        let nosteer = lea + 5 + 5;
        assert_eq!(&c[lea + 5..nosteer], &[0xC6, 0x44, 0x24, 0x58, 0x01]);
        let flat = flatten_code([0; 4]);
        let call = nosteer + flat.len();
        assert_eq!(&c[nosteer..nosteer + 3], &[0x44, 0x8B, 0x15]);
        assert_eq!(&c[nosteer + 7..call], &flat[7..]);
        assert_eq!(c[call], 0xE8);
        let r = i32::from_le_bytes(c[call + 1..call + 5].try_into().unwrap());
        assert_eq!((cave + call as u64 + 5).wrapping_add_signed(i64::from(r)), cave + go as u64);
        // Near jumps: 7 to GO (before the frame), 2 to NOSTEER (no target, too near), which is
        // where the No Bullet Drop code starts (so unsteered shots get it too).
        let (mut to_go, mut to_nosteer) = (0, 0);
        for i in 0..nosteer {
            if c[i] == 0x0F && matches!(c[i + 1], 0x84 | 0x85 | 0x86 | 0x8D) && i + 6 <= c.len() {
                let r = i32::from_le_bytes(c[i + 2..i + 6].try_into().unwrap());
                let to = i as i64 + 6 + i64::from(r);
                if to == go as i64 {
                    to_go += 1;
                } else if to == nosteer as i64 {
                    to_nosteer += 1;
                }
            }
        }
        assert_eq!((to_go, to_nosteer), (7, 2));
        // The rip-relative reads and writes.
        let rip_at = |k: usize| (cave + k as u64 + 4).wrapping_add_signed(i64::from(i32::from_le_bytes(c[k..k + 4].try_into().unwrap())));
        let rip = |op: &[u8]| rip_at(c.windows(op.len()).position(|w| w == op).unwrap() + op.len());
        assert_eq!(rip(&[0x8B, 0x05]), cave + ON_AT);
        assert_eq!(rip(&[0x0B, 0x05]), cave + FEAT_AT);
        assert_eq!(rip(&[0x0F, 0x10, 0x05]), cave + TARGET_AT);
        assert_eq!(rip(&[0x0F, 0x54, 0x05]), cave + MASK_AT);
        assert_eq!(rip(&[0x0F, 0x2F, 0x0D]), cave + MIN_AT);
        assert_eq!(rip(&[0x48, 0x89, 0x0D]), cave + SHOT_AT);
        assert_eq!(rip(&[0x0F, 0x11, 0x0D]), cave + ORIGIN_AT);
        assert_eq!(rip(&[0x0F, 0x11, 0x1D]), cave + OLD_AT);
        assert_eq!(rip(&[0x0F, 0x11, 0x05]), cave + NEW_AT);
        // Both reads of the feature word (the No Bullet Drop code, the writes after the call).
        let feats: Vec<usize> = c.windows(3).enumerate().filter(|(_, w)| *w == [0x44, 0x8B, 0x15]).map(|(i, _)| i + 3).collect();
        assert_eq!(feats.len(), 2);
        assert!(feats.iter().all(|k| rip_at(*k) == cave + FEAT_AT));
        let incs: Vec<usize> = c.windows(3).enumerate().filter(|(_, w)| *w == [0xF0, 0xFF, 0x05]).map(|(i, _)| i + 3).collect();
        assert_eq!(incs.iter().map(|k| rip_at(*k)).collect::<Vec<_>>(), [cave + ALL_AT, cave + COUNT_AT]);
        // The second "mov eax, [rip+on]" (the steer check) reads "on" too.
        let movs: Vec<usize> = c.windows(2).enumerate().filter(|(_, w)| *w == [0x8B, 0x05]).map(|(i, _)| i + 2).collect();
        assert!(movs.iter().take(2).all(|k| rip_at(*k) == cave + ON_AT));
        // The features: each test's jz lands on the next test, or on the frame's end.
        let end = c.windows(4).position(|w| w == [0x48, 0x83, 0xC4, FRAME]).unwrap();
        let mut k = call + 5 + 5 + 7;
        for (bit, writes, steered_only) in feature_writes() {
            assert_eq!(&c[k..k + 3], &[0x41, 0xF7, 0xC2], "test at {k}");
            assert_eq!(u32::from_le_bytes(c[k + 3..k + 7].try_into().unwrap()), bit);
            assert_eq!(c[k + 7], 0x74);
            let body = &c[k + 9..k + 9 + c[k + 8] as usize];
            let mut b = 0;
            if steered_only {
                assert_eq!(&body[..7], &[0x80, 0x7C, 0x24, 0x58, 0x00, 0x74, (body.len() - 7) as u8]);
                b = 7;
            }
            for (off, val) in writes {
                let (op, n): (Vec<u8>, usize) = if off < 0x80 { (vec![0xC7, 0x41, off as u8], 3) } else { ([vec![0xC7, 0x81], off.to_le_bytes().to_vec()].concat(), 6) };
                assert_eq!(&body[b..b + n], &op[..]);
                assert_eq!(u32::from_le_bytes(body[b + n..b + n + 4].try_into().unwrap()), val);
                b += n + 4;
            }
            assert_eq!(b, body.len());
            k += 9 + body.len();
        }
        assert_eq!(k, end);
        assert_eq!(c[end + 4], 0xC3);
        // The frame keeps the stack 16-aligned at the call; the player's stack slots 5 to 7 copied.
        assert_eq!((8 + u32::from(FRAME)) % 16, 0);
        let from = c.windows(4).position(|w| w == [0x48, 0x8B, 0x84, 0x24]).unwrap() + 4;
        assert_eq!(u32::from_le_bytes(c[from..from + 4].try_into().unwrap()), u32::from(FRAME) + 0x28);
        // The player's body through the root on every call; the slots fit the block, 16-aligned for
        // SSE.
        let k = c.windows(2).position(|w| w == [0x49, 0xBA]).unwrap() + 2;
        assert_eq!(u64::from_le_bytes(c[k..k + 8].try_into().unwrap()), root);
        assert!(TARGET_AT.is_multiple_of(16) && MASK_AT.is_multiple_of(16) && ORIGIN_AT.is_multiple_of(16));
        assert!(ALL_AT + 4 <= BLOCK as u64);
        // What the features write.
        assert_eq!(feature_writes()[0].1, vec![(0x378, 1.0e6f32.to_bits())]);
        assert_eq!(feature_writes()[1].1, vec![(0x158, 10_000f32.to_bits())]);
        assert_eq!(feature_writes()[2].1, vec![(0xC, 100_000f32.to_bits()), (0x8, (100_000f32 / 3.0).to_bits())]);
    }

    /// The No Bullet Drop code: its four short jumps all land on its end (the call), the loop jumps
    /// back to its own start, it reads and writes the fields it should, and what it computes
    /// (`flattened`) keeps the pitch and takes out the curve.
    #[test]
    fn flatten_code_jumps_and_fields() {
        let c = flatten_code([1, 2, 3, 4]);
        assert_eq!(&c[..7], &[0x44, 0x8B, 0x15, 1, 2, 3, 4]);
        assert_eq!(&c[7..10], &[0x41, 0xF7, 0xC2]);
        assert_eq!(u32::from_le_bytes(c[10..14].try_into().unwrap()), FEAT_NODROP);
        // Every short forward jump (jz 74, jb 72, jbe 76) ends at the end of the code.
        let end = c.len();
        let mut found = 0;
        for i in 0..end - 1 {
            if matches!(c[i], 0x74 | 0x72 | 0x76) && i + 2 + c[i + 1] as usize == end {
                found += 1;
            }
        }
        assert_eq!(found, 4);
        // The fields: the points pointer, the count, the direction's x, y and z, the first
        // point's height and distance, and the height written in the loop.
        for op in [
            &[0x48, 0x8B, 0x81, 0x60, 0x01, 0x00, 0x00][..],
            &[0x44, 0x0F, 0xB7, 0x91, 0x6A, 0x01, 0x00, 0x00],
            &[0xF3, 0x41, 0x0F, 0x10, 0x08],
            &[0xF3, 0x41, 0x0F, 0x10, 0x50, 0x04],
            &[0xF3, 0x41, 0x0F, 0x10, 0x50, 0x08],
            &[0xF3, 0x0F, 0x10, 0x40, 0x04],
            &[0xF3, 0x0F, 0x10, 0x18],
            &[0xF3, 0x0F, 0x11, 0x60, 0x04],
            &[0xF3, 0x0F, 0x51, 0xC9],
            &[0xF3, 0x0F, 0x5E, 0xD1],
        ] {
            assert!(c.windows(op.len()).any(|w| w == op), "{op:02x?}");
        }
        // The loop: starts at "add rax, 12", its "ja" goes back to that start.
        let start = c.windows(4).position(|w| w == [0x48, 0x83, 0xC0, 0x0C]).unwrap();
        let ja = c.len() - 2;
        assert_eq!(c[ja], 0x77);
        assert_eq!((ja as i64 + 2 + i64::from(c[ja + 1] as i8)) as usize, start);
        // Every short jump in it reaches: the first one (over all the rest) is the longest.
        assert!(c.len() - 14 <= 0x7F, "{}", c.len());
        // What it computes: a TAC50-like arc at 10 deg up (25 points over 400 units) comes out
        // on the aim line, with the heights above the first point growing as x * tan(10 deg).
        let (v, g2) = (1100f32, 4.905f32);
        let pitch = 10f32.to_radians();
        let dir = [0.0, pitch.cos(), pitch.sin()];
        let pts: Vec<[f32; 3]> = (0..25)
            .map(|i| {
                let x = i as f32 * (400.0 / 24.0);
                let t = x / (v * pitch.cos());
                [x, 2.5 + x * pitch.tan() - g2 * t * t, 0.1 * i as f32]
            })
            .collect();
        let flat = flattened(&pts, dir);
        for (p, q) in pts.iter().zip(&flat) {
            assert_eq!(p[0], q[0]);
            assert_eq!(p[2], q[2]);
            assert!((q[1] - (2.5 + p[0] * pitch.tan())).abs() < 1e-3, "{p:?} -> {q:?}");
        }
        // The last point was 0.67 units low before (the drop at 400 units), none after.
        assert!((pts[24][1] - flat[24][1] + 0.67).abs() < 0.02);
        // Level aim: every height becomes the first's. Straight up: nothing changes.
        let level = flattened(&pts, [1.0, 0.0, 0.0]);
        assert!(level.iter().all(|p| p[1] == 2.5));
        assert_eq!(flattened(&pts, [0.0, 0.0, 1.0]), pts);
        assert_eq!(flattened(&pts[..1], dir), pts[..1].to_vec());
        // Walked as the CPU would: n points, points 1..n written.
        for n in [2u16, 3, 25] {
            let (mut rax, mut r10, mut written) = (0usize, n, 0);
            loop {
                rax += 1;
                written += 1;
                r10 -= 1;
                if r10 <= 1 {
                    break;
                }
            }
            assert_eq!((rax, written), (n as usize - 1, n as usize - 1));
        }
    }

    /// Never anything for a shot that could reach a player: Magic Bullets not with a player near
    /// the line to the target (walls would not stop it), the others not with one near the player's
    /// crosshair; each only when its switch is on.
    #[test]
    fn features_hold_back_for_players() {
        let t = [0.0, 100.0, 1.6];
        let all = (true, true, true);
        assert_eq!(features(Some(t), EYE, FWD, &[], all), FEAT_MAGIC | FEAT_RANGE | FEAT_INSTANT);
        assert_eq!(features(None, EYE, FWD, &[], all), FEAT_RANGE | FEAT_INSTANT);
        assert_eq!(features(Some(t), EYE, FWD, &[], (false, false, false)), 0);
        assert_eq!(features(Some(t), EYE, FWD, &[], (true, false, false)), FEAT_MAGIC);
        // A co-op partner behind the wall, on the line to the soldier: nothing at all.
        assert_eq!(features(Some(t), EYE, FWD, &[[0.5, 50.0, 1.6]], all), 0);
        // Friend beside the line but near the crosshair (8 deg): Magic yes, the others no.
        let beside = [50.0 * 8f64.to_radians().tan(), 50.0, 1.6];
        assert!(!player_near_line(EYE, t, &[beside], PLAYER_LINE_UNITS));
        assert_eq!(features(Some(t), EYE, FWD, &[beside], all), FEAT_MAGIC);
        // Friend far off to the side (40 deg) and away from the line: everything.
        let side = [40.0, 50.0, 1.6];
        assert_eq!(features(Some(t), EYE, FWD, &[side], all), FEAT_MAGIC | FEAT_RANGE | FEAT_INSTANT);
        // Behind the target or behind the player: not on the line.
        assert!(!player_near_line(EYE, t, &[[0.0, 120.0, 1.6], [0.0, -20.0, 1.6]], PLAYER_LINE_UNITS));
        // A position that is not a number counts as in the way.
        assert!(player_near_line(EYE, t, &[[f64::NAN, 1.0, 1.0]], PLAYER_LINE_UNITS));
    }

    /// Priority: crosshair, nearest, farthest; the circle and the range still limit all three.
    #[test]
    fn priority_orders_inside_the_circle_and_range() {
        let tan = |deg: f64| deg.to_radians().tan();
        let c = [
            soldier([200.0 * tan(1.0), 200.0, 1.6]), // 0: 1 deg off, far
            soldier([30.0 * tan(8.0), 30.0, 1.6]),   // 1: 8 deg off, near
            soldier([100.0 * tan(4.0), 100.0, 1.6]), // 2: 4 deg off, middle
            soldier([5.0 * tan(30.0), 5.0, 1.6]),    // 3: nearest of all but outside 20 wide
            soldier([0.0, 400.0, 1.6]),              // 4: dead centre, out of range
        ];
        assert_eq!(pick(EYE, FWD, ME, &c, 20.0, 300.0, Priority::Crosshair), Some(0));
        assert_eq!(pick(EYE, FWD, ME, &c, 20.0, 300.0, Priority::Nearest), Some(1));
        assert_eq!(pick(EYE, FWD, ME, &c, 20.0, 300.0, Priority::Farthest), Some(0));
        assert_eq!(pick(EYE, FWD, ME, &c[1..], 20.0, 300.0, Priority::Farthest), Some(1));
        for (word, p) in [("Crosshair", Priority::Crosshair), ("nearest distance", Priority::Nearest), ("FARTHEST", Priority::Farthest), ("closest to crosshair", Priority::Crosshair)] {
            assert_eq!(Priority::parse(word), Some(p), "{word}");
            assert_eq!(Priority::parse(p.name()), Some(p));
        }
        assert_eq!(Priority::parse("sideways"), None);
        // What set_priority keeps and priority reads back, without changing the setting: the
        // pictures test reads it at the same time.
        for p in Priority::ALL {
            assert_eq!(priority_at(priority_slot(p)), p, "{p:?}");
        }
        assert_eq!(priority_at(99), Priority::Crosshair);
    }

    /// The cone Silent Aim picks from is the circle drawn on screen: a point is inside the circle
    /// exactly when it is inside the cone. Projected the way the ESP projects (pinhole, square
    /// pixels), for several FOVs, camera zooms and picture sizes.
    #[test]
    fn the_circle_is_the_cone() {
        for (h, w) in [(1080.0f64, 1920.0f64), (1440.0, 3440.0), (720.0, 1280.0)] {
            for cam_deg in [30.0f64, 50.0, 65.0, 90.0] {
                let cam = cam_deg.to_radians();
                for fov in [2.0, 10.0, 20.0, 45.0, 90.0] {
                    let r = crate::vkdraw::circle_px(fov, cam, h);
                    for off in [0.5, 0.95, 0.999, 1.001, 1.05, 1.5] {
                        let a = (fov / 2.0 * off).to_radians();
                        for spin in 0..8 {
                            let s = f64::from(spin) * std::f64::consts::TAU / 8.0;
                            // Camera looks along +y; right +x, up +z (as `boxes` projects).
                            let p = [a.tan() * s.cos(), 1.0, a.tan() * s.sin()];
                            let x = p[0] / p[1] / ((cam / 2.0).tan() * w / h) * (w / 2.0);
                            let y = p[2] / p[1] / (cam / 2.0).tan() * (h / 2.0);
                            let inside_circle = (x * x + y * y).sqrt() <= r;
                            let inside_cone = in_cone(angle_deg([0.0; 3], [0.0, 1.0, 0.0], p), fov);
                            assert_eq!(inside_circle, inside_cone, "h {h} cam {cam_deg} fov {fov} off {off} spin {spin}");
                        }
                    }
                }
            }
        }
        // Scoped in (narrower camera): the same aim cone is a bigger circle.
        assert!(crate::vkdraw::circle_px(20.0, 30f64.to_radians(), 1080.0) > crate::vkdraw::circle_px(20.0, 65f64.to_radians(), 1080.0));
    }

    /// The head is the body's live position plus the head's offset from the ESP's last frame, so a
    /// soldier who ran a metre since that frame is aimed at where he is now.
    #[test]
    fn head_follows_the_body_now() {
        let (p, part) = aim_point([101.0, 50.0, 3.0], Some([0.1, -0.05, 1.62]));
        assert_eq!(part, Part::Head);
        assert!((p[0] - 101.1).abs() < 1e-9 && (p[1] - 49.95).abs() < 1e-9 && (p[2] - 4.62).abs() < 1e-9);
        assert_eq!(aim_point([101.0, 50.0, 3.0], None), ([101.0, 50.0, 3.0 + CHEST_UP], Part::Chest));
        assert!((between_deg([1.0, 0.0, 0.0], [0.0, 2.0, 0.0]) - 90.0).abs() < 1e-9);
        assert!(between_deg([0.0; 3], [1.0, 0.0, 0.0]).is_nan());
    }

    /// Drop from physics: at 950 units/s a shot falls g/2 t^2; the aim goes up by that.
    #[test]
    fn drop_is_from_the_speed() {
        let v = 950.0f64;
        let dir = [0.0, 1.0, 0.0];
        let origin = [0.0, 0.0, 1.6];
        let fall = |d: f64| 4.905 * (d / v).powi(2);
        let (along, fell) = flown(origin, dir, [0.0, 200.0, 1.6 - fall(200.0)]);
        assert!((along - 200.0).abs() < 1e-9 && (fell - fall(200.0)).abs() < 1e-12);
        // Before any shot: the VHSD2's 950.
        let mut d = Drop::default();
        assert!((d.k() - 4.905 / (v * v)).abs() < 1e-15);
        let p = d.raise(origin, [0.0, 300.0, 1.6]);
        assert!((p[2] - 1.6 - fall(300.0)).abs() < 1e-9, "{}", p[2]);
        // A faster gun falls less; misread speeds change nothing.
        d.set_speed(1900.0);
        assert!((d.k() * 4.0 - 4.905 / (v * v)).abs() < 1e-15);
        for bad in [f64::NAN, 0.0, -950.0, 5.0, 1e9] {
            d.set_speed(bad);
            assert_eq!(d.speed, 1900.0);
        }
        // Measured falls are kept for the log, but never change the aim.
        let k = d.k();
        assert!(d.add(along, fell) && d.add(along, 2.0));
        assert!(!d.add(10.0, 0.01) && !d.add(f64::NAN, 0.01) && !d.add(100.0, 50.0));
        assert_eq!((d.k(), d.shots), (k, 2));
    }

    /// The player's log (b313a91), the soldier 93.5 units off: the learned drop (k 1.70e-4, from
    /// falls measured on raised shots) aimed 1.48 units over his head. The player's first unraised
    /// shots fell 0.05 to 0.37 over about 100 units; the raise from the speed is 0.05, inside the
    /// head, and the player's raised readings no longer move it.
    #[test]
    fn her_shots_no_longer_go_over_the_head() {
        let unraised = [(131.7, -3.227), (105.1, 0.054), (97.3, 0.370), (98.0, 0.311), (98.0, 0.138)];
        let raised = [(88.8, 2.361), (86.2, 1.476), (91.8, 1.338), (91.8, 1.319), (89.2, 1.353), (91.8, 1.553), (91.8, 1.630), (91.8, 1.419), (91.8, 1.406), (91.8, 2.446), (91.7, 2.354), (92.1, 2.187)];
        let mut d = Drop::default();
        d.set_speed(950.0);
        for (along, fell) in unraised.into_iter().chain(raised) {
            d.add(along, fell);
        }
        // The old way: the median of the last 15 aimed this high.
        assert!(d.measured * 93.5 * 93.5 > 1.0, "{}", d.measured);
        let up = d.raise([11507.6, 11217.0, 139.84], [11518.77, 11125.07, 126.60])[2] - 126.60;
        assert!(up > 0.03 && up < 0.08, "{up}");
    }

    /// The direction the cave works out, done the same way in f32 here: normalize(target - origin).
    #[test]
    fn direction_is_unit_towards_the_target() {
        let (t, o) = ([10.0f32, 20.0, 5.0], [1.0f32, 2.0, 3.0]);
        let d = [t[0] - o[0], t[1] - o[1], t[2] - o[2]];
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        let u = d.map(|v| v / l);
        assert!(((u[0] * u[0] + u[1] * u[1] + u[2] * u[2]) - 1.0).abs() < 1e-6);
        assert!(l * l > MIN_LEN2);
    }
}
