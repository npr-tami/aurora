//! **Draft,**. Menu > Experiments > "Ignore only me (untested)". Same kind
//! of switch as Undetected (unseen.rs): the game's enemy NPC AI only, a single-player difficulty
//! change. It touches no anti-cheat, network or account code.
//!
//! Every enemy NPC's perception goes through one function, its inbox (sub_29cb140 in Steam build
//! 25349072): sight, the close-range sense, noise and the rest
//! (research\docs\perception.md, rounds 2 and 3). Each record says who caused it. While
//! on, records caused by the player's are dropped there, the way the game drops every record for an
//! NPC whose perception is off. Records about anyone else pass as usual.
//!
//! How: the inbox's first two instructions (`sub rsp,28h; test byte [rcx+2C0h],10h`, 11 bytes) run
//! in our code instead, then:
//! - perception off (the test gave ZF=1): straight back; the game skips as always;
//! - the record's type (u32 at +8) not set in `mask`: back with the test redone (ZF=0);
//! - its source (`[[rec]]` when bit 31 of `[[rec]+0xc]` is set: the game's own check right after)
//!   equals the player's body (`[globals::BODY_ROOT]`, read on every call, so a respawn is fine):
//!   back with ZF=1, so the game's next `je` takes its own skip path (`add rsp,28h; ret`);
//!   otherwise back with ZF=0.
//!
//! The jump covers two instructions, so it goes in only while every other thread is stopped and
//! none sits between them (`memory::swap_code_u64_paused`).
//!
//! Not known yet (the noise recording, grbresearch-dll's noise.request, answers both):
//! - that a record's source entity is the same pointer as the player's body (if not, nothing is
//!   dropped: safe, but the switch does nothing);
//! - which types to drop. Until then the mask holds every type.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{globals, log, memory};

/// The inbox's start: `sub rsp,28h; test byte [rcx+2C0h],10h; mov r9,rdx; mov r10,rcx`. Unique in
/// the Vulkan and the DX11 exe.
const INBOX: &str = "48 83 EC 28 F6 81 C0 02 00 00 10 4C 8B CA 4C 8B D1";
/// The two instructions our code runs instead.
const MOVED: [u8; 11] = [0x48, 0x83, 0xEC, 0x28, 0xF6, 0x81, 0xC0, 0x02, 0x00, 0x00, 0x10];
/// Where the type mask (256 bits) sits in our block.
const MASK_AT: u64 = 0x80;
const BLOCK: usize = 0xA0;

struct Hook {
    word: u64,
    old: u64,
    new: u64,
    at: u64,
    on: bool,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static BAD: AtomicBool = AtomicBool::new(false);

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

/// Our code at `cave` for the inbox at `at`. `mask` is the type mask's address, `body` the address
/// that holds the player's body.
fn cave_code(cave: u64, at: u64, mask: u64, body: u64) -> Result<Vec<u8>, String> {
    let mut c = MOVED.to_vec();
    c.extend_from_slice(&[0x74, 0]); // je BACK (perception off)
    let je_back = c.len() - 1;
    c.extend_from_slice(&[
        0x50, // Push rax
        0x51, // Push rcx
        0x8B, 0x4A, 0x08, // Mov ecx, [rdx+8]: the type
        0x81, 0xF9, 0xFF, 0x00, 0x00, 0x00, // Cmp ecx, 0xff
        0x77, 0, // ja PASS
    ]);
    let ja_pass = c.len() - 1;
    c.extend_from_slice(&[0x48, 0xB8]); // Mov rax, mask
    c.extend_from_slice(&mask.to_le_bytes());
    c.extend_from_slice(&[0x0F, 0xA3, 0x08, 0x73, 0]); // bt [rax], ecx; jnc PASS
    let jnc_pass = c.len() - 1;
    c.extend_from_slice(&[
        0x48, 0x8B, 0x02, // Mov rax, [rdx]: the source handle
        0x48, 0x63, 0x48, 0x0C, // Movsxd rcx, dword [rax+0xc]
        0x48, 0xC1, 0xF9, 0x3F, // Sar rcx, 63: all ones when bit 31 is set
        0x48, 0x23, 0x08, // And rcx, [rax]: the entity, or 0
        0x74, 0, // jz PASS
    ]);
    let jz_pass = c.len() - 1;
    c.extend_from_slice(&[0x48, 0xB8]); // Mov rax, body
    c.extend_from_slice(&body.to_le_bytes());
    c.extend_from_slice(&[0x48, 0x3B, 0x08, 0x75, 0]); // Cmp rcx, [rax]; jne PASS
    let jne_pass = c.len() - 1;
    c.extend_from_slice(&[
        0x59, // Pop rcx
        0x58, // Pop rax
        0x48, 0x39, 0xE4, // Cmp rsp, rsp: ZF=1, the game skips this record
        0xE9, // Jmp at+11
    ]);
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, at + MOVED.len() as u64)?);
    let pass = c.len();
    c.extend_from_slice(&[0x59, 0x58]); // Pop rcx; pop rax
    c.extend_from_slice(&MOVED[4..]); // Test byte [rcx+2C0h], 10h again: ZF=0
    let back = c.len();
    c.push(0xE9); // Jmp at+11
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, at + MOVED.len() as u64)?);
    for (from, to) in [(je_back, back), (ja_pass, pass), (jnc_pass, pass), (jz_pass, pass), (jne_pass, pass)] {
        c[from] = i8::try_from(to as i64 - (from as i64 + 1)).map_err(|_| "short jump too far")? as u8;
    }
    if c.len() as u64 > MASK_AT {
        return Err("inbox code too long".into());
    }
    Ok(c)
}

fn prepare() -> Result<Hook, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let body = exe.base + globals::BODY_ROOT.rva().ok_or("the player's body's place was not found")?;
    let at = memory::find_code(&exe, "inbox", INBOX)?;
    if memory::read(at, MOVED.len())? != MOVED {
        return Err("the inbox start is not as expected".into());
    }
    let word = at & !7;
    if at + 5 > word + 8 {
        return Err("the inbox start crosses a code word".into());
    }
    let cave = memory::alloc_code_near(exe.base, BLOCK).ok_or("no room for the inbox code")?;
    memory::write(cave + MASK_AT, &[0xFF; 32])?;
    memory::write(cave, &cave_code(cave, at, cave + MASK_AT, body)?)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (at - word) as usize;
    new_bytes[i] = 0xE9;
    new_bytes[i + 1..i + 5].copy_from_slice(&rel(at + 5, cave)?);
    log(&format!("inbox ready: exe+{:#x}, code at {cave:#x}", at - exe.base));
    Ok(Hook { word, old, new: u64::from_le_bytes(new_bytes.try_into().unwrap()), at, on: false })
}

pub fn is_on() -> bool {
    HOOK.lock().ok().and_then(|h| h.as_ref().map(|h| h.on)).unwrap_or(false)
}

pub fn set(on: bool) -> Result<(), String> {
    let mut slot = HOOK.lock().map_err(|_| "inbox lock")?;
    if on && slot.is_none() {
        if BAD.load(Ordering::Relaxed) {
            return Err("ignore only me is off for this session (see the log)".into());
        }
        match prepare() {
            Ok(h) => *slot = Some(h),
            Err(e) => {
                log(&format!("inbox: {e}"));
                BAD.store(true, Ordering::SeqCst);
                return Err(e);
            }
        }
    }
    let Some(h) = slot.as_mut() else { return Ok(()) };
    if h.on == on {
        return Ok(());
    }
    let (want, put) = if on { (h.old, h.new) } else { (h.new, h.old) };
    // In: no thread may sit at the second instruction (at+4). Out: none can be there.
    let was = if on { memory::swap_code_u64_paused(h.word, put, h.at + 1..h.at + 5) } else { memory::swap_code_u64(h.word, put) };
    match was {
        Ok(was) if was == want => {
            h.on = on;
            log(if on { "inbox on (ignore only me)" } else { "inbox off" });
            Ok(())
        }
        Ok(was) => {
            // Someone else changed this code: put back what was there and stop.
            let _ = memory::swap_code_u64(h.word, was);
            BAD.store(true, Ordering::SeqCst);
            let e = format!("inbox code word was {was:#x}, expected {want:#x}; left alone");
            log(&format!("inbox: {e}"));
            Err(e)
        }
        Err(e) => {
            log(&format!("inbox: {e}"));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_jumps_land_on_labels() {
        let (cave, at) = (0x1_4000_0000u64, 0x1_429c_b140u64);
        let c = cave_code(cave, at, cave + MASK_AT, 0x1_463e_beb0).unwrap();
        assert!(c.starts_with(&MOVED));
        // je BACK at 11: lands on the last jmp (E9 + 4 bytes).
        let back = c.len() - 5;
        assert_eq!(c[11], 0x74);
        assert_eq!(13 + c[12] as usize, back);
        assert_eq!(c[back], 0xE9);
        // The pass path is pop rcx; pop rax; then the test again.
        let pass = back - 2 - 7;
        assert_eq!(&c[pass..pass + 2], &[0x59, 0x58]);
        assert_eq!(&c[pass + 2..back], &MOVED[4..]);
        // The skip path ends `cmp rsp, rsp; jmp` right before it; both jmps go to at+11.
        assert_eq!(&c[pass - 8..pass - 5], &[0x48, 0x39, 0xE4]);
        for j in [pass - 5, back] {
            assert_eq!(c[j], 0xE9);
            let r = i32::from_le_bytes(c[j + 1..j + 5].try_into().unwrap()) as i64;
            assert_eq!((cave + j as u64 + 5).wrapping_add_signed(r), at + 11);
        }
        // Every short jump lands on PASS or BACK.
        for (j, op) in [(11, 0x74), (24, 0x77), (39, 0x73), (55, 0x74), (70, 0x75)] {
            assert_eq!(c[j], op, "opcode at {j}");
            let to = j + 2 + c[j + 1] as usize;
            assert!(to == pass || to == back, "jump at {j} lands at {to}");
        }
    }

    #[test]
    fn cave_runs_and_decides() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
        // A fake "game": at+11 is `setz al; movzx eax, al; add rsp, 28h; ret`, so the call returns
        // 1 when the record was dropped (ZF=1) and 0 when it passed.
        // SAFETY: fresh executable memory for this test only.
        let mem = unsafe { VirtualAlloc(std::ptr::null(), 0x1000, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) } as u64;
        assert_ne!(mem, 0);
        let at = mem;
        let cave = mem + 0x100;
        let tail = [0x0F, 0x94, 0xC0, 0x0F, 0xB6, 0xC0, 0x48, 0x83, 0xC4, 0x28, 0xC3];
        let mut body_slot = Box::new(0u64);
        let code = cave_code(cave, at, cave + MASK_AT, &*body_slot as *const u64 as u64).unwrap();
        // SAFETY: writing our own fresh block.
        unsafe {
            std::ptr::copy_nonoverlapping([0xE9u8].as_ptr(), at as *mut u8, 1);
            std::ptr::copy_nonoverlapping(rel(at + 5, cave).unwrap().as_ptr(), (at + 1) as *mut u8, 4);
            std::ptr::copy_nonoverlapping(tail.as_ptr(), (at + 11) as *mut u8, tail.len());
            std::ptr::copy_nonoverlapping(code.as_ptr(), cave as *mut u8, code.len());
            std::ptr::write_bytes((cave + MASK_AT) as *mut u8, 0xFF, 32);
        }
        // SAFETY: the fake function above follows the win64 call shape (rcx, rdx).
        let f: extern "win64" fn(u64, u64) -> u64 = unsafe { std::mem::transmute(at) };
        let mut npc = vec![0u8; 0x300];
        let body = 0x1234_5678u64;
        // Handle object: [0] = entity, [0xc] = flags with bit 31 set.
        let mut handle = [0u64; 2];
        handle[0] = body;
        handle[1] = 0x8000_0000u64 << 32;
        let rec = [&handle as *const _ as u64, 0x38u64];
        let run = |npc: &[u8]| f(npc.as_ptr() as u64, rec.as_ptr() as u64);
        // Perception off: passes through (the game skips it itself: ZF=1).
        assert_eq!(run(&npc), 1);
        npc[0x2c0] = 0x10;
        // The player's body not set yet: someone else's record, passes.
        assert_eq!(run(&npc), 0);
        // SAFETY: our own box; volatile so the write is kept (only our generated code reads it).
        unsafe { std::ptr::write_volatile(&mut *body_slot, body) };
        // Own: dropped.
        assert_eq!(run(&npc), 1);
        // Type not in the mask: passes.
        // SAFETY: our block.
        unsafe { std::ptr::write_bytes((cave + MASK_AT) as *mut u8, 0, 32) };
        assert_eq!(run(&npc), 0);
        unsafe { std::ptr::write_bytes((cave + MASK_AT) as *mut u8, 0xFF, 32) };
        // Handle not live (bit 31 clear): passes.
        // SAFETY: as above.
        unsafe { std::ptr::write_volatile(&mut handle[1], 0) };
        assert_eq!(run(&npc), 0);
        drop(body_slot);
    }
}
