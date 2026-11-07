//! God mode: the game checks an "immune" byte on the entity it is about to damage. We find that
//! check by its byte pattern (from the Recon Cheese table, matches the current build once), catch
//! the entity when prip takes one hit, set the byte and hold it. Proved live.

use std::sync::Mutex;


use crate::{freeze, hwbp, log, memory};

/// The damage check. First byte of the match is where the catch goes.
const PATTERN: &str = "48 8B ?? 48 8B ?? FF 90 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? ?? ?? 44 38 A7";
/// Offset of the immune byte inside the entity.
const IMMUNE: u64 = 0x378;
/// Register index of the entity in the caught registers (rax rbx rcx rdx rsi rdi...).
const ENTITY_REG: usize = 5;

static ENTITY: Mutex<u64> = Mutex::new(0);
/// The last entity we set, with its class tag, so on-again after off is instant.
static LAST: Mutex<(u64, u64)> = Mutex::new((0, 0));
static ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Waiting quietly for the first hit to learn the entity (god mode stays off).
static LEARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ERROR: Mutex<String> = Mutex::new(String::new());
/// Arm and learn threads, joined on unload (a thread left inside the DLL crashes the game).
static THREADS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

fn keep(h: std::thread::JoinHandle<()>) {
    if let Ok(mut t) = THREADS.lock() {
        t.retain(|h| !h.is_finished());
        t.push(h);
    }
}

/// End the arm / learn waits and wait for their threads, 5 s each at most.
pub fn shutdown() {
    hwbp::shutdown();
    let threads: Vec<_> = THREADS.lock().map(|mut t| t.drain(..).collect()).unwrap_or_default();
    for h in threads {
        crate::join_for(h, "god mode wait", std::time::Duration::from_secs(5));
    }
}

/// The entity god mode holds, 0 when off. The hold stops by itself when the world changes (its
/// guard sees the object go): then god mode is off, and says so.
fn held() -> u64 {
    let Ok(mut e) = ENTITY.lock() else { return 0 };
    if *e != 0 && !freeze::running("god") {
        log(&format!("god mode off: entity {:#x} went away (world change)", *e));
        *e = 0;
    }
    *e
}

/// Current state for the window.
pub fn status() -> (u64, bool, String) {
    (
        held(),
        ARMED.load(std::sync::atomic::Ordering::Relaxed),
        ERROR.lock().map(|e| e.clone()).unwrap_or_default(),
    )
}

/// Arm on a thread of its own: returns at once, `status` tells how it went.
pub fn arm_background(millis: u32) -> Result<(), String> {
    if ARMED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("already armed and waiting".into());
    }
    if let Ok(mut e) = ERROR.lock() {
        e.clear();
    }
    keep(std::thread::spawn(move || {
        let result = on(millis);
        if let Err(e) = result
            && let Ok(mut err) = ERROR.lock()
        {
            *err = e;
        }
        ARMED.store(false, std::sync::atomic::Ordering::SeqCst);
    }));
    Ok(())
}

/// Learn the player's entity from the first hit after injecting, without turning god mode on, so
/// turning it on later is instant. God mode is off until the player asks. Gives up quietly after
/// `millis`, or when another catch takes the watchpoint; no-op when already learned or learning.
pub fn learn_background(millis: u32) {
    use std::sync::atomic::Ordering;
    if remembered().is_some() || ARMED.load(Ordering::SeqCst) || LEARNING.swap(true, Ordering::SeqCst) {
        return;
    }
    keep(std::thread::spawn(move || {
        let learn = || -> Result<(), String> {
            // Nothing armed while the game loads: the game crashed twice loading with this watch on
            // all its threads. In the world, the player's character is usually found from the
            // player's body and no hit is needed at all. An unload ends both waits at once.
            let start = std::time::Instant::now();
            while crate::fly::body_addr().is_none() {
                if start.elapsed().as_millis() > u128::from(millis) || hwbp::watching() != 0 {
                    return Ok(());
                }
                if !crate::nap(1000) {
                    return Ok(());
                }
            }
            if !crate::nap(5000) || remembered().is_some() {
                return Ok(());
            }
            let site = damage_site()?;
            // Not after the unload began: no new watch.
            if crate::stopping() {
                return Ok(());
            }
            let (caught, _) = hwbp::find_writers(site, 0, millis, 1)?;
            if let Some(hit) = caught.first() {
                let entity = hit.regs[ENTITY_REG];
                if mine(entity) && memory::readable(entity + IMMUNE, 1) {
                    remember(entity);
                    log(&format!("god mode ready: entity {entity:#x} learned from the first hit (still off)"));
                }
            }
            Ok(())
        };
        if let Err(e) = learn() {
            log(&format!("god mode learn: {e}"));
        }
        LEARNING.store(false, Ordering::SeqCst);
    }));
}

/// The caught entity is the player's own character. A hit on anyone else is refused and logged.
fn mine(entity: u64) -> bool {
    match crate::entities::owner_check(entity) {
        Ok(()) => true,
        Err((its, me)) => {
            log(&format!("god mode: caught entity {entity:#x} is not the player (its body {its:#x}, the player {me:#x}), ignored"));
            false
        }
    }
}

fn remember(entity: u64) {
    if let Ok(mut l) = LAST.lock()
        && let Ok(tag) = memory::read(entity, 8)
    {
        *l = (entity, u64::from_le_bytes(tag.try_into().unwrap()));
    }
}

fn damage_site() -> Result<u64, String> {
    let exe = memory::find_module("").ok_or("exe not found")?;
    let at = memory::find_code(&exe, "god damage check", PATTERN)?;
    log(&format!("god mode: damage check at exe+{:#x}, waiting for a hit", at - exe.base));
    Ok(at)
}

/// The player's entity: found from the player's body first (always current), else the one a hit
/// taught us if it still looks like the same object.
fn remembered() -> Option<u64> {
    // The immune flag is 0 or 1; anything else means this isn't the player's character.
    if let Some(e) = crate::entities::my_character().filter(|e| memory::read(e + IMMUNE, 1).is_ok_and(|b| b[0] <= 1)) {
        return Some(e);
    }
    let (ent, tag) = LAST.lock().ok().map(|l| *l)?;
    if ent == 0 || !memory::readable(ent, 8) || !memory::readable(ent + IMMUNE, 1) {
        return None;
    }
    let now = u64::from_le_bytes(memory::read(ent, 8).ok()?.try_into().unwrap());
    (now == tag && crate::entities::owner_check(ent).is_ok()).then_some(ent)
}

/// God mode on from the remembered entity only (no waiting for a hit). True when on.
pub fn on_if_known() -> bool {
    remembered().is_some() && on(0).is_ok()
}

/// God mode is on right now.
pub fn is_on() -> bool {
    held() != 0
}

pub fn on(millis: u32) -> Result<u64, String> {
    if let Some(entity) = remembered() {
        // Held, not written once: a respawn or the game may clear it again.
        let tag = memory::read(entity, 8)?;
        freeze::set_guarded("god", entity + IMMUNE, vec![1], Some((entity, tag)))?;
        if let Ok(mut e) = ENTITY.lock() {
            *e = entity;
        }
        remember(entity);
        log(&format!("god mode on: entity {entity:#x} (found from the player's body)"));
        return Ok(entity);
    }
    let site = damage_site()?;
    // Max 1: stop at the first hit instead of waiting the whole time.
    let (caught, _) = hwbp::find_writers(site, 0, millis, 1)?;
    let Some(hit) = caught.first() else {
        return Err("no hit caught: take one hit while it waits".into());
    };
    let entity = hit.regs[ENTITY_REG];
    if !mine(entity) {
        return Err("the hit caught was on someone else: take a hit yourself and try again".into());
    }
    if !memory::readable(entity + IMMUNE, 1) {
        return Err(format!("entity {entity:#x} not readable"));
    }
    // Guarded by the entity's class tag: after a world change the entity is gone and the hold must
    // stop, not keep writing into freed memory.
    let tag = memory::read(entity, 8)?;
    freeze::set_guarded("god", entity + IMMUNE, vec![1], Some((entity, tag)))?;
    if let Ok(mut e) = ENTITY.lock() {
        *e = entity;
    }
    remember(entity);
    log(&format!("god mode on: entity {entity:#x}"));
    Ok(entity)
}

pub fn off() -> Result<(), String> {
    // Off while still waiting for the hit: stop waiting, or the hit turns it on later.
    if ARMED.load(std::sync::atomic::Ordering::SeqCst) {
        hwbp::cancel();
    }
    // Only clear the byte while the hold is still on: once it dropped, the object may be gone and
    // its memory someone else's.
    let entity = held();
    let tag = LAST.lock().ok().map(|l| *l).filter(|l| l.0 == entity).map(|l| l.1);
    freeze::set("god", 0, Vec::new())?;
    // Still the same object: its class tag is the one the hold's guard checked (at most 15 ms ago),
    // and it is the player's when the player's body can be read. During a load the player's body can't be
    // read: then the tag alone decides, or the byte would stay 1 while the menu says off.
    let same = tag.is_some_and(|t| memory::read(entity, 8).is_ok_and(|b| b == t.to_le_bytes()));
    let own = match crate::entities::owner_check(entity) {
        Ok(()) => true,
        Err((_, me)) => me == 0,
    };
    if entity != 0 && same && own && memory::readable(entity + IMMUNE, 1) {
        let _ = memory::write(entity + IMMUNE, &[0]);
    }
    if let Ok(mut e) = ENTITY.lock() {
        *e = 0;
    }
    log("god mode off");
    Ok(())
}
