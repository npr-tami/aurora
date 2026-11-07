//! Stand While Flying: while Fly is on, the player's character never goes into the falling state,
//! so the player stands in the air and can aim and shoot. Two hooks, both only for the player and
//! only while Fly is on. There is no switch: the hooks go in with Fly and out with it
//! (`fly::follow_fly`). The hooks:
//! - Fall: when a character starts to fall, the game runs one function on that character's fall
//!   object (exe+0x353d0c0 in that build): it asks the state manager for state 0x2D (falling). The
//!   fall object's +0x38 is the manager, the player's character's +0x310. Our code returns at once.
//! - In the air: leaving the ground also switches on an "in the air" part of the character's
//!   behaviour tree, which sets the blackboard value 0xF1 to true (exe+0x3539ac0). While that is
//!   true the whole weapon branch stays off: no aiming, no shooting. The part's +8 is the tree
//!   owner, whose +0xA8 is the character. For the player's our code stores false instead, every
//!   time (crouching in the air switches the part on again). Proven live first: setting the
//!   player's value to false in the air let the player's aim and shoot.
//!
//! (night, water snap): the fall code counts the falls it keeps from the player (`blocked`, said at
//! Fly off), and Let Game Fall (Aurora > Experiments, untested) lets the player's falls through
//! while the in-air hook stays. The fall function (exe+0x353d0c0) is a part of the player's
//! behaviour tree: it sets the movement mode (4 or 5) on the player's movement component and the
//! blackboard value 0xF2 (read offline). Suspected, not proven: kept from the player, a movement
//! mode from before can stay, such as swimming after the player flew out of water.

use std::sync::Mutex;

use crate::{log, memory};

/// The fall object's start: `mov [rsp+0x18], rbx` (5 bytes), then its frame.
const FALL: &str = "48 89 5C 24 18 55 48 8D AC 24 60 FF FF FF 48 81 EC A0 01 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 90 00 00 00 48 8B D9 E8";
/// In the "in the air" part's start: `mov rdi, rcx`, then `mov byte [rsp+0x20], 1` (the value it is
/// about to store, 5 bytes, hooked) and the store of blackboard value 0xF1.
const IN_AIR: &str = "48 8B F9 C6 44 24 20 01 48 8B 49 08 4C 8D 44 24 20 BA F1 00 00 00 48 8B 49 38 E8";
const IN_AIR_AT: u64 = 3;
const IN_AIR_ORIG: [u8; 5] = [0xC6, 0x44, 0x24, 0x20, 0x01];
/// In the fall object: its character's state manager.
const OBJ_MANAGER: u8 = 0x38;
/// In the player's character object: the state manager.
const CHAR_MANAGER: u64 = 0x310;
/// Our blocks: the code, then who the player is (0 = leave everyone alone).
pub(crate) const SLOT_AT: u64 = 0x40;
const BLOCK: usize = 0x80;
/// In the fall block after the player's manager: how many falls our code kept from the player.
const FALL_COUNT: u64 = SLOT_AT + 0x8;
/// Where the fall code goes on into the game's function (`go:`).
const FALL_GO: usize = 28;
/// Fly: Let Game Fall: the player's falls are not kept from the player while the player flies.
/// Aiming in the air stays (the in-air hook). Off by default.
static LET_FALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn let_fall() -> bool {
    LET_FALL.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_let_fall(on: bool) {
    LET_FALL.store(on, std::sync::atomic::Ordering::SeqCst);
    refresh();
}

pub(crate) struct Hook {
    pub(crate) block: u64,
    word: u64,
    /// Where the jump starts, inside `word`.
    at: u64,
    old: u64,
    new: u64,
}

struct Hooks {
    fall: Hook,
    in_air: Hook,
    on: bool,
}

static HOOK: Mutex<Option<Hooks>> = Mutex::new(None);
static BROKEN: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

fn find(exe: &aurora_core::proto::ModuleInfo, pattern: &str, what: &str) -> Result<u64, String> {
    memory::find_code(exe, &format!("airwalk {what}"), pattern)
}

/// Put our code in a new block and work out the 8-byte code word that jumps to it from `at`. `code`
/// gets the block address and returns the bytes; they end by jumping back.
pub(crate) fn hook_at(exe: &aurora_core::proto::ModuleInfo, at: u64, what: &str, code: impl Fn(u64) -> Result<Vec<u8>, String>) -> Result<Hook, String> {
    if at % 8 > 3 {
        return Err(format!("the {what} jump would cross a code word"));
    }
    let block = memory::alloc_code_near(exe.base, BLOCK).ok_or(format!("no room for the {what} code"))?;
    let c = code(block)?;
    if c.len() as u64 > SLOT_AT {
        return Err(format!("{what} code too long"));
    }
    memory::write(block, &c)?;
    memory::write(block + SLOT_AT, &[0; 8])?;
    let word = at & !7;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut b = old_bytes;
    let i = (at - word) as usize;
    b[i] = 0xE9;
    b[i + 1..i + 5].copy_from_slice(&rel(at + 5, block)?);
    log(&format!("{what} hook ready: exe+{:#x}, code at {block:#x}", at - exe.base));
    Ok(Hook { block, word, at, old, new: u64::from_le_bytes(b.try_into().unwrap()) })
}

/// The fall code, 39 bytes at `block`, for the fall object's function at `at` (its first 5 bytes
/// `orig`):
/// ```text
///       push rax
///       mov rax, [rip+manager]   ; the player state manager (0: leave everyone alone)
///       test rax, rax
///       jz go
///       cmp [rcx+0x38], rax; this fall object's manager
///       jne go
///       inc qword [rip+count]    ; one more fall kept from the player
///       pop rax
///       ret; the player does not fall
/// go:   pop rax
///       mov [rsp+0x18], rbx; the function's first instruction
///       jmp at+5
/// ```
/// Only the flags change on the player's way (free at a function's start); rax is put back.
fn fall_code(block: u64, at: u64, orig: &[u8]) -> Result<Vec<u8>, String> {
    let mut c = vec![0x50, 0x48, 0x8B, 0x05];
    c.extend_from_slice(&rel(block + 8, block + SLOT_AT)?);
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, (FALL_GO - 13) as u8, 0x48, 0x39, 0x41, OBJ_MANAGER, 0x75, (FALL_GO - 19) as u8]);
    c.extend_from_slice(&[0x48, 0xFF, 0x05]);
    c.extend_from_slice(&rel(block + 26, block + FALL_COUNT)?);
    c.extend_from_slice(&[0x58, 0xC3]);
    if c.len() != FALL_GO {
        return Err("fall code jumps are off".into());
    }
    c.push(0x58);
    c.extend_from_slice(orig);
    c.push(0xE9);
    c.extend_from_slice(&rel(block + c.len() as u64 + 4, at + 5)?);
    Ok(c)
}

/// How many falls Stand While Flying kept from the player's since load (0 when its hooks are not
/// ready).
pub fn blocked() -> u64 {
    let Ok(slot) = HOOK.lock() else { return 0 };
    let Some(h) = slot.as_ref() else { return 0 };
    memory::read(h.fall.block + FALL_COUNT, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0)
}

fn prepare() -> Result<Hooks, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let fall_at = find(&exe, FALL, "fall")?;
    let air_at = find(&exe, IN_AIR, "in-air")? + IN_AIR_AT;
    let fall_orig = memory::read(fall_at, 5)?;
    if memory::read(air_at, 5)? != IN_AIR_ORIG {
        return Err("in-air code is not what we expect".into());
    }
    let fall = hook_at(&exe, fall_at, "fall", |block| fall_code(block, fall_at, &fall_orig))?;
    // Push rax; mov rax, [rdi+8]; test rax, rax; jz other; mov rax, [rax+0xa8]; test rax, rax; jz
    // other; cmp rax, [rip+the player]; jne other; pop rax; mov byte [rsp+0x20], 0; jmp back;
    // other: pop rax; mov byte [rsp+0x20], 1; jmp back
    let in_air = hook_at(&exe, air_at, "in-air", |block| {
        let mut c = vec![0x50, 0x48, 0x8B, 0x47, 0x08, 0x48, 0x85, 0xC0, 0x74, 0x20];
        c.extend_from_slice(&[0x48, 0x8B, 0x80, 0xA8, 0x00, 0x00, 0x00, 0x48, 0x85, 0xC0, 0x74, 0x14]);
        c.extend_from_slice(&[0x48, 0x3B, 0x05]);
        c.extend_from_slice(&rel(block + c.len() as u64 + 4, block + SLOT_AT)?);
        c.extend_from_slice(&[0x75, 0x0B, 0x58, 0xC6, 0x44, 0x24, 0x20, 0x00, 0xE9]);
        c.extend_from_slice(&rel(block + c.len() as u64 + 4, air_at + 5)?);
        if c.len() != 42 {
            return Err("in-air code jumps are off".into());
        }
        c.push(0x58);
        c.extend_from_slice(&IN_AIR_ORIG);
        c.push(0xE9);
        c.extend_from_slice(&rel(block + c.len() as u64 + 4, air_at + 5)?);
        Ok(c)
    })?;
    Ok(Hooks { fall, in_air, on: false })
}

/// Find the functions once (at load, so switching on is instant).
pub fn warm() {
    let _ = ready();
}

fn ready() -> Result<(), String> {
    let mut slot = HOOK.lock().map_err(|_| "stand lock")?;
    if slot.is_some() {
        return Ok(());
    }
    if let Some(e) = BROKEN.lock().ok().and_then(|b| b.clone()) {
        return Err(e);
    }
    match prepare() {
        Ok(h) => {
            *slot = Some(h);
            Ok(())
        }
        Err(e) => {
            log(&format!("stand while flying: {e}"));
            if let Ok(mut b) = BROKEN.lock() {
                *b = Some(e.clone());
            }
            Err(e)
        }
    }
}

/// The player's character and its state manager while Fly is on, else zeros (leave everyone alone).
fn player() -> (u64, u64) {
    if !crate::fly::is_on() {
        return (0, 0);
    }
    let Some(c) = crate::entities::my_character() else { return (0, 0) };
    let m = memory::read(c + CHAR_MANAGER, 8)
        .ok()
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .filter(|&m| m > 0x10000 && memory::readable(m, 8))
        .unwrap_or(0);
    if m == 0 { (0, 0) } else { (c, m) }
}

/// Tell our code who the player is now (Fly on or off, a respawn moves the player). Each second and
/// at every Fly switch.
pub fn refresh() {
    // Who the player is is looked up without the lock: finding the player's character can take a
    // moment.
    let (c, m) = if is_on() { player() } else { (0, 0) };
    let Ok(slot) = HOOK.lock() else { return };
    let Some(h) = slot.as_ref() else { return };
    let (c, m) = if h.on { (c, m) } else { (0, 0) };
    // Let Game Fall: the player's falls go through; the in-air part still keeps the player's
    // aiming.
    let fall_m = if let_fall() { 0 } else { m };
    let _ = memory::write(h.fall.block + SLOT_AT, &fall_m.to_le_bytes());
    let _ = memory::write(h.in_air.block + SLOT_AT, &c.to_le_bytes());
}

/// Swap one hook's code word; `false` if it was not what we expected (then it is left alone).
pub(crate) fn swap(h: &Hook, on: bool) -> Result<bool, String> {
    let (want, put) = if on { (h.old, h.new) } else { (h.new, h.old) };
    let was = memory::swap_code_u64(h.word, put)?;
    if was == want {
        return Ok(true);
    }
    let _ = memory::swap_code_u64(h.word, was);
    Ok(false)
}

/// [`swap`] for a jump over more than one of the game's instructions: in with the game's threads
/// stopped and none of them inside the jump's bytes. Out needs no pause: no thread can be inside
/// the jump's bytes then.
pub(crate) fn swap_paused(h: &Hook, on: bool) -> Result<bool, String> {
    let (want, put) = if on { (h.old, h.new) } else { (h.new, h.old) };
    let was = if on { memory::swap_code_u64_paused(h.word, put, h.at + 1..h.at + 5)? } else { memory::swap_code_u64(h.word, put)? };
    if was == want {
        return Ok(true);
    }
    let _ = memory::swap_code_u64(h.word, was);
    Ok(false)
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        ready()?;
    }
    {
        let mut slot = HOOK.lock().map_err(|_| "stand lock")?;
        let Some(h) = slot.as_mut() else { return Ok(()) };
        if h.on == on {
            return Ok(());
        }
        if !on {
            // Leave everyone alone again before the jumps go away.
            let _ = memory::write(h.fall.block + SLOT_AT, &[0; 8]);
            let _ = memory::write(h.in_air.block + SLOT_AT, &[0; 8]);
        }
        if !swap(&h.fall, on)? {
            return Err("fall code word changed; left alone".into());
        }
        if !swap(&h.in_air, on)? {
            let _ = swap(&h.fall, !on);
            return Err("in-air code word changed; left alone".into());
        }
        h.on = on;
    }
    refresh();
    log(&format!("stand while flying {}", if on { "on" } else { "off" }));
    Ok(())
}

/// The hooks are in (only while Fly is on).
fn is_on() -> bool {
    HOOK.lock().ok().and_then(|h| h.as_ref().map(|h| h.on)).unwrap_or(false)
}

/// Off (unload).
pub fn shutdown() {
    let _ = set(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fall_code_counts_her_falls_and_runs_the_game_s_first_instruction() {
        let block = 0x7ff6_0000_1000u64;
        let at = 0x7ff6_1353_d0c0u64;
        let orig = [0x48, 0x89, 0x5C, 0x24, 0x18];
        let c = fall_code(block, at, &orig).unwrap();
        assert_eq!(c.len(), 39);
        assert!(c.len() as u64 <= SLOT_AT, "the code ends before the slot");
        const { assert!(FALL_COUNT + 8 <= BLOCK as u64, "inside the block") };
        let target = |end: usize| {
            let disp = i32::from_le_bytes(c[end - 4..end].try_into().unwrap()) as i64;
            ((block + end as u64) as i64 + disp) as u64
        };
        // Push rax; mov rax, [manager]; test rax, rax
        assert_eq!(&c[0..4], &[0x50, 0x48, 0x8B, 0x05]);
        assert_eq!(target(8), block + SLOT_AT);
        assert_eq!(&c[8..11], &[0x48, 0x85, 0xC0]);
        // Jz and jne both go on to the game (`go:` pops rax first).
        assert_eq!(&c[13..17], &[0x48, 0x39, 0x41, OBJ_MANAGER]);
        for (op, end) in [(0x74u8, 13usize), (0x75, 19)] {
            assert_eq!(c[end - 2], op);
            assert_eq!(end + c[end - 1] as usize, FALL_GO);
        }
        // Own: one more in the count, rax back, return.
        assert_eq!(&c[19..22], &[0x48, 0xFF, 0x05]);
        assert_eq!(target(26), block + FALL_COUNT);
        assert_eq!(&c[26..28], &[0x58, 0xC3]);
        // go: pop rax, the game's first instruction, back after it.
        assert_eq!(c[FALL_GO], 0x58);
        assert_eq!(&c[FALL_GO + 1..FALL_GO + 6], &orig);
        assert_eq!(c[FALL_GO + 6], 0xE9);
        assert_eq!(target(39), at + 5);
        // The pattern starts with that instruction.
        assert!(FALL.starts_with("48 89 5C 24 18"));
    }
}
