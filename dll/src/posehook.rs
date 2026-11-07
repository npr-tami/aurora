//! Pose object catch without exceptions. The hardware watch on the game's pose function took
//! thousands of hits per catch and the game crashed after one now and then, even with the disarm
//! race fixed. So while bone ESP is on, the function's first instruction (`mov [rsp+8], rbx`, 5
//! bytes) jumps to a few bytes of ours that note r8 (the pose object) in a ring and carry on.
//! Nothing is raised; the ESP reads the ring.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::{log, memory};

/// Entries in the ring (8 bytes each).
pub(crate) const RING: usize = 8192;
/// Where the write index and the ring sit in our code block.
const IDX_AT: u64 = 0x40;
const RING_AT: u64 = 0x80;

struct Hook {
    /// The function, the aligned code word holding its first instruction, and its two versions.
    func: u64,
    word: u64,
    old: u64,
    new: u64,
    cave: u64,
    on: bool,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64)
        .map(|r| r.to_le_bytes())
        .map_err(|_| "jump too far".to_string())
}

fn prepare(func: u64) -> Result<Hook, String> {
    let first = memory::read(func, 5)?;
    if first != [0x48, 0x89, 0x5C, 0x24, 0x08] {
        return Err(format!("pose function starts {first:02x?}, not mov [rsp+8], rbx"));
    }
    let word = func & !7;
    if func + 5 > word + 8 {
        return Err("the first instruction crosses a code word".into());
    }
    let exe = memory::find_module("").ok_or("no exe")?;
    let cave = memory::alloc_code_near(exe.base, RING_AT as usize + RING * 8).ok_or("no room for the pose code")?;
    // Rax and r11 are free at a function's start (not arguments, not kept for the caller); so are
    // the flags.
    let mut code = vec![0xB8, 0x01, 0x00, 0x00, 0x00]; // Mov eax, 1
    code.extend_from_slice(&[0xF0, 0x0F, 0xC1, 0x05]); // Lock xadd [rip+idx], eax
    code.extend_from_slice(&rel(cave + code.len() as u64 + 4, cave + IDX_AT)?);
    code.push(0x25); // And eax, RING - 1
    code.extend_from_slice(&((RING - 1) as u32).to_le_bytes());
    code.extend_from_slice(&[0x4C, 0x8D, 0x1D]); // Lea r11, [rip+ring]
    code.extend_from_slice(&rel(cave + code.len() as u64 + 4, cave + RING_AT)?);
    code.extend_from_slice(&[0x4D, 0x89, 0x04, 0xC3]); // Mov [r11+rax*8], r8
    code.extend_from_slice(&[0x48, 0x89, 0x5C, 0x24, 0x08]); // Mov [rsp+8], rbx (the one replaced)
    code.push(0xE9); // Jmp back
    code.extend_from_slice(&rel(cave + code.len() as u64 + 4, func + 5)?);
    if code.len() as u64 > IDX_AT {
        return Err("pose code too long".into());
    }
    memory::write(cave, &code)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (func - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(func + 5, cave)?);
    let new = u64::from_le_bytes(new_bytes.try_into().unwrap());
    log(&format!("pose hook ready: code at {cave:#x}"));
    Ok(Hook { func, word, old, new, cave, on: false })
}

/// Hook on (`func` is the pose function) or off. Cheap when nothing changes.
pub fn set(func: u64, on: bool) -> Result<(), String> {
    let mut slot = HOOK.lock().map_err(|_| "pose hook lock")?;
    if slot.as_ref().map(|h| h.on).unwrap_or(false) == on {
        return Ok(());
    }
    if slot.as_ref().is_none_or(|h| h.func != func) {
        if !on {
            return Ok(());
        }
        *slot = Some(prepare(func)?);
    }
    let Some(h) = slot.as_mut() else { return Ok(()) };
    let (want, put) = if on { (h.old, h.new) } else { (h.new, h.old) };
    match memory::swap_code_u64(h.word, put) {
        Ok(was) if was == want => {
            h.on = on;
            log(if on { "pose hook on" } else { "pose hook off" });
            Ok(())
        }
        Ok(was) => {
            // Someone else changed this code: put back what was there and forget it.
            let _ = memory::swap_code_u64(h.word, was);
            *slot = None;
            Err(format!("code word was {was:#x}, expected {want:#x}; left alone"))
        }
        Err(e) => Err(e),
    }
}

/// Off, whatever function it was on (bone ESP off, unload).
pub fn off() {
    let func = HOOK.lock().ok().and_then(|h| h.as_ref().map(|h| h.func));
    if let Some(func) = func
        && let Err(e) = set(func, false)
    {
        log(&format!("pose hook: {e}"));
    }
}

/// How many times the game ran the pose function since our code went in: the ring's write count at
/// code+0x40 (it only counts up and wraps at 2^32). A read, for the bone trace's calls per second.
pub fn calls() -> Option<u32> {
    let cave = HOOK.lock().ok().and_then(|h| h.as_ref().filter(|h| h.on).map(|h| h.cave))?;
    let b = memory::read(cave + IDX_AT, 4).ok()?;
    Some(u32::from_le_bytes(b.try_into().ok()?))
}

/// The pose objects noted since the hook went on (the last `RING` calls), each once.
pub fn objects() -> Vec<u64> {
    let Some(cave) = HOOK.lock().ok().and_then(|h| h.as_ref().filter(|h| h.on).map(|h| h.cave)) else {
        return Vec::new();
    };
    let Ok(raw) = memory::read(cave + RING_AT, RING * 8) else { return Vec::new() };
    let mut seen = HashSet::new();
    raw.as_chunks::<8>()
        .0
        .iter()
        .map(|c| u64::from_le_bytes(*c))
        .filter(|&v| v != 0 && seen.insert(v))
        .collect()
}
