//! No Recoil, No Spread, Infinite Mag (was Unlimited Ammo), No Screen Shake, No Zoomed Sway,
//! Infinite Stamina and the damage multiplier. The game has one function that kicks a character's
//! aim up after a shot, one that spreads its shots and one that asks whether a gun may fire without
//! using a round; for the player's own character the first two are skipped (return at once) and the
//! third answers yes, everyone else is left alone. Each function's start jumps to a few bytes of
//! ours that compare with the player's body or the player's network part (refreshed every second by
//! `tick`) and either return or run the original start. Infinite Reserve: the reload's call that
//! loads the mag is sent to our code instead; for the player's it makes the reserve come out as
//! `RESERVE_FULL` after every reload. Fire Rate: the wait between shots is 60 / the gun's RPM,
//! worked out by one function each shot; its call in the countdown is sent to our code, which
//! shortens the wait for the player's guns only (the gun item's +0x88 is its owner's network part).

use std::sync::Mutex;

use crate::{log, memory};

/// Recoil: starts `mov rax, rsp; push rbp; push r13` (6 bytes); the character is at [rcx+8].
const RECOIL: &str = "48 8B C4 55 41 55 48 81 EC ?? ?? ?? ?? 49 83 78 ?? 00 4D";
/// Spread: starts `push rbp; push rsi; push r14` (5 bytes); the character is in rax.
const SPREAD: &str = "40 55 56 41 56 48 8D AC 24 ?? ?? FF FF 48 81 EC ?? ?? ?? ?? 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 ?? ?? ?? ?? 80 7A";
/// Ammo: starts `mov [rsp+x], rbx` (5 bytes); the gun is rcx, its owner's network part at
/// [rcx+0x88]. Returning 1 means the shot uses no round.
const AMMO: &str = "48 89 5C 24 ?? 57 48 83 EC ?? 8B DA 48 8B F9 E8 ?? ?? ?? ?? 83 78 ?? 02 0F 84";
/// The player's network part in the player's character object (the table says this moves with game
/// updates; a wrong one only means the ammo check never matches).
const NET: u64 = 0x190;
/// In the weapon item: its owner's network part (Infinite Mag's `cmp [rcx+0x88]`).
const ITEM_NET: u64 = 0x88;
/// Screen shake: starts `mov rax, rsp; push r12; push r13` (7 bytes); the character is at [rcx+8].
const SHAKE: &str = "48 8B C4 41 54 41 55 48 81 EC ?? ?? ?? ?? 4D 8B 60 ?? 4C 8B E9 4D 85 E4 0F 84";
/// Zoomed sway: starts `mov [rsp+x], rbx` (5 bytes); the character is at [rcx+8].
const SWAY: &str = "48 89 5C 24 ?? 57 48 83 EC ?? 49 83 78 ?? 00 49 8B F8 48 8B D9 0F 84 ?? ?? ?? ?? 49 83 78 ?? 00 0F 84 ?? ?? ?? ?? 48";
/// Stamina: `cmp byte [rdi+0x1F1], 0` (7 bytes) in the stamina update, rdi the player's stamina
/// part, [rdi+8] its character. The game skips using stamina while that byte is set.
const STAMINA: &str = "80 BF F1 01 00 00 00 75 ?? F3 0F 10 8F ?? ?? ?? ?? 0F 57 C0 0F 2F C8 76";
/// Damage: `cmp dword [rdi+0xA8], 6` (7 bytes) right after the game works out a hit's damage
/// (xmm0); r14 is the shooter's body. Only the player's hits are multiplied.
const DAMAGE: &str = "83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8";
/// Reserve: in the reload, bx is the reserve, si the rounds taken from it and rbp the gun
/// reloading; then `call` (loads the mag, returns ax), `sub bx, si`, `lea rcx, [rsp+0x20]`, `add
/// ax, bx`: the new reserve is reserve - si + ax. The call is `RESERVE_CALL` bytes in.
const RESERVE: &str = "0F B7 F3 66 45 85 F6 74 09 66 44 3B F3 66 41 0F 46 F6 0F B7 D6 44 8B C7 48 8B CD E8 ?? ?? ?? ?? 66 2B DE 48 8D 4C 24 20 66 03 C3 0F B7 D0";
const RESERVE_CALL: u64 = 27;
/// What the player's reserve is after each reload with Infinite Reserve on.
const RESERVE_FULL: u16 = 500;
/// Fire rate: the shot countdown is set by `push rbx; sub rsp, 0x20; mov rbx, rcx; mov rcx,
/// [rcx+0x10]; mov rcx, [rcx+0x48]` (the gun), `call` (the wait in seconds: 60 / RPM, in xmm0),
/// `mov rax, [rbx+0x10]; addss xmm0, [rax+0x38]; movss [rbx+0x18], xmm0`. The call is `FIRE_CALL`
/// bytes in.
const FIRE: &str = "40 53 48 83 EC 20 48 8B D9 48 8B 49 10 48 8B 49 48 E8 ?? ?? ?? ?? 48 8B 43 10 F3 0F 58 40 38 F3 0F 11 43 18";
const FIRE_CALL: u64 = 17;
const N: usize = 9;
const NAMES: [&str; N] =
    ["no recoil", "no spread", "infinite mag", "no screen shake", "no scoped sway", "infinite stamina", "damage multiplier", "infinite reserve", "fire rate"];
/// Our block: the bits of code, then the player's body, the player's network part and the player's
/// stamina part.
const CODE_AT: [u64; N] = [0, 0x60, 0xC0, 0x120, 0x180, 0x1E0, 0x280, 0x300, 0x380];
const BODY_AT: u64 = 0x260;
const NET_AT: u64 = 0x268;
const SEEN_AT: u64 = 0x270;
/// The damage factor (f32) the player's hits are multiplied by.
const FACTOR_AT: u64 = 0x278;
/// What the player's guns' wait between shots is multiplied by (f32, 1 / the fire rate).
const FIRE_AT: u64 = 0x27C;
/// The shortest wait between the player's shots, in seconds (f32), whatever the fire rate: Fire
/// Rate goes to 1000 now, and 1000 times a gun's own wait is well under a millisecond, many shots
/// in one frame. Held to 2 ms (500 shots a second), so a big value can't empty a mag in one frame
/// or flood the game with shots.
const FIRE_MIN_AT: u64 = 0x3F8;
const FIRE_MIN_S: f32 = 0.002;
const BLOCK: usize = 0x400;

/// A 5-byte jump at a function's start, swapped in and out as one aligned code word.
struct Jump {
    word: u64,
    old: u64,
    new: u64,
    on: bool,
}

struct Hooks {
    block: u64,
    jumps: [Jump; N],
}

static HOOKS: Mutex<Option<Hooks>> = Mutex::new(None);
static BROKEN: Mutex<Option<String>> = Mutex::new(None);
/// The code search runs once: the warm-up and a switch turned on before it ends wait for the same
/// one.
static PREP: Mutex<()> = Mutex::new(());

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64)
        .map(|r| r.to_le_bytes())
        .map_err(|_| "jump too far".to_string())
}

fn find(exe: &aurora_core::proto::ModuleInfo, pattern: &str, name: &str) -> Result<u64, String> {
    memory::find_code(exe, &format!("weapon {name}"), pattern)
}

/// The code that answers for the player: rax (or rdx) holds the value at `slot`; `check` compares
/// it, and on a match `answer` runs and the function returns. Otherwise its first `start.len` bytes
/// run and it carries on.
fn skip_code(at: u64, slot: u64, func: u64, check: &[u8], answer: &[u8], start: &[u8], rdx: bool) -> Result<Vec<u8>, String> {
    let (push, pop) = if rdx { (0x52, 0x5A) } else { (0x50, 0x58) };
    // Push; mov reg, [rip+slot]; test reg, reg; jz not_hers
    let mut c = vec![push, 0x48, 0x8B, if rdx { 0x15 } else { 0x05 }];
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, slot)?);
    c.extend_from_slice(if rdx { &[0x48, 0x85, 0xD2] } else { &[0x48, 0x85, 0xC0] });
    c.extend_from_slice(&[0x74, (check.len() + 2 + 1 + answer.len() + 1) as u8]);
    c.extend_from_slice(check);
    c.extend_from_slice(&[0x75, (1 + answer.len() + 1) as u8]); // Jne not_hers
    c.push(pop); // own
    c.extend_from_slice(answer);
    c.push(0xC3); // ret
    c.push(pop); // not_hers
    c.extend_from_slice(start);
    c.push(0xE9);
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, func + start.len() as u64)?);
    Ok(c)
}

/// For the player's stamina part: note it and set its "free stamina" byte, then the replaced
/// compare.
fn stamina_code(at: u64, body: u64, seen: u64, site: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x50, 0x48, 0x8B, 0x05]; // Push rax; mov rax, [rip+body]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, body)?);
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, 0x14]); // Test rax, rax; jz skip
    c.extend_from_slice(&[0x48, 0x39, 0x47, 0x08, 0x75, 0x0E]); // Cmp [rdi+8], rax; jne skip
    c.extend_from_slice(&[0x48, 0x89, 0x3D]); // Mov [rip+seen], rdi
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, seen)?);
    c.extend_from_slice(&[0xC6, 0x87, 0xF1, 0x01, 0x00, 0x00, 0x01]); // Mov byte [rdi+0x1F1], 1
    c.push(0x58); // Skip: pop rax
    c.extend_from_slice(&memory::read(site, 7)?);
    c.push(0xE9);
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, site + 7)?);
    Ok(c)
}

/// For the player's hits: xmm0 (the damage) times the factor, then the replaced compare.
fn damage_code(at: u64, body: u64, factor: u64, site: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x50, 0x48, 0x8B, 0x05]; // Push rax; mov rax, [rip+body]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, body)?);
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, 0x0D]); // Test rax, rax; jz skip
    c.extend_from_slice(&[0x4C, 0x39, 0xF0, 0x75, 0x08]); // Cmp rax, r14; jne skip
    c.extend_from_slice(&[0xF3, 0x0F, 0x59, 0x05]); // Mulss xmm0, [rip+factor]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, factor)?);
    c.push(0x58); // Skip: pop rax
    c.extend_from_slice(&memory::read(site, 7)?);
    c.push(0xE9);
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, site + 7)?);
    Ok(c)
}

/// For the player's reload: the mag loads as before, then ax (what goes back to the reserve) is
/// made `RESERVE_FULL` - bx + si, so `sub bx, si` and `add ax, bx` give `RESERVE_FULL`. Anyone else
/// goes straight on to the game's own function with nothing changed. The player's check is Infinite
/// Mag's: rbp is the weapon item (vtable exe+0x4830490), not the player's character, and its +0x88
/// is its owner's network part. For the player the function is called from here, with a frame of
/// our own (home space and the player's stack slots 5 to 7 copied, should it take more than the
/// four it gets in registers), and `ret` goes back on the game's own return address, so the stack
/// stays as the game made it.
fn reserve_code(at: u64, net: u64, func: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x50, 0x48, 0x8B, 0x05]; // Push rax; mov rax, [rip+net]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, net)?);
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, 0x40]); // Test rax, rax; jz not_hers
    c.extend_from_slice(&[0x48, 0x39, 0x85]); // Cmp [rbp+ITEM_NET], rax
    c.extend_from_slice(&(ITEM_NET as u32).to_le_bytes());
    c.extend_from_slice(&[0x75, 0x37]); // Jne not_hers
    c.push(0x58); // Own: pop rax
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x38]); // Sub rsp, 0x38
    for (from, to) in [(0x60, 0x20), (0x68, 0x28), (0x70, 0x30)] {
        c.extend_from_slice(&[0x48, 0x8B, 0x44, 0x24, from]); // Mov rax, [rsp+from]
        c.extend_from_slice(&[0x48, 0x89, 0x44, 0x24, to]); // Mov [rsp+to], rax
    }
    c.push(0xE8); // Call func
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, func)?);
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x38]); // Add rsp, 0x38
    c.extend_from_slice(&[0x66, 0xB8]); // Mov ax, RESERVE_FULL
    c.extend_from_slice(&RESERVE_FULL.to_le_bytes());
    c.extend_from_slice(&[0x66, 0x2B, 0xC3]); // Sub ax, bx
    c.extend_from_slice(&[0x66, 0x03, 0xC6]); // Add ax, si
    c.push(0xC3); // ret
    c.push(0x58); // not_hers: pop rax
    c.push(0xE9); // Jmp func
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, func)?);
    Ok(c)
}

/// The gun's wait between shots, as the game's own function gives it, then for the player's guns
/// times the factor at `factor`. Rcx is the gun; its +0xc8 is a handle whose first word is the gun
/// item when the handle's +0xc sign bit is set (the game's own function reads it so), and the
/// item's +0x88 is its owner's network part (`ITEM_NET`). Matched live: the player's three guns'
/// handles led to the player's items, the others' to someone else's. For the player's guns the wait
/// is then held to at least the f32 at `least`.
fn fire_code(at: u64, net: u64, factor: u64, least: u64, func: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![0x53]; // Push rbx
    c.extend_from_slice(&[0x48, 0x83, 0xEC, 0x20]); // Sub rsp, 0x20
    c.extend_from_slice(&[0x48, 0x89, 0xCB]); // Mov rbx, rcx
    c.push(0xE8); // Call func
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, func)?);
    c.extend_from_slice(&[0x48, 0x8B, 0x05]); // Mov rax, [rip+net]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, net)?);
    // The short jumps to `done` grew by 8 for the maxss: 43, 31, 25, 17, 8 before.
    c.extend_from_slice(&[0x48, 0x85, 0xC0, 0x74, 51]); // Test rax, rax; jz done
    c.extend_from_slice(&[0x48, 0x8B, 0x8B, 0xC8, 0x00, 0x00, 0x00]); // Mov rcx, [rbx+0xc8]
    c.extend_from_slice(&[0x48, 0x85, 0xC9, 0x74, 39]); // Test rcx, rcx; jz done
    c.extend_from_slice(&[0x83, 0x79, 0x0C, 0x00, 0x7D, 33]); // Cmp dword [rcx+0xc], 0; jge done
    c.extend_from_slice(&[0x48, 0x8B, 0x09]); // Mov rcx, [rcx]
    c.extend_from_slice(&[0x48, 0x85, 0xC9, 0x74, 25]); // Test rcx, rcx; jz done
    c.extend_from_slice(&[0x48, 0x39, 0x81]); // Cmp [rcx+ITEM_NET], rax
    c.extend_from_slice(&(ITEM_NET as u32).to_le_bytes());
    c.extend_from_slice(&[0x75, 16]); // Jne done
    c.extend_from_slice(&[0xF3, 0x0F, 0x59, 0x05]); // Mulss xmm0, [rip+factor]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, factor)?);
    c.extend_from_slice(&[0xF3, 0x0F, 0x5F, 0x05]); // Maxss xmm0, [rip+least]
    c.extend_from_slice(&rel(at + c.len() as u64 + 4, least)?);
    c.extend_from_slice(&[0x48, 0x83, 0xC4, 0x20]); // Done: add rsp, 0x20
    c.push(0x5B); // Pop rbx
    c.push(0xC3); // ret
    Ok(c)
}

/// `bytes` put at `at`, swapped in and out as one aligned code word.
fn patch(at: u64, bytes: &[u8]) -> Result<Jump, String> {
    let word = at & !7;
    if at + bytes.len() as u64 > word + 8 {
        return Err("the patch crosses a code word".into());
    }
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut new_bytes = old_bytes;
    let i = (at - word) as usize;
    new_bytes[i..i + bytes.len()].copy_from_slice(bytes);
    Ok(Jump { word, old, new: u64::from_le_bytes(new_bytes.try_into().unwrap()), on: false })
}

fn jump(func: u64, to: u64) -> Result<Jump, String> {
    let mut b = vec![0xE9];
    b.extend_from_slice(&rel(func + 5, to)?);
    patch(func, &b)
}

/// The function a `call rel32` at `call` goes to, if it is one into the exe.
fn call_target(exe: &aurora_core::proto::ModuleInfo, call: u64, name: &str) -> Result<u64, String> {
    let b = memory::read(call, 5)?;
    let to = (call + 5).wrapping_add_signed(i64::from(i32::from_le_bytes([b[1], b[2], b[3], b[4]])));
    if b[0] != 0xE8 || to < exe.base || to >= exe.base + exe.size {
        return Err(format!("{name}: the call is not what it was (game patched?)"));
    }
    Ok(to)
}

fn prepare() -> Result<Hooks, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let funcs = [
        find(&exe, RECOIL, "recoil")?,
        find(&exe, SPREAD, "accuracy")?,
        find(&exe, AMMO, "ammo")?,
        find(&exe, SHAKE, "shake")?,
        find(&exe, SWAY, "sway")?,
        find(&exe, STAMINA, "stamina")?,
        find(&exe, DAMAGE, "damage")?,
        find(&exe, RESERVE, "reserve")? + RESERVE_CALL,
        find(&exe, FIRE, "fire rate")? + FIRE_CALL,
    ];
    let reload = call_target(&exe, funcs[7], "reserve")?;
    let wait = call_target(&exe, funcs[8], "fire rate")?;
    let block = memory::alloc_code_near(exe.base, BLOCK).ok_or("no room for the weapon code")?;
    let at = CODE_AT.map(|o| block + o);
    let (body, net) = (block + BODY_AT, block + NET_AT);
    let code = [
        // Cmp [rcx+8], rax
        skip_code(at[0], body, funcs[0], &[0x48, 0x39, 0x41, 0x08], &[], &memory::read(funcs[0], 6)?, false)?,
        // Cmp rax, rdx
        skip_code(at[1], body, funcs[1], &[0x48, 0x39, 0xD0], &[], &memory::read(funcs[1], 5)?, true)?,
        // Cmp [rcx+0x88], rax; answer mov al, 1
        skip_code(at[2], net, funcs[2], &[0x48, 0x39, 0x81, 0x88, 0x00, 0x00, 0x00], &[0xB0, 0x01], &memory::read(funcs[2], 5)?, false)?,
        // Cmp [rcx+8], rax (both skip the whole function for the player)
        skip_code(at[3], body, funcs[3], &[0x48, 0x39, 0x41, 0x08], &[], &memory::read(funcs[3], 7)?, false)?,
        skip_code(at[4], body, funcs[4], &[0x48, 0x39, 0x41, 0x08], &[], &memory::read(funcs[4], 5)?, false)?,
        stamina_code(at[5], body, block + SEEN_AT, funcs[5])?,
        damage_code(at[6], body, block + FACTOR_AT, funcs[6])?,
        reserve_code(at[7], net, reload)?,
        fire_code(at[8], net, block + FIRE_AT, block + FIRE_MIN_AT, wait)?,
    ];
    let ends = [CODE_AT[1], CODE_AT[2], CODE_AT[3], CODE_AT[4], CODE_AT[5], BODY_AT, CODE_AT[7], CODE_AT[8], FIRE_MIN_AT];
    for (i, c) in code.iter().enumerate() {
        if CODE_AT[i] + c.len() as u64 > ends[i] {
            return Err("weapon code too long".into());
        }
        memory::write(at[i], c)?;
    }
    memory::write(body, &[0; 24])?;
    memory::write(block + FACTOR_AT, &1f32.to_le_bytes())?;
    memory::write(block + FIRE_AT, &1f32.to_le_bytes())?;
    memory::write(block + FIRE_MIN_AT, &FIRE_MIN_S.to_le_bytes())?;
    log(&format!(
        "weapon hooks ready: {}, code at {block:#x}",
        funcs.iter().zip(NAMES).map(|(f, n)| format!("{n} exe+{:#x}", f - exe.base)).collect::<Vec<_>>().join(", ")
    ));
    let mut jumps = Vec::new();
    for i in 0..7 {
        jumps.push(jump(funcs[i], at[i])?);
    }
    // Reserve: no jump of five bytes fits one code word after the call (the sub, lea, add and movzx
    // each start too near a word's end), so only the call's own 4-byte offset changes: it sits
    // alone in its word (exe+0x2161738 in that build), and no instruction is moved.
    jumps.push(patch(funcs[7] + 1, &rel(funcs[7] + 5, at[7])?)?);
    // Fire rate the same way: the call's 4-byte offset sits in one word (exe+0x36b2390).
    jumps.push(patch(funcs[8] + 1, &rel(funcs[8] + 5, at[8])?)?);
    let Ok(jumps) = jumps.try_into() else { return Err("weapon jumps".into()) };
    Ok(Hooks { block, jumps })
}

fn swap(j: &mut Jump, on: bool) -> Result<(), String> {
    if j.on == on {
        return Ok(());
    }
    let (want, put) = if on { (j.old, j.new) } else { (j.new, j.old) };
    match memory::swap_code_u64(j.word, put)? {
        was if was == want => {
            j.on = on;
            Ok(())
        }
        was => {
            // Someone else changed this code: put back what was there.
            let _ = memory::swap_code_u64(j.word, was);
            Err(format!("code word was {was:#x}, expected {want:#x}; left alone"))
        }
    }
}

/// The player's body and the player's network part, for the hooks to know the player by.
fn refresh(block: u64) {
    // The player's character object's +8 is the player's body (the game's "entity"). Not
    // `fly::body_addr`: that is where the player's position sits, and the hooks never matched it.
    let ch = crate::entities::my_character();
    let ptr_at = |off: u64| {
        ch.and_then(|c| memory::read(c + off, 8).ok())
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
            .filter(|&p| p > 0x10000 && memory::readable(p, 8))
            .unwrap_or(0)
    };
    let (body, net) = (ptr_at(8), ptr_at(NET));
    let mut b = body.to_le_bytes().to_vec();
    b.extend_from_slice(&net.to_le_bytes());
    let _ = memory::write(block + BODY_AT, &b);
}

/// Turn feature `i` (see `NAMES`) on or off.
fn set(i: usize, on: bool) -> Result<(), String> {
    if let Some(e) = BROKEN.lock().ok().and_then(|b| b.clone()) {
        return Err(e);
    }
    if on {
        ensure()?;
    }
    let mut slot = HOOKS.lock().map_err(|_| "weapon lock")?;
    let Some(h) = slot.as_mut() else { return Ok(()) };
    // Who the player is first, so the hook knows the player from the first shot.
    if on {
        refresh(h.block);
    }
    swap(&mut h.jumps[i], on).inspect_err(|e| log(&format!("{}: {e}", NAMES[i])))?;
    if i == 5 && !on {
        // Give the player's stamina back to the game: clear the byte we set (re-read first).
        if let Ok(b) = memory::read(h.block + SEEN_AT, 8) {
            let part = u64::from_le_bytes(b.try_into().unwrap());
            if part > 0x10000 && memory::readable(part + 0x1F1, 1) {
                let _ = memory::write(part + 0x1F1, &[0]);
            }
        }
    }
    log(&format!("{} {}", NAMES[i], if on { "on" } else { "off" }));
    Ok(())
}

fn is_on(i: usize) -> bool {
    HOOKS.lock().ok().and_then(|h| h.as_ref().map(|h| h.jumps[i].on)).unwrap_or(false)
}

pub fn set_recoil(on: bool) -> Result<(), String> {
    set(0, on)
}

pub fn set_accuracy(on: bool) -> Result<(), String> {
    set(1, on)
}

pub fn set_ammo(on: bool) -> Result<(), String> {
    set(2, on)
}

pub fn set_shake(on: bool) -> Result<(), String> {
    set(3, on)
}

pub fn set_sway(on: bool) -> Result<(), String> {
    set(4, on)
}

pub fn set_stamina(on: bool) -> Result<(), String> {
    set(5, on)
}

/// The player's hits do `factor` times the damage; 0 turns it off.
pub fn set_damage(factor: f32) -> Result<(), String> {
    let on = factor > 0.0;
    set(6, on)?;
    if on {
        let block = HOOKS.lock().ok().and_then(|h| h.as_ref().map(|h| h.block)).ok_or("weapon hooks not ready")?;
        memory::write(block + FACTOR_AT, &factor.to_le_bytes())?;
    }
    DAMAGE_X.store(if on { factor.to_bits() } else { 0 }, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

static DAMAGE_X: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The damage factor now (0 when off).
pub fn damage() -> f32 {
    if is_on(6) { f32::from_bits(DAMAGE_X.load(std::sync::atomic::Ordering::Relaxed)) } else { 0.0 }
}

pub fn stamina_on() -> bool {
    is_on(5)
}

pub fn shake_on() -> bool {
    is_on(3)
}

pub fn sway_on() -> bool {
    is_on(4)
}

pub fn recoil_on() -> bool {
    is_on(0)
}

pub fn accuracy_on() -> bool {
    is_on(1)
}

pub fn ammo_on() -> bool {
    is_on(2)
}

pub fn set_reserve(on: bool) -> Result<(), String> {
    set(7, on)
}

pub fn reserve_on() -> bool {
    is_on(7)
}

/// The player's guns fire `x` times as fast; 0 turns it off. The factor is written before the hook
/// goes on, so the player's first shot already has it.
pub fn set_fire_rate(x: f32) -> Result<(), String> {
    let on = x > 0.0;
    if on {
        ensure()?;
        let block = HOOKS.lock().ok().and_then(|h| h.as_ref().map(|h| h.block)).ok_or("weapon hooks not ready")?;
        memory::write(block + FIRE_AT, &(1.0 / x).to_le_bytes())?;
    }
    set(8, on)?;
    FIRE_X.store(if on { x.to_bits() } else { 0 }, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

static FIRE_X: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The fire rate factor now (0 when off).
pub fn fire_rate() -> f32 {
    if is_on(8) { f32::from_bits(FIRE_X.load(std::sync::atomic::Ordering::Relaxed)) } else { 0.0 }
}

/// Each second: the player's body and network part move after a respawn or a world change.
pub fn tick() {
    let Ok(slot) = HOOKS.lock() else { return };
    let Some(h) = slot.as_ref() else { return };
    if h.jumps.iter().any(|j| j.on) {
        refresh(h.block);
    }
}

/// Find the three functions and write our code, all hooks still off, so the first switch-on is
/// instant. Run once at load. The search runs without the lock: the menu reads the switches every
/// frame and could not be moved for seconds after a load.
pub fn warm() {
    let _ = ensure();
}

/// The hooks ready, searching for them first if no one has (see `PREP`).
fn ensure() -> Result<(), String> {
    let _one = PREP.lock().map_err(|_| "weapon lock")?;
    if HOOKS.lock().map_err(|_| "weapon lock")?.is_some() {
        return Ok(());
    }
    if let Some(e) = BROKEN.lock().ok().and_then(|b| b.clone()) {
        return Err(e);
    }
    match prepare() {
        Ok(h) => {
            *HOOKS.lock().map_err(|_| "weapon lock")? = Some(h);
            Ok(())
        }
        Err(e) => {
            log(&format!("weapon: {e}"));
            if let Ok(mut b) = BROKEN.lock() {
                *b = Some(e.clone());
            }
            Err(e)
        }
    }
}

/// All off (unload).
pub fn shutdown() {
    for i in 0..N {
        let _ = set(i, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fire rate code's short jumps all land on `done` (add rsp, 0x20), with the 2 ms floor in,
    /// and it fits its room in the block.
    #[test]
    fn fire_code_jumps_land_on_done() {
        let (at, net, factor, least, func) = (0x1_0000_0380u64, 0x1_0000_0268, 0x1_0000_027C, 0x1_0000_03F8, 0x1_4000_0000);
        let c = fire_code(at, net, factor, least, func).unwrap();
        let done = c.windows(4).position(|w| w == [0x48, 0x83, 0xC4, 0x20]).unwrap();
        assert_eq!(&c[done + 4..], &[0x5B, 0xC3]);
        // Every jz / jge / jne short jump (74, 7D, 75 after a test or cmp) goes to done.
        let mut jumps = 0;
        for i in 0..c.len() - 1 {
            let after_test = i >= 3 && matches!(&c[i - 3..i], [0x48, 0x85, 0xC0] | [0x48, 0x85, 0xC9]);
            let after_cmp = (i >= 4 && c[i - 4..i] == [0x83, 0x79, 0x0C, 0x00]) || (i >= 4 && c[i - 4..i] == (ITEM_NET as u32).to_le_bytes());
            if (after_test && c[i] == 0x74) || (after_cmp && matches!(c[i], 0x7D | 0x75)) {
                assert_eq!(i + 2 + c[i + 1] as usize, done, "jump at {i}");
                jumps += 1;
            }
        }
        assert_eq!(jumps, 5);
        // The maxss reads the floor, the mulss the factor (rip-relative, from the next byte on).
        let rip = |op: [u8; 4]| {
            let k = c.windows(4).position(|w| w == op).unwrap() + 4;
            (at + k as u64 + 4).wrapping_add_signed(i64::from(i32::from_le_bytes(c[k..k + 4].try_into().unwrap())))
        };
        assert_eq!(rip([0xF3, 0x0F, 0x59, 0x05]), factor);
        assert_eq!(rip([0xF3, 0x0F, 0x5F, 0x05]), least);
        assert!(CODE_AT[8] + c.len() as u64 <= FIRE_MIN_AT && FIRE_MIN_AT + 4 <= BLOCK as u64);
        // 1000x a 600 RPM gun's 0.1 s is 0.1 ms: held to 2 ms.
        assert!((0.1f32 / 1000.0).max(FIRE_MIN_S) == FIRE_MIN_S);
    }
}
