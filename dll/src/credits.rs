//! Credits: catch the object the game reads credits from while a shop is open, then decode and
//! write its balance.
//!
//! A shop opening is noticed by itself: a small hook at the read function's start, on from inject,
//! notes each object the player's own credits caller reads. Only that caller; nothing is changed in
//! the game.
//!
//! Credits are stored scrambled: the stored 32-bit value XOR a key built from one byte behind each
//! of four pointers at the start of the credits object (the community table for the Vulkan build,
//! v1.5, 2026).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};


use crate::{hwbp, log, memory};

/// Inside the credits read function (the table's hook site, minus a changed prologue).
const PATTERN: &str = "48 8B 01 33 D2 4C 8B C1";
/// The function starts 5 bytes before the pattern with `mov [rsp+8], rbx` (48 89 5C 24 08), which
/// fits our 5-byte jump inside one code word.
const START: [u8; 5] = [0x48, 0x89, 0x5C, 0x24, 0x08];
/// Our code block: the caller address, the count, the last 16 objects, then the code.
const CALLER_AT: u64 = 0;
const COUNT_AT: u64 = 8;
const RING_AT: u64 = 0x10;
const RING: u64 = 16;
const CODE_AT: u64 = 0x100;
/// The object is in rcx at that spot; register index in the caught registers.
const OBJ_REG: usize = 2;
const BALANCE_OFF: u64 = 0x20;
/// The highest a signed whole number goes; above it the game may show it as negative.
pub const MAX: u32 = 0x7FFF_FFFF;

/// The credits object once caught; 0 before.
static OBJ: AtomicU64 = AtomicU64::new(0);
static CATCHING: AtomicBool = AtomicBool::new(false);
/// What the last watch saw: the same read gives other points too, so the player picks the one the
/// shop shows.
static FOUND: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// The shop hook: our block, and the code word swapped at the function start.
struct Shop {
    block: u64,
    word: u64,
    old: u64,
    new: u64,
}

static SHOP: std::sync::Mutex<Option<Shop>> = std::sync::Mutex::new(None);
/// Set by `shutdown` under the `SHOP` lock: the hook never goes in after that.
static CLOSED: AtomicBool = AtomicBool::new(false);
/// How many reads the hook had counted when we last looked.
static SEEN: AtomicU64 = AtomicU64::new(0);
/// The player picked other points the player: a shop opening doesn't switch back to the credits
/// until that pick is gone.
static OWN_PICK: AtomicBool = AtomicBool::new(false);
/// Every kind of points the last 'other points' watch saw.
static OTHERS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

/// When the player's credits caller is the one asking, keep rcx (the object) in the ring.
fn shop_code(block: u64, func: u64) -> Result<Vec<u8>, String> {
    let at = block + CODE_AT;
    let mut c = vec![0x50, 0x52, 0x48, 0x8B, 0x05]; // Push rax; push rdx; mov rax, [rip+caller]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, block + CALLER_AT)?);
    c.extend_from_slice(&[0x48, 0x39, 0x44, 0x24, 0x10, 0x75, 0x1A]); // Cmp [rsp+0x10], rax; jne skip (26 bytes on)
    c.extend_from_slice(&[0x48, 0x8D, 0x15]); // Lea rdx, [rip+ring]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, block + RING_AT)?);
    c.extend_from_slice(&[0x8B, 0x05]); // Mov eax, [rip+count]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, block + COUNT_AT)?);
    c.extend_from_slice(&[0xFF, 0x05]); // Inc dword [rip+count]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, block + COUNT_AT)?);
    c.extend_from_slice(&[0x83, 0xE0, (RING - 1) as u8]); // And eax, 15
    c.extend_from_slice(&[0x48, 0x89, 0x0C, 0xC2]); // Mov [rdx+rax*8], rcx
    c.extend_from_slice(&[0x5A, 0x58]); // Skip: pop rdx; pop rax
    c.extend_from_slice(&START);
    c.push(0xE9);
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, func + 5)?);
    Ok(c)
}

/// Put the shop hook in (once, at inject, off the menu's thread).
pub fn warm() {
    match install() {
        Ok(()) => log("credits: shop hook on"),
        Err(e) => log(&format!("credits: shop hook not put in: {e}")),
    }
}

fn install() -> Result<(), String> {
    if CLOSED.load(Ordering::SeqCst) {
        return Err("the mod is unloading".into());
    }
    if SHOP.lock().map_err(|_| "shop lock")?.is_some() {
        return Ok(());
    }
    let exe = memory::find_module("").ok_or("exe not found")?;
    let func = memory::find_code(&exe, "credits shop read", PATTERN)? - 5;
    let word = func & !7;
    if memory::read(func, 5)? != START || func + 5 > word + 8 {
        return Err(format!("function start at exe+{:#x} is not as expected", func - exe.base));
    }
    let block = memory::alloc_code_near(exe.base, 0x200).ok_or("no room for the shop code")?;
    let caller = crate::globals::CREDITS_CALLER.rva().ok_or("credits caller not found (game updated?)")?;
    memory::write(block + CALLER_AT, &(exe.base + caller).to_le_bytes())?;
    memory::write(block + CODE_AT, &shop_code(block, func)?)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (func - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(func + 5, block + CODE_AT)?);
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    // The swap and the record under one lock with the closed check, so `shutdown` either runs
    // before (no hook) or after (takes it out).
    let mut s = SHOP.lock().map_err(|_| "shop lock")?;
    if CLOSED.load(Ordering::SeqCst) {
        return Err("the mod is unloading".into());
    }
    let was = memory::swap_code_u64(word, new)?;
    if was != old {
        let _ = memory::swap_code_u64(word, was);
        return Err("the function start changed under us; left alone".into());
    }
    *s = Some(Shop { block, word, old, new });
    drop(s);
    log(&format!("credits: shop hook at exe+{:#x}", func - exe.base));
    Ok(())
}

/// Take the hook out (unload).
pub fn shutdown() {
    let mut s = SHOP.lock().unwrap_or_else(|e| e.into_inner());
    CLOSED.store(true, Ordering::SeqCst);
    if let Some(h) = s.take() {
        match memory::swap_code_u64(h.word, h.old) {
            Ok(was) if was == h.new => {}
            Ok(was) => {
                let _ = memory::swap_code_u64(h.word, was);
                log("credits: shop hook word changed; left alone");
            }
            Err(e) => log(&format!("credits: shop hook out: {e}")),
        }
    }
}

/// New reads since we last looked: the objects of this shop opening become the found list, and when
/// there's only one, it's the player's credits. True when a shop was seen this time.
fn look() -> bool {
    let Some(block) = SHOP.lock().ok().and_then(|s| s.as_ref().map(|h| h.block)) else { return false };
    let Ok(c) = memory::read(block + COUNT_AT, 4) else { return false };
    let count = u64::from(u32::from_le_bytes(c.try_into().unwrap()));
    let seen = SEEN.swap(count, Ordering::Relaxed);
    if count == seen {
        return false;
    }
    let Ok(ring) = memory::read(block + RING_AT, (RING * 8) as usize) else { return false };
    let fresh = count.wrapping_sub(seen).min(RING);
    let mut objs: Vec<u64> = Vec::new();
    for k in 0..fresh {
        let slot = count.wrapping_sub(1 + k) % RING;
        let o = u64::from_le_bytes(ring[(slot * 8) as usize..(slot * 8 + 8) as usize].try_into().unwrap());
        if (0x10000..0x7fff_ffff_0000).contains(&o) && !objs.contains(&o) && balance_of(o).is_some_and(|b| b < 100_000_000) {
            objs.push(o);
        }
    }
    if objs.is_empty() {
        return false;
    }
    let held = OBJ.load(Ordering::Relaxed);
    let keep = OWN_PICK.load(Ordering::Relaxed) && held != 0;
    if objs.len() == 1 && held != objs[0] && !keep {
        pick(objs[0]);
    }
    if let Ok(mut f) = FOUND.lock() {
        *f = objs;
    }
    true
}

/// Build the key from the four bytes (one per pointer): bit n of byte k goes to bit 4n+k.
fn key_from_bytes(b: [u8; 4]) -> u32 {
    let mut key = 0u32;
    for n in 0..8 {
        for (k, byte) in b.iter().enumerate() {
            if (byte >> n) & 1 == 1 {
                key |= 1 << (4 * n + k);
            }
        }
    }
    key
}

/// The key of a credits object, from one byte behind each of its first four pointers.
fn key_of(obj: u64) -> Option<u32> {
    let head = memory::read(obj, 32).ok()?;
    let mut b = [0u8; 4];
    for (i, slot) in b.iter_mut().enumerate() {
        let p = u64::from_le_bytes(head[i * 8..i * 8 + 8].try_into().unwrap());
        if !(0x10000..0x7fff_ffff_0000).contains(&p) {
            return None;
        }
        *slot = memory::read(p, 1).ok()?[0];
    }
    Some(key_from_bytes(b))
}

fn balance_of(obj: u64) -> Option<u32> {
    let key = key_of(obj)?;
    let stored = u32::from_le_bytes(memory::read(obj.checked_add(BALANCE_OFF)?, 4).ok()?.try_into().unwrap());
    Some(stored ^ key)
}

/// The balance now, when the object is caught and still readable.
pub fn balance() -> Option<u32> {
    look();
    match OBJ.load(Ordering::Relaxed) {
        0 => None,
        obj => {
            let b = balance_of(obj);
            // Stale after a world change or logout: forget it, so Enter catches again.
            if b.is_none() {
                OBJ.store(0, Ordering::Relaxed);
                OWN_PICK.store(false, Ordering::Relaxed);
            }
            b
        }
    }
}

/// The 20 s watch is running.
pub fn watching() -> bool {
    CATCHING.load(Ordering::Relaxed)
}

pub fn catching() -> bool {
    CATCHING.load(Ordering::Relaxed) || (shop_hook() && OBJ.load(Ordering::Relaxed) == 0 && found().is_empty())
}

fn shop_hook() -> bool {
    SHOP.lock().is_ok_and(|s| s.is_some())
}

/// Watch the credits read for 20 s on a thread, while the player opens a shop; keeps the player's
/// own credits object. False when a watch is already running.
pub fn start_catch() -> bool {
    if shop_hook() {
        return true;
    }
    start_watch(false)
}

/// Other points than the credits: the 20 s watch of every read while the player opens a shop, for
/// the player to pick from. False when a watch is already running.
pub fn start_pick_other() -> bool {
    start_watch(true)
}

fn start_watch(others: bool) -> bool {
    if CATCHING.swap(true, Ordering::SeqCst) {
        return false;
    }
    let h = std::thread::spawn(move || {
        match catch(20_000, others) {
            Ok(objs) => {
                log(&format!("credits: {} kinds of points: {:?}", objs.len(), objs.iter().map(|o| balance_of(*o)).collect::<Vec<_>>()));
                if let Ok(mut f) = if others { OTHERS.lock() } else { FOUND.lock() } {
                    *f = objs;
                }
            }
            Err(e) => log(&format!("credits: {e}")),
        }
        CATCHING.store(false, Ordering::SeqCst);
    });
    if let Ok(mut w) = WATCH.lock() {
        *w = Some(h);
    }
    true
}

/// The 20 s watch's thread, waited for on unload.
static WATCH: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// Wait for the watch on unload, after the watchpoints are cancelled (god mode's shutdown), so it
/// ends soon after.
pub fn join_watch() {
    if let Some(h) = WATCH.lock().ok().and_then(|mut w| w.take()) {
        crate::join_for(h, "credits watch", std::time::Duration::from_secs(5));
    }
}

fn catch(millis: u32, others: bool) -> Result<Vec<u64>, String> {
    let exe = memory::find_module("").ok_or("exe not found")?;
    let hits = [memory::find_code(&exe, "credits shop read", PATTERN)?];
    let caller = crate::globals::CREDITS_CALLER.rva().ok_or("credits caller not found (game updated?)")?;
    log(&format!("credits: watching exe+{:#x}", hits[0] - exe.base));
    // The watch keeps one read per object and ends once it holds 256; the inventory's reads can
    // fill it before a shop opens. For other points, watch again with the time left until the
    // shop's own reader is seen; a few times at most (arming every thread over and over has crashed
    // the game) and never after a cancel (a freeze, or another watch taking over).
    let start = std::time::Instant::now();
    let cancels = hwbp::cancels();
    let mut passes = 0;
    let (caught, total) = loop {
        passes += 1;
        let left = millis.saturating_sub(start.elapsed().as_millis() as u32).max(1);
        // Other points: done 2 s after the shop's (or inventory's) credits read.
        let finish = others.then_some((exe.base + caller, 2000));
        let (caught, total) = hwbp::find_writers_until(hits[0], 0, left, 200, Some(OBJ_REG), finish)?;
        // The read itself may not be in the list (one read per object is kept).
        let shop_seen = (others && hwbp::finish_seen()) || caught.iter().any(|h| h.ret.saturating_sub(exe.base) == caller);
        let left = millis.saturating_sub(start.elapsed().as_millis() as u32);
        if !others || shop_seen || left < 1000 || passes >= 5 || hwbp::cancels() != cancels {
            break (caught, total);
        }
        log(&format!("credits: {} objects, no shop read yet; watching again", caught.len()));
    };
    log(&format!("credits: {} objects, {total} hits, {passes} passes", caught.len()));
    // Other points: what was first read from the shop's credits read on (the list is in the order
    // things were first read). What came before is the inventory's item counts and such.
    let shop_at = caught.iter().position(|h| h.ret.saturating_sub(exe.base) == caller);
    let caught: Vec<_> = match shop_at {
        Some(i) if others => caught[i..].to_vec(),
        _ => caught,
    };
    // The known caller's objects first, then any other readable one.
    let mut objs: Vec<u64> = Vec::new();
    for mine in [true, false] {
        for h in &caught {
            let obj = h.regs[OBJ_REG];
            if (h.ret.saturating_sub(exe.base) == caller) == mine && !objs.contains(&obj) && balance_of(obj).is_some() {
                objs.push(obj);
            }
        }
    }
    if objs.is_empty() {
        return Err("no shop opened, or the credits were not read".into());
    }
    Ok(objs)
}

/// The points the last watch saw, with their amounts now, for the player to pick from.
pub fn found() -> Vec<(u64, u32)> {
    look();
    // Amounts past 100 million are other data read the same way, not points.
    FOUND.lock().map(|f| f.iter().filter_map(|o| Some((*o, balance_of(*o).filter(|b| *b < 100_000_000)?))).collect()).unwrap_or_default()
}

/// Use this one as the credits from now on.
pub fn pick(obj: u64) {
    OBJ.store(obj, Ordering::Relaxed);
    OWN_PICK.store(false, Ordering::Relaxed);
    log(&format!("credits: picked {obj:#x}, balance {:?}", balance_of(obj)));
}

/// What the 'other points' watch saw, with amounts now, highest first. Every amount, 0 too; over
/// 100 million is other data, as in found.
pub fn others() -> Vec<(u64, u32)> {
    let mut v: Vec<(u64, u32)> = OTHERS.lock().map(|f| f.iter().filter_map(|o| Some((*o, balance_of(*o).filter(|b| *b < 100_000_000)?))).collect()).unwrap_or_default();
    v.sort_by_key(|o| std::cmp::Reverse(o.1));
    v
}

/// The player's own pick from the list (stays until it's gone, see OWN_PICK).
pub fn pick_own(obj: u64) {
    pick(obj);
    OWN_PICK.store(true, Ordering::Relaxed);
}

/// Write a new balance and read it back; puts the old one back if the check fails.
pub fn set(value: u32) -> Result<u32, String> {
    let obj = OBJ.load(Ordering::Relaxed);
    if obj == 0 {
        return Err("Open a shop first".into());
    }
    let value = value.min(MAX);
    let key = key_of(obj).ok_or("Credits moved; open a shop again")?;
    let at = obj + BALANCE_OFF;
    let before = memory::read(at, 4)?;
    memory::write(at, &(value ^ key).to_le_bytes())?;
    match balance_of(obj) {
        Some(v) if v == value => {
            log(&format!("credits: wrote {value} to {obj:#x}"));
            Ok(v)
        }
        other => {
            let _ = memory::write(at, &before);
            Err(format!("Check failed ({other:?}), put back"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hook_after_shutdown() {
        // Nothing is in, so this only closes; install must then refuse before any search.
        shutdown();
        assert_eq!(install(), Err("the mod is unloading".to_string()));
    }

    #[test]
    fn key_interleaves_bits() {
        assert_eq!(key_from_bytes([0, 0, 0, 0]), 0);
        assert_eq!(key_from_bytes([1, 0, 0, 0]), 1);
        assert_eq!(key_from_bytes([0, 1, 0, 0]), 2);
        assert_eq!(key_from_bytes([0, 0, 0, 1]), 8);
        assert_eq!(key_from_bytes([2, 0, 0, 0]), 1 << 4);
        assert_eq!(key_from_bytes([0xFF, 0xFF, 0xFF, 0xFF]), 0xFFFF_FFFF);
    }
}
