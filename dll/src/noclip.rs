//! Fly Through Everything: while Fly is on, the game no longer moves the player's body, so slopes,
//! walls and objects can't push the player out and the player flies through them. One hook, only
//! for the player and only while Fly is on: it goes in with Fly and out with it
//! (`fly::follow_fly`), and holds the player's once Fly's fall check is over (`fly::worker`,
//! `hold`). It lets go at once when the player's body goes away (`let_go`).
//!
//! Found live (research DLL, the player's game): every frame the game puts each body's place and
//! turn through one function (exe+0x11db540 in that build, the body's vtable slot +0x98: it copies
//! 0x50 bytes from rdx to body+0x20, then tells the rest of the game). For the player that place is
//! the one after the game's collision and ground snap: pushed out of a slope, pulled down onto the
//! ground. Fly writes the player's place every 4 ms, so the two fought: the player slid, got stuck
//! and seemed pulled back. The player's calls come from the per-frame update (exe+0xc73721 ->
//! exe+0x293d90 -> exe+0x293850 -> the setter), with the new place on the calling thread's stack,
//! 0x4a8 bytes above the setter's rsp.
//!
//! Our code: when the body is the player's and the new place sits just above the stack pointer (a caller's
//! own copy, within 64 KB), it first copies the player's current place (where Fly has the player)
//! into it. The player's turn still comes from the game, and the game still runs its whole setter,
//! so the rest of the game hears where the player is. Any other body, or a place handed over from
//! anywhere else, runs as before.
//!
//! The game's own copy of the player's place: that per-frame update (exe+0xc73620 in that build)
//! builds the player's place from a physics object: its `this` holds the player's body at +0x10 and
//! the object at +0x440; the object's +0x50 is x y z as floats from the world's origin,
//! `[[object+8]+0xa40]+0x30` (three doubles, all three added, exe+0xd00e10). While the player was
//! held the game left that copy where Fly came on, and the first frame after Fly off put the
//! player's back there. keeping the copy on the player's every 5 ms
//! from outside the game took the snap away on land (much better). A second
//! hook where the game reads those floats notes the object for the player's body (`copy_code`), and
//! Fly's worker writes the copy each tick while the player is held (`keep_copy`). In water the game
//! still moves the copy itself every frame.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::airwalk::{Hook, SLOT_AT, hook_at, rel, swap, swap_paused};
use crate::{log, memory};

/// The setter's start: `mov [rsp+8], rbx` (5 bytes, hooked), the rest of its frame, then `movzx
/// ebp, r8b; mov byte [rcx+0x176], 0; mov rdi, rcx; call` (the 0x50-byte copy).
const SETTER: &str = "48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 20 41 0F B6 E8 C6 81 76 01 00 00 00 48 8B F9 E8";
const SETTER_ORIG: [u8; 5] = [0x48, 0x89, 0x5C, 0x24, 0x08];
/// In the body: x y z (doubles), as `fly::BODY_POS`.
const POS: u8 = 0x20;
/// The new place counts as a caller's own copy when it is less than this far above rsp.
const NEAR: u32 = 0x10000;
/// Where our code goes on into the game's own first instruction.
const GAME_AT: usize = 46;

/// In the per-frame update: `mov rdi, rcx; xorps xmm2, xmm2; mov rcx, [rcx+0x440]; mov rbx, rdx`,
/// then `movups xmm3, [rcx+0x50]; movups xmm0, [rcx+0x20]` (the two we move, 8 bytes at COPY_AT)
/// and `movups xmm1, [rcx+0x30]`. One hit in the Vulkan and the DirectX 11 exe.
const COPY: &str = "48 8B F9 0F 57 D2 48 8B 89 40 04 00 00 48 8B DA 0F 10 59 50 0F 10 41 20 0F 10 49 30";
const COPY_AT: u64 = 0x10;
const COPY_ORIG: [u8; 8] = [0x0F, 0x10, 0x59, 0x50, 0x0F, 0x10, 0x41, 0x20];
/// In `this` (rdi there): the body it places, and the physics object (rcx there).
const THIS_BODY: u8 = 0x10;
const THIS_OBJ: u64 = 0x440;
/// In the object: x y z floats from the origin; the world's part at +8, the origin's at +0xa40.
const OBJ_POS: u64 = 0x50;
const OBJ_WORLD: u64 = 0x8;
const WORLD_ORIGIN: u64 = 0xa40;
const ORIGIN_AT: u64 = 0x30;
/// In the copy block after the player's body (SLOT_AT): the object, `this`, and how many times the
/// game read the copy for the player.
const GOT_OBJ: u64 = SLOT_AT + 0x8;
const GOT_THIS: u64 = SLOT_AT + 0x10;
const GOT_COUNT: u64 = SLOT_AT + 0x18;
/// Where the copy code goes on into the game's two instructions.
const COPY_GAME_AT: usize = 39;

struct State {
    hook: Hook,
    /// The copy hook. Fly Through works without it (then the snap after Fly off is back).
    copy: Option<Hook>,
    on: bool,
    copy_in: bool,
}

static HOOK: Mutex<Option<State>> = Mutex::new(None);
static BROKEN: Mutex<Option<String>> = Mutex::new(None);
/// The setter search runs once: the warm-up and a Fly turned on before it ends wait for the same
/// one. It runs without `HOOK`, so Fly off, `hold` and the unload never wait for it.
static PREP: Mutex<()> = Mutex::new(());

/// Our code, 56 bytes at `block`, for the setter at `at`:
/// ```text
///       cmp rcx, [rip+player]      ; the player body (0: nobody)
///       jne game
///       mov rax, rdx; the new place is a caller's copy on the stack:
///       sub rax, rsp;   less than NEAR above rsp (below rsp wraps round
///       cmp rax, NEAR           ;   to a huge number, so it goes to the game too)
///       jae game
///       mov rax, [rcx+0x20]     ; x y z: where Fly holds the player
///       mov [rdx], rax
///       mov rax, [rcx+0x28]
///       mov [rdx+0x8], rax
///       mov rax, [rcx+0x30]
///       mov [rdx+0x10], rax
/// game: mov [rsp+0x8], rbx; the setter's first instruction
///       jmp setter+5
/// ```
/// Only rax and the flags change, both free at a function's start.
fn code(block: u64, at: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x48, 0x3B, 0x0D];
    c.extend_from_slice(&rel(block + 7, block + SLOT_AT)?);
    c.extend_from_slice(&[0x75, (GAME_AT - 9) as u8]);
    c.extend_from_slice(&[0x48, 0x8B, 0xC2, 0x48, 0x2B, 0xC4, 0x48, 0x3D]);
    c.extend_from_slice(&NEAR.to_le_bytes());
    c.extend_from_slice(&[0x73, (GAME_AT - 23) as u8]);
    c.extend_from_slice(&[0x48, 0x8B, 0x41, POS, 0x48, 0x89, 0x02]);
    c.extend_from_slice(&[0x48, 0x8B, 0x41, POS + 8, 0x48, 0x89, 0x42, 0x08]);
    c.extend_from_slice(&[0x48, 0x8B, 0x41, POS + 0x10, 0x48, 0x89, 0x42, 0x10]);
    if c.len() != GAME_AT {
        return Err("fly through code jumps are off".into());
    }
    c.extend_from_slice(&SETTER_ORIG);
    c.push(0xE9);
    c.extend_from_slice(&rel(block + c.len() as u64 + 4, at + 5)?);
    Ok(c)
}

/// Our code at the copy read, 52 bytes at `block`, for the read at `at`:
/// ```text
///       mov rax, [rip+player]      ; the player body (0: nobody)
///       test rax, rax
///       jz game
///       cmp [rdi+0x10], rax; the body this one places
///       jne game
///       mov [rip+obj], rcx; own: note the object and `this`,
///       mov [rip+this], rdi
///       inc qword [rip+count]   ;   and one more read
/// game: movups xmm3, [rcx+0x50] ; the game's two instructions
///       movups xmm0, [rcx+0x20]
///       jmp at+8
/// ```
/// Only rax and the flags change. Both are free there: rax is next written by a call's return, and
/// no flag is read before the next compare. Nothing in the game changes.
fn copy_code(block: u64, at: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x48, 0x8B, 0x05];
    c.extend_from_slice(&rel(block + 7, block + SLOT_AT)?);
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, (COPY_GAME_AT - 12) as u8]);
    c.extend_from_slice(&[0x48, 0x39, 0x47, THIS_BODY, 0x75, (COPY_GAME_AT - 18) as u8]);
    c.extend_from_slice(&[0x48, 0x89, 0x0D]);
    c.extend_from_slice(&rel(block + 25, block + GOT_OBJ)?);
    c.extend_from_slice(&[0x48, 0x89, 0x3D]);
    c.extend_from_slice(&rel(block + 32, block + GOT_THIS)?);
    c.extend_from_slice(&[0x48, 0xFF, 0x05]);
    c.extend_from_slice(&rel(block + 39, block + GOT_COUNT)?);
    if c.len() != COPY_GAME_AT {
        return Err("fly through copy code jumps are off".into());
    }
    c.extend_from_slice(&COPY_ORIG);
    c.push(0xE9);
    c.extend_from_slice(&rel(block + c.len() as u64 + 4, at + COPY_ORIG.len() as u64)?);
    Ok(c)
}

fn prepare() -> Result<State, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let at = memory::find_code(&exe, "fly through setter", SETTER)?;
    if memory::read(at, 5)? != SETTER_ORIG {
        return Err("setter code is not what we expect".into());
    }
    let hook = hook_at(&exe, at, "fly through", |block| code(block, at))?;
    let copy = match prepare_copy(&exe) {
        Ok(h) => Some(h),
        Err(e) => {
            log(&format!("fly through copy: {e} (Fly Through works, the snap after Fly off may come back)"));
            None
        }
    };
    Ok(State { hook, copy, on: false, copy_in: false })
}

fn prepare_copy(exe: &aurora_core::proto::ModuleInfo) -> Result<Hook, String> {
    let at = memory::find_code(exe, "fly through copy", COPY)? + COPY_AT;
    if memory::read(at, COPY_ORIG.len())? != COPY_ORIG {
        return Err("the copy read is not what we expect".into());
    }
    hook_at(exe, at, "fly through copy", |block| copy_code(block, at))
}

/// Find the setter once (at load, so Fly switches on at once).
pub fn warm() {
    let _ = ready();
}

/// The hook ready, searching for the setter first if no one has (see `PREP`).
fn ready() -> Result<(), String> {
    let _one = PREP.lock().map_err(|_| "fly through lock")?;
    if HOOK.lock().map_err(|_| "fly through lock")?.is_some() {
        return Ok(());
    }
    if let Some(e) = BROKEN.lock().ok().and_then(|b| b.clone()) {
        return Err(e);
    }
    match prepare() {
        Ok(s) => {
            *HOOK.lock().map_err(|_| "fly through lock")? = Some(s);
            Ok(())
        }
        Err(e) => {
            log(&format!("fly through: {e}"));
            if let Ok(mut b) = BROKEN.lock() {
                *b = Some(e.clone());
            }
            Err(e)
        }
    }
}

/// In with Fly (holding nobody yet), out with it.
pub fn set(on: bool) -> Result<(), String> {
    if on {
        ready()?;
    }
    let mut slot = HOOK.lock().map_err(|_| "fly through lock")?;
    let Some(s) = slot.as_mut() else { return Ok(()) };
    if s.on == on {
        return Ok(());
    }
    // Nobody is held while the jumps go in or out.
    let _ = memory::write(s.hook.block + SLOT_AT, &[0; 8]);
    if let Some(c) = &s.copy {
        let _ = memory::write(c.block + SLOT_AT, &[0; 8]);
    }
    if !swap(&s.hook, on)? {
        return Err("setter code word changed; left alone".into());
    }
    s.on = on;
    // The copy hook follows; on its own failure Fly Through still works.
    if let Some(c) = &s.copy
        && s.copy_in != on
    {
        // Its jump covers two of the game's instructions (patch 3.20): in with the game's threads
        // paused, none at the second one.
        match swap_paused(c, on) {
            Ok(true) => s.copy_in = on,
            Ok(false) => log("fly through copy: its code word changed; left alone"),
            Err(e) => log(&format!("fly through copy: {e}")),
        }
    }
    log(&format!("fly through {}", if on { "in" } else { "out" }));
    Ok(())
}

/// From now on the game leaves `body` (the player's body) where Fly has the player. Only while the
/// hook is in.
pub fn hold(body: u64) {
    let Ok(slot) = HOOK.lock() else { return };
    if let Some(s) = slot.as_ref().filter(|s| s.on) {
        match memory::write(s.hook.block + SLOT_AT, &body.to_le_bytes()) {
            Ok(()) => log(&format!("fly through: holding the player's body {body:#x}")),
            Err(e) => log(&format!("fly through: could not hold the player: {e}")),
        }
        if let Some(c) = s.copy.as_ref().filter(|_| s.copy_in) {
            let _ = memory::write(c.block + SLOT_AT, &body.to_le_bytes());
        }
    }
}

/// Nobody is held any more: the player's body went away (a respawn or a world change), and another
/// body could get its address. Fly holds the player again once it is back (`hold`).
pub fn let_go() {
    let_go_because("the player's body went away");
}

/// Nobody is held any more, for `why` (said in the log).
pub fn let_go_because(why: &str) {
    let Ok(slot) = HOOK.lock() else { return };
    if let Some(s) = slot.as_ref().filter(|s| s.on) {
        if let Some(c) = &s.copy {
            let _ = memory::write(c.block + SLOT_AT, &[0; 8]);
        }
        match memory::write(s.hook.block + SLOT_AT, &[0; 8]) {
            Ok(()) => log(&format!("fly through: let go ({why})")),
            Err(e) => log(&format!("fly through: could not let go: {e}")),
        }
    }
}

/// Off (unload).
pub fn shutdown() {
    let _ = set(false);
}

/// The copy block while both hooks are in.
fn copy_block() -> Option<u64> {
    let slot = HOOK.lock().ok()?;
    let s = slot.as_ref()?;
    if s.on && s.copy_in { s.copy.as_ref().map(|c| c.block) } else { None }
}

/// The game read its copy for the player within this long: still the player's to write (a world change or a
/// load ends the reads).
const FRESH: Duration = Duration::from_millis(250);
/// At the first look in a hold the copy must be this close to where Fly has the player (world
/// units; 0 to 1.6 seen live).
const OWN: f64 = 5.0;
/// The game moved its copy when it is this far from what Fly wrote (world units).
const MOVED: f64 = 0.5;
/// The origin and places are sane below this (world units).
const SANE: f64 = 1.0e6;

/// What Fly keeps between ticks to hold the game's copy of the player's place on the player
/// (`keep_copy`).
#[derive(Default)]
pub struct Copy {
    seen: u64,
    fresh: Option<Instant>,
    obj: u64,
    own: Option<bool>,
    wrote: Option<[f32; 3]>,
    writes: u64,
    moved: u64,
    /// The game's last move of the copy: by how much (x y z), and when.
    last_move: Option<([f32; 3], Instant)>,
    /// This tick: how far the game moved the copy since Fly's last write (any size), and the copy
    /// in the world before Fly's write. None when the copy was not looked at this tick.
    delta: Option<[f32; 3]>,
    in_world: Option<[f64; 3]>,
}

/// A move of the game's copy that is not a plain fall: more than 0.1 units sideways, more than 0.1
/// up, or more than 1 down in one go. Falling moves the copy down a little each physics step (under
/// 1 unit up to about 45 units a second); swimming put it 5.7 units down at once and pulled it
/// sideways.
pub fn foreign(d: [f32; 3]) -> bool {
    let [x, y, z] = d.map(f64::from);
    let side = (x * x + y * y).sqrt();
    !side.is_finite() || !z.is_finite() || side > 0.1 || z > 0.1 || z < -1.0
}

impl Copy {
    /// The game moved the copy (more than half a unit) within `within`.
    pub fn busy(&self, within: Duration) -> bool {
        self.last_move.is_some_and(|(_, t)| t.elapsed() < within)
    }

    /// Moves of more than half a unit while Fly kept the copy.
    pub fn moves(&self) -> u64 {
        self.moved
    }

    /// This tick's move by the game (see `delta`).
    pub fn delta(&self) -> Option<[f32; 3]> {
        self.delta
    }

    /// This tick's copy in the world, before Fly's write.
    pub fn seen(&self) -> Option<[f64; 3]> {
        self.in_world
    }

    /// The copy in the world right now, read again (also after Fly let go). Only for the object
    /// found the player's in this hold.
    pub fn now(&self) -> Option<[f64; 3]> {
        if self.obj < 0x10000 || self.own != Some(true) {
            return None;
        }
        let o = origin(self.obj)?;
        let cur = read_f32x3(self.obj + OBJ_POS)?;
        let w = world(cur, o);
        sane(w).then_some(w)
    }

    /// One line when a hold ends: how often Fly kept the copy on the player and the game moved it.
    pub fn said(&self) {
        if self.writes == 0 {
            return;
        }
        let last = match self.last_move {
            Some((d, t)) => format!(", the last {} ms before the end by ({:.2}, {:.2}, {:.2})", t.elapsed().as_millis(), d[0], d[1], d[2]),
            None => String::new(),
        };
        log(&format!("fly through: kept the game's copy on the player ({} writes; the game moved it {} times{last})", self.writes, self.moved));
    }
}

fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

fn read_u64(addr: u64) -> Option<u64> {
    memory::read(addr, 8).ok().map(|b| u64_at(&b, 0))
}

fn read_f32x3(addr: u64) -> Option<[f32; 3]> {
    let b = memory::read(addr, 12).ok()?;
    Some([0, 4, 8].map(|i| f32::from_le_bytes(b[i..i + 4].try_into().unwrap())))
}

/// The world's origin for this object, as the game adds it (exe+0xd00e10 in that build).
fn origin(obj: u64) -> Option<[f64; 3]> {
    let w = read_u64(obj + OBJ_WORLD).filter(|&w| w > 0x10000)?;
    let o = read_u64(w + WORLD_ORIGIN).filter(|&o| o > 0x10000)?;
    let b = memory::read(o + ORIGIN_AT, 24).ok()?;
    let v = [0, 8, 16].map(|i| f64::from_le_bytes(b[i..i + 8].try_into().unwrap()));
    sane(v).then_some(v)
}

fn sane(v: [f64; 3]) -> bool {
    v.iter().all(|x| x.is_finite() && x.abs() < SANE)
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The copy's floats as a place in the world.
pub fn world(copy: [f32; 3], origin: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| f64::from(copy[i]) + origin[i])
}

/// A place in the world as the copy's floats, if sane.
pub fn local(place: [f64; 3], origin: [f64; 3]) -> Option<[f32; 3]> {
    if !sane(place) || !sane(origin) {
        return None;
    }
    let v = [0, 1, 2].map(|i| (place[i] - origin[i]) as f32);
    v.iter().all(|x| x.is_finite() && f64::from(x.abs()) < SANE).then_some(v)
}

/// The game moved its copy away from what Fly wrote.
pub fn moved(now: [f32; 3], wrote: [f32; 3]) -> bool {
    let d = (0..3).map(|i| (f64::from(now[i]) - f64::from(wrote[i])).powi(2)).sum::<f64>().sqrt();
    !d.is_finite() || d > MOVED
}

/// Keep the game's copy of the player's place on `target` (where Fly has the player): each tick
/// while the player is held. Everything is read again right before the write: the game's own way
/// from the player's body to the copy, the origin, the copy itself. At the first look of a hold the
/// copy must already be near the player, or it is left alone for that hold.
pub fn keep_copy(c: &mut Copy, body: u64, target: [f64; 3]) {
    c.delta = None;
    c.in_world = None;
    let Some(block) = copy_block() else { return };
    let Ok(got) = memory::read(block + GOT_OBJ, 24) else { return };
    let (obj, this, count) = (u64_at(&got, 0), u64_at(&got, 8), u64_at(&got, 16));
    let now = Instant::now();
    if count != c.seen {
        c.seen = count;
        c.fresh = Some(now);
    }
    if obj < 0x10000 || !c.fresh.is_some_and(|t| now.duration_since(t) < FRESH) {
        return;
    }
    if read_u64(this.wrapping_add(u64::from(THIS_BODY))) != Some(body) || read_u64(this.wrapping_add(THIS_OBJ)) != Some(obj) {
        return;
    }
    if obj != c.obj {
        // The first look this hold, or the game made a new object: check it is the player's first.
        c.obj = obj;
        c.own = None;
        c.wrote = None;
    }
    let Some(o) = origin(obj) else { return };
    let Some(cur) = read_f32x3(obj + OBJ_POS) else { return };
    if c.own.is_none() {
        let d = dist(world(cur, o), target);
        let own = d <= OWN;
        if !own {
            log(&format!("fly through: the game's copy is {d:.1} units from the player; left alone this time"));
        }
        c.own = Some(own);
    }
    if c.own != Some(true) {
        return;
    }
    c.in_world = Some(world(cur, o));
    if let Some(w) = c.wrote {
        let d = [cur[0] - w[0], cur[1] - w[1], cur[2] - w[2]];
        c.delta = Some(d);
        if moved(cur, w) {
            c.moved += 1;
            c.last_move = Some((d, now));
        }
    }
    let Some(l) = local(target, o) else { return };
    let mut b = Vec::with_capacity(12);
    for v in l {
        b.extend_from_slice(&v.to_le_bytes());
    }
    if memory::write(obj + OBJ_POS, &b).is_ok() {
        c.wrote = Some(l);
        c.writes += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_fits_the_block_and_its_jumps_land() {
        let block = 0x7ff6_0000_1000u64;
        let at = 0x7ff6_1000_0000u64;
        let c = code(block, at).unwrap();
        assert_eq!(c.len(), 56);
        assert!(c.len() as u64 <= SLOT_AT, "the code ends before the slot");
        // The slot read: the end of the cmp plus its distance is the slot.
        let disp = i32::from_le_bytes(c[3..7].try_into().unwrap()) as i64;
        assert_eq!((block + 7) as i64 + disp, (block + SLOT_AT) as i64);
        // Jne and jae both go on to the game's own first instruction.
        for (op, end) in [(0x75u8, 9usize), (0x73, 23)] {
            assert_eq!(c[end - 2], op);
            assert_eq!(end + c[end - 1] as usize, GAME_AT);
        }
        assert_eq!(&c[GAME_AT..GAME_AT + 5], &SETTER_ORIG);
        // Then back into the setter, after the instruction we moved.
        assert_eq!(c[GAME_AT + 5], 0xE9);
        let back = i32::from_le_bytes(c[GAME_AT + 6..GAME_AT + 10].try_into().unwrap()) as i64;
        assert_eq!((block + GAME_AT as u64 + 10) as i64 + back, (at + 5) as i64);
    }

    #[test]
    fn it_copies_her_x_y_z_only_into_a_copy_near_rsp() {
        let c = code(0x1000, 0x2000_0000).unwrap();
        // Mov rax, rdx; sub rax, rsp; cmp rax, 0x10000
        assert_eq!(&c[9..21], &[0x48, 0x8B, 0xC2, 0x48, 0x2B, 0xC4, 0x48, 0x3D, 0x00, 0x00, 0x01, 0x00]);
        assert_eq!(&c[23..30], &[0x48, 0x8B, 0x41, 0x20, 0x48, 0x89, 0x02]);
        assert_eq!(&c[30..38], &[0x48, 0x8B, 0x41, 0x28, 0x48, 0x89, 0x42, 0x08]);
        assert_eq!(&c[38..46], &[0x48, 0x8B, 0x41, 0x30, 0x48, 0x89, 0x42, 0x10]);
    }

    #[test]
    fn a_jump_back_too_far_is_refused() {
        assert!(code(0x1000, 0x7ff6_0000_0000).is_err());
        assert!(copy_code(0x1000, 0x7ff6_0000_0000).is_err());
    }

    /// Where a rip-relative operand ending at `end` (its rel32 in the 4 bytes before) points.
    fn target(c: &[u8], block: u64, end: usize) -> u64 {
        let disp = i32::from_le_bytes(c[end - 4..end].try_into().unwrap()) as i64;
        ((block + end as u64) as i64 + disp) as u64
    }

    #[test]
    fn copy_code_notes_her_object_and_runs_the_game_s_two_reads() {
        let block = 0x7ff6_0000_1000u64;
        let at = 0x7ff6_1000_0651u64;
        let c = copy_code(block, at).unwrap();
        assert_eq!(c.len(), 52);
        assert!(c.len() as u64 <= SLOT_AT, "the code ends before the slot");
        // Mov rax, [the player]; test rax, rax; jz game; cmp [rdi+0x10], rax; jne game
        assert_eq!(&c[0..3], &[0x48, 0x8B, 0x05]);
        assert_eq!(target(&c, block, 7), block + SLOT_AT);
        assert_eq!(&c[7..10], &[0x48, 0x85, 0xC0]);
        assert_eq!(&c[12..16], &[0x48, 0x39, 0x47, 0x10]);
        for (op, end) in [(0x74u8, 12usize), (0x75, 18)] {
            assert_eq!(c[end - 2], op);
            assert_eq!(end + c[end - 1] as usize, COPY_GAME_AT);
        }
        // The object, `this` and the count go to their own words after the player's body.
        assert_eq!(&c[18..21], &[0x48, 0x89, 0x0D]);
        assert_eq!(target(&c, block, 25), block + GOT_OBJ);
        assert_eq!(&c[25..28], &[0x48, 0x89, 0x3D]);
        assert_eq!(target(&c, block, 32), block + GOT_THIS);
        assert_eq!(&c[32..35], &[0x48, 0xFF, 0x05]);
        assert_eq!(target(&c, block, 39), block + GOT_COUNT);
        const { assert!(GOT_COUNT + 8 <= 0x80, "inside the block") };
        // The game's two instructions, then back after them.
        assert_eq!(&c[COPY_GAME_AT..COPY_GAME_AT + 8], &COPY_ORIG);
        assert_eq!(c[COPY_GAME_AT + 8], 0xE9);
        assert_eq!(target(&c, block, 52), at + 8);
        // The pattern holds the two instructions at COPY_AT, and the offsets we read.
        let pat: Vec<u8> = COPY.split(' ').map(|h| u8::from_str_radix(h, 16).unwrap()).collect();
        assert_eq!(&pat[COPY_AT as usize..COPY_AT as usize + 8], &COPY_ORIG);
        assert_eq!(&pat[6..13], &[0x48, 0x8B, 0x89, 0x40, 0x04, 0x00, 0x00], "mov rcx, [rcx+0x440]");
        assert_eq!(u32::from_le_bytes(pat[9..13].try_into().unwrap()) as u64, THIS_OBJ);
        assert_eq!(pat[COPY_AT as usize + 3] as u64, OBJ_POS);
    }

    #[test]
    fn the_copy_is_floats_from_the_origin() {
        // The player's game: origin (11453.739, 11258.869, 0), the player's place (11557.21,
        // 11970.51, 202.92).
        let o = [11453.739257812502, 11258.869140625002, 0.0];
        let pos = [11557.21, 11970.51, 202.92];
        let l = local(pos, o).unwrap();
        assert!((f64::from(l[0]) - 103.47).abs() < 0.01 && (f64::from(l[1]) - 711.64).abs() < 0.01, "{l:?}");
        assert!(dist(world(l, o), pos) < 0.001, "and back");
        // All three are added, z too.
        let up = local(pos, [o[0], o[1], 2.0]).unwrap();
        assert!((f64::from(up[2]) - 200.92).abs() < 0.001);
        // Never a place from a broken read.
        assert_eq!(local([f64::NAN, 0.0, 0.0], o), None);
        assert_eq!(local(pos, [f64::INFINITY, 0.0, 0.0]), None);
        assert_eq!(local([2.0e6, 0.0, 0.0], o), None);
        assert!(!sane([0.0, -1.0e6, 0.0]) && sane([0.0, -9.9e5, 0.0]));
    }

    #[test]
    fn a_plain_fall_is_not_foreign() {
        // Falling: a little down each physics step, even fast (40 units a second at 45 steps).
        assert!(!foreign([0.0, 0.0, 0.0]));
        assert!(!foreign([0.0, 0.0, -0.05]));
        assert!(!foreign([0.02, -0.03, -0.9]));
        // Swimming, live 5.7 down at once; pulled sideways 0.2 to 0.6 a step.
        assert!(foreign([0.0, 0.0, -5.7]));
        assert!(foreign([-0.25, -0.07, 0.0]));
        // Pushed up (out of the ground), or a broken read.
        assert!(foreign([0.0, 0.0, 0.2]));
        assert!(foreign([f32::NAN, 0.0, 0.0]));
        assert!(foreign([0.0, 0.0, f32::INFINITY]));
    }

    #[test]
    fn a_move_is_more_than_half_a_unit() {
        let w = [100.0, 200.0, 30.0];
        assert!(!moved(w, w));
        assert!(!moved([100.3, 200.0, 30.3], w), "0.42 units");
        assert!(moved([100.0, 200.0, 24.3], w), "pulled down to the water (5.7 units, live 6 Nov)");
        assert!(moved([f32::NAN, 200.0, 30.0], w), "a broken copy counts as moved");
    }
}
