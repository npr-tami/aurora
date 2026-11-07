//! Game > Progress > Editor > Gear Score: the score of what the player has on, slot by slot, and of
//! everything the player owns. An item's score is its i32 at +0x7c: the stash, the slots and the
//! player's gear level all read it from there, and it stays after a fast travel and a relog.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_PRIVATE, MEMORY_BASIC_INFORMATION, PAGE_GUARD, PAGE_NOACCESS, PAGE_NOCACHE, PAGE_READWRITE,
    PAGE_WRITECOMBINE, VirtualQuery,
};

use crate::{entities, globals, log, memory};

/// The slots root holds the slot table: its records at +0x328, their count (u16) at +0x332. Each
/// record is 0x20: +0 the slot, +8 a handle; the item is [handle] while the handle's i32 at +0xc is
/// below 0 (the game's own lookup, exe+0x258db40). (That root is `globals::SLOTS_ROOT`;
/// `[globals::LEVEL_ROOT]+0x48c` is the player's gear level as the game shows it; weapon and gear
/// items have the class tables `globals::WEAPON_VT` and `globals::GEAR_VT`.)
const SCORE: u64 = 0x7c;
/// The most without Enable Experimental Features: higher broke things, a 5160 weapon showed the
/// player as level 0 to a friend.
pub const MAX: u32 = 1000;
/// The most while Aurora > Settings > Enable Experimental Features is on, and the highest score
/// read back as real. The menu picks the cap (`gear_max`); the clamps here are the last guard.
pub const HIGH_MAX: u32 = 1_000_000;
/// The menu's cap (`gear_max`), for Set All Owned Items' thread: the switch going off during its
/// search keeps the writes to 1000.
static CAP: AtomicU32 = AtomicU32::new(MAX);

/// The cap now: the menu sets it with Enable Experimental Features.
pub fn set_cap(max: u32) {
    CAP.store(max.min(HIGH_MAX), Ordering::SeqCst);
}

/// The cap as it is now (`set_cap`).
pub fn cap() -> u32 {
    CAP.load(Ordering::SeqCst)
}

/// The rows' order: weapons, then gear.
pub const SLOTS: [&str; 8] = ["Primary", "Secondary", "Pistol", "Head", "Hands", "Vest", "Pants", "Shoes"];
/// Each slot's id (the slot's u32 at +8), matched live by the player's swaps.
const SLOT_IDS: [u32; 8] = [0x4001_e65f, 0x4001_e660, 0x4001_e661, 0x4001_e662, 0x4001_e664, 0x4001_e663, 0x4001_e665, 0x4001_e666];
/// Each slot's weight in the player's gear level (the slot's f32 at +0x3c); the first 3 are
/// weapons.
const WEIGHTS: [f64; 8] = [25.0, 25.0, 10.0, 10.0, 5.0, 10.0, 10.0, 5.0];

fn q(a: u64) -> Option<u64> {
    memory::read(a, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))
}

fn d(a: u64) -> Option<i32> {
    memory::read(a, 4).ok().map(|b| i32::from_le_bytes(b.try_into().unwrap()))
}

/// A pointer read from the game that can be followed: in user space, so adding an offset never
/// overflows (a torn-down object can hold anything).
fn ptr(v: u64) -> Option<u64> {
    (0x10000..0x8000_0000_0000).contains(&v).then_some(v)
}

/// The exe's base, looked up once (a module snapshot per redraw was a waste).
fn exe() -> Option<u64> {
    static BASE: OnceLock<u64> = OnceLock::new();
    if let Some(b) = BASE.get() {
        return Some(*b);
    }
    let b = memory::find_module("")?.base;
    Some(*BASE.get_or_init(|| b))
}

fn is_item(exe: u64, item: u64) -> bool {
    let (Some(w), Some(g)) = (globals::WEAPON_VT.rva(), globals::GEAR_VT.rva()) else { return false };
    ptr(item).is_some() && q(item).is_some_and(|vt| vt == exe + w || vt == exe + g)
}

/// The item in each slot, in `SLOTS` order; None for an empty slot.
pub fn equipped() -> [Option<u64>; 8] {
    let mut out = [None; 8];
    let Some(exe) = exe() else { return out };
    let Some(slots) = globals::SLOTS_ROOT.rva() else { return out };
    let Some(root) = q(exe + slots).and_then(ptr) else { return out };
    let (Some(recs), Some(n)) = (q(root + 0x328).and_then(ptr), memory::read(root + 0x332, 2).ok()) else { return out };
    let n = u16::from_le_bytes([n[0], n[1]]).min(32);
    for i in 0..u64::from(n) {
        let rec = recs + i * 0x20;
        let (Some(slot), Some(handle)) = (q(rec).and_then(ptr), q(rec + 8).and_then(ptr)) else { continue };
        let Some(at) = d(slot + 8).and_then(|id| SLOT_IDS.iter().position(|&s| s == id as u32)) else { continue };
        if d(handle + 0xc).is_some_and(|x| x < 0)
            && let Some(item) = q(handle).filter(|&it| is_item(exe, it))
        {
            out[at] = Some(item);
        }
    }
    out
}

pub fn score(item: u64) -> Option<u32> {
    d(item + SCORE).filter(|v| (0..=HIGH_MAX as i32).contains(v)).map(|v| v as u32)
}

/// What the item in slot `i` scores now.
pub fn slot_score(i: usize) -> Option<u32> {
    equipped()[i].and_then(score)
}

/// The player's gear level from what the player has on, as the game works it out (exe+0x2733ee0):
/// half the sum of the weapons' and the gear's weighted means. It matched the game's at 558, 486,
/// 440, 1355 and 1386. The game only works it out again on a swap, so after a write its own number
/// stays old; with a slot empty, the game's own.
pub fn total() -> Option<u32> {
    let on = equipped();
    let (mut sum, mut weight) = ([0.0f64; 2], [0.0f64; 2]);
    for (i, item) in on.iter().enumerate() {
        let Some(v) = item.and_then(score) else { return game_total() };
        let kind = usize::from(i >= 3);
        sum[kind] += f64::from(v) * WEIGHTS[i];
        weight[kind] += WEIGHTS[i];
    }
    Some(((sum[0] / weight[0] + sum[1] / weight[1]) / 2.0) as u32)
}

/// The player's gear level as the game shows it.
fn game_total() -> Option<u32> {
    let root = q(exe()? + globals::LEVEL_ROOT.rva()?).and_then(ptr)?;
    d(root + 0x48c).filter(|v| (0..=HIGH_MAX as i32).contains(v)).map(|v| v as u32)
}

/// Set what slot `i` has on. The slot is read again here, right before the write: a swap in between
/// gives the new item, never a stale one.
pub fn set_slot(i: usize, v: u32) -> Result<(), String> {
    let item = equipped()[i].ok_or("Nothing in that slot")?;
    memory::write(item + SCORE, &v.min(HIGH_MAX).to_le_bytes())
}

/// Set everything the player has on; how many were set. One read of the slots, each item checked
/// again right before its write.
pub fn set_equipped(v: u32) -> Result<usize, String> {
    let exe = exe().ok_or("exe not found")?;
    let mut n = 0;
    for item in equipped().into_iter().flatten() {
        if is_item(exe, item) && memory::write(item + SCORE, &v.min(HIGH_MAX).to_le_bytes()).is_ok() {
            n += 1;
        }
    }
    if n == 0 { Err("Nothing on to set".into()) } else { Ok(n) }
}

static OWNED_BUSY: AtomicBool = AtomicBool::new(false);
/// Set on unload: the search stops, and `shutdown` waits for its thread (a thread left inside the
/// DLL crashes the game, see godmode).
static STOP: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

pub fn shutdown() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = THREAD.lock().ok().and_then(|mut t| t.take()) {
        let _ = h.join();
    }
}

/// Clears OWNED_BUSY however the thread ends, a panic too.
struct Busy;

impl Drop for Busy {
    fn drop(&mut self) {
        OWNED_BUSY.store(false, Ordering::SeqCst);
    }
}
/// The last Set All Owned Items: what it set them to and how many.
static OWNED_LAST: Mutex<Option<(u32, usize)>> = Mutex::new(None);

pub fn owned_busy() -> bool {
    OWNED_BUSY.load(Ordering::SeqCst)
}

pub fn owned_last() -> Option<(u32, usize)> {
    OWNED_LAST.lock().ok().and_then(|l| *l)
}

/// Set every weapon and piece of gear the player owns, stash too, on a thread (the search reads all
/// of the game's memory, some seconds). False when one is running already. `hushed`: started from
/// the command box with Suppress > Command Box on, so the result only goes to the log.
pub fn start_set_owned(v: u32, hushed: bool) -> bool {
    if STOP.load(Ordering::SeqCst) || OWNED_BUSY.swap(true, Ordering::SeqCst) {
        return false;
    }
    let v = v.min(HIGH_MAX);
    let h = std::thread::spawn(move || {
        let _busy = Busy;
        let msg = match set_owned(v) {
            Ok((n, v)) => {
                if let Ok(mut l) = OWNED_LAST.lock() {
                    *l = Some((v, n));
                }
                format!("{n} owned items set to {v}")
            }
            Err(e) => e,
        };
        log(&format!("gear score: {msg}"));
        if !hushed {
            crate::menu::toast(msg);
        }
    });
    if let Ok(mut t) = THREAD.lock() {
        *t = Some(h);
    }
    true
}

/// How many were set, and to what: the cap at write time.
fn set_owned(v: u32) -> Result<(usize, u32), String> {
    let exe = exe().ok_or("exe not found")?;
    // Always allowed. Tested with Alyssa near: the player 3 weapons were in memory here and got the
    // write, but the player's real gear stayed the same, so other players' items only change in
    // this game's view. Logs who was near and the scores it found.
    let others = entities::near(f64::MAX, usize::MAX, false, true).map(|v| v.iter().filter(|b| b.player).count()).unwrap_or(0);
    log(&format!("gear score: set owned with {others} other player(s) near"));
    let started = std::time::Instant::now();
    let items = scan(exe);
    log(&format!("gear score: {} items found in {} ms", items.len(), started.elapsed().as_millis()));
    let mut seen: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for (_, was) in &items {
        *seen.entry(*was).or_default() += 1;
    }
    log(&format!("gear score: scores found (score x count): {seen:?}"));
    if STOP.load(Ordering::SeqCst) {
        return Err("Stopped".into());
    }
    let (mut n, mut set) = (0, v);
    for (item, was) in items {
        // The cap for each item: Enable Experimental Features can go off meanwhile.
        set = v.min(cap());
        // Read again right before the write (a world change can free it meanwhile).
        if is_item(exe, item) && score(item) == Some(was) && memory::write(item + SCORE, &set.to_le_bytes()).is_ok() {
            n += 1;
        }
    }
    if n == 0 { Err("No owned items found".into()) } else { Ok((n, set)) }
}

/// Every weapon and gear object with a score, and that score. 0 and 1 are things without one.
fn scan(exe: u64) -> Vec<(u64, u32)> {
    let (Some(w), Some(g)) = (globals::WEAPON_VT.rva(), globals::GEAR_VT.rva()) else { return Vec::new() };
    let (wvt, gvt) = ((exe + w).to_le_bytes(), (exe + g).to_le_bytes());
    let mut out = Vec::new();
    let mut read_total = 0u64;
    for (base, size) in heap() {
        let mut off = 0;
        while off < size && !STOP.load(Ordering::SeqCst) {
            let want = (size - off).min(1 << 20);
            if let Ok(buf) = memory::read(base + off, want as usize) {
                for (i, w) in buf.as_chunks::<8>().0.iter().enumerate() {
                    if *w == wvt || *w == gvt {
                        let item = base + off + i as u64 * 8;
                        if let Some(s) = score(item).filter(|&s| s >= 2) {
                            out.push((item, s));
                        }
                    }
                }
            }
            // A short breather now and then, as entities::near does.
            if read_total / (64 << 20) != (read_total + want) / (64 << 20) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            read_total += want;
            off += want;
        }
    }
    out
}

/// The game's plain read-write heap: not the graphics card's memory and not a thread stack. No size
/// cap: items sit in big heap blocks too.
fn heap() -> Vec<(u64, u64)> {
    let mut v = Vec::new();
    let mut stacks: Vec<u64> = Vec::new();
    let mut cur: u64 = 0;
    loop {
        // SAFETY: inspection of our own address space.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { VirtualQuery(cur as *const c_void, &mut info, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 {
            break;
        }
        let (base, size) = (info.BaseAddress as u64, info.RegionSize as u64);
        if info.Protect & PAGE_GUARD != 0 {
            stacks.push(info.AllocationBase as u64);
        }
        let ok = info.State == MEM_COMMIT
            && info.Type == MEM_PRIVATE
            && info.Protect & 0xff == PAGE_READWRITE
            && info.Protect & (PAGE_NOACCESS | PAGE_GUARD | PAGE_NOCACHE | PAGE_WRITECOMBINE) == 0;
        if ok {
            v.push((base, size, info.AllocationBase as u64));
        }
        match base.checked_add(size) {
            Some(next) if next > cur => cur = next,
            _ => break,
        }
    }
    v.into_iter().filter(|(_, _, alloc)| !stacks.contains(alloc)).map(|(b, s, _)| (b, s)).collect()
}
