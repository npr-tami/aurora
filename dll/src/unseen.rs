//! **What this is, and what it is not.** "Undetected" is only the menu's name for a gameplay switch
//! about the game's enemy NPC AI: whether its computer-controlled soldiers and drones *see* the
//! player and fill their orange alert meter. It is NOT a bypass, NOT anti-cheat evasion, and NOT
//! hiding the mod or the player from anything. It touches no anti-cheat, network, account or
//! logging code: only the NPCs' eyesight and alert meter, a single-player difficulty change
//! (enemies go blind). In the code and docs it is called `unseen` / NPC perception; "Undetected"
//! stays only as the menu label.
//!
//! Undetected: enemy NPCs don't see the player. The game's sight check loads a sight-range factor
//! (`movss xmm15, [rcx+0x3c]`) and multiplies what an NPC sees by it; while on, that load jumps to
//! a few bytes of ours that multiply it by `FACTOR` (0: nothing is seen). It changes what every
//! enemy NPC sees of the player, so it is off until the player turns it on. (Research round 2 says
//! this function works out the player's visibility, 0 to 1, which every enemy sensor then uses;
//! dll\docs\offsets.md 2.11 has both readings.)
//!
//! Close up (1-10 m) a second system fills the orange meter even with sight at 0: each NPC's
//! awareness object keeps it at +0x22c (0..100), and one function adds to it while the NPC is
//! noticing the player. Its `jae` past that fill becomes a plain `jmp` while on, so the meter never
//! rises; draining and the 0..100 clamp still run. That alone did not stop it (something else sets
//! it too), so the same function's last word, the clamp `maxss xmm0, xmm8` (xmm8 is 0 there),
//! becomes `movaps xmm0, xmm8`: each update leaves the meter at 0.
//!
//! With that one held at 0 a twin next to it (+0x228, its own function at exe+0x363a6e0, called
//! right before the first) filled in under 2 s and the player was still spotted. Its last writes
//! cross code words, so its start jumps to a wrapper of ours instead: run the function, then set
//! +0x228 to 0.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{log, memory};

/// The sight-range load and what follows it.
const SIGHT: &str = "F3 44 0F 10 79 3C 48 85 D2 74 17 80 BA D0 00 00 00 00";
/// Sight range is multiplied by this while on.
const FACTOR: f32 = 0.0;
/// The meter fill's check (`movss xmm0, [rcx+22c]; comiss xmm0, [rcx+250]; jae`) and what follows;
/// the `jae` is at +15.
const FILL: &str = "F3 0F 10 81 2C 02 00 00 0F 2F 81 50 02 00 00 0F 83 ?? ?? ?? ?? 48 8B 81 08 02 00 00";

struct Patch {
    /// The aligned code word holding the jump, and its two versions.
    word: u64,
    old: u64,
    new: u64,
    on: bool,
}

/// The sight load, then the meter fill. The meter clamp (`movss xmm0, [rbx+22c]; maxss xmm0, xmm8;
/// minss; movss [rbx+22c], xmm0`); the `maxss` is at +8.
const CLAMP: &str = "F3 0F 10 83 2C 02 00 00 F3 41 0F 5F C0 F3 0F 5D 05 ?? ?? ?? ?? F3 0F 11 83 2C 02 00 00";

/// The second meter's function start (`push rbx; sub rsp, 0xa0`, 9 bytes) and what follows.
const SECOND: &str = "40 53 48 81 EC A0 00 00 00 0F 29 B4 24 90 00 00 00 44 0F 29 44 24 70 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 44 24 60 48 8B 81 08 02 00 00 0F 28 F1 44 8B 81 20 02 00 00";

static PATCH: Mutex<Option<[Patch; 4]>> = Mutex::new(None);
static BAD: AtomicBool = AtomicBool::new(false);
/// The code search runs once: the warm-up and an early switch wait for the same one.
static PREP: Mutex<()> = Mutex::new(());

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64)
        .map(|r| r.to_le_bytes())
        .map_err(|_| "jump too far".to_string())
}

/// The meter fill's `jae rel32` (0F 83) becomes `nop; jmp rel32` to the same place.
fn prepare_fill(exe: &aurora_core::proto::ModuleInfo) -> Result<Patch, String> {
    let at = memory::find_code(exe, "unseen meter fill", FILL)? + 15;
    let word = at & !7;
    if at + 6 > word + 8 {
        return Err("the meter check crosses a code word".into());
    }
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let i = (at - word) as usize;
    if old_bytes[i..i + 2] != [0x0F, 0x83] {
        return Err(format!("meter check is {:02x?}, not jae", &old_bytes[i..i + 2]));
    }
    // Same end, so the same rel32.
    let mut new_bytes = old_bytes;
    new_bytes[i] = 0x90;
    new_bytes[i + 1] = 0xE9;
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    log(&format!("unseen ready: meter fill exe+{:#x}", at - exe.base));
    Ok(Patch { word, old, new, on: false })
}

/// The meter clamp's `maxss xmm0, xmm8` becomes `movaps xmm0, xmm8; nop`.
fn prepare_clamp(exe: &aurora_core::proto::ModuleInfo) -> Result<Patch, String> {
    let at = memory::find_code(exe, "unseen meter clamp", CLAMP)? + 8;
    let word = at & !7;
    if at + 5 > word + 8 {
        return Err("the meter clamp crosses a code word".into());
    }
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let i = (at - word) as usize;
    let mut new_bytes = old_bytes;
    new_bytes[i..i + 5].copy_from_slice(&[0x41, 0x0F, 0x28, 0xC0, 0x90]);
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    log(&format!("unseen ready: meter clamp exe+{:#x}", at - exe.base));
    Ok(Patch { word, old, new, on: false })
}

/// The second meter's function start jumps to: call it (through its first 9 bytes, then the rest),
/// then `mov dword [obj+0x228], 0`, return. Rbx keeps the object (and is put back).
fn prepare_second(exe: &aurora_core::proto::ModuleInfo) -> Result<Patch, String> {
    let func = memory::find_code(exe, "unseen second meter", SECOND)?;
    let word = func & !7;
    if func + 5 > word + 8 {
        return Err("the second meter start crosses a code word".into());
    }
    let cave = memory::alloc_code_near(exe.base, 64).ok_or("no room for the second meter code")?;
    let mut code = vec![
        0x53, // Push rbx
        0x48, 0x83, 0xEC, 0x20, // Sub rsp, 0x20 (shadow space; the stack stays 16-aligned)
        0x48, 0x8B, 0xD9, // Mov rbx, rcx
        0xE8, // Call tramp
    ];
    let call_at = code.len() as u64;
    code.extend_from_slice(&[0; 4]);
    code.extend_from_slice(&[
        0xC7, 0x83, 0x28, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // Mov dword [rbx+0x228], 0
        0x48, 0x83, 0xC4, 0x20, // Add rsp, 0x20
        0x5B, // Pop rbx
        0xC3, // ret
    ]);
    // Tramp: the function's first 9 bytes, then on into it.
    let tramp = cave + code.len() as u64;
    let r = rel(cave + call_at + 4, tramp)?;
    code[call_at as usize..call_at as usize + 4].copy_from_slice(&r);
    code.extend_from_slice(&memory::read(func, 9)?);
    code.push(0xE9);
    code.extend_from_slice(&rel(cave + code.len() as u64 + 4, func + 9)?);
    memory::write(cave, &code)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (func - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(func + 5, cave)?);
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    log(&format!("unseen ready: second meter exe+{:#x}, code at {cave:#x}", func - exe.base));
    Ok(Patch { word, old, new, on: false })
}

fn prepare() -> Result<[Patch; 4], String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let at = memory::find_code(&exe, "unseen sight", SIGHT)?;
    // The 6-byte load is replaced by a 5-byte jump; its last byte is never run.
    let back = at + 6;
    let word = at & !7;
    if at + 5 > word + 8 {
        return Err("the sight load crosses a code word".into());
    }
    let cave = memory::alloc_code_near(exe.base, 64).ok_or("no room for the sight code")?;
    let mut code = vec![
        0xF3, 0x44, 0x0F, 0x10, 0x79, 0x3C, // Movss xmm15, [rcx+0x3c]
        0xF3, 0x44, 0x0F, 0x59, 0x3D, 0x05, 0x00, 0x00, 0x00, // Mulss xmm15, [rip+5] (FACTOR)
        0xE9, // Jmp back
    ];
    code.extend_from_slice(&rel(cave + code.len() as u64 + 4, back)?);
    code.extend_from_slice(&FACTOR.to_le_bytes());
    memory::write(cave, &code)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (at - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(at + 5, cave)?);
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    log(&format!("unseen ready: sight check exe+{:#x}, code at {cave:#x}", at - exe.base));
    Ok([Patch { word, old, new, on: false }, prepare_fill(&exe)?, prepare_clamp(&exe)?, prepare_second(&exe)?])
}

/// Find the code and write ours, all still off, so the first switch-on is instant. Run once at
/// load. The search runs without the lock, so the menu (which reads the switch every frame) never
/// waits on it.
pub fn warm() {
    let _ = ensure();
}

/// The patches ready, searching for them first if no one has (see `PREP`).
fn ensure() -> Result<(), String> {
    let _one = PREP.lock().map_err(|_| "unseen lock")?;
    if PATCH.lock().map_err(|_| "unseen lock")?.is_some() {
        return Ok(());
    }
    if BAD.load(Ordering::Relaxed) {
        return Err("unseen is off for this session (see the log)".into());
    }
    match prepare() {
        Ok(p) => {
            *PATCH.lock().map_err(|_| "unseen lock")? = Some(p);
            Ok(())
        }
        Err(e) => {
            log(&format!("unseen: {e}"));
            BAD.store(true, Ordering::SeqCst);
            Err(e)
        }
    }
}

pub fn is_on() -> bool {
    PATCH.lock().ok().and_then(|p| p.as_ref().map(|p| p.iter().any(|p| p.on))).unwrap_or(false)
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        ensure()?;
    }
    let mut slot = PATCH.lock().map_err(|_| "unseen lock")?;
    // Off is always allowed: after a failure, whatever is still on must be able to go off.
    if on && BAD.load(Ordering::Relaxed) {
        return Err("unseen is off for this session (see the log)".into());
    }
    let Some(ps) = slot.as_mut() else {
        return if on { Err("unseen not ready".into()) } else { Ok(()) };
    };
    if ps.iter().all(|p| p.on == on) {
        return Ok(());
    }
    let mut switched = Vec::new();
    let mut failed = None;
    for (i, p) in ps.iter_mut().enumerate() {
        if p.on == on {
            continue;
        }
        let (want, put) = if on { (p.old, p.new) } else { (p.new, p.old) };
        match memory::swap_code_u64(p.word, put) {
            Ok(was) if was == want => {
                p.on = on;
                switched.push(i);
            }
            Ok(was) => {
                // Someone else changed this code: put back what was there and stop.
                let _ = memory::swap_code_u64(p.word, was);
                failed = Some(format!("code word was {was:#x}, expected {want:#x}; left alone"));
                break;
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if let Some(e) = failed {
        BAD.store(true, Ordering::SeqCst);
        // Turning on half-way leaves NPCs half-blind: undo the ones this call switched on.
        if on {
            for i in switched {
                let p = &mut ps[i];
                match memory::swap_code_u64(p.word, p.old) {
                    Ok(was) if was == p.new => p.on = false,
                    // Not ours any more: put back what was there, like the forward path.
                    Ok(was) => {
                        let _ = memory::swap_code_u64(p.word, was);
                    }
                    Err(_) => {}
                }
            }
        }
        log(&format!("unseen: {e}"));
        return Err(e);
    }
    log(if on { "unseen on" } else { "unseen off" });
    Ok(())
}
