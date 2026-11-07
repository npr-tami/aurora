//! Things near the player: every physics body of the player's body class, with its position. Found
//! by scanning the heap for the class tag (first 8 bytes of the body). The ESP reads the game's own
//! list of every entity in the player's world first (`WorldList`, instant ESP); the scan is its
//! fallback.

use std::ffi::c_void;

use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, PAGE_NOCACHE, PAGE_WRITECOMBINE, VirtualQuery,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::{fly, globals, memory};

/// NPC markers live in small blocks of 64 to 256 KB. Anything bigger is skipped, which is what
/// makes the scan fast.
const MAX_BLOCK: u64 = 512 * 1024;
const CHUNK: usize = 8 * 1024 * 1024;

/// Copy our own memory through the kernel: a block freed mid-scan fails the read instead of
/// faulting the game.
pub(crate) fn safe_read(addr: u64, buf: &mut [u8]) -> usize {
    let mut got = 0usize;
    // SAFETY: buffer is ours; the kernel checks the source range.
    let ok = unsafe { ReadProcessMemory(GetCurrentProcess(), addr as *const c_void, buf.as_mut_ptr() as *mut c_void, buf.len(), &mut got) };
    if ok == 0 { 0 } else { got }
}

const POS: u64 = 0x20;

/// A physics body near the player.
#[derive(Debug, Clone, PartialEq)]
pub struct Body {
    pub body: u64,
    pub pos: [f64; 3],
    pub dist: f64,
    /// The character object that points at this body (its +8), the way to the rest of the NPC.
    pub owner: u64,
    /// Another player, not an NPC.
    pub player: bool,
}

/// The player's body position and class tag.
fn player() -> Option<(u64, [f64; 3], u64)> {
    let pos_addr = fly::body_addr()?;
    let body = pos_addr.checked_sub(POS)?;
    let tag = u64::from_le_bytes(memory::read(body, 8).ok()?.try_into().unwrap());
    let b = memory::read(pos_addr, 24).ok()?;
    let p = [
        f64::from_le_bytes(b[0..8].try_into().unwrap()),
        f64::from_le_bytes(b[8..16].try_into().unwrap()),
        f64::from_le_bytes(b[16..24].try_into().unwrap()),
    ];
    Some((body, p, tag))
}

pub(crate) fn pos_of(body: u64) -> Option<[f64; 3]> {
    let b = memory::read(body.checked_add(POS)?, 24).ok()?;
    let p = [
        f64::from_le_bytes(b[0..8].try_into().unwrap()),
        f64::from_le_bytes(b[8..16].try_into().unwrap()),
        f64::from_le_bytes(b[16..24].try_into().unwrap()),
    ];
    p.iter().all(|v| v.is_finite() && v.abs() < 1.0e5).then_some(p)
}

/// Memory blocks that held bodies last time; later scans read only these.
static BLOCKS: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());
/// Every block seen at the last walk of memory. A quick scan also reads blocks that are new since
/// then: newly spawned NPCs land there, and waited for the 10 s full scan before.
static KNOWN: std::sync::Mutex<Option<std::collections::HashSet<(u64, u64)>>> = std::sync::Mutex::new(None);
/// When the last full scan ran.
static LAST_FULL: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
/// New NPCs spawn into blocks a quick scan does not know, so a quick scan turns full once this long
/// has passed.
const FULL_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn full_due() -> bool {
    LAST_FULL.lock().map(|l| l.is_none_or(|t| t.elapsed() > FULL_EVERY)).unwrap_or(true)
}

pub(crate) fn mark_full() {
    if let Ok(mut l) = LAST_FULL.lock() {
        *l = Some(std::time::Instant::now());
    }
}

/// Warm-up at inject: one full scan in the background so later scans are instant. The thread is
/// joined on unload; an unload during the wait skips the scan.
pub fn warm_up() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        // Give the window a moment to connect first.
        if crate::nap(2000) {
            let _ = near(5000.0, 1, true, true);
        }
    })
}

/// Every committed private writable block of a sane size.
fn all_blocks() -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    // Thread stacks hold copies of the marker words in their locals: a thread stack (an allocation
    // with a guard page) is never a character. After a re-inject the first match was such a copy,
    // God Mode wrote into the stack and the game closed.
    let mut stacks: Vec<u64> = Vec::new();
    let mut cur: u64 = 0;
    loop {
        // SAFETY: inspection of our own address space.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            break;
        }
        let base = info.BaseAddress as u64;
        let size = info.RegionSize as u64;
        // Not the graphics card's memory (write-combined or uncached): characters never live there,
        // and reading it while a load moves it around is a suspect for the GPU resets.
        let gpu = info.Protect & (PAGE_NOCACHE | PAGE_WRITECOMBINE) != 0;
        let ok = info.State == MEM_COMMIT && info.Type == MEM_PRIVATE && info.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0 && info.Protect & 0xCC != 0 && !gpu;
        if info.Protect & PAGE_GUARD != 0 {
            stacks.push(info.AllocationBase as u64);
        }
        if ok && size <= MAX_BLOCK {
            v.push((base, size, info.AllocationBase as u64));
        }
        match base.checked_add(size) {
            Some(next) if next > cur => cur = next,
            _ => break,
        }
    }
    v.into_iter().filter(|(_, _, alloc)| !stacks.contains(alloc)).map(|(b, s, _)| (b, s)).collect()
}

/// One object of this class per NPC, its +8 is the NPC's body. The player has none.
/// (`globals::CHAR_COMPONENT`) The same kind of object for players (the player and any other
/// player): its +8 is the body too. (`globals::PLAYER_COMPONENT`) The class an NPC's (or a
/// player's) character object must have, as an address.
pub fn marker_class(player: bool) -> Option<u64> {
    let exe = memory::find_module("")?;
    Some(exe.base + if player { globals::PLAYER_COMPONENT.rva()? } else { globals::CHAR_COMPONENT.rva()? })
}

/// Characters within `radius` metres of the player, nearest first. The player is left out.
static MY_CHAR: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

fn read_u64(addr: u64) -> Option<u64> {
    memory::read(addr, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

/// Whose character an object is: `Ok` when its +8 is the player's body, else `Err((its +8, the
/// player's body))` (0 where unreadable). God mode checks every caught entity with this: the damage
/// check runs for whoever is being damaged, so a hit the player deals to an NPC must not be taken
/// as the player's.
pub fn owner_check(entity: u64) -> Result<(), (u64, u64)> {
    let me = player().map(|p| p.0).unwrap_or(0);
    let its = read_u64(entity + 8).unwrap_or(0);
    if me != 0 && its == me { Ok(()) } else { Err((its, me)) }
}

/// The player's own character object: the player-class object whose +8 is the player's body. God
/// mode's entity is exactly this object, so god mode can find it without a hit, and a death or
/// respawn cannot leave a stale one.
pub fn my_character() -> Option<u64> {
    let (me, _, _) = player()?;
    let exe = memory::find_module("")?;
    let tag = exe.base + globals::PLAYER_COMPONENT.rva()?;
    let cached = MY_CHAR.lock().ok().map(|c| *c).unwrap_or(0);
    if cached != 0 && read_u64(cached) == Some(tag) && read_u64(cached + 8) == Some(me) {
        return Some(cached);
    }
    let remembered = BLOCKS.lock().map(|b| b.clone()).unwrap_or_default();
    let mut buf = vec![0u8; CHUNK];
    for blocks in [remembered, all_blocks()] {
        for (base, size) in blocks {
            let want = (size as usize).min(buf.len());
            let got = safe_read(base, &mut buf[..want]);
            let (words, _) = buf[..got].as_chunks::<8>();
            for (i, w) in words.iter().enumerate() {
                if u64::from_le_bytes(*w) == tag && words.get(i + 1).map(|n| u64::from_le_bytes(*n)) == Some(me) {
                    let found = base + (i * 8) as u64;
                    if let Ok(mut c) = MY_CHAR.lock() {
                        *c = found;
                    }
                    return Some(found);
                }
            }
        }
    }
    None
}

/// `players` adds other players (marked); only the ESP asks for them.
pub fn near(radius: f64, max: usize, full: bool, players: bool) -> Result<Vec<Body>, String> {
    let (me, mine, _) = player().ok_or("player body not found")?;
    let exe = memory::find_module("").ok_or("exe not found")?;
    let tag = exe.base + globals::CHAR_COMPONENT.rva().ok_or("npc class not found yet")?;
    let player_tag = exe.base + globals::PLAYER_COMPONENT.rva().ok_or("player class not found yet")?;
    let remembered = BLOCKS.lock().map(|b| b.clone()).unwrap_or_default();
    let use_cache = !full && !remembered.is_empty();
    let started = std::time::Instant::now();
    let now_blocks = all_blocks();
    let mut fresh = 0usize;
    let blocks = if use_cache {
        let mut v = remembered;
        if let Ok(k) = KNOWN.lock()
            && let Some(known) = k.as_ref()
        {
            let had: std::collections::HashSet<(u64, u64)> = v.iter().copied().collect();
            for b in &now_blocks {
                if !known.contains(b) && !had.contains(b) {
                    v.push(*b);
                    fresh += 1;
                }
            }
        }
        v
    } else {
        now_blocks.clone()
    };
    if let Ok(mut k) = KNOWN.lock() {
        *k = Some(now_blocks.into_iter().collect());
    }
    let mut read_total = 0u64;
    let mut out = Vec::new();
    let mut hit_blocks: Vec<(u64, u64)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut buf = vec![0u8; CHUNK];
    for (base, size) in blocks {
        let mut had = false;
        // Small blocks are in use as a whole; no page-by-page check needed.
        for (rbase, rsize) in [(base, size)] {
            let mut off = 0u64;
            while off < rsize {
                let want = ((rsize - off) as usize).min(buf.len());
                let got = safe_read(rbase + off, &mut buf[..want]);
                if got < 16 {
                    break;
                }
                // A short breather now and then, so the scan never hogs the memory manager while
                // the game (or its graphics driver) needs it.
                if read_total / (64 << 20) != (read_total + got as u64) / (64 << 20) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                read_total += got as u64;
                let (words, _) = buf[..got].as_chunks::<8>();
                for (i, w) in words.iter().enumerate() {
                    let word = u64::from_le_bytes(*w);
                    let is_player = word == player_tag;
                    if (word != tag && !is_player) || i + 1 >= words.len() {
                        continue;
                    }
                    had = true;
                    let body = u64::from_le_bytes(words[i + 1]);
                    if body < 0x10000 || body == me || (is_player && !players) || !seen.insert(body) {
                        continue;
                    }
                    if let Some(p) = pos_of(body) {
                        let d = ((p[0] - mine[0]).powi(2) + (p[1] - mine[1]).powi(2) + (p[2] - mine[2]).powi(2)).sqrt();
                        if d <= radius {
                            let owner = rbase + off + (i * 8) as u64;
                            out.push(Body { body, pos: p, dist: d, owner, player: is_player });
                        }
                    }
                }
                off += got as u64;
            }
        }
        if had {
            hit_blocks.push((base, size));
        }
    }
    crate::log(&format!(
        "characters: {} near, {} blocks with characters, {} new blocks, {} MB read in {} ms ({})",
        out.len(),
        hit_blocks.len(),
        fresh,
        read_total / (1024 * 1024),
        started.elapsed().as_millis(),
        if use_cache { "remembered blocks" } else { "full" }
    ));
    if !use_cache {
        let mut sizes: Vec<String> = hit_blocks.iter().map(|(b, sz)| format!("{b:#x}:{}K", sz / 1024)).collect();
        sizes.truncate(80);
        crate::log(&format!("character blocks: {}", sizes.join(" ")));
    }
    if let Ok(mut b) = BLOCKS.lock() {
        *b = hit_blocks;
    }
    out.sort_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(max);
    Ok(out)
}

/// Move a body to `pos` and hold it there for `hold_ms` (its controller would put it back
/// otherwise), then let go.
pub fn move_body(body: u64, pos: [f64; 3], hold_ms: u32) -> Result<(), String> {
    if body.checked_add(POS).is_none_or(|a| !memory::readable(a, 24)) {
        return Err("body not readable".into());
    }
    let mut b = Vec::with_capacity(24);
    for v in pos {
        b.extend_from_slice(&v.to_le_bytes());
    }
    if crate::freeze::any_with_prefix("move-") {
        return Err("one move at a time: wait two seconds".into());
    }
    let id = format!("move-{body:x}");
    // Guard on the class tag: the freeze drops itself when the body is freed or reused.
    let tag = memory::read(body, 8)?;
    crate::freeze::set_guarded(&id, body + POS, b, Some((body, tag)))?;
    // Waited for on unload, and its wait ends at once then. The holds' own shutdown lets go of the
    // body then.
    let h = std::thread::spawn(move || {
        if crate::nap(u64::from(hold_ms.clamp(50, 10_000))) {
            let _ = crate::freeze::set(&id, 0, Vec::new());
        }
    });
    crate::keep_warm("body move", h);
    Ok(())
}

/// In the player root (`globals::CAM_ROOT`): the camera forward vector (x y z floats).
const FORWARD: u64 = 0x1f0;
/// Eye height above the body position, for the aim ray.
const EYE: f64 = 1.6;
/// How far off the crosshair a target may be.
const MAX_ANGLE: f64 = 12.0;

static TARGET: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
static TARGET_READ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `Aurora\Dumper\target.txt`.
fn target_file() -> std::path::PathBuf {
    aurora_core::paths::dumper().join("target.txt")
}

pub(crate) fn camera_forward() -> Option<[f64; 3]> {
    let exe = memory::find_module("")?;
    let root = u64::from_le_bytes(memory::read(exe.base + globals::CAM_ROOT.rva()?, 8).ok()?.try_into().unwrap());
    if root < 0x10000 {
        return None;
    }
    let b = memory::read(root + FORWARD, 12).ok()?;
    let f = |i: usize| f64::from(f32::from_le_bytes(b[i..i + 4].try_into().unwrap()));
    let v = [f(0), f(4), f(8)];
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    (n > 0.5 && n < 1.5).then(|| [v[0] / n, v[1] / n, v[2] / n])
}

/// Angle in degrees between the camera ray and the direction to `p`.
fn off_crosshair(eye: [f64; 3], fwd: [f64; 3], p: [f64; 3]) -> f64 {
    let d = [p[0] - eye[0], p[1] - eye[1], p[2] + 1.0 - eye[2]];
    let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
    let c = (d[0] * fwd[0] + d[1] * fwd[1] + d[2] * fwd[2]) / n;
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// The camera's own position: the crosshair ray starts there, over the player's shoulder, not at
/// the player's head.
pub(crate) fn camera_pos() -> Option<[f64; 3]> {
    let exe = memory::find_module("")?;
    let root = u64::from_le_bytes(memory::read(exe.base + globals::CAM_ROOT.rva()?, 8).ok()?.try_into().unwrap());
    let b = memory::read(root + 0x1c0, 24).ok()?;
    let f = |i: usize| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    let p = [f(0), f(1), f(2)];
    p.iter().all(|v| v.is_finite() && v.abs() < 1.0e5).then_some(p)
}

/// Pick the NPC nearest to the crosshair (within 12 degrees, 600 units), make it the target.
pub fn aim() -> Result<(u64, [f64; 3], f64, f64), String> {
    let fwd = camera_forward().ok_or("camera direction not found")?;
    let me = player_pos().ok_or("player not found")?;
    // The player's tag far away was missed: aim from the camera, and reach further.
    let eye = camera_pos().unwrap_or([me[0], me[1], me[2] + EYE]);
    let list = near(600.0, 500, false, false)?;
    let best = list
        .iter()
        .map(|b| (off_crosshair(eye, fwd, b.pos), b))
        .filter(|(a, _)| *a <= MAX_ANGLE)
        .min_by(|a, b| (a.0 + a.1.dist * 0.01).partial_cmp(&(b.0 + b.1.dist * 0.01)).unwrap_or(std::cmp::Ordering::Equal));
    let Some((angle, b)) = best else {
        return Err("no NPC near the crosshair".into());
    };
    if let Ok(mut t) = TARGET.lock() {
        *t = b.body;
    }
    // Kept in a file, so a new build swapped in keeps the mark; never in unit tests
    // (crate::dumper_for_writing).
    if crate::dumper_for_writing().is_some() {
        let _ = std::fs::write(target_file(), format!("{:x}", b.body));
    }
    crate::log(&format!("target {:#x} at {:.1} m, {:.1} deg off", b.body, b.dist, angle));
    Ok((b.body, b.pos, b.dist, angle))
}

/// The target's body, 0 for none: one lock, no reads (the ESP asks every frame). A mark kept from
/// before a DLL swap is picked up by the first [`target`] call.
pub fn target_body() -> u64 {
    TARGET.lock().map(|t| *t).unwrap_or(0)
}

/// The current target with its live position, or None.
pub fn target() -> Option<(u64, [f64; 3], f64)> {
    let mut body = TARGET.lock().ok().map(|t| *t)?;
    // Freshly loaded: the mark from before the swap, if that body is still there.
    if body == 0 && !TARGET_READ.swap(true, std::sync::atomic::Ordering::Relaxed) {
        body = std::fs::read_to_string(target_file()).ok().and_then(|t| u64::from_str_radix(t.trim(), 16).ok()).filter(|b| pos_of(*b).is_some()).unwrap_or(0);
        if let Ok(mut t) = TARGET.lock() {
            *t = body;
        }
    }
    if body == 0 {
        return None;
    }
    let p = pos_of(body)?;
    let me = player_pos()?;
    let d = ((p[0] - me[0]).powi(2) + (p[1] - me[1]).powi(2) + (p[2] - me[2]).powi(2)).sqrt();
    Some((body, p, d))
}

pub fn yeet_target() -> Result<(), String> {
    let (body, p, _) = target().ok_or("no target")?;
    move_body(body, [p[0], p[1], p[2] + 50.0], 2000)
}

pub fn bring_target() -> Result<(), String> {
    let (body, _, _) = target().ok_or("no target")?;
    let me = player_pos().ok_or("player not found")?;
    let f = camera_forward().unwrap_or([0.0, 1.0, 0.0]);
    // Two metres in front of you, on your height.
    move_body(body, [me[0] + f[0] * 2.0, me[1] + f[1] * 2.0, me[2] + 0.2], 2000)
}

pub fn player_pos() -> Option<[f64; 3]> {
    player().map(|(_, p, _)| p)
}

// ---------------------------------------------------------------------------------------------
// Instant ESP: the game's own list of every entity in the player's world.
//
// Found offline and checked live, read only, in Erewhon: 18,100 entries, each with its own slot, handle and world; 95 of them characters; all 81
// bodies the marker scan could draw were among them.
// ---------------------------------------------------------------------------------------------

/// Instant ESP: the ESP takes its bodies from the game's world list, read every 200 ms, so a new or
/// far body shows within about a quarter of a second. `false`: the marker scan alone, as before.
pub const WORLD_LIST: bool = true;

/// In an entity (a body is one): the world it is in, 0 when none. The player's world is the
/// player's body's: the game's "current world" ([exe+0x62F8128]+0x98) went to a small menu world
/// while the player's body stayed in the big one.
const E_WORLD: u64 = 0x18;
/// In a world: the list keeper, whose first word points back at the world (a check), and the list:
/// 8-byte handles, then its count (u32) and room (u32, the top 3 bits are flags).
const W_KEEPER: u64 = 0x3E8;
const W_LIST: u64 = 0x3F0;
/// In a handle: its entity, and at +0xC a word whose bit 31 says the entity is there.
const H_LIVE: usize = 0xC;
/// In an entity: component list and count (u16), type word (u32: & 7 is 1 for a player, 2 for an
/// NPC; bit 0x4000 dead), its slot in the list (u32) and its own handle.
const E_COMPS: u64 = 0x90;
const E_NCOMP: u64 = 0x9A;
const E_TYPE: u64 = 0x108;
const E_SLOT: u64 = 0x144;
const E_HANDLE: u64 = 0x160;
/// In an entity: its box, f32 min x y z then max, around its position.
const E_BOX: u64 = 0xDC;
/// In the type word: the dead bit. A drone the player shot stayed listed with it.
const TYPE_DEAD: u32 = 0x4000;
/// One read per entity: from its world word to the end of its own handle.
const HEAD_AT: u64 = E_WORLD;
const HEAD_LEN: usize = (E_HANDLE + 8 - E_WORLD) as usize;
/// From a body to its marker: one of its components holds a gameplay container X at +0x28. X has
/// its index table at +8 (inside the exe), an off flag (0x10) in the byte at +0x18, its slots at
/// +0x20 and their count (u16) at +0x2A. The NPC marker is in slot table[0x67], a player's in
/// table[0x28] (the game's own lookup, 0x2975920 in the Vulkan exe).
const LINK_X: u64 = 0x28;
const X_TABLE: usize = 0x8;
const X_FLAGS: usize = 0x18;
const X_OFF: u8 = 0x10;
const X_SLOTS: usize = 0x20;
const X_COUNT: usize = 0x2A;
const X_LEN: usize = X_COUNT + 2;
const IDX_NPC: u64 = 0x67;
const IDX_PLAYER: u64 = 0x28;
/// More than this is taken as a wrong read (live: 18,100 entries, 13 to 85 components).
const MAX_ENTRIES: usize = 1 << 20;
const MAX_COMPS: usize = 256;
/// Entries checked again in a pass although their slot holds the same handle as last time: the last
/// ones (where the game adds and takes away) and a window that rolls over the rest. A freed handle
/// given to a new entity in the same slot is so caught (not seen live; guarded anyway).
const TAIL: usize = 64;
const ROLL: usize = 256;
/// A character without a marker is looked at again on the next 4 passes (a new body may get its
/// marker a moment after it is listed), then after 2, 4, 8 ... passes, at most 50 (10 s).
const FAST_TRIES: u32 = 4;
const RETRY_MAX: u64 = 50;
/// How often the world list's numbers go to the log.
const LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// The game's memory: address, buffer -> bytes read (all or nothing counts).
type Read<'a> = &'a dyn Fn(u64, &mut [u8]) -> usize;

fn ptr_like(v: u64) -> bool {
    v > 0x10000 && v < 0x7fff_ffff_0000
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn read_u64_with(read: Read, at: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    (read(at, &mut b) == 8).then(|| u64::from_le_bytes(b))
}

/// The entity a handle holds, if bit 31 of its word at +0xC says it is there.
fn handle_entity(b: &[u8; 16]) -> Option<u64> {
    let e = le64(b, 0);
    (le32(b, H_LIVE) & 0x8000_0000 != 0 && ptr_like(e)).then_some(e)
}

/// The entity handle `h` holds now.
fn entity_of(read: Read, h: u64) -> Option<u64> {
    let mut b = [0u8; 16];
    if read(h, &mut b) != 16 {
        return None;
    }
    handle_entity(&b)
}

/// A character's kind from its type word: `Some(true)` a player (& 7 == 1), `Some(false)` an NPC
/// (2), `None` anything else. Live: the player 0xD09, other players 0xE09, NPCs 0x1D12, 0x1D1A,
/// 0x912, and 0x502 (13 bodies that have no marker at all).
fn char_kind(typ: u32) -> Option<bool> {
    match typ & 7 {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// The pass at which a character still without a marker after `tries` looks is looked at again.
fn next_try(p: u64, tries: u32) -> u64 {
    p + if tries <= FAST_TRIES { 1 } else { (1u64 << (tries - FAST_TRIES).min(6)).min(RETRY_MAX) }
}

/// What a pass reads of an entity, in one read.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Head {
    world: u64,
    pos: [f64; 3],
    comps: u64,
    ncomp: u16,
    typ: u32,
    slot: u32,
    handle: u64,
    /// The entity's box (`E_BOX`).
    bmin: [f32; 3],
    bmax: [f32; 3],
}

impl Head {
    fn parse(b: &[u8; HEAD_LEN]) -> Head {
        let at = |o: u64| (o - HEAD_AT) as usize;
        let f = |o: u64| f64::from_le_bytes(b[at(o)..at(o) + 8].try_into().unwrap());
        let g = |o: u64| f32::from_le_bytes(b[at(o)..at(o) + 4].try_into().unwrap());
        Head {
            world: le64(b, at(E_WORLD)),
            pos: [f(POS), f(POS + 8), f(POS + 16)],
            comps: le64(b, at(E_COMPS)),
            ncomp: le16(b, at(E_NCOMP)),
            typ: le32(b, at(E_TYPE)),
            slot: le32(b, at(E_SLOT)),
            handle: le64(b, at(E_HANDLE)),
            bmin: [g(E_BOX), g(E_BOX + 4), g(E_BOX + 8)],
            bmax: [g(E_BOX + 12), g(E_BOX + 16), g(E_BOX + 20)],
        }
    }

    fn read(read: Read, e: u64) -> Option<Head> {
        let mut b = [0u8; HEAD_LEN];
        (read(e.checked_add(HEAD_AT)?, &mut b) == HEAD_LEN).then(|| Head::parse(&b))
    }

    /// At slot `i` of world `w`'s list under handle `h`, by its own words (live: 18,100 of 18,100).
    fn listed(&self, w: u64, i: usize, h: u64) -> bool {
        self.world == w && self.slot as usize == i && self.handle == h
    }

    fn pos_ok(&self) -> bool {
        self.pos.iter().all(|v| v.is_finite() && v.abs() < 1.0e5)
    }
}

/// What a pass needs to know: the player's body and position, the two marker classes, the exe's
/// range.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    me: u64,
    mine: [f64; 3],
    npc_tag: u64,
    player_tag: u64,
    image: (u64, u64),
}

/// The body's marker and whether it is a player's (see `LINK_X`). Every step is checked: X's index
/// table must be in the exe, the slot inside X's count, the marker of its class and pointing back
/// at the body. Components are tried from the last: the link sat at 41 to 49 of about 50 on NPCs
/// and 60 to 68 of about 70 on players.
fn find_marker(read: Read, body: u64, comps: u64, ncomp: u16, cx: &Ctx) -> Option<(u64, bool)> {
    let n = usize::from(ncomp).min(MAX_COMPS);
    if n == 0 || !ptr_like(comps) {
        return None;
    }
    let mut list = vec![0u8; n * 8];
    if read(comps, &mut list) != list.len() {
        return None;
    }
    let (words, _) = list.as_chunks::<8>();
    for c in words.iter().rev().map(|w| u64::from_le_bytes(*w)) {
        if !ptr_like(c) {
            continue;
        }
        let Some(x) = read_u64_with(read, c + LINK_X).filter(|x| ptr_like(*x)) else { continue };
        let mut xb = [0u8; X_LEN];
        if read(x, &mut xb) != X_LEN {
            continue;
        }
        let (table, slots, count) = (le64(&xb, X_TABLE), le64(&xb, X_SLOTS), le16(&xb, X_COUNT));
        if !(cx.image.0..cx.image.1).contains(&table) || !ptr_like(slots) || xb[X_FLAGS] & X_OFF != 0 {
            continue;
        }
        for (idx, tag, player) in [(IDX_NPC, cx.npc_tag, false), (IDX_PLAYER, cx.player_tag, true)] {
            let mut s = [0u8; 1];
            if read(table + idx, &mut s) != 1 || u16::from(s[0]) >= count {
                continue;
            }
            let Some(m) = read_u64_with(read, slots + u64::from(s[0]) * 8).filter(|m| ptr_like(*m)) else { continue };
            let mut mb = [0u8; 16];
            if read(m, &mut mb) == 16 && le64(&mb, 0) == tag && le64(&mb, 8) == body {
                return Some((m, player));
            }
        }
    }
    None
}

/// Drones: at most this many a pass, nearest first.
const DRONES_MAX: usize = 32;

/// A drone in the player's world: its entity, its handle and the world, so each frame can check it
/// is still that drone (`drone_at`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drone {
    pub ent: u64,
    pub handle: u64,
    pub world: u64,
}

/// What a frame needs of a drone: its middle, its size (middle to a corner of its diamond, world
/// units) and whether the game marks it dead (`TYPE_DEAD`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DroneNow {
    pub mid: [f64; 3],
    pub half: f64,
    pub dead: bool,
}

/// Drone `d` now, read again (each frame): None when its handle no longer holds it, it is no longer
/// in that world under that handle, it stopped being a drone, or its position is not a sane number.
pub fn drone_at(d: &Drone) -> Option<DroneNow> {
    drone_at_with(&safe_read, d)
}

fn drone_at_with(read: Read, d: &Drone) -> Option<DroneNow> {
    if entity_of(read, d.handle) != Some(d.ent) {
        return None;
    }
    let hd = Head::read(read, d.ent)?;
    (hd.world == d.world && hd.handle == d.handle && hd.pos_ok() && drone_type(hd.typ)).then(|| drone_now(&hd))
}

/// Drones: main type 5, and the kind bits (10-13) of the type word, where 1 is the world's own.
const DRONE_MAIN: u32 = 5;
const KIND_SHIFT: u32 = 10;
const KIND_WORLD: u32 = 1;
/// A drone's box is used when each side is 0.1 to 40 units and no corner is further out; the
/// diamond's size (middle to a corner) is then a sixth of its three sides, 0.4 to 8 units. Without
/// a box: 0.9.
const DRONE_BOX_MIN: f64 = 0.1;
const DRONE_BOX_MAX: f64 = 40.0;
const DRONE_HALF: f64 = 0.9;
const DRONE_HALF_MIN: f64 = 0.4;
const DRONE_HALF_MAX: f64 = 8.0;

/// Drones: main type 5 in the type word (`typ & 7`, the game's own "main 5" check). Seen: sub 2
/// (0x1115, the ground drone the player watched drive and then shot, 53 components) and sub 1
/// (0x110d, 25 components). Kind 1 is left out: a 3-component shadow of the ground drone (0x515)
/// sits at the same spot. The 3 parts on it (main 6, 0x1106, 1.3 up) are not drones. Players'
/// drones may show too.
fn drone_type(typ: u32) -> bool {
    typ & 7 == DRONE_MAIN && (typ >> KIND_SHIFT) & 0xF != KIND_WORLD
}

/// From the entity's box when it looks like one (live night: the ground drone's
/// (-1.61,-1.44,0.05)-(1.64,1.45,1.30) around its position, so its middle is 0.68 up), else its
/// position and `DRONE_HALF` (the small drone's read (0,0,0)-(0.6,0,0)).
fn drone_now(hd: &Head) -> DroneNow {
    let ext = [0, 1, 2].map(|k| f64::from(hd.bmax[k]) - f64::from(hd.bmin[k]));
    let fits = hd.bmin.iter().chain(&hd.bmax).all(|v| v.is_finite() && f64::from(v.abs()) <= DRONE_BOX_MAX)
        && ext.iter().all(|v| (DRONE_BOX_MIN..=DRONE_BOX_MAX).contains(v));
    let (up, half) = if fits {
        ((f64::from(hd.bmin[2]) + f64::from(hd.bmax[2])) / 2.0, (ext.iter().sum::<f64>() / 6.0).clamp(DRONE_HALF_MIN, DRONE_HALF_MAX))
    } else {
        (0.0, DRONE_HALF)
    };
    DroneNow { mid: [hd.pos[0], hd.pos[1], hd.pos[2] + up], half, dead: hd.typ & TYPE_DEAD != 0 }
}

/// What the world list knows about one handle: its entity, its kind (`char_kind`) and whether it is
/// a drone.
#[derive(Clone, Copy, Debug)]
struct Known {
    ent: u64,
    kind: Option<bool>,
    drone: bool,
}

/// A character in the list (the player's own body too, never shown). `marker` 0: none found yet,
/// looked for again at pass `retry`.
#[derive(Clone, Copy, Debug)]
struct Char {
    ent: u64,
    marker: u64,
    player: bool,
    retry: u64,
    tries: u32,
}

/// Handles are pointers: one multiply is hash enough and far cheaper than the default.
#[derive(Default)]
struct PtrHash(u64);

impl std::hash::Hasher for PtrHash {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u64(self.0 ^ u64::from(b));
        }
    }

    fn write_u64(&mut self, v: u64) {
        let x = (v ^ (v >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd);
        self.0 = x ^ (x >> 33);
    }
}

type ByHandle<V> = std::collections::HashMap<u64, V, std::hash::BuildHasherDefault<PtrHash>>;
type Handles = std::collections::HashSet<u64, std::hash::BuildHasherDefault<PtrHash>>;

/// Counts of the last pass.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldCounts {
    pub entries: usize,
    /// Characters in the list (the player's own body not counted).
    pub chars: usize,
    /// Of those, with a marker.
    pub linked: usize,
    /// Entries looked at for the first time.
    pub new: usize,
    /// Reads of game memory.
    pub reads: u64,
}

/// The numbers for the log line every 10 s.
#[derive(Default)]
struct WorldLog {
    passes: u32,
    total: std::time::Duration,
    max: std::time::Duration,
    new: usize,
    reads: u64,
    since: Option<std::time::Instant>,
}

/// The world list between passes. Kept by the ESP's scan thread. Entries are known by handle: a
/// slot that still holds the same handle is not read again (except the last ones and a rolling
/// window, see `TAIL`), so a pass costs a few reads per character, not per entry.
#[derive(Default)]
pub struct WorldList {
    world: u64,
    /// Last pass's list and, per slot, whether its entry was read fine (else it is read again).
    prev: Vec<u64>,
    prev_ok: Vec<bool>,
    cur: Vec<u64>,
    cur_ok: Vec<bool>,
    known: ByHandle<Known>,
    chars: ByHandle<Char>,
    /// Drones by handle -> entity, and the last pass's list of them.
    drones: ByHandle<u64>,
    drone_list: Vec<Drone>,
    pass: u64,
    roll: usize,
    buf: Vec<u8>,
    pub last: WorldCounts,
    log: WorldLog,
}

impl WorldList {
    /// One pass: every character in the player's world within `radius` of the player, nearest
    /// first, at most `max`, the player's own body left out. `owner` is the body's marker, 0 when
    /// none was found (the caller may take the marker scan's). Err: the list can't be used now (the
    /// player is in no world, the world or list looks wrong, it is empty or unreadable, it holds no
    /// character): the caller falls back.
    pub fn pass(&mut self, radius: f64, max: usize) -> Result<Vec<Body>, String> {
        let (me, mine, _) = player().ok_or("player body not found")?;
        let exe = memory::find_module("").ok_or("exe not found")?;
        let npc_tag = exe.base + globals::CHAR_COMPONENT.rva().ok_or("npc class not found yet")?;
        let player_tag = exe.base + globals::PLAYER_COMPONENT.rva().ok_or("player class not found yet")?;
        let cx = Ctx { me, mine, npc_tag, player_tag, image: (exe.base, exe.base + exe.size) };
        let t = std::time::Instant::now();
        let out = self.pass_with(&safe_read, &cx, radius, max);
        let took = t.elapsed();
        let l = &mut self.log;
        l.since.get_or_insert_with(std::time::Instant::now);
        l.passes += 1;
        l.total += took;
        l.max = l.max.max(took);
        l.new += self.last.new;
        l.reads += self.last.reads;
        out
    }

    /// The drones of the last pass in the player's world within its radius, nearest first, at most
    /// `DRONES_MAX`. Empty after a pass that failed early.
    pub fn drones(&self) -> &[Drone] {
        &self.drone_list
    }

    /// The log line, every 10 s (None in between).
    pub fn report(&mut self) -> Option<String> {
        let l = &mut self.log;
        if l.passes == 0 || l.since.is_none_or(|t| t.elapsed() < LOG_EVERY) {
            return None;
        }
        let c = self.last;
        let line = format!(
            "world list: {} entries, {} characters ({} with a marker), {} new in {} passes; a pass {} us on average, {} us at most, {} reads",
            c.entries,
            c.chars,
            c.linked,
            l.new,
            l.passes,
            l.total.as_micros() / u128::from(l.passes),
            l.max.as_micros(),
            l.reads / u64::from(l.passes)
        );
        *l = WorldLog::default();
        Some(line)
    }

    fn pass_with(&mut self, read: Read, cx: &Ctx, radius: f64, max: usize) -> Result<Vec<Body>, String> {
        let reads = std::cell::Cell::new(0u64);
        let counted = |a: u64, b: &mut [u8]| {
            reads.set(reads.get() + 1);
            read(a, b)
        };
        let read: Read = &counted;
        self.last = WorldCounts::default();
        self.drone_list.clear();
        let out = self.walk(read, cx, radius, max);
        self.last.reads = reads.get();
        out
    }

    fn walk(&mut self, read: Read, cx: &Ctx, radius: f64, max: usize) -> Result<Vec<Body>, String> {
        self.pass += 1;
        let p = self.pass;
        // The player's world, read again every pass.
        let w = read_u64_with(read, cx.me + E_WORLD).filter(|v| ptr_like(*v)).ok_or("the player's body is in no world")?;
        if read_u64_with(read, w + W_KEEPER) != Some(w) {
            return Err("the player's world does not look like one".into());
        }
        if w != self.world {
            self.world = w;
            self.prev.clear();
            self.prev_ok.clear();
            self.known.clear();
            self.chars.clear();
            self.drones.clear();
            self.roll = 0;
        }
        // Count and array, fresh: the array moves when it grows.
        let mut hdr = [0u8; 16];
        if read(w + W_LIST, &mut hdr) != 16 {
            return Err("world list unreadable".into());
        }
        let (arr, count, room) = (le64(&hdr, 0), le32(&hdr, 8) as usize, (le32(&hdr, 12) & 0x1FFF_FFFF) as usize);
        if count == 0 {
            return Err("world list empty".into());
        }
        if count > MAX_ENTRIES || count > room || !ptr_like(arr) {
            return Err(format!("world list looks wrong ({count} entries, room for {room})"));
        }
        self.buf.resize(count * 8, 0);
        if read(arr, &mut self.buf) != count * 8 {
            return Err("world list unreadable".into());
        }
        self.cur.clear();
        self.cur.extend(self.buf.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)));
        self.last.entries = count;
        self.forget_gone();
        let tail = count.saturating_sub(TAIL);
        let roll = self.roll % count;
        let rolling = |i: usize| (i + count - roll) % count < ROLL;
        self.cur_ok.clear();
        self.cur_ok.resize(count, false);
        for (i, &h) in self.cur.iter().enumerate() {
            if !ptr_like(h) {
                continue;
            }
            // Same handle in the same slot, read fine last time: the same entity, not read again.
            // Swap-remove moves entries around, so anything else is checked by its own words.
            let same = self.prev.get(i) == Some(&h) && self.prev_ok.get(i) == Some(&true);
            if same && i < tail && !rolling(i) {
                self.cur_ok[i] = true;
                continue;
            }
            if let Some(k) = self.known.get(&h) {
                if still(read, h, k, w) {
                    self.cur_ok[i] = true;
                    continue;
                }
                self.known.remove(&h);
                self.chars.remove(&h);
                self.drones.remove(&h);
            }
            if let Some((k, c)) = look(read, h, i, w, cx, p) {
                if k.drone {
                    self.drones.insert(h, k.ent);
                }
                self.known.insert(h, k);
                if let Some(c) = c {
                    self.chars.insert(h, c);
                }
                self.cur_ok[i] = true;
                self.last.new += 1;
            }
        }
        self.roll = (roll + ROLL) % count;
        std::mem::swap(&mut self.prev, &mut self.cur);
        std::mem::swap(&mut self.prev_ok, &mut self.cur_ok);
        // The characters, each read again: its handle must still hold it, and it must be in the
        // player's world at its own slot of this pass's list (a body the game took away is gone at
        // once).
        let mut out = Vec::new();
        let mut lost = Vec::new();
        for (&h, c) in self.chars.iter_mut() {
            if c.ent == cx.me {
                continue;
            }
            if entity_of(read, h) != Some(c.ent) {
                lost.push(h);
                continue;
            }
            let Some(hd) = Head::read(read, c.ent) else { continue };
            if hd.world != w || hd.handle != h || self.prev.get(hd.slot as usize) != Some(&h) || !hd.pos_ok() {
                continue;
            }
            self.last.chars += 1;
            if c.marker != 0 {
                // Still its marker, of its class and pointing back?
                let tag = if c.player { cx.player_tag } else { cx.npc_tag };
                let mut mb = [0u8; 16];
                if read(c.marker, &mut mb) != 16 || le64(&mb, 0) != tag || le64(&mb, 8) != c.ent {
                    (c.marker, c.retry, c.tries) = (0, p, 0);
                }
            }
            if c.marker == 0 && p >= c.retry {
                match find_marker(read, c.ent, hd.comps, hd.ncomp, cx) {
                    Some((m, player)) => (c.marker, c.player, c.tries) = (m, player, 0),
                    None => {
                        c.tries += 1;
                        c.retry = next_try(p, c.tries);
                    }
                }
            }
            if c.marker != 0 {
                self.last.linked += 1;
            }
            let d = ((hd.pos[0] - cx.mine[0]).powi(2) + (hd.pos[1] - cx.mine[1]).powi(2) + (hd.pos[2] - cx.mine[2]).powi(2)).sqrt();
            if d <= radius {
                out.push(Body { body: c.ent, pos: hd.pos, dist: d, owner: c.marker, player: c.player });
            }
        }
        // Drones, each read again the same way, nearest first.
        let mut drones = Vec::new();
        for (&h, &e) in &self.drones {
            if entity_of(read, h) != Some(e) {
                lost.push(h);
                continue;
            }
            let Some(hd) = Head::read(read, e) else { continue };
            if hd.world != w || hd.handle != h || self.prev.get(hd.slot as usize) != Some(&h) || !hd.pos_ok() || !drone_type(hd.typ) {
                continue;
            }
            let d = ((hd.pos[0] - cx.mine[0]).powi(2) + (hd.pos[1] - cx.mine[1]).powi(2) + (hd.pos[2] - cx.mine[2]).powi(2)).sqrt();
            if d <= radius {
                drones.push((hd.typ & TYPE_DEAD != 0, d, Drone { ent: e, handle: h, world: w }));
            }
        }
        // Live ones first: shot drones stay listed, and many of them nearby must not push a live
        // one out of the nearest `DRONES_MAX`.
        drones.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)));
        drones.truncate(DRONES_MAX);
        self.drone_list = drones.into_iter().map(|(_, _, d)| d).collect();
        // A handle given to another entity (not seen live): forgotten, and its slot read again.
        for h in lost {
            self.known.remove(&h);
            self.chars.remove(&h);
            self.drones.remove(&h);
            if let Some(i) = self.prev.iter().position(|&x| x == h) {
                self.prev_ok[i] = false;
            }
        }
        if self.last.chars == 0 {
            return Err("no characters in the world list".into());
        }
        out.sort_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal));
        out.truncate(max);
        Ok(out)
    }

    /// Handles that left the list since the last pass, forgotten. Only changed slots are looked at:
    /// a handle in a slot that changed (or fell off the end) is gone unless it is in a changed slot
    /// now (swap-remove moves the last entry into the freed slot).
    fn forget_gone(&mut self) {
        if self.prev.is_empty() {
            return;
        }
        let changed = |i: usize| self.prev.get(i) != self.cur.get(i);
        let came: Handles = (0..self.cur.len()).filter(|&i| changed(i)).map(|i| self.cur[i]).collect();
        for i in (0..self.prev.len()).filter(|&i| changed(i)) {
            let old = self.prev[i];
            if !came.contains(&old) {
                self.known.remove(&old);
                self.chars.remove(&old);
                self.drones.remove(&old);
            }
        }
    }
}

/// A known entry is still what it was: the handle holds the same entity, and the entity is still of
/// the same kind, in world `w` and has this handle as its own.
fn still(read: Read, h: u64, k: &Known, w: u64) -> bool {
    entity_of(read, h) == Some(k.ent)
        && Head::read(read, k.ent).is_some_and(|hd| char_kind(hd.typ) == k.kind && drone_type(hd.typ) == k.drone && hd.world == w && hd.handle == h)
}

/// A handle seen for the first time (or again after a change): its entity, checked by its own slot,
/// handle and world, then sorted. None: not kept, read again next pass.
fn look(read: Read, h: u64, i: usize, w: u64, cx: &Ctx, p: u64) -> Option<(Known, Option<Char>)> {
    let e = entity_of(read, h)?;
    let hd = Head::read(read, e)?;
    if !hd.listed(w, i, h) {
        return None;
    }
    let kind = char_kind(hd.typ);
    let c = kind.map(|player| {
        if e == cx.me {
            // The player's own body is never shown: no marker needed (unless it stops being the player's).
            return Char { ent: e, marker: 0, player, retry: p + 1, tries: 0 };
        }
        match find_marker(read, e, hd.comps, hd.ncomp, cx) {
            Some((marker, player)) => Char { ent: e, marker, player, retry: 0, tries: 0 },
            None => Char { ent: e, marker: 0, player, retry: next_try(p, 1), tries: 1 },
        }
    });
    let drone = drone_type(hd.typ);
    Some((Known { ent: e, kind, drone }, c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// Made-up game memory: one block of our own, so the addresses are real ones (the cost test
    /// reads it through `safe_read`, the way the mod reads the game).
    struct Mem {
        bytes: Vec<u8>,
        start: usize,
        top: u64,
    }

    impl Mem {
        fn new(size: usize) -> Mem {
            let bytes = vec![0u8; size + 64];
            let start = (64 - bytes.as_ptr() as usize % 64) % 64;
            Mem { bytes, start, top: 64 }
        }

        fn base(&self) -> u64 {
            self.bytes.as_ptr() as u64 + self.start as u64
        }

        fn end(&self) -> u64 {
            self.base() + (self.bytes.len() - self.start) as u64
        }

        /// `len` zero bytes, 16-aligned.
        fn alloc(&mut self, len: usize) -> u64 {
            let at = self.base() + self.top;
            self.top += (len as u64 + 15) & !15;
            assert!(self.base() + self.top <= self.end(), "test memory full");
            at
        }

        fn put(&mut self, a: u64, v: &[u8]) {
            let o = (a - self.base()) as usize + self.start;
            self.bytes[o..o + v.len()].copy_from_slice(v);
        }

        fn u64(&mut self, a: u64, v: u64) {
            self.put(a, &v.to_le_bytes());
        }

        fn u32(&mut self, a: u64, v: u32) {
            self.put(a, &v.to_le_bytes());
        }

        fn u16(&mut self, a: u64, v: u16) {
            self.put(a, &v.to_le_bytes());
        }

        fn u8(&mut self, a: u64, v: u8) {
            self.put(a, &[v]);
        }

        /// All or nothing, like the kernel copy: 0 outside the block.
        fn read(&self, a: u64, b: &mut [u8]) -> usize {
            if a < self.base() || a.checked_add(b.len() as u64).is_none_or(|e| e > self.end()) {
                return 0;
            }
            let o = (a - self.base()) as usize + self.start;
            b.copy_from_slice(&self.bytes[o..o + b.len()]);
            b.len()
        }
    }

    const NPC_TAG: u64 = 0x7FF6_1111_0000;
    const PLAYER_TAG: u64 = 0x7FF6_2222_0000;
    /// Where the index table sends the two markers in a container's slots.
    const NPC_SLOT: u8 = 3;
    const PLAYER_SLOT: u8 = 5;

    /// A made-up world as the game keeps it.
    struct Game {
        m: Mem,
        w: u64,
        arr: u64,
        count: usize,
        table: u64,
        me: u64,
    }

    /// An entity: its handle, itself, and for a linked character its marker and the slots that hold
    /// it.
    #[derive(Clone, Copy, Debug)]
    struct Ent {
        h: u64,
        e: u64,
        marker: u64,
        slots: u64,
    }

    impl Game {
        fn new(size: usize, room: usize) -> Game {
            let mut m = Mem::new(size);
            let table = m.alloc(0x100);
            m.u8(table + IDX_NPC, NPC_SLOT);
            m.u8(table + IDX_PLAYER, PLAYER_SLOT);
            let mut g = Game { m, w: 0, arr: 0, count: 0, table, me: 0 };
            g.world(room);
            g
        }

        /// A new world, now the current one (`add` puts entities there).
        fn world(&mut self, room: usize) {
            let w = self.m.alloc(0x400);
            let arr = self.m.alloc(room * 8);
            self.m.u64(w + W_KEEPER, w);
            self.m.u64(w + W_LIST, arr);
            // The room with a flag in its top 3 bits.
            self.m.u32(w + W_LIST + 12, room as u32 | 0x2000_0000);
            (self.w, self.arr) = (w, arr);
            self.set_count(0);
        }

        fn set_count(&mut self, n: usize) {
            self.count = n;
            self.m.u32(self.w + W_LIST + 8, n as u32);
        }

        fn cx(&self) -> Ctx {
            Ctx { me: self.me, mine: [0.0; 3], npc_tag: NPC_TAG, player_tag: PLAYER_TAG, image: (self.table, self.table + 0x100) }
        }

        fn read(&self) -> impl Fn(u64, &mut [u8]) -> usize + '_ {
            move |a: u64, b: &mut [u8]| self.m.read(a, b)
        }

        fn container(&mut self, table: u64, count: u16, flags: u8) -> (u64, u64) {
            let c = self.m.alloc(0x30);
            let x = self.m.alloc(X_LEN);
            let slots = self.m.alloc(8 * 8);
            self.m.u64(c + LINK_X, x);
            self.m.u64(x + X_TABLE as u64, table);
            self.m.u8(x + X_FLAGS as u64, flags);
            self.m.u64(x + X_SLOTS as u64, slots);
            self.m.u16(x + X_COUNT as u64, count);
            (c, slots)
        }

        fn marker(&mut self, tag: u64, body: u64) -> u64 {
            let m = self.m.alloc(0x70);
            self.m.u64(m, tag);
            self.m.u64(m + 8, body);
            m
        }

        fn comps(&mut self, e: u64, list: &[u64]) {
            let at = self.m.alloc(list.len() * 8);
            for (k, c) in list.iter().enumerate() {
                self.m.u64(at + 8 * k as u64, *c);
            }
            self.m.u64(e + E_COMPS, at);
            self.m.u16(e + E_NCOMP, list.len() as u16);
        }

        /// An entity behind handle `h`, in the current world, not put in the list. `link`: with a
        /// marker of that kind (true: a player's), the link 4th from the last of `n` components.
        fn ent(&mut self, h: u64, typ: u32, pos: [f64; 3], link: Option<bool>, n: usize) -> Ent {
            let e = self.m.alloc(0x170);
            self.m.u64(h, e);
            self.m.u32(h + H_LIVE as u64, 0x8000_0001);
            self.m.u64(e + E_WORLD, self.w);
            for (k, v) in pos.iter().enumerate() {
                self.m.put(e + POS + 8 * k as u64, &v.to_le_bytes());
            }
            self.m.u32(e + E_TYPE, typ);
            self.m.u64(e + E_HANDLE, h);
            let mut list: Vec<u64> = (0..n).map(|_| self.m.alloc(0x30)).collect();
            let (mut marker, mut slots) = (0, 0);
            if let Some(player) = link {
                let (c, s) = self.container(self.table, 8, 0);
                marker = self.marker(if player { PLAYER_TAG } else { NPC_TAG }, e);
                self.m.u64(s + 8 * u64::from(if player { PLAYER_SLOT } else { NPC_SLOT }), marker);
                list.insert(n.saturating_sub(3), c);
                slots = s;
            }
            self.comps(e, &list);
            Ent { h, e, marker, slots }
        }

        fn add_n(&mut self, typ: u32, pos: [f64; 3], link: Option<bool>, n: usize) -> Ent {
            let h = self.m.alloc(16);
            let x = self.ent(h, typ, pos, link, n);
            self.join(x);
            x
        }

        fn add(&mut self, typ: u32, pos: [f64; 3], link: Option<bool>) -> Ent {
            self.add_n(typ, pos, link, 12)
        }

        /// Put `x` at the end of the current world's list.
        fn join(&mut self, x: Ent) {
            self.m.u64(x.e + E_WORLD, self.w);
            let i = self.count;
            self.place(i, x);
            self.set_count(i + 1);
        }

        fn place(&mut self, i: usize, x: Ent) {
            self.m.u64(self.arr + 8 * i as u64, x.h);
            self.m.u32(x.e + E_SLOT, i as u32);
        }

        fn at(&self, i: usize) -> Ent {
            let r = self.read();
            let h = read_u64_with(&r, self.arr + 8 * i as u64).unwrap();
            Ent { h, e: read_u64_with(&r, h).unwrap(), marker: 0, slots: 0 }
        }

        /// Swap-remove, the way the game takes an entity out: the last one moves into slot `i`.
        fn remove(&mut self, i: usize) {
            let last = self.count - 1;
            let moved = self.at(last);
            self.place(i, moved);
            self.m.u64(self.arr + 8 * last as u64, 0);
            self.set_count(last);
        }

        fn pass(&self, wl: &mut WorldList, radius: f64, max: usize) -> Result<Vec<Body>, String> {
            wl.pass_with(&self.read(), &self.cx(), radius, max)
        }

        fn find(&self, e: u64) -> Option<(u64, bool)> {
            let r = self.read();
            let hd = Head::read(&r, e)?;
            find_marker(&r, e, hd.comps, hd.ncomp, &self.cx())
        }
    }

    fn bodies(l: &[Body]) -> Vec<u64> {
        l.iter().map(|b| b.body).collect()
    }

    /// The player's body at 0,0,0; an NPC at 5 m, a character without a marker (like the 0x502
    /// ones) at 6 m, a player at 20 m, an NPC at 50 m, and props.
    struct Small {
        g: Game,
        me: Ent,
        near: Ent,
        bare: Ent,
        pl: Ent,
        far: Ent,
    }

    fn small() -> Small {
        let mut g = Game::new(1 << 20, 64);
        let me = g.add(0xD09, [0.0; 3], Some(true));
        g.me = me.e;
        g.add(0, [1.0, 1.0, 0.0], None);
        let near = g.add(0x1D12, [5.0, 0.0, 0.0], Some(false));
        g.add(4, [2.0, 0.0, 0.0], None);
        let pl = g.add(0xE09, [0.0, 20.0, 0.0], Some(true));
        let far = g.add(0x912, [0.0, 0.0, 50.0], Some(false));
        let bare = g.add(0x502, [6.0, 0.0, 0.0], None);
        Small { g, me, near, bare, pl, far }
    }

    #[test]
    fn handle_and_type_words() {
        let mut b = [0u8; 16];
        b[..8].copy_from_slice(&0x1FF5_4A31_1400u64.to_le_bytes());
        assert_eq!(handle_entity(&b), None, "bit 31 clear: not there");
        b[12..].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        assert_eq!(handle_entity(&b), Some(0x1FF5_4A31_1400));
        b[..8].copy_from_slice(&0x40u64.to_le_bytes());
        assert_eq!(handle_entity(&b), None, "no entity");
        // The words seen live.
        for (t, want) in [(0xD09, Some(true)), (0xE09, Some(true)), (0x511, Some(true)), (0x1D12, Some(false)), (0x1D1A, Some(false)), (0x912, Some(false)), (0x502, Some(false)), (0, None), (3, None), (4, None)] {
            assert_eq!(char_kind(t), want, "{t:#x}");
        }
    }

    #[test]
    fn head_is_one_read_with_its_own_words() {
        let mut g = Game::new(1 << 16, 8);
        let a = g.add(0x1D12, [1.0, 2.0, 3.0], None);
        let r = g.read();
        let hd = Head::read(&r, a.e).unwrap();
        assert_eq!(hd.pos, [1.0, 2.0, 3.0]);
        assert_eq!((hd.world, hd.typ, hd.slot, hd.handle, hd.ncomp), (g.w, 0x1D12, 0, a.h, 12));
        assert!(hd.listed(g.w, 0, a.h));
        assert!(!hd.listed(g.w, 1, a.h), "another slot");
        assert!(!hd.listed(g.w + 8, 0, a.h), "another world");
        assert!(!hd.listed(g.w, 0, a.h + 16), "another handle");
        assert!(Head::read(&r, 0x7FF0_0000_0000).is_none(), "unreadable");
    }

    #[test]
    fn retries_come_fast_then_back_off() {
        let after: Vec<u64> = (1..=12).map(|t| next_try(100, t) - 100).collect();
        assert_eq!(after, vec![1, 1, 1, 1, 2, 4, 8, 16, 32, 50, 50, 50]);
    }

    #[test]
    fn marker_rule_links_npcs_and_players() {
        let mut g = Game::new(1 << 18, 8);
        let npc = g.add(0x1D12, [5.0, 0.0, 0.0], Some(false));
        let pl = g.add(0xE09, [6.0, 0.0, 0.0], Some(true));
        let none = g.add(0x502, [7.0, 0.0, 0.0], None);
        assert_eq!(g.find(npc.e), Some((npc.marker, false)));
        assert_eq!(g.find(pl.e), Some((pl.marker, true)));
        assert_eq!(g.find(none.e), None);
    }

    #[test]
    fn marker_rule_checks_every_step() {
        let mut g = Game::new(1 << 18, 8);
        let a = g.add(0x1D12, [5.0, 0.0, 0.0], None);
        let other = g.add(0x1D12, [9.0, 0.0, 0.0], None);
        let (t, at) = (g.table, 8 * u64::from(NPC_SLOT));
        // Each decoy breaks one step; every one holds a marker that would fit otherwise.
        let outside = g.m.alloc(0x100);
        g.m.u8(outside + IDX_NPC, NPC_SLOT);
        let mut decoys = vec![0x1234, 0xFFFF_FFFF_FFFF_0000];
        for (table, count, flags, tag, body) in [
            (outside, 8, 0, NPC_TAG, a.e),           // Index table not in the exe
            (t, 8, X_OFF, NPC_TAG, a.e),             // Container switched off
            (t, u16::from(NPC_SLOT), 0, NPC_TAG, a.e), // Slot not inside its count
            (t, 8, 0, 0x7FF6_3333_0000, a.e),        // Marker of another class
            (t, 8, 0, NPC_TAG, other.e),             // Marker of another body
        ] {
            let (c, s) = g.container(table, count, flags);
            let m = g.marker(tag, body);
            g.m.u64(s + at, m);
            decoys.push(c);
        }
        // A player's slot holding an NPC marker.
        let (c, s) = g.container(t, 8, 0);
        let m = g.marker(NPC_TAG, a.e);
        g.m.u64(s + 8 * u64::from(PLAYER_SLOT), m);
        decoys.push(c);
        // A container we can't read.
        let c = g.m.alloc(0x30);
        g.m.u64(c + LINK_X, 0x7FF0_0000_0000);
        decoys.push(c);
        g.comps(a.e, &decoys);
        assert_eq!(g.find(a.e), None);
        // The right one first in the list (so tried last): found past every decoy.
        let (good, s) = g.container(t, 8, 0);
        let m = g.marker(NPC_TAG, a.e);
        g.m.u64(s + at, m);
        decoys.insert(0, good);
        g.comps(a.e, &decoys);
        assert_eq!(g.find(a.e), Some((m, false)));
        // No list, an unreadable one, an empty one.
        let (r, cx) = (g.read(), g.cx());
        assert_eq!(find_marker(&r, a.e, 0, 5, &cx), None);
        assert_eq!(find_marker(&r, a.e, 0x7FF0_0000_0000, 5, &cx), None);
        let hd = Head::read(&r, a.e).unwrap();
        assert_eq!(find_marker(&r, a.e, hd.comps, 0, &cx), None);
    }

    /// Drones: listed live ones first, each nearest first; the shadow, the parts and drones past
    /// the radius are not; a shot one stays listed, marked dead; one the game takes away is gone,
    /// at once for a frame.
    #[test]
    fn pass_lists_drones_nearest_first_without_shadow_or_parts() {
        let mut s = small();
        let ground = s.g.add(0x1115, [30.0, 0.0, 0.0], None);
        s.g.add(0x515, [30.0, 0.0, 0.0], None);
        for k in 0..3 {
            s.g.add(0x1106, [30.0, 0.2 * f64::from(k), 1.3], None);
        }
        let little = s.g.add(0x110d, [0.0, 10.0, 2.0], None);
        let shot = s.g.add(0x5115, [0.0, -12.0, 0.0], None);
        s.g.add(0x110d, [2000.0, 0.0, 0.0], None);
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        let ents = |wl: &WorldList| wl.drones().iter().map(|d| d.ent).collect::<Vec<_>>();
        assert_eq!(ents(&wl), vec![little.e, ground.e, shot.e]);
        assert!(wl.drones().iter().all(|d| d.world == s.g.w));
        let now = |wl: &WorldList, x: &Ent| {
            let r = s.g.read();
            wl.drones().iter().find(|d| d.ent == x.e).and_then(|d| drone_at_with(&r, d))
        };
        assert!(!now(&wl, &ground).unwrap().dead);
        assert!(now(&wl, &shot).unwrap().dead);
        // The same pass again: the same list.
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(ents(&wl), vec![little.e, ground.e, shot.e]);
        // The game frees the ground drone: its handle lets it go and it leaves the list.
        let d = *wl.drones().iter().find(|d| d.ent == ground.e).unwrap();
        s.g.m.u32(ground.h + H_LIVE as u64, 0);
        assert_eq!(drone_at_with(&s.g.read(), &d), None);
        s.g.remove(7);
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(ents(&wl), vec![little.e, shot.e]);
        // Its slot now holds another drone (the far one, moved there): still not listed.
        s.g.pass(&mut wl, 3000.0, 120).unwrap();
        assert_eq!(wl.drones().len(), 3);
        assert!(!ents(&wl).contains(&ground.e));
    }

    /// Shot drones stay listed: `DRONES_MAX` of them nearer than a live one must not push it out of
    /// the list.
    #[test]
    fn dead_drones_do_not_crowd_out_a_live_one() {
        let mut s = small();
        let mut x = 10.0;
        for _ in 0..DRONES_MAX {
            s.g.add(0x5115, [x, 0.0, 0.0], None);
            x += 1.0;
        }
        let live = s.g.add(0x110d, [0.0, 90.0, 0.0], None);
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(wl.drones().len(), DRONES_MAX);
        assert_eq!(wl.drones()[0].ent, live.e, "the live one first");
    }

    /// A handle first read while its entity was something else (type 0 at first, or a freed prop's
    /// memory reused for a drone) is listed once it is a drone, and one that stops being a drone
    /// leaves the list.
    #[test]
    fn a_handle_that_becomes_a_drone_is_listed() {
        let mut s = small();
        let x = s.g.add(0, [0.0, 15.0, 0.0], None);
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert!(wl.drones().is_empty());
        s.g.m.u32(x.e + E_TYPE, 0x1115);
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(wl.drones().iter().map(|d| d.ent).collect::<Vec<_>>(), vec![x.e]);
        s.g.m.u32(x.e + E_TYPE, 0x515);
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert!(wl.drones().is_empty());
    }

    /// A drone's diamond from its own box: the ground drone's box (live) puts its middle 0.68 up
    /// and its size at a sixth of its sides; the small drone's (0, 0, 0)-(0.6, 0, 0) and boxes that
    /// look wrong are not used.
    #[test]
    fn drone_middle_and_size_from_its_box() {
        fn put(b: &mut [u8; HEAD_LEN], o: u64, v: &[u8]) {
            let at = (o - HEAD_AT) as usize;
            b[at..at + v.len()].copy_from_slice(v);
        }
        fn boxed(b: &mut [u8; HEAD_LEN], typ: u32, v: [f32; 6]) -> DroneNow {
            put(b, E_TYPE, &typ.to_le_bytes());
            for (k, x) in v.iter().enumerate() {
                put(b, E_BOX + 4 * k as u64, &x.to_le_bytes());
            }
            drone_now(&Head::parse(b))
        }
        let mut b = [0u8; HEAD_LEN];
        for (k, v) in [10.0f64, 20.0, 30.0].iter().enumerate() {
            put(&mut b, POS + 8 * k as u64, &v.to_le_bytes());
        }
        let n = boxed(&mut b, 0x1115, [-1.61, -1.44, 0.05, 1.64, 1.45, 1.30]);
        assert_eq!((n.mid[0], n.mid[1]), (10.0, 20.0));
        assert!((n.mid[2] - 30.675).abs() < 1e-3, "{:?}", n.mid);
        assert!((n.half - (3.25 + 2.89 + 1.25) / 6.0).abs() < 1e-3, "{}", n.half);
        assert!(!n.dead);
        let plain = DroneNow { mid: [10.0, 20.0, 30.0], half: DRONE_HALF, dead: true };
        assert_eq!(boxed(&mut b, 0x510d, [0.0, 0.0, 0.0, 0.6, 0.0, 0.0]), plain);
        assert_eq!(boxed(&mut b, 0x510d, [f32::NAN, 0.0, 0.0, 1.0, 1.0, 1.0]), plain);
        assert_eq!(boxed(&mut b, 0x510d, [-500.0, -500.0, 0.0, 500.0, 500.0, 2.0]), plain);
        // A tiny box: the smallest diamond.
        assert_eq!(boxed(&mut b, 0x1115, [-0.15, -0.15, 0.0, 0.15, 0.15, 0.3]).half, DRONE_HALF_MIN);
    }

    /// Main type 5 is a drone in every kind but the world's own (1), dead or not; characters, their
    /// parts (6), vehicles (4) and players' 0xD3C things are not.
    #[test]
    fn drone_type_is_main_five_not_the_worlds_own() {
        for t in [0x1115, 0x110d, 0x5115, 0x510d, 0xD15] {
            assert!(drone_type(t), "{t:#x}");
        }
        for t in [0x515, 0x1106, 0x5106, 0x5117, 0x1D12, 0x121A, 0xD09, 0xE09, 0x50C, 0x51C, 0xD3C, 0x100, 0] {
            assert!(!drone_type(t), "{t:#x}");
        }
    }

    #[test]
    fn pass_gives_characters_nearest_first_and_never_her() {
        let s = small();
        let mut wl = WorldList::default();
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(bodies(&l), vec![s.near.e, s.bare.e, s.pl.e, s.far.e]);
        assert!(!bodies(&l).contains(&s.me.e));
        assert_eq!((l[0].owner, l[0].player, l[0].dist, l[0].pos), (s.near.marker, false, 5.0, [5.0, 0.0, 0.0]));
        assert_eq!(l[1].owner, 0, "no marker: the caller decides");
        assert_eq!((l[2].owner, l[2].player), (s.pl.marker, true));
        assert_eq!(wl.last, WorldCounts { entries: 7, chars: 4, linked: 3, new: 7, reads: wl.last.reads });
        // Radius and max.
        assert_eq!(bodies(&s.g.pass(&mut wl, 21.0, 120).unwrap()), vec![s.near.e, s.bare.e, s.pl.e]);
        assert_eq!(bodies(&s.g.pass(&mut wl, 1000.0, 2).unwrap()), vec![s.near.e, s.bare.e]);
        assert_eq!(wl.last.new, 0, "nothing looked at from scratch again");
    }

    #[test]
    fn a_body_that_moves_is_where_it_is_now() {
        let mut s = small();
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        s.g.m.put(s.far.e + POS + 16, &1.0f64.to_le_bytes());
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!((l[0].body, l[0].dist), (s.far.e, 1.0));
    }

    #[test]
    fn a_new_body_shows_on_the_next_pass() {
        let mut s = small();
        let mut wl = WorldList::default();
        let before = s.g.pass(&mut wl, 1000.0, 120).unwrap().len();
        let new = s.g.add(0x1D1A, [0.0, 2.0, 0.0], Some(false));
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(l.len(), before + 1);
        assert_eq!((l[0].body, l[0].owner), (new.e, new.marker));
        assert_eq!(wl.last.new, 1);
    }

    #[test]
    fn swap_remove_drops_the_body_at_once_and_keeps_the_moved_one() {
        let mut s = small();
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        // `near` is in slot 2; the last entry (`bare`) moves there. Its memory stays as it was.
        s.g.remove(2);
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(bodies(&l), vec![s.bare.e, s.pl.e, s.far.e]);
        assert_eq!(wl.last.new, 0, "the moved one is known by its handle");
        assert!(!wl.known.contains_key(&s.near.h) && !wl.chars.contains_key(&s.near.h));
        assert_eq!(bodies(&s.g.pass(&mut wl, 1000.0, 120).unwrap()), vec![s.bare.e, s.pl.e, s.far.e]);
        // The last one taken away.
        s.g.remove(s.g.count - 1);
        assert_eq!(bodies(&s.g.pass(&mut wl, 1000.0, 120).unwrap()), vec![s.bare.e, s.pl.e]);
    }

    /// A long list: slot 1 is neither in the tail nor in the next rolling window.
    fn long() -> (Game, Ent, Ent, Ent) {
        let mut g = Game::new(4 << 20, 2048);
        let me = g.add(0xD09, [0.0; 3], Some(true));
        g.me = me.e;
        let first = g.add(0x1D12, [3.0, 0.0, 0.0], Some(false));
        for k in 0..1000 {
            g.add(0, [k as f64, 1.0, 0.0], None);
        }
        let last = g.add(0x1D12, [9.0, 0.0, 0.0], Some(false));
        (g, me, first, last)
    }

    #[test]
    fn an_entry_that_was_not_ready_is_read_again_next_pass() {
        let mut g = Game::new(4 << 20, 2048);
        let me = g.add(0xD09, [0.0; 3], Some(true));
        g.me = me.e;
        let half = g.add(0x1D12, [3.0, 0.0, 0.0], Some(false));
        // Not set up yet: it says another slot.
        g.m.u32(half.e + E_SLOT, 999);
        for k in 0..1000 {
            g.add(0, [k as f64, 1.0, 0.0], None);
        }
        let other = g.add(0x1D12, [8.0, 0.0, 0.0], Some(false));
        let mut wl = WorldList::default();
        assert_eq!(bodies(&g.pass(&mut wl, 1000.0, 120).unwrap()), vec![other.e]);
        g.m.u32(half.e + E_SLOT, 1);
        assert_eq!(bodies(&g.pass(&mut wl, 1000.0, 120).unwrap()), vec![half.e, other.e]);
    }

    #[test]
    fn a_handle_given_to_a_new_entity_is_seen() {
        let (mut g, _, first, last) = long();
        let mut wl = WorldList::default();
        g.pass(&mut wl, 1000.0, 120).unwrap();
        // Both handles go to new NPCs in the same slots; the old ones keep their memory.
        let i = g.count - 1;
        let new_last = g.ent(last.h, 0x1D12, [8.0, 0.0, 0.0], Some(false), 12);
        g.place(i, new_last);
        let new_first = g.ent(first.h, 0x1D12, [2.0, 0.0, 0.0], Some(false), 12);
        g.place(1, new_first);
        // The old ones are gone at once; the tail one is back on this pass, the other next pass.
        assert_eq!(bodies(&g.pass(&mut wl, 1000.0, 120).unwrap()), vec![new_last.e]);
        assert_eq!(bodies(&g.pass(&mut wl, 1000.0, 120).unwrap()), vec![new_first.e, new_last.e]);
    }

    #[test]
    fn a_new_world_starts_over() {
        let mut s = small();
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        let old = s.g.w;
        // A load: the player's body is in another world now, with one NPC.
        s.g.world(16);
        s.g.join(s.me);
        let npc = s.g.add(0x1D12, [7.0, 0.0, 0.0], Some(false));
        assert_eq!(bodies(&s.g.pass(&mut wl, 1000.0, 120).unwrap()), vec![npc.e]);
        assert_eq!((wl.world, wl.known.len(), wl.chars.len()), (s.g.w, 2, 2));
        // The old world is not read any more.
        s.g.m.u64(old + W_KEEPER, 0);
        assert_eq!(bodies(&s.g.pass(&mut wl, 1000.0, 120).unwrap()), vec![npc.e]);
    }

    #[test]
    fn a_list_that_looks_wrong_is_an_error() {
        let Small { mut g, me, .. } = small();
        let mut wl = WorldList::default();
        let mut err = |g: &Game| g.pass(&mut wl, 1000.0, 120).err().unwrap_or_default();
        assert_eq!(err(&g), "");
        g.m.u64(g.w + W_KEEPER, g.w + 8);
        assert_eq!(err(&g), "the player's world does not look like one");
        g.m.u64(g.w + W_KEEPER, g.w);
        g.m.u32(g.w + W_LIST + 8, 0);
        assert_eq!(err(&g), "world list empty");
        g.m.u32(g.w + W_LIST + 8, 65);
        assert_eq!(err(&g), "world list looks wrong (65 entries, room for 64)");
        g.m.u32(g.w + W_LIST + 8, 7);
        g.m.u64(g.w + W_LIST, 0x1000);
        assert_eq!(err(&g), "world list looks wrong (7 entries, room for 64)");
        g.m.u64(g.w + W_LIST, 0x7FF0_0000_0000);
        assert_eq!(err(&g), "world list unreadable");
        g.m.u64(g.w + W_LIST, g.arr);
        g.m.u64(me.e + E_WORLD, 0);
        assert_eq!(err(&g), "the player's body is in no world");
        g.m.u64(me.e + E_WORLD, g.w);
        assert_eq!(err(&g), "");
    }

    #[test]
    fn a_world_without_characters_is_an_error() {
        let mut g = Game::new(1 << 16, 8);
        let me = g.add(0xD09, [0.0; 3], Some(true));
        g.me = me.e;
        g.add(0, [1.0; 3], None);
        let mut wl = WorldList::default();
        assert_eq!(g.pass(&mut wl, 1000.0, 120).unwrap_err(), "no characters in the world list");
    }

    #[test]
    fn garbage_in_the_list_is_skipped() {
        let mut s = small();
        let g = &mut s.g;
        // A slot with no pointer, a dead handle, an entity that says another slot, one in another
        // world, one far out (a broken position).
        let i = g.count;
        g.m.u64(g.arr + 8 * i as u64, 0x55);
        g.set_count(i + 1);
        let dead = g.add(0x1D12, [1.0, 0.0, 0.0], Some(false));
        g.m.u32(dead.h + H_LIVE as u64, 1);
        let wrong = g.add(0x1D12, [1.5, 0.0, 0.0], Some(false));
        g.m.u32(wrong.e + E_SLOT, 0);
        let away = g.add(0x1D12, [2.5, 0.0, 0.0], Some(false));
        g.m.u64(away.e + E_WORLD, g.w + 0x10);
        let broken = g.add(0x1D12, [f64::NAN, 0.0, 0.0], Some(false));
        let mut wl = WorldList::default();
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!(bodies(&l), vec![s.near.e, s.bare.e, s.pl.e, s.far.e]);
        assert!(!bodies(&l).contains(&broken.e));
    }

    #[test]
    fn a_lost_marker_is_found_again() {
        let mut s = small();
        let mut wl = WorldList::default();
        s.g.pass(&mut wl, 1000.0, 120).unwrap();
        // Its marker is freed (class word gone): no marker...
        s.g.m.u64(s.near.marker, 0);
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!((l[0].body, l[0].owner), (s.near.e, 0));
        // ...till the game gives it a new one: found on the next pass.
        let m2 = s.g.marker(NPC_TAG, s.near.e);
        s.g.m.u64(s.near.slots + 8 * u64::from(NPC_SLOT), m2);
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert_eq!((l[0].body, l[0].owner), (s.near.e, m2));
    }

    #[test]
    fn her_body_never_shows_but_her_old_one_does() {
        let mut s = small();
        let mut wl = WorldList::default();
        assert!(!bodies(&s.g.pass(&mut wl, 1000.0, 120).unwrap()).contains(&s.me.e));
        // A respawn: a new body is the player's, the old one stays in the world.
        let new_me = s.g.add(0xD09, [0.0, 0.0, 1.0], Some(true));
        s.g.me = new_me.e;
        let l = s.g.pass(&mut wl, 1000.0, 120).unwrap();
        assert!(!bodies(&l).contains(&new_me.e));
        let old = l.iter().find(|b| b.body == s.me.e).expect("the player's old body is listed now");
        assert_eq!((old.owner, old.player), (s.me.marker, true));
    }

    /// Like Erewhon: 18,100 entries, 95 characters (the player's, 14 players, 67 NPCs with a marker, 13
    /// without), about 50 components each. Read through `safe_read`, the kernel copy the mod uses.
    /// The numbers go to the test output (a debug build: the mod is faster).
    #[test]
    fn world_list_pass_cost() {
        let mut g = Game::new(24 << 20, 20_000);
        let me = g.add_n(0xD09, [0.0; 3], Some(true), 70);
        g.me = me.e;
        let mut n = 0;
        for i in 1..18_100 {
            let pos = [(n % 10) as f64 * 9.0 + 2.0, (n / 10) as f64 * 9.0 + 2.0, 0.0];
            if i % 190 == 0 && n < 94 {
                match n {
                    0..14 => g.add_n(0xE09, pos, Some(true), 70),
                    14..81 => g.add_n(0x1D12, pos, Some(false), 50),
                    _ => g.add_n(0x502, pos, None, 50),
                };
                n += 1;
            } else {
                g.add_n(0, pos, None, 0);
            }
        }
        let cx = g.cx();
        let mut wl = WorldList::default();
        let time = |wl: &mut WorldList| {
            let t = Instant::now();
            let l = wl.pass_with(&safe_read, &cx, 1000.0, 120).unwrap();
            (t.elapsed(), l)
        };
        let (first, l) = time(&mut wl);
        let first_reads = wl.last.reads;
        assert_eq!((l.len(), wl.last.chars, wl.last.linked, wl.last.entries), (94, 94, 81, 18_100));
        let (mut total, mut most, mut reads) = (Duration::ZERO, Duration::ZERO, 0);
        for _ in 0..100 {
            let (t, l) = time(&mut wl);
            assert_eq!(l.len(), 94);
            total += t;
            most = most.max(t);
            reads += wl.last.reads;
        }
        let new = g.add(0x1D12, [1.0, 1.0, 0.0], Some(false));
        let (t_new, l) = time(&mut wl);
        assert_eq!((l[0].body, l[0].owner), (new.e, new.marker), "the new body, on the very next pass");
        let new_reads = wl.last.reads;
        g.remove(9_000);
        let (t_rm, l) = time(&mut wl);
        assert_eq!(l.len(), 95);
        println!(
            "world list, 18,100 entries, 95 characters (debug build): first pass {} us ({first_reads} reads); then {} us on average, {} us at most ({} reads); a new body {} us ({new_reads} reads); a swap-remove {} us",
            first.as_micros(),
            (total / 100).as_micros(),
            most.as_micros(),
            reads / 100,
            t_new.as_micros(),
            t_rm.as_micros()
        );
        assert!(total / 100 < Duration::from_millis(50), "a pass should cost well under 50 ms");
    }
}
