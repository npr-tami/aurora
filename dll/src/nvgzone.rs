//! Game > Disables > **NVG In Erewhon**. The player's own character only; every other character
//! runs the game's own way.
//!
//! Found live:
//! - The player's character runs a behaviour tree. Its top selector picks the hub branch in safe
//!   zones such as Erewhon, so the gameplay branch, which holds the goggles group (the NVG /
//!   thermal / drone devices and the vision controller that reads the player's NVG key), is never
//!   visited there.
//! - The hub branch has its own goggles leaf (a "goggles off" node, normally idle). Letting the
//!   tree visit the goggles group in that leaf's place gave a test: V puts the goggles
//!   on the player's face with the animation, nothing else breaks. (Letting the whole gameplay
//!   branch run broke the player's animations: rejected.)
//! - The vision controller's own "may I run" check also says no in safe zones (a zone rule on its
//!   owner); for the player's that rule is skipped.
//!
//! How, two parts, both only for the player:
//! 1. **Visit:** the tree iterator looks each child up by id (`call lookup`, unique in both exes).
//!    That one call goes to our code, which does the same lookup and, when the result is the
//!    player's hub goggles leaf, hands back the player's goggles group instead. The tree definition
//!    (shared by every player character) is never changed. Which leaf and which group are worked
//!    out from the player's own tree the first time the player is in a hub (`find`), and checked
//!    first.
//! 2. **Check:** the vision controller's "may I run" check (`push rbx; sub rsp,20h`, 6 bytes) runs
//!    our code first: for the player's controller, the game's other two conditions without the zone
//!    rule. Put in while every other thread is stopped (two instructions, as inbox.rs).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{globals, log, memory};

/// The vision controller's check: `push rbx; sub rsp,20h; mov rbx,rcx; mov rcx,[rcx+18h]; call
/// <zone rule>; test al,al; jne; cmp dword [rbx+20h],0Ch; jne`. Unique in the Vulkan and the DX11
/// exe.
const CHECK: &str = "40 53 48 83 EC 20 48 8B D9 48 8B 49 18 E8 ?? ?? ?? ?? 84 C0 75 06 83 7B 20 0C 75 08";
/// The two instructions our code runs instead.
const MOVED: [u8; 6] = [0x40, 0x53, 0x48, 0x83, 0xEC, 0x20];
/// The tree iterator's child lookup: `lea rcx,[rbp+8]; mov r8d,eax; movzx edx,al; shr r8d,3; and
/// r8d,1FFFFFE0h; call lookup; mov rbx,rax` (Vulkan rva 0x1a32f8b, DX11 0x1a12e8b).
const VISIT: &str = "48 8D 4D 08 44 8B C0 0F B6 D0 41 C1 E8 03 41 81 E0 E0 FF FF 1F E8 ?? ?? ?? ?? 48 8B D8";
/// Where the call sits in `VISIT`.
const VISIT_CALL: u64 = 21;
/// The lookup itself: `mov rcx,[rcx]; lea edx,[rdx*8]; mov eax,r8d; add rax,[rdx+rcx]; ret`.
const LOOKUP: [u8; 18] = [0x48, 0x8B, 0x09, 0x8D, 0x14, 0xD5, 0, 0, 0, 0, 0x41, 0x8B, 0xC0, 0x48, 0x03, 0x04, 0x0A, 0xC3];
/// The hub goggles leaf's "may I run" check (its class table's slot 0x28; Vulkan rva 0x34af5b0,
/// DX11 0x348f870).
const LEAF_CHECK: &str = "40 53 48 83 EC 20 48 8B D9 E8 ?? ?? ?? ?? 48 8B D0 48 8B CB E8 ?? ?? ?? ?? 48 8B CB E8 ?? ?? ?? ?? 84";
/// The vision controller's constructor putting its class table in: `lea rax,[table]` (Vulkan rva
/// 0x351497a, DX11 0x34f4b7a).
const VISION_TABLE: &str = "48 8D 05 ?? ?? ?? ?? 48 8D 8B B0 00 00 00 48 89 03 C6";
/// A class table's "may I run" slot.
const SLOT_CHECK: u64 = 0x28;
const BLOCK: usize = 0x200;
/// Our data inside the code block: +0 the leaf's tree index (u16), +2 the group's id (u16), +8 the
/// tree seen (its id lookup base), +0x10 the leaf object seen.
const DATA: u64 = 0x1C0;

/// The behaviour tree: object `+8` points at the per-kind object bases, `+0x48` at the shared
/// definition (whose first word is the node table), `+0x50` the node count. A node table entry is
/// 10 bytes: id, first child, last in subtree, next sibling, own index (0xFFFF = none). A node
/// object: `+0x10` its index (u16), `+0x12` bit 2 "doesn't claim its selector", `+0x18` the owner,
/// whose `+0x10` is the body.
const NONE: u16 = 0xFFFF;
const MAX_NODES: u64 = 0x4000;

struct Code {
    word: u64,
    old: u64,
    new: u64,
    at: u64,
}

struct Hook {
    check: Code,
    visit: Code,
    /// Our data block (see `DATA`).
    data: u64,
    leaf_vt: u64,
    vision_vt: u64,
    body: u64,
    on: bool,
}

static HOOK: Mutex<Option<Hook>> = Mutex::new(None);
static BAD: AtomicBool = AtomicBool::new(false);
/// The finder thread runs while the switch is on and the group is not known yet.
static FINDING: AtomicBool = AtomicBool::new(false);

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

/// Patch short jumps (`(at, to)` offsets in `c`).
fn link(c: &mut [u8], jumps: &[(usize, usize)]) -> Result<(), String> {
    for &(from, to) in jumps {
        c[from] = i8::try_from(to as i64 - (from as i64 + 1)).map_err(|_| "short jump too far")? as u8;
    }
    Ok(())
}

/// Part 2: our code at `cave` for the check at `at`. `body` is the address that holds the player's
/// body. On entry rcx is the controller; only rax and r11 are used (free at a function's start).
fn check_code(cave: u64, at: u64, body: u64) -> Result<Vec<u8>, String> {
    let mut c = vec![
        0x48, 0x8B, 0x41, 0x18, // Mov rax, [rcx+18h]: the owner (the game reads it next)
        0x48, 0x85, 0xC0, // Test rax, rax
        0x74, 0, // jz GAME
    ];
    let jz_owner = c.len() - 1;
    c.extend_from_slice(&[0x49, 0xBB]); // Mov r11, body
    c.extend_from_slice(&body.to_le_bytes());
    c.extend_from_slice(&[
        0x4D, 0x8B, 0x1B, // Mov r11, [r11]: the player's body
        0x4D, 0x85, 0xDB, // Test r11, r11
        0x74, 0, // jz GAME
    ]);
    let jz_body = c.len() - 1;
    c.extend_from_slice(&[
        0x4C, 0x3B, 0x58, 0x10, // Cmp r11, [rax+10h]
        0x75, 0, // Jne GAME
    ]);
    let jne_hers = c.len() - 1;
    // Own copy: the game's other two conditions, without the zone rule.
    c.extend_from_slice(&[
        0x83, 0x79, 0x20, 0x0C, // Cmp dword [rcx+20h], 0Ch
        0x74, 0, // je NO
    ]);
    let je_type = c.len() - 1;
    c.extend_from_slice(&[
        0x81, 0x79, 0x24, 0x6E, 0x02, 0x00, 0x00, // Cmp dword [rcx+24h], 26Eh
        0x75, 0, // Jne YES
    ]);
    let jne_yes = c.len() - 1;
    c.extend_from_slice(&[
        0x81, 0x79, 0x28, 0x6E, 0x02, 0x00, 0x00, // Cmp dword [rcx+28h], 26Eh
        0x74, 0, // je NO
    ]);
    let je_no = c.len() - 1;
    let yes = c.len();
    c.extend_from_slice(&[0xB0, 0x01, 0xC3]); // Mov al, 1; ret
    let no = c.len();
    c.extend_from_slice(&[0x32, 0xC0, 0xC3]); // Xor al, al; ret
    let game = c.len();
    c.extend_from_slice(&MOVED); // Push rbx; sub rsp, 20h
    c.push(0xE9); // Jmp at+6
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, at + MOVED.len() as u64)?);
    link(&mut c, &[(jz_owner, game), (jz_body, game), (jne_hers, game), (je_type, no), (jne_yes, yes), (je_no, no)])?;
    Ok(c)
}

/// Part 1: our lookup. Called as the game's lookup (rcx = the tree's base list, edx = the id's
/// kind, r8d = its offset), returns the object in rax. Uses only rax, r8 to r11 (the call already
/// counts them as lost). `data` is our data block (`DATA`).
fn visit_code(leaf_vt: u64, body: u64, data: u64) -> Vec<u8> {
    let mut c = vec![
        0x4C, 0x8B, 0x09, // Mov r9, [rcx]: the bases
        0x8B, 0xC2, // Mov eax, edx
        0x49, 0x8B, 0x04, 0xC1, // Mov rax, [r9+rax*8]
        0x45, 0x8B, 0xD0, // Mov r10d, r8d
        0x4C, 0x01, 0xD0, // Add rax, r10: the game's answer
        0x49, 0xBA, // Mov r10, leaf_vt
    ];
    c.extend_from_slice(&leaf_vt.to_le_bytes());
    c.extend_from_slice(&[
        0x4C, 0x39, 0x10, // Cmp [rax], r10
        0x75, 0, // Jne DONE
    ]);
    let j1 = c.len() - 1;
    c.extend_from_slice(&[
        0x4C, 0x8B, 0x50, 0x18, // Mov r10, [rax+18h]: the owner
        0x4D, 0x85, 0xD2, // Test r10, r10
        0x74, 0, // jz DONE
    ]);
    let j2 = c.len() - 1;
    c.extend_from_slice(&[0x49, 0xBB]); // Mov r11, body
    c.extend_from_slice(&body.to_le_bytes());
    c.extend_from_slice(&[
        0x4D, 0x8B, 0x1B, // Mov r11, [r11]: the player's body
        0x4D, 0x85, 0xDB, // Test r11, r11
        0x74, 0, // jz DONE
    ]);
    let j3 = c.len() - 1;
    c.extend_from_slice(&[
        0x4D, 0x3B, 0x5A, 0x10, // Cmp r11, [r10+10h]
        0x75, 0, // Jne DONE: not the player's
    ]);
    let j4 = c.len() - 1;
    c.extend_from_slice(&[0x49, 0xBB]); // Mov r11, data
    c.extend_from_slice(&data.to_le_bytes());
    c.extend_from_slice(&[
        0x45, 0x0F, 0xB7, 0x53, 0x02, // Movzx r10d, word [r11+2]: the group's id
        0x45, 0x85, 0xD2, // Test r10d, r10d
        0x74, 0, // jz SEEN: not worked out yet
    ]);
    let j5 = c.len() - 1;
    c.extend_from_slice(&[
        0x44, 0x0F, 0xB7, 0x40, 0x10, // Movzx r8d, word [rax+10h]: this leaf's index
        0x66, 0x45, 0x3B, 0x03, // Cmp r8w, [r11]
        0x75, 0, // Jne DONE: another leaf of that kind
    ]);
    let j6 = c.len() - 1;
    c.extend_from_slice(&[
        0x45, 0x0F, 0xB6, 0xDA, // Movzx r11d, r10b
        0x4F, 0x8B, 0x1C, 0xD9, // Mov r11, [r9+r11*8]
        0x41, 0xC1, 0xEA, 0x03, // Shr r10d, 3
        0x41, 0x81, 0xE2, 0xE0, 0xFF, 0xFF, 0x1F, // And r10d, 1FFFFFE0h
        0x4D, 0x01, 0xD3, // Add r11, r10
        0x4C, 0x89, 0xD8, // Mov rax, r11: the player's goggles group
        0xC3, // ret
    ]);
    let seen = c.len();
    c.extend_from_slice(&[
        0x49, 0x89, 0x4B, 0x08, // Mov [r11+8], rcx: the player's tree
        0x49, 0x89, 0x43, 0x10, // Mov [r11+10h], rax: the leaf
    ]);
    let done = c.len();
    c.push(0xC3); // ret
    link(&mut c, &[(j1, done), (j2, done), (j3, done), (j4, done), (j5, seen), (j6, done)]).expect("short jumps");
    c
}

fn rd64(a: u64) -> Result<u64, String> {
    Ok(u64::from_le_bytes(memory::read(a, 8)?.try_into().unwrap()))
}

fn rip_target(at: u64, len: u64) -> Result<u64, String> {
    let disp = i32::from_le_bytes(memory::read(at + len - 4, 4)?.try_into().unwrap());
    Ok((at + len).wrapping_add_signed(i64::from(disp)))
}

/// One 8-byte code word holding `[at, at+len)` with `new_bytes` put at `at`.
fn code_word(at: u64, new_bytes: &[u8]) -> Result<Code, String> {
    let word = at & !7;
    if at + new_bytes.len() as u64 > word + 8 {
        return Err("the patch crosses a code word".into());
    }
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut b = old_bytes;
    let i = (at - word) as usize;
    b[i..i + new_bytes.len()].copy_from_slice(new_bytes);
    Ok(Code { word, old, new: u64::from_le_bytes(b.try_into().unwrap()), at })
}

fn prepare() -> Result<Hook, String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let body = exe.base + globals::BODY_ROOT.rva().ok_or("the player's body's place was not found")?;
    // The two class tables, checked against the checks they point at.
    let check_at = memory::find_code(&exe, "nvg safe zones", CHECK)?;
    if memory::read(check_at, MOVED.len())? != MOVED {
        return Err("the check's start is not as expected".into());
    }
    let lea = memory::find_code(&exe, "nvg vision table", VISION_TABLE)?;
    let vision_vt = rip_target(lea, 7)?;
    if rd64(vision_vt + SLOT_CHECK)? != check_at {
        return Err("the vision controller's class table is not as expected".into());
    }
    let leaf_check = memory::find_code(&exe, "nvg hub leaf check", LEAF_CHECK)?;
    // The leaf's class table sits near the vision one: the one 8-byte pointer to its check within 1
    // MB.
    let from = vision_vt.saturating_sub(0x10_0000) & !7;
    let near = memory::read(from, 0x20_0000)?;
    let hits: Vec<u64> = near.chunks_exact(8).enumerate().filter(|(_, w)| u64::from_le_bytes((*w).try_into().unwrap()) == leaf_check).map(|(i, _)| from + i as u64 * 8).collect();
    let [slot] = hits[..] else { return Err(format!("hub leaf class table: {} pointers to its check, want 1", hits.len())) };
    let leaf_vt = slot - SLOT_CHECK;
    // The iterator's lookup call, and what it calls.
    let site = memory::find_code(&exe, "nvg tree visit", VISIT)?;
    let call = site + VISIT_CALL;
    let lookup = rip_target(call, 5)?;
    if memory::read(lookup, LOOKUP.len())? != LOOKUP {
        return Err("the tree lookup is not as expected".into());
    }
    let cave = memory::alloc_code_near(exe.base, BLOCK).ok_or("no room for the NVG code")?;
    let check_cave = cave;
    let visit_cave = cave + 0x80;
    let data = cave + DATA;
    let cc = check_code(check_cave, check_at, body)?;
    let vc = visit_code(leaf_vt, body, data);
    if cc.len() > 0x80 || vc.len() > (DATA - 0x80) as usize {
        return Err("NVG code too long".into());
    }
    memory::write(data, &[0u8; 0x18])?;
    memory::write(check_cave, &cc)?;
    memory::write(visit_cave, &vc)?;
    let mut jmp = vec![0xE9];
    jmp.extend_from_slice(&rel(check_at + 5, check_cave)?);
    let check = code_word(check_at, &jmp)?;
    let mut new_call = vec![0xE8];
    new_call.extend_from_slice(&rel(call + 5, visit_cave)?);
    let visit = code_word(call, &new_call)?;
    log(&format!(
        "nvg in erewhon ready: check exe+{:#x}, visit exe+{:#x}, leaf table exe+{:#x}, vision table exe+{:#x}, code at {cave:#x}",
        check_at - exe.base,
        call - exe.base,
        leaf_vt - exe.base,
        vision_vt - exe.base
    ));
    Ok(Hook { check, visit, data, leaf_vt, vision_vt, body, on: false })
}

/// The player's tree's indices: (hub goggles leaf index, goggles group id), worked out from the
/// tree the visit code saw the player's leaf in. Everything is read through `memory::read` and
/// checked.
struct Found {
    leaf: u16,
    group_id: u16,
}

fn find(h: &Hook) -> Result<Option<Found>, String> {
    let base_list = rd64(h.data + 8)?;
    let seen_leaf = rd64(h.data + 0x10)?;
    if base_list == 0 || seen_leaf == 0 {
        return Ok(None);
    }
    let tree = base_list - 8;
    let bases = rd64(base_list)?;
    let table = rd64(rd64(tree + 0x48)?)?;
    let n = rd64(tree + 0x50)?;
    if n == 0 || n > MAX_NODES {
        return Err(format!("tree node count {n:#x} looks wrong"));
    }
    let raw = memory::read(table, n as usize * 10)?;
    let e = |i: usize, f: usize| u16::from_le_bytes([raw[i * 10 + f * 2], raw[i * 10 + f * 2 + 1]]);
    let object = |id: u16| -> Result<u64, String> { Ok(rd64(bases + u64::from(id & 0xFF) * 8)? + u64::from((id >> 3) as u32 & 0x1FFF_FFE0)) };
    let player = rd64(h.body)?;
    let own = |o: u64| -> bool { rd64(o + 0x18).and_then(|w| if w == 0 { Err(String::new()) } else { rd64(w + 0x10) }).is_ok_and(|b| b != 0 && b == player) };
    // The seen leaf really is this tree's node.
    let leaf_idx = u16::from_le_bytes(memory::read(seen_leaf + 0x10, 2)?.try_into().unwrap()) as usize;
    if leaf_idx >= n as usize || object(e(leaf_idx, 0))? != seen_leaf {
        return Err("the leaf seen is not in that tree".into());
    }
    // Parents, from the child and sibling links.
    let mut parent = vec![NONE; n as usize];
    for i in 0..n as usize {
        let mut c = e(i, 1);
        let mut steps = 0;
        while c != NONE && (c as u64) < n && steps < n {
            parent[c as usize] = i as u16;
            c = e(c as usize, 3);
            steps += 1;
        }
    }
    // The branch right under the top: hub and gameplay branches differ there.
    let top = |mut i: usize| -> usize {
        let mut steps = 0;
        while parent[i] != NONE && parent[parent[i] as usize] != NONE && steps < n {
            i = parent[i] as usize;
            steps += 1;
        }
        i
    };
    let vt = |i: usize| object(e(i, 0)).and_then(|o| rd64(o).map(|v| (o, v)));
    let mut vision = Vec::new();
    let mut leaves = Vec::new();
    for i in 0..n as usize {
        let Ok((o, v)) = vt(i) else { continue };
        if v == h.vision_vt && own(o) {
            vision.push(i);
        } else if v == h.leaf_vt && e(i, 1) == NONE && own(o) {
            leaves.push(i);
        }
    }
    let [leaf] = leaves[..] else { return Err(format!("{} hub goggles leaves, want 1", leaves.len())) };
    // The player has one vision controller per top branch. The on-foot one sits in the biggest top
    // branch (0x161, about 250 nodes; the others are far smaller), never in the hub's.
    let size = |t: usize| e(t, 2).wrapping_sub(t as u16) as usize;
    let mut best: Option<(usize, usize)> = None;
    let mut tie = false;
    for &v in &vision {
        let t = top(v);
        if t == top(leaf) || parent[v] == NONE {
            continue;
        }
        match best {
            Some((_, s)) if size(t) < s => {}
            Some((_, s)) if size(t) == s => tie = true,
            _ => {
                best = Some((v, size(t)));
                tie = false;
            }
        }
    }
    let Some((vis, _)) = best else { return Err(format!("no vision controller of the player outside the hub ({} in all)", vision.len())) };
    if tie {
        return Err("two vision controllers in equally big branches".into());
    }
    let group = parent[vis] as usize;
    // Both must leave their selector free (+0x12 bit 2), or swapping one for the other would change
    // which sibling runs.
    let free = |i: usize| object(e(i, 0)).and_then(|o| memory::read(o + 0x12, 1)).is_ok_and(|b| b[0] & 4 != 0);
    if !free(leaf) || !free(group) || e(group, 1) == NONE {
        return Err("the goggles group or the hub leaf is not as expected".into());
    }
    let group_id = e(group, 0);
    if group_id == 0 {
        return Err("the goggles group has id 0".into());
    }
    Ok(Some(Found { leaf: leaf as u16, group_id }))
}

/// Runs while the switch is on until the player's group is known: the player has to be in a hub
/// once.
fn finder() {

    // when the unload begins (the unload waits for this thread).
    let mut said = String::new();
    while FINDING.load(Ordering::SeqCst) && crate::nap(250) {
        let Ok(slot) = HOOK.lock() else { break };
        let Some(h) = slot.as_ref() else { break };
        if !h.on {
            break;
        }
        match find(h) {
            Ok(None) => continue,
            Ok(Some(f)) => {
                // Leaf first, then the id: the visit code acts once the id is there.
                let _ = memory::write(h.data, &f.leaf.to_le_bytes());
                let _ = memory::write(h.data + 2, &f.group_id.to_le_bytes());
                log(&format!("nvg in erewhon: the player's hub goggles leaf {:#x}, goggles group id {:#x}", f.leaf, f.group_id));
                break;
            }
            Err(e) => {
                if e != said {
                    log(&format!("nvg in erewhon: {e}; trying again with the next tree seen"));
                    said = e;
                }
                let _ = memory::write(h.data + 8, &[0u8; 16]);
            }
        }
    }
    FINDING.store(false, Ordering::SeqCst);
}

pub fn is_on() -> bool {
    HOOK.lock().ok().and_then(|h| h.as_ref().map(|h| h.on)).unwrap_or(false)
}

fn swap(c: &Code, on: bool, paused: bool) -> Result<(), String> {
    let (want, put) = if on { (c.old, c.new) } else { (c.new, c.old) };
    let was = if paused { memory::swap_code_u64_paused(c.word, put, c.at + 1..c.at + 5)? } else { memory::swap_code_u64(c.word, put)? };
    if was != want {
        // Someone else changed this code: put back what was there and stop.
        let _ = memory::swap_code_u64(c.word, was);
        return Err(format!("code word {:#x} was {was:#x}, expected {want:#x}; left alone", c.word));
    }
    Ok(())
}

pub fn set(on: bool) -> Result<(), String> {
    let mut slot = HOOK.lock().map_err(|_| "nvg in erewhon lock")?;
    if on && slot.is_none() {
        if BAD.load(Ordering::Relaxed) {
            return Err("NVG in Erewhon is off for this session (see the log)".into());
        }
        match prepare() {
            Ok(h) => *slot = Some(h),
            Err(e) => {
                log(&format!("nvg in erewhon: {e}"));
                BAD.store(true, Ordering::SeqCst);
                return Err(e);
            }
        }
    }
    let Some(h) = slot.as_mut() else { return Ok(()) };
    if h.on == on {
        return Ok(());
    }
    let result = if on {
        // The check needs the other threads stopped (two instructions); the visit is one call for
        // another, in one aligned word.
        swap(&h.check, true, true).and_then(|()| swap(&h.visit, true, false).inspect_err(|_| {
            let _ = swap(&h.check, false, false);
        }))
    } else {
        let a = swap(&h.visit, false, false);
        let b = swap(&h.check, false, false);
        a.and(b)
    };
    match result {
        Ok(()) => {
            h.on = on;
            log(if on { "nvg in erewhon on" } else { "nvg in erewhon off" });
            let known = memory::read(h.data + 2, 2).is_ok_and(|b| b != [0, 0]);
            if on && !known && !FINDING.swap(true, Ordering::SeqCst) {
                crate::keep_warm("nvg in erewhon finder", std::thread::spawn(finder));
            }
            if !on {
                FINDING.store(false, Ordering::SeqCst);
            }
            Ok(())
        }
        Err(e) => {
            BAD.store(true, Ordering::SeqCst);
            log(&format!("nvg in erewhon: {e}"));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_block() -> u64 {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc};
        // SAFETY: fresh executable memory for this test only.
        let mem = unsafe { VirtualAlloc(std::ptr::null(), 0x1000, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) } as u64;
        assert_ne!(mem, 0);
        mem
    }

    #[test]
    fn check_runs_and_decides() {
        let mem = exec_block();
        // A fake "game check" at `at`: after our 6 moved bytes, `mov al, 7; add rsp, 20h; pop rbx;
        // ret`, so 7 means "the game's own check ran".
        let at = mem;
        let cave = mem + 0x100;
        let tail = [0xB0, 0x07, 0x48, 0x83, 0xC4, 0x20, 0x5B, 0xC3];
        let mut body_slot = Box::new(0u64);
        let code = check_code(cave, at, &*body_slot as *const u64 as u64).unwrap();
        // SAFETY: writing our own fresh block.
        unsafe {
            std::ptr::copy_nonoverlapping([0xE9u8].as_ptr(), at as *mut u8, 1);
            std::ptr::copy_nonoverlapping(rel(at + 5, cave).unwrap().as_ptr(), (at + 1) as *mut u8, 4);
            std::ptr::copy_nonoverlapping(tail.as_ptr(), (at + 6) as *mut u8, tail.len());
            std::ptr::copy_nonoverlapping(code.as_ptr(), cave as *mut u8, code.len());
        }
        // SAFETY: the fake function follows the win64 call shape (rcx), returns al.
        let f: extern "win64" fn(u64) -> u64 = unsafe { std::mem::transmute(at) };
        let run = |ctrl: &[u64]| f(ctrl.as_ptr() as u64) & 0xFF;
        let body = 0x1234_5678u64;
        let owner = [0u64, 0, body];
        let mut ctrl = [0u64; 8];
        ctrl[3] = owner.as_ptr() as u64; // +0x18
        ctrl[4] = 4 | (0x38 << 32); // +0x20 = 4, +0x24 = 0x38
        ctrl[5] = 0x15d; // +0x28
        // The player's body not known yet: the game's check.
        assert_eq!(run(&ctrl), 7);
        // SAFETY: our own box; volatile so the write is kept (only our generated code reads it).
        unsafe { std::ptr::write_volatile(&mut *body_slot, body) };
        assert_eq!(run(&ctrl), 1);
        ctrl[4] = 0xc | (0x38 << 32);
        assert_eq!(run(&ctrl), 0);
        ctrl[4] = 4 | (0x26e << 32);
        ctrl[5] = 0x26e;
        assert_eq!(run(&ctrl), 0);
        ctrl[5] = 0x15d;
        assert_eq!(run(&ctrl), 1);
        let other = [0u64, 0, 0x9999];
        ctrl[3] = other.as_ptr() as u64;
        assert_eq!(run(&ctrl), 7);
        ctrl[3] = 0;
        assert_eq!(run(&ctrl), 7);
        drop(body_slot);
    }

    #[test]
    fn visit_swaps_only_her_leaf() {
        let mem = exec_block();
        let data = mem + DATA;
        let leaf_vt = 0xAAAA_0000u64;
        let body = 0x1234_5678u64;
        let mut body_slot = Box::new(body);
        let code = visit_code(leaf_vt, &*body_slot as *const u64 as u64, data);
        assert!(code.len() <= DATA as usize);
        // SAFETY: our own fresh block.
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), mem as *mut u8, code.len()) };
        let f: extern "win64" fn(u64, u64, u64) -> u64 = unsafe { std::mem::transmute(mem) };
        // Objects: kind 4 at pool A, kind 0x44 at pool B. Ids: kind | offset << 3.
        let her_owner = [0u64, 0, body];
        let other_owner = [0u64, 0, 0x9999];
        let mut pool_a = vec![0u64; 0x40]; // The hub leaf at +0 (kind 4, offset 0)
        pool_a[0] = leaf_vt;
        pool_a[2] = 0x2e; // +0x10 index
        pool_a[3] = her_owner.as_ptr() as u64;
        let mut pool_b = vec![0u64; 0x800]; // The group at offset 0x3A0 (id 0x1D44: kind 0x44, (0x1D44>>3)&~0x1F = 0x3A0)
        pool_b[0x3A0 / 8] = 0xBBBB_0000;
        let mut bases = vec![0u64; 0x100];
        bases[4] = pool_a.as_ptr() as u64;
        bases[0x44] = pool_b.as_ptr() as u64;
        let base_list = [bases.as_ptr() as u64];
        let look = |id: u16| f(base_list.as_ptr() as u64, u64::from(id & 0xFF), u64::from((id >> 3) as u32 & 0x1FFF_FFE0));
        let leaf = pool_a.as_ptr() as u64;
        let group = pool_b.as_ptr() as u64 + 0x3A0;
        // Not worked out yet: the game's answer, and the tree and leaf get noted.
        assert_eq!(look(0x4), leaf);
        // SAFETY: reading our own block.
        unsafe {
            assert_eq!(*((data + 8) as *const u64), base_list.as_ptr() as u64);
            assert_eq!(*((data + 0x10) as *const u64), leaf);
        }
        // Worked out: the player's leaf gives the group, everything else stays.
        // SAFETY: our own block.
        unsafe {
            *(data as *mut u16) = 0x2e;
            *((data + 2) as *mut u16) = 0x1D44;
        }
        assert_eq!(look(0x4), group);
        assert_eq!(look(0x1D44), group);
        // Another index of that kind: the game's answer.
        pool_a[2] = 0x2f;
        assert_eq!(look(0x4), leaf);
        pool_a[2] = 0x2e;
        // Someone else's leaf: the game's answer.
        pool_a[3] = other_owner.as_ptr() as u64;
        assert_eq!(look(0x4), leaf);
        // No owner: the game's answer.
        pool_a[3] = 0;
        assert_eq!(look(0x4), leaf);
        // The player's body not known: the game's answer.
        pool_a[3] = her_owner.as_ptr() as u64;
        // SAFETY: our own box; volatile so the write is kept.
        unsafe { std::ptr::write_volatile(&mut *body_slot, 0) };
        assert_eq!(look(0x4), leaf);
        drop(body_slot);
    }
}
