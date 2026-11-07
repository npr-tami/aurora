//! No NVG Overlay: NVG on keeps the goggles down on the player's face but without the green screen.
//! When NVG turns on, the game sets a byte in its NVG display (`mov byte [rdi+0x299], 1`,
//! exe+0x214d7a9 in that build); with that byte 0 the screen stays normal. The switch changes the 1
//! in that instruction to a 0, so it works from the next time NVG turns on.
//!
//! Teleport: a teleport with a loading screen sets the display up again and stores that byte from a
//! value it was handed (`mov [r13+0x299], r15b`, exe+0x11bb332; seen live with r13 = the display).
//! While the switch is on, our code stores 0 there.

use std::sync::Mutex;

use crate::{log, memory};

/// `cmp eax, 1; jne; call; mov byte [rdi+0x299], 1; call`: the 1 is `IMM` bytes in.
const SET: &str = "83 F8 01 75 0C E8 ?? ?? ?? ?? C6 87 99 02 00 00 01 E8";
const IMM: u64 = 16;
/// `mov [r13+0x120c], al; mov [r13+0x299], r15b; add rsp, 0x38`: the second is hooked.
const LOAD: &str = "41 88 85 0C 12 00 00 45 88 BD 99 02 00 00 48 83 C4 38";
const LOAD_AT: u64 = 7;
const LOAD_ORIG: [u8; 7] = [0x45, 0x88, 0xBD, 0x99, 0x02, 0x00, 0x00];
const BLOCK: usize = 0x40;

/// One 8-byte code word and what goes in it when the switch is on.
struct Word {
    word: u64,
    old: u64,
    new: u64,
}

struct Patch {
    set: Word,
    load: Word,
    on: bool,
}

static PATCH: Mutex<Option<Patch>> = Mutex::new(None);
static BROKEN: Mutex<Option<String>> = Mutex::new(None);

fn find(exe: &aurora_core::proto::ModuleInfo, pattern: &str, what: &str) -> Result<u64, String> {
    memory::find_code(exe, &format!("nvg {what}"), pattern)
}

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

fn word_at(at: u64) -> Result<(u64, u64, Vec<u8>), String> {
    let word = at & !7;
    let bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(bytes.clone().try_into().unwrap());
    Ok((word, old, bytes))
}

fn prepare() -> Result<Patch, String> {
    let exe = memory::find_module("").ok_or("no exe")?;

    let at = find(&exe, SET, "switch-on")? + IMM;
    let (word, old, mut b) = word_at(at)?;
    let i = (at - word) as usize;
    if b[i] != 1 {
        return Err("nvg: the byte is not what it was (game patched?)".into());
    }
    b[i] = 0;
    let set = Word { word, old, new: u64::from_le_bytes(b.try_into().unwrap()) };
    log(&format!("nvg overlay byte at exe+{:#x}", at - exe.base));

    let at = find(&exe, LOAD, "loading")? + LOAD_AT;
    if memory::read(at, 7)? != LOAD_ORIG {
        return Err("nvg: loading code is not what we expect".into());
    }
    if at % 8 > 3 {
        return Err("nvg: the loading jump would cross a code word".into());
    }
    let block = memory::alloc_code_near(exe.base, BLOCK).ok_or("nvg: no room for the loading code")?;
    // Mov byte [r13+0x299], 0; jmp back
    let mut c = vec![0x41, 0xC6, 0x85, 0x99, 0x02, 0x00, 0x00, 0x00, 0xE9];
    c.extend_from_slice(&rel(block + c.len() as u64 + 4, at + 7)?);
    memory::write(block, &c)?;
    let (word, old, mut b) = word_at(at)?;
    let i = (at - word) as usize;
    b[i] = 0xE9;
    b[i + 1..i + 5].copy_from_slice(&rel(at + 5, block)?);
    let load = Word { word, old, new: u64::from_le_bytes(b.try_into().unwrap()) };
    log(&format!("nvg loading hook at exe+{:#x}, code at {block:#x}", at - exe.base));

    Ok(Patch { set, load, on: false })
}

/// Find the code once (at load, so switching on is instant).
pub fn warm() {
    let _ = ready();
}

fn ready() -> Result<(), String> {
    let mut slot = PATCH.lock().map_err(|_| "nvg lock")?;
    if slot.is_some() {
        return Ok(());
    }
    if let Some(e) = BROKEN.lock().ok().and_then(|b| b.clone()) {
        return Err(e);
    }
    match prepare() {
        Ok(p) => {
            *slot = Some(p);
            Ok(())
        }
        Err(e) => {
            log(&format!("nvg: {e}"));
            if let Ok(mut b) = BROKEN.lock() {
                *b = Some(e.clone());
            }
            Err(e)
        }
    }
}

/// Swap one code word; `false` if it was not what we expected (then it is left alone).
fn swap(w: &Word, on: bool) -> Result<bool, String> {
    let (want, put) = if on { (w.old, w.new) } else { (w.new, w.old) };
    let was = memory::swap_code_u64(w.word, put)?;
    if was == want {
        return Ok(true);
    }
    // Someone else changed this code: put back what was there.
    let _ = memory::swap_code_u64(w.word, was);
    Ok(false)
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        ready()?;
    }
    let mut slot = PATCH.lock().map_err(|_| "nvg lock")?;
    let Some(p) = slot.as_mut() else { return Ok(()) };
    if p.on == on {
        return Ok(());
    }
    if !swap(&p.set, on)? {
        return Err("nvg switch-on code word changed; left alone".into());
    }
    if !swap(&p.load, on)? {
        let _ = swap(&p.set, !on);
        return Err("nvg loading code word changed; left alone".into());
    }
    p.on = on;
    log(&format!("no nvg overlay {}", if on { "on" } else { "off" }));
    Ok(())
}

pub fn is_on() -> bool {
    PATCH.lock().ok().and_then(|p| p.as_ref().map(|p| p.on)).unwrap_or(false)
}

/// Off (unload).
pub fn shutdown() {
    let _ = set(false);
}
