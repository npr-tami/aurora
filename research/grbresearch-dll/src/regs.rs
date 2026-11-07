//! Who runs this code, with what? For a while, each spot jumps to a bit of ours that notes all 16
//! registers in a ring and carries on; then `regs.txt` says, per spot and register, how often it
//! held each value asked about (the player's body always), or pointed at an object holding it, and
//! its top values.
//!
//! `regs.request`, one item per line, `#` starts a comment:
//! - `secs 60`: how long the spots stay on (5 to 300, default 60).
//! - `spot <name>; <where>; <offset>; <bytes>`: `where` is a byte pattern (`F3 0F 10 ?? 48`, every
//!   hit up to 4 is hooked) or an address (`exe+0x2a5a4c0`, `0x7ff6...`). The hook goes `offset`
//!   bytes after it (hex like every address: `10` and `0x10` both mean 16), and `bytes` (decimal, 5
//!   to 32) is how many bytes of whole instructions from there run inside our bit instead. A
//!   decoder checks them: they must end on an instruction boundary, use nothing relative to rip,
//!   and hold no jump, call or return. The first 5 must sit inside one aligned 8-byte word, and no
//!   two hooks may share a byte.
//! - `value <name> <addr>`: also count this value (the player's character, an NPC's body...).
//! - `ring <calls>`: how many calls each spot keeps, a power of two, 2048 to 16384 (default 2048).
//! - A spot may end with a 5th field, reads made at the moment of each call: `spot inbox; <where>;
//!   0; 11; rsp+0 rdx+0 rdx+8:4`. Each read is `<register>+<hex offset>` (or `-`), with `:1` `:2`
//!   `:4` `:8` (default) or `:f` (a 4-byte float). The register is its value when the spot runs.
//!   **The read happens inside the game's thread: only read what the game itself reads there, or
//!   its stack.** At most 15 per spot. Spots with reads also get every kept call, timed, in
//!   `regs-rows.csv`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aurora_core::proto::REG_NAMES;

use crate::{folder, log, memory, parse_addr, take_request};

const RING: usize = 2048;
const RING_MAX: usize = 16384;
/// One call: 16 registers of 8 bytes in `REG_NAMES` order (0x00), up to 15 reads (0x80), the time
/// stamp counter (0xF8).
const ENTRY: usize = 0x100;
const READS_AT: usize = 0x80;
const TSC_AT: usize = 0xF8;
const MAX_READS: usize = 15;
const IDX_AT: u64 = 0x400;
const RING_AT: u64 = 0x800;
const MAX_SPOTS: usize = 16;
const HITS_PER_PATTERN: usize = 4;
/// `[exe+0x63ebeb0]`: the player's physics body (the mod's fly.rs BODY_ROOT).
const BODY_ROOT: u64 = 0x063e_beb0;
/// The x86 number of each register in `REG_NAMES` order (rax 0, rcx 1, rdx 2, rbx 3, rsp 4, rbp 5,
/// rsi 6, rdi 7, r8..r15).
const HW: [u8; 16] = [0, 3, 1, 2, 6, 7, 5, 4, 8, 9, 10, 11, 12, 13, 14, 15];

static HANDLE: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);

struct Spot {
    name: String,
    at: SpotAt,
    off: u64,
    len: usize,
    reads: Vec<Read>,
}

enum SpotAt {
    Pattern(aurora_core::pattern::Pattern),
    Addr(u64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Size {
    B1,
    B2,
    B4,
    B8,
    F32,
}

/// One read made when a spot runs: `[register + off]`, `size` bytes kept.
#[derive(Clone, Debug, PartialEq)]
struct Read {
    text: String,
    reg: usize,
    off: i32,
    size: Size,
}

/// `rdx+8:4`, `rsp+0x28:f`, `rcx`, `rbp-10`: a register from `REG_NAMES`, a hex offset, a size.
fn parse_read(s: &str) -> Option<Read> {
    let (body, size) = match s.split_once(':') {
        Some((b, z)) => (
            b,
            match z {
                "1" => Size::B1,
                "2" => Size::B2,
                "4" => Size::B4,
                "8" => Size::B8,
                "f" | "F" => Size::F32,
                _ => return None,
            },
        ),
        None => (s, Size::B8),
    };
    // Through i64::try_from: a huge offset is refused instead of wrapping (or, negated,
    // overflowing).
    let (reg, off) = if let Some((r, o)) = body.split_once('+') {
        (r, i64::try_from(parse_hex(o)?).ok()?)
    } else if let Some((r, o)) = body.split_once('-') {
        (r, -i64::try_from(parse_hex(o)?).ok()?)
    } else {
        (body, 0)
    };
    let reg = REG_NAMES.iter().position(|n| n.eq_ignore_ascii_case(reg.trim()))?;
    Some(Read { text: s.to_string(), reg, off: i32::try_from(off).ok()?, size })
}

/// A kept value as the request asked for it: masked to its size, or a float.
fn show(v: u64, size: Size) -> String {
    match size {
        Size::B1 => format!("{:#x}", v & 0xff),
        Size::B2 => format!("{:#x}", v & 0xffff),
        Size::B4 => format!("{:#x}", v & 0xffff_ffff),
        Size::B8 => format!("{v:#x}"),
        Size::F32 => format!("{:.2}", f32::from_bits(v as u32)),
    }
}

fn masked(v: u64, size: Size) -> u64 {
    match size {
        Size::B1 => v & 0xff,
        Size::B2 => v & 0xffff,
        Size::B4 | Size::F32 => v & 0xffff_ffff,
        Size::B8 => v,
    }
}

pub(crate) struct Request {
    pub(crate) secs: u64,
    ring: usize,
    spots: Vec<Spot>,
    values: Vec<(String, u64)>,
}

pub(crate) fn parse(text: &str) -> Result<Request, String> {
    let mut r = Request { secs: 60, ring: RING, spots: Vec::new(), values: Vec::new() };
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let bad = |what: &str| format!("line {}: {what}: {line}", n + 1);
        if let Some(v) = line.strip_prefix("secs ") {
            r.secs = v.trim().parse::<u64>().map_err(|_| bad("bad secs"))?.clamp(5, 300);
        } else if let Some(v) = line.strip_prefix("ring ") {
            let ring = v.trim().parse::<usize>().map_err(|_| bad("bad ring"))?;
            if !ring.is_power_of_two() || !(RING..=RING_MAX).contains(&ring) {
                return Err(bad("ring must be a power of two, 2048 to 16384"));
            }
            r.ring = ring;
        } else if let Some(v) = line.strip_prefix("value ") {
            let (name, addr) = v.trim().rsplit_once(char::is_whitespace).ok_or_else(|| bad("want value <name> <addr>"))?;
            r.values.push((name.trim().to_string(), parse_addr(addr).ok_or_else(|| bad("bad address"))?));
        } else if let Some(v) = line.strip_prefix("spot ") {
            let f: Vec<&str> = v.split(';').map(str::trim).collect();
            if f.len() != 4 && f.len() != 5 {
                return Err(bad("want spot <name>; <pattern or address>; <offset>; <bytes>[; <reads>]"));
            }
            let reads = match f.get(4) {
                Some(list) => list.split_whitespace().map(|s| parse_read(s).ok_or_else(|| bad(&format!("bad read {s}")))).collect::<Result<Vec<_>, _>>()?,
                None => Vec::new(),
            };
            if reads.len() > MAX_READS {
                return Err(bad("at most 15 reads"));
            }
            let off = parse_hex(f[2]).ok_or_else(|| bad("bad offset (hex)"))?;
            let len = f[3].parse::<usize>().map_err(|_| bad("bad byte count"))?;
            if !(5..=32).contains(&len) {
                return Err(bad("bytes must be 5 to 32"));
            }
            // An address has a module+ or 0x; anything else is a pattern.
            let at = if f[1].contains('+') || f[1].starts_with("0x") {
                SpotAt::Addr(parse_addr(f[1]).ok_or_else(|| bad("bad address (module not loaded?)"))?)
            } else {
                SpotAt::Pattern(aurora_core::pattern::Pattern::parse(f[1]).map_err(|e| bad(&format!("bad pattern ({e:?})")))?)
            };
            r.spots.push(Spot { name: f[0].to_string(), at, off, len, reads });
        } else {
            return Err(bad("unknown line"));
        }
    }
    if r.spots.is_empty() {
        return Err("no spot lines".into());
    }
    if r.spots.len() > MAX_SPOTS {
        return Err(format!("at most {MAX_SPOTS} spots"));
    }
    Ok(r)
}

/// A hex number, with or without `0x` (the spot offset).
fn parse_hex(s: &str) -> Option<u64> {
    let s = s.trim();
    let h = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    u64::from_str_radix(h, 16).ok()
}

/// Do two hooks share a byte? Each one's 5-byte jump lies inside its own moved bytes (`len` is at
/// least 5), so they clash exactly when the moved ranges overlap: then one's jump back lands inside
/// the other's jump, and the game runs half an instruction.
fn overlaps(a: (u64, usize), b: (u64, usize)) -> bool {
    a.0 < b.0 + b.1 as u64 && b.0 < a.0 + a.1 as u64
}

/// Decode the bytes a spot moves into our bit. `code` holds them and may run on past `len` (so the
/// last instruction can be seen whole). They must be whole instructions that end exactly at `len`,
/// with no rip-relative operand, no relative jump or call, and no jump, call or return at all (one
/// may be someone's hook, the mod's included).
fn check_moved(code: &[u8], len: usize) -> Result<(), String> {
    use libudis86_sys::{ud_mnemonic_code as M, ud_type as T};
    if code.len() < len {
        return Err(format!("only {} of {len} bytes readable", code.len()));
    }
    let mut off = 0usize;
    // SAFETY: a decoder state on our stack, set up by ud_init before use; it reads only
    // `code` (pointer and length given), which outlives it.
    unsafe {
        let mut u: libudis86_sys::ud = std::mem::zeroed();
        let u = &mut u as *mut libudis86_sys::ud;
        libudis86_sys::ud_init(u);
        libudis86_sys::ud_set_mode(u, 64);
        libudis86_sys::ud_set_syntax(u, None);
        libudis86_sys::ud_set_input_buffer(u, code.as_ptr(), code.len());
        while off < len {
            let n = libudis86_sys::ud_disassemble(u) as usize;
            if n == 0 {
                return Err(format!("+{off:#x}: the bytes end inside an instruction"));
            }
            let m = libudis86_sys::ud_insn_mnemonic(u);
            if matches!(m, M::UD_Iinvalid) {
                return Err(format!("+{off:#x}: not an instruction the decoder knows"));
            }
            if matches!(m, M::UD_Icall | M::UD_Ijmp | M::UD_Iret | M::UD_Iretf | M::UD_Iiretq | M::UD_Iint3 | M::UD_Isyscall) {
                return Err(format!("+{off:#x}: a jump, call or return"));
            }
            for i in 0..4 {
                let op = libudis86_sys::ud_insn_opr(u, i);
                if op.is_null() {
                    break;
                }
                if matches!((*op).otype, T::UD_OP_JIMM) {
                    return Err(format!("+{off:#x}: a relative jump"));
                }
                if matches!((*op).otype, T::UD_OP_MEM) && matches!((*op).base, T::UD_R_RIP) {
                    return Err(format!("+{off:#x}: relative to rip"));
                }
            }
            off += n;
        }
    }
    if off != len {
        return Err(format!("{len} bytes cut an instruction in half (whole ones end at {off})"));
    }
    Ok(())
}

fn rel(from_end: u64, to: u64) -> Result<[u8; 4], String> {
    i32::try_from(to as i64 - from_end as i64).map(|r| r.to_le_bytes()).map_err(|_| "jump too far".to_string())
}

struct Hook {
    name: String,
    at: u64,
    cave: u64,
    word: u64,
    old: u64,
    new: u64,
    reads: Vec<Read>,
}

/// The bit of ours one spot jumps to: count the call, note the 16 registers in the ring, make the
/// reads, stamp the time, run the moved bytes, jump back. `ring` is a power of two.
fn cave_code(cave: u64, at: u64, moved: &[u8], ring: usize, reads: &[Read]) -> Result<Vec<u8>, String> {
    if reads.len() > MAX_READS {
        return Err("too many reads".into());
    }
    let mut c = vec![0x50, 0x41, 0x53, 0x9C]; // Push rax; push r11; pushfq
    c.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00, 0xF0, 0x0F, 0xC1, 0x05]); // Mov eax, 1; lock xadd [rip+idx], eax
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, cave + IDX_AT)?);
    c.push(0x25); // And eax, ring - 1
    c.extend_from_slice(&((ring - 1) as u32).to_le_bytes());
    c.extend_from_slice(&[0xC1, 0xE0, 0x08]); // Shl eax, 8 (ENTRY)
    c.extend_from_slice(&[0x4C, 0x8D, 0x1D]); // Lea r11, [rip+ring]
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, cave + RING_AT)?);
    c.extend_from_slice(&[0x4C, 0x01, 0xD8]); // Add rax, r11
    // The stack now holds flags, r11, rax; the player's rsp was 0x18 higher.
    c.extend_from_slice(&[0x4C, 0x8B, 0x5C, 0x24, 0x10, 0x4C, 0x89, 0x58, 0x00]); // Mov r11, [rsp+0x10]; mov [rax], r11 (rax)
    c.extend_from_slice(&[0x4C, 0x8B, 0x5C, 0x24, 0x08, 0x4C, 0x89, 0x58, 0x58]); // Mov r11, [rsp+8]; mov [rax+0x58], r11 (r11)
    c.extend_from_slice(&[0x4C, 0x8D, 0x5C, 0x24, 0x18, 0x4C, 0x89, 0x58, 0x38]); // Lea r11, [rsp+0x18]; mov [rax+0x38], r11 (rsp)
    for (slot, &hw) in HW.iter().enumerate() {
        if matches!(slot, 0 | 7 | 11) {
            continue;
        }
        // Mov [rax+slot*8], reg
        c.extend_from_slice(&[if hw >= 8 { 0x4C } else { 0x48 }, 0x89, 0x40 | ((hw & 7) << 3), (slot * 8) as u8]);
    }
    // Reads: the register's own value comes back from the entry (rax, r11 and rsp are ours by now,
    // their real values are there), then the 8 bytes it points at go to the entry.
    for (i, r) in reads.iter().enumerate() {
        c.extend_from_slice(&[0x4C, 0x8B, 0x58, (r.reg * 8) as u8]); // Mov r11, [rax+reg*8]
        c.extend_from_slice(&[0x4D, 0x8B, 0x9B]); // Mov r11, [r11+off]
        c.extend_from_slice(&r.off.to_le_bytes());
        c.extend_from_slice(&[0x4C, 0x89, 0x98]); // Mov [rax+READS_AT+i*8], r11
        c.extend_from_slice(&((READS_AT + i * 8) as u32).to_le_bytes());
    }
    // Time stamp: rdtsc writes edx:eax, so keep the entry in r11 and rdx on the stack.
    c.extend_from_slice(&[0x49, 0x89, 0xC3]); // Mov r11, rax
    c.push(0x52); // Push rdx
    c.extend_from_slice(&[0x0F, 0x31]); // rdtsc
    c.extend_from_slice(&[0x48, 0xC1, 0xE2, 0x20]); // Shl rdx, 32
    c.extend_from_slice(&[0x48, 0x09, 0xD0]); // Or rax, rdx
    c.extend_from_slice(&[0x49, 0x89, 0x83]); // Mov [r11+TSC_AT], rax
    c.extend_from_slice(&(TSC_AT as u32).to_le_bytes());
    c.push(0x5A); // Pop rdx
    c.extend_from_slice(&[0x9D, 0x41, 0x5B, 0x58]); // Popfq; pop r11; pop rax
    c.extend_from_slice(moved);
    c.push(0xE9);
    c.extend_from_slice(&rel(cave + c.len() as u64 + 4, at + moved.len() as u64)?);
    if c.len() as u64 > IDX_AT {
        return Err("regs code too long".into());
    }
    Ok(c)
}

fn hook_at(s: &Spot, at: u64, ring: usize) -> Result<Hook, String> {
    let (name, len) = (s.name.as_str(), s.len);
    let word = at & !7;
    if at + 5 > word + 8 {
        return Err(format!("{name} at {at:#x}: the jump would cross a code word"));
    }
    // 15 more bytes (one longest instruction) so a cut last instruction shows; fewer near the end
    // of readable memory.
    let code = memory::read(at, len + 15).or_else(|_| memory::read(at, len))?;
    check_moved(&code, len).map_err(|e| format!("{name} at {at:#x}: refused, {e}"))?;
    let moved = &code[..len];
    let cave = memory::alloc_code_near(at, RING_AT as usize + ring * ENTRY).ok_or("no room for our code")?;
    memory::write(cave, &cave_code(cave, at, moved, ring, &s.reads)?)?;
    let old_bytes = memory::read(word, 8)?;
    let old = u64::from_le_bytes(old_bytes.clone().try_into().unwrap());
    let mut nb = old_bytes;
    let i = (at - word) as usize;
    nb[i] = 0xE9;
    nb[i + 1..i + 5].copy_from_slice(&rel(at + 5, cave)?);
    Ok(Hook { name: name.to_string(), at, cave, word, old, new: u64::from_le_bytes(nb.try_into().unwrap()), reads: s.reads.clone() })
}

/// The hooks of one spot. `taken` holds the moved ranges of the hooks made so far; a place
/// overlapping one of them is refused.
fn hooks_for(exe: &aurora_core::proto::ModuleInfo, s: &Spot, ring: usize, taken: &mut Vec<(u64, usize)>) -> Vec<Hook> {
    let places = match &s.at {
        SpotAt::Pattern(p) => memory::scan_module(exe, p, HITS_PER_PATTERN),
        SpotAt::Addr(a) => vec![*a],
    };
    if places.is_empty() {
        log(&format!("regs: {}: no hits", s.name));
    }
    let mut out = Vec::new();
    for p in places {
        let at = p + s.off;
        if let Some(t) = taken.iter().find(|&&t| overlaps((at, s.len), t)) {
            log(&format!("regs: {} at {at:#x} ({} bytes): refused, overlaps the hook at {:#x} ({} bytes)", s.name, s.len, t.0, t.1));
            continue;
        }
        match hook_at(s, at, ring) {
            Ok(h) => {
                taken.push((at, s.len));
                out.push(h);
            }
            Err(e) => log(&format!("regs: {e}")),
        }
    }
    out
}

#[allow(unused_unsafe)]
fn tsc() -> u64 {
    // SAFETY: rdtsc only reads the time stamp counter.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// One kept call of a spot with reads: its time stamp, rcx, and the reads (masked).
struct Row {
    tsc: u64,
    rcx: u64,
    vals: Vec<u64>,
}

/// The kept calls in time order. `raw` holds only written slots (a ring not yet full is read up to
/// its count).
fn rows_of(raw: &[u8], reads: &[Read]) -> Vec<Row> {
    let word = |e: &[u8; ENTRY], at: usize| u64::from_le_bytes(e[at..at + 8].try_into().unwrap());
    let mut rows: Vec<Row> = raw
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map(|e| Row {
            tsc: word(e, TSC_AT),
            rcx: word(e, 2 * 8),
            vals: reads.iter().enumerate().map(|(i, r)| masked(word(e, READS_AT + i * 8), r.size)).collect(),
        })
        .collect();
    // A call caught between taking its slot and stamping it (at unhook) has no time: drop it.
    rows.retain(|r| r.tsc != 0);
    rows.sort_by_key(|r| r.tsc);
    rows
}

/// Calls with the same reads together: (reads, calls, how many different rcx), most first.
fn group(rows: &[Row]) -> Vec<(Vec<u64>, u32, usize)> {
    let mut m: HashMap<&[u64], (u32, HashSet<u64>)> = HashMap::new();
    for r in rows {
        let e = m.entry(r.vals.as_slice()).or_default();
        e.0 += 1;
        e.1.insert(r.rcx);
    }
    let mut out: Vec<_> = m.into_iter().map(|(k, (c, set))| (k.to_vec(), c, set.len())).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

fn run(req: Request) -> Result<(), String> {
    let exe = memory::find_module("").ok_or("no exe")?;
    let mut taken = Vec::new();
    let hooks: Vec<Hook> = req.spots.iter().flat_map(|s| hooks_for(&exe, s, req.ring, &mut taken)).collect();
    let mut live = Vec::new();
    for h in &hooks {
        // Our jump covers bytes at+1..at+5; no thread may be stopped at an instruction there.
        match memory::replace_code_u64_paused(h.word, h.old, h.new, h.at + 1..h.at + 5) {
            Ok(true) => live.push(h),
            Ok(false) => log(&format!("regs: {} code word changed meanwhile, skipped", h.name)),
            Err(e) => log(&format!("regs: {}: {e}", h.name)),
        }
    }
    log(&format!("regs: {} spots on for {} s", live.len(), req.secs));
    // The time stamps are raw counter ticks: measured here against the clock to give ms.
    let (tsc0, t0) = (tsc(), Instant::now());
    let start_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let end = Instant::now() + Duration::from_secs(req.secs);
    while Instant::now() < end && !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
    }
    for h in &live {
        match memory::replace_code_u64(h.word, h.new, h.old) {
            Ok(true) => {}
            // Our jump was changed by someone else: leave their code be. Our bit is never freed, so
            // a jump to it stays safe even after we unload.
            Ok(false) => log(&format!("regs: {} code word changed while on, left as it is", h.name)),
            Err(e) => log(&format!("regs: {} unhook: {e}", h.name)),
        }
    }
    // Let calls still inside our code finish their rows before the rings are read.
    std::thread::sleep(Duration::from_millis(50));
    let ticks_per_ms = tsc().wrapping_sub(tsc0) as f64 / (t0.elapsed().as_secs_f64() * 1000.0).max(1.0);
    let body = memory::read_u64(exe.base + BODY_ROOT).unwrap_or(0);
    let mut values = vec![("the player's body".to_string(), body)];
    values.extend(req.values);
    let mut s = values.iter().map(|(n, v)| format!("{n} {v:#x}")).collect::<Vec<_>>().join(", ");
    let _ = writeln!(s, "; started at unix {start_unix}");
    // Notes for 8-byte values: in the exe, one of the values, or pointing at one (a handle).
    let mut notes: HashMap<u64, String> = HashMap::new();
    let mut note = |v: u64| -> String {
        notes
            .entry(v)
            .or_insert_with(|| {
                if v >= exe.base && v < exe.base + exe.size {
                    return format!(" (exe+{:#x})", v - exe.base);
                }
                if let Some((n, _)) = values.iter().find(|(_, w)| *w != 0 && *w == v) {
                    return format!(" ({n})");
                }
                let d = if v > 0x10000 { memory::read_u64(v) } else { None };
                match d.and_then(|d| values.iter().find(|(_, w)| *w != 0 && *w == d)) {
                    Some((n, _)) => format!(" (->{n})"),
                    None => String::new(),
                }
            })
            .clone()
    };
    let mut csv = String::new();
    for h in &live {
        let calls = memory::read(h.cave + IDX_AT, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
        let n = (calls as usize).min(req.ring);
        let raw = memory::read(h.cave + RING_AT, n * ENTRY).unwrap_or_default();
        let _ = writeln!(s, "== {} at exe+{:#x} ({:#x}): {calls} calls, last {n} kept", h.name, h.at.wrapping_sub(exe.base), h.at);
        for (r, name) in REG_NAMES.iter().enumerate() {
            let mut vals: HashMap<u64, u32> = HashMap::new();
            for e in raw.as_chunks::<ENTRY>().0 {
                *vals.entry(u64::from_le_bytes(e[r * 8..r * 8 + 8].try_into().unwrap())).or_default() += 1;
            }
            // Per value asked about: how often the register was it, and how often it pointed at
            // something holding it in its first 8 words.
            let mut is = vec![0u32; values.len()];
            let mut holds = vec![0u32; values.len()];
            for (&v, &count) in &vals {
                let words: Vec<u64> = if v > 0x10000 && memory::readable(v, 0x40) {
                    memory::read(v, 0x40).map(|b| b.as_chunks::<8>().0.iter().map(|w| u64::from_le_bytes(*w)).collect()).unwrap_or_default()
                } else {
                    Vec::new()
                };
                for (k, (_, want)) in values.iter().enumerate() {
                    if *want == 0 {
                        continue;
                    }
                    is[k] += if v == *want { count } else { 0 };
                    holds[k] += if words.contains(want) { count } else { 0 };
                }
            }
            let mut top: Vec<_> = vals.into_iter().collect();
            top.sort_by_key(|x| std::cmp::Reverse(x.1));
            let asked: Vec<String> = values.iter().enumerate().map(|(k, (vn, _))| format!("{vn} {} (holds {})", is[k], holds[k])).collect();
            let _ = writeln!(
                s,
                "  {name}: {}; {} values, top {}",
                asked.join(", "),
                top.len(),
                top.iter().take(4).map(|(v, c)| format!("{v:#x}x{c}")).collect::<Vec<_>>().join(" ")
            );
        }
        if h.reads.is_empty() {
            continue;
        }
        let rows = rows_of(&raw, &h.reads);
        let groups = group(&rows);
        let names = h.reads.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(" | ");
        let _ = writeln!(s, "  reads ({names}): {} different, top 40 (calls, different rcx):", groups.len());
        for (vals, count, callers) in groups.iter().take(40) {
            let shown: Vec<String> = vals.iter().zip(&h.reads).map(|(&v, r)| show(v, r.size) + &if r.size == Size::B8 { note(v) } else { String::new() }).collect();
            let _ = writeln!(s, "    {count} x, rcx {callers}: {}", shown.join(" | "));
        }
        let _ = writeln!(csv, "# {}: spot,ms,rcx,{}", h.name, h.reads.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join(","));
        for r in &rows {
            let ms = r.tsc.wrapping_sub(tsc0) as i64 as f64 / ticks_per_ms;
            let shown: Vec<String> = r.vals.iter().zip(&h.reads).map(|(&v, rd)| show(v, rd.size) + &if rd.size == Size::B8 { note(v) } else { String::new() }).collect();
            let _ = writeln!(csv, "{},{ms:.1},{:#x},{}", h.name, r.rcx, shown.join(","));
        }
    }
    if !csv.is_empty() {
        std::fs::write(folder().join("regs-rows.csv"), csv).map_err(|e| e.to_string())?;
    }
    std::fs::write(folder().join("regs.txt"), s).map_err(|e| e.to_string())?;
    log("regs: done");
    Ok(())
}

/// Called by the worker twice a second.
pub fn check_request() {
    let Some(text) = take_request("regs.request") else { return };
    let req = match parse(&text) {
        Ok(r) => r,
        Err(e) => {
            log(&format!("regs request: {e}"));
            let _ = std::fs::write(folder().join("regs.txt"), format!("error: {e}\n"));
            return;
        }
    };
    let Ok(mut slot) = HANDLE.lock() else { return };
    if slot.as_ref().is_some_and(|h| !h.is_finished()) {
        log("regs request: already running");
        return;
    }
    STOP.store(false, Ordering::SeqCst);
    *slot = Some(std::thread::spawn(move || {
        if let Err(e) = run(req) {
            log(&format!("regs: {e}"));
        }
    }));
}

/// Unload: take the jumps out now (the report is still written) and wait for it.
pub fn shutdown() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = HANDLE.lock().ok().and_then(|mut h| h.take()) {
        let _ = h.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_lines() {
        let r = parse("secs 20\n# the player's damage\nspot damage; 83 BF A8 00 00 00 06 48 8B CE; 0; 7\nspot range; F3 0F 10 30 48 85 C9 74 30 E8; 0x0; 7\nvalue character 0x1234\n").unwrap();
        assert_eq!(r.secs, 20);
        assert_eq!(r.spots.len(), 2);
        assert_eq!((r.spots[1].off, r.spots[1].len), (0, 7));
        assert_eq!(r.values, vec![("character".to_string(), 0x1234)]);
        assert!(parse("spot a; 90 90; 0; 4").is_err(), "fewer than 5 bytes");
        assert!(parse("secs 20").is_err(), "no spot");
        assert!(parse("spot a; 90 90 90 90 90; 0").is_err(), "missing field");
        assert!(parse("jump 1").is_err());
    }

    #[test]
    fn offset_is_hex() {
        let r = parse("spot a; 90 90 90 90 90; 10; 5\nspot b; 90 90 90 90 90; 0x1f; 5\nspot c; 90 90 90 90 90; 0XA; 5").unwrap();
        assert_eq!(r.spots.iter().map(|s| s.off).collect::<Vec<_>>(), vec![0x10, 0x1f, 0xa]);
        assert!(parse("spot a; 90 90 90 90 90; zz; 5").is_err(), "not hex");
        assert!(parse("spot a; 90 90 90 90 90; 0; 0x7").is_err(), "byte count is decimal");
    }

    #[test]
    fn overlapping_hooks_refused() {
        // The review's case: A at X with 7 bytes, B at X+5.
        assert!(overlaps((0x1000, 7), (0x1005, 5)));
        assert!(overlaps((0x1005, 5), (0x1000, 7)));
        assert!(overlaps((0x1000, 5), (0x1000, 5)), "same place");
        assert!(overlaps((0x1000, 32), (0x1010, 5)), "inside");
        assert!(!overlaps((0x1000, 7), (0x1007, 5)), "touching is fine");
        assert!(!overlaps((0x1010, 5), (0x1000, 16)));
    }

    #[test]
    fn decoder_checks_moved_bytes() {
        let pad = |b: &[u8]| [b, &[0x90; 15]].concat();
        // Mov rax, [rcx]; mov rbx, rax: plain, 6 bytes.
        assert_eq!(check_moved(&pad(&[0x48, 0x8B, 0x01, 0x48, 0x89, 0xC3]), 6), Ok(()));
        // Movss xmm6, [rax]; test rcx, rcx: 7 bytes (the README's range spot).
        assert_eq!(check_moved(&pad(&[0xF3, 0x0F, 0x10, 0x30, 0x48, 0x85, 0xC9]), 7), Ok(()));
        // Mov rax, [rip+0x10]
        let e = check_moved(&pad(&[0x48, 0x8B, 0x05, 0x10, 0, 0, 0]), 7).unwrap_err();
        assert!(e.contains("rip"), "{e}");
        // nop; nop; lea rcx, [rip+0x20]: rip-relative after the first byte.
        assert!(check_moved(&pad(&[0x90, 0x90, 0x48, 0x8D, 0x0D, 0x20, 0, 0, 0]), 9).is_err());
        // Call rel32, and a mov followed by the mod's jmp rel32.
        assert!(check_moved(&pad(&[0xE8, 0, 0, 0, 0]), 5).is_err());
        assert!(check_moved(&pad(&[0x48, 0x89, 0xC3, 0xE9, 0, 0, 0, 0]), 8).is_err());
        // Jne rel8, and ret.
        assert!(check_moved(&pad(&[0x48, 0x89, 0xC3, 0x75, 0x05]), 5).is_err());
        assert!(check_moved(&pad(&[0x48, 0x89, 0xC3, 0x90, 0xC3]), 5).is_err());
        // Mov rax, [rcx+8]; mov rcx, [rcx+0x10]: 5 bytes cut the second one.
        let e = check_moved(&pad(&[0x48, 0x8B, 0x41, 0x08, 0x48, 0x8B, 0x49, 0x10]), 5).unwrap_err();
        assert!(e.contains("cut"), "{e}");
        // Fewer bytes than asked for.
        assert!(check_moved(&[0x90; 4], 5).is_err());
    }

    #[test]
    fn reads_and_ring_lines() {
        let r = parse("ring 16384\nspot inbox; 48 83 EC 28 F6 81 C0; 0; 11; rsp+0 rdx+0 rdx+8:4\nspot noise; 0x10; 0; 5; rsp+28:f rsp+38:F rsp+40:4 r9-8:2 rcx:1\n").unwrap();
        assert_eq!(r.ring, 16384);
        assert_eq!(r.spots[0].reads.len(), 3);
        assert_eq!(r.spots[0].reads[2], Read { text: "rdx+8:4".into(), reg: 3, off: 8, size: Size::B4 });
        let n = &r.spots[1].reads;
        assert_eq!((n[0].reg, n[0].off, n[0].size), (7, 0x28, Size::F32));
        assert_eq!(n[1].size, Size::F32);
        assert_eq!((n[3].reg, n[3].off, n[3].size), (9, -8, Size::B2));
        assert_eq!((n[4].reg, n[4].off, n[4].size), (2, 0, Size::B1));
        assert!(parse("ring 3000\nspot a; 90 90 90 90 90; 0; 5").is_err(), "not a power of two");
        assert!(parse("ring 32768\nspot a; 90 90 90 90 90; 0; 5").is_err(), "too big");
        assert!(parse("spot a; 90 90 90 90 90; 0; 5; rzz+8").is_err(), "no such register");
        assert!(parse("spot a; 90 90 90 90 90; 0; 5; rdx+8:3").is_err(), "no such size");
        let many = (0..16).map(|i| format!("rdx+{i:x}")).collect::<Vec<_>>().join(" ");
        assert!(parse(&format!("spot a; 90 90 90 90 90; 0; 5; {many}")).is_err(), "16 reads");
        assert_eq!(parse("spot a; 90 90 90 90 90; 0; 5").unwrap().ring, RING, "default ring");
    }

    /// The request for the noise session: it parses, and the bytes each spot moves (the first 11 of
    /// the inbox pattern, the first 5 of the noise one) pass the decoder.
    #[test]
    fn noise_request_ready() {
        let r = parse(include_str!("../requests/noise.request")).unwrap();
        assert_eq!((r.secs, r.ring, r.spots.len()), (300, 16384, 2));
        assert_eq!(r.spots.iter().map(|s| (s.off, s.len, s.reads.len())).collect::<Vec<_>>(), vec![(0, 11, 6), (0, 5, 7)]);
        let inbox = [0x48, 0x83, 0xEC, 0x28, 0xF6, 0x81, 0xC0, 0x02, 0x00, 0x00, 0x10, 0x4C, 0x8B, 0xCA, 0x4C, 0x8B, 0xD1];
        assert_eq!(check_moved(&inbox, 11), Ok(()));
        let noise = [0x48, 0x89, 0x5C, 0x24, 0x10, 0x48, 0x89, 0x6C, 0x24, 0x18, 0x48, 0x89, 0x74, 0x24, 0x20, 0x57];
        assert_eq!(check_moved(&noise, 5), Ok(()));
    }

    #[test]
    fn values_shown_by_size() {
        assert_eq!(show(0x1122_3344_5566_7788, Size::B1), "0x88");
        assert_eq!(show(0x1122_3344_5566_7788, Size::B2), "0x7788");
        assert_eq!(show(0x1122_3344_0000_0010, Size::B4), "0x10");
        assert_eq!(show(0x1122_3344_5566_7788, Size::B8), "0x1122334455667788");
        assert_eq!(show(0xdead_beef_0000_0000 | 30.5f32.to_bits() as u64, Size::F32), "30.50");
        assert_eq!(masked(0xffff_ffff_0000_0010, Size::B4), 0x10);
    }

    #[test]
    fn rows_sorted_and_grouped() {
        // Offsets too big for i64 are refused, not wrapped.
        assert!(parse_read("rdx-8000000000000000").is_none());
        assert!(parse_read("rdx+ffffffffffffffff").is_none());
        assert!(parse_read("rdx-100000000").is_none(), "beyond i32");
        assert_eq!(parse_read("rsp-10").unwrap().off, -0x10);
        let reads = vec![parse_read("rdx+8:4").unwrap()];
        let entry = |tsc: u64, rcx: u64, v: u64| {
            let mut e = [0u8; ENTRY];
            e[TSC_AT..TSC_AT + 8].copy_from_slice(&tsc.to_le_bytes());
            e[16..24].copy_from_slice(&rcx.to_le_bytes());
            e[READS_AT..READS_AT + 8].copy_from_slice(&v.to_le_bytes());
            e
        };
        // Ring order is not time order once it wraps.
        let raw = [entry(30, 0xA, 0x7_0000_0010), entry(10, 0xA, 0x10), entry(20, 0xB, 0x10), entry(40, 0xA, 0x0e), entry(0, 0xC, 0x99)].concat();
        let rows = rows_of(&raw, &reads);
        assert_eq!(rows.iter().map(|r| r.tsc).collect::<Vec<_>>(), vec![10, 20, 30, 40], "the unstamped row is dropped");
        assert_eq!(rows[2].vals, vec![0x10], "masked to 4 bytes");
        let g = group(&rows);
        assert_eq!(g[0], (vec![0x10], 3, 2), "type 0x10: 3 calls from 2 different rcx");
        assert_eq!(g[1], (vec![0x0e], 1, 1));
    }

    /// Runs the real recorder code in this test process: a stand-in "game function" (5 moved nops,
    /// then ret) called through our cave with known registers and memory.
    #[test]
    fn cave_runs_and_records() {
        use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree};
        let ring = RING;
        let len = RING_AT as usize + ring * ENTRY + 0x1000;
        // SAFETY: fresh memory of ours, freed at the end; nothing else uses it.
        let block = unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE | MEM_COMMIT, PAGE_EXECUTE_READWRITE) } as u64;
        assert_ne!(block, 0);
        let (cave, func) = (block, block + len as u64 - 0x1000);
        let reads = ["rsp+0", "rdx+0", "rdx+8:4", "rcx+4:f", "rsp-8"].map(|r| parse_read(r).unwrap());
        let code = cave_code(cave, func, &[0x90; 5], ring, &reads).unwrap();
        let record: [u64; 2] = [0x1122_3344_5566_7788, 0xAAAA_BBBB_0000_0010];
        let floats: [f32; 2] = [1.0, 30.5];
        // SAFETY: both regions are ours and writable; the cave ends in a jump to `func + 5`,
        // Which returns to us. It saves and restores everything it touches.
        let f: extern "win64" fn(u64, u64) -> u64 = unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), cave as *mut u8, code.len());
            std::ptr::copy_nonoverlapping([0x90, 0x90, 0x90, 0x90, 0x90, 0xC3].as_ptr(), func as *mut u8, 6);
            std::mem::transmute(cave)
        };
        let (p_floats, p_record) = (floats.as_ptr() as u64, record.as_ptr() as u64);
        f(p_floats, p_record);
        f(p_floats, p_record);
        // SAFETY: reading back our own block.
        let (count, e0, e1) = unsafe {
            let count = *((cave + IDX_AT) as *const u32);
            let e = |i: usize| std::slice::from_raw_parts((cave + RING_AT + (i * ENTRY) as u64) as *const u8, ENTRY).to_vec();
            (count, e(0), e(1))
        };
        let word = |e: &[u8], at: usize| u64::from_le_bytes(e[at..at + 8].try_into().unwrap());
        assert_eq!(count, 2);
        assert_eq!(word(&e0, 2 * 8), p_floats, "rcx");
        assert_eq!(word(&e0, 3 * 8), p_record, "rdx");
        let ret = word(&e0, READS_AT);
        assert_ne!(ret, 0, "return address");
        assert_eq!(word(&e0, READS_AT + 8), record[0], "rdx+0");
        assert_eq!(masked(word(&e0, READS_AT + 16), Size::B4), 0x10, "rdx+8:4");
        assert_eq!(show(word(&e0, READS_AT + 24), Size::F32), "30.50", "rcx+4:f");
        assert_eq!(word(&e0, READS_AT + 32), word(&e0, 0), "rsp-8 is the rax we pushed first");
        assert!(word(&e1, TSC_AT) > word(&e0, TSC_AT) && word(&e0, TSC_AT) != 0, "time stamps rise");
        // Two call sites a few bytes apart: both return addresses point into this test.
        assert!(word(&e1, READS_AT) != ret && word(&e1, READS_AT).abs_diff(ret) < 0x40, "return addresses of the two calls");
        // 15 reads still fit before the counter.
        let fifteen: Vec<Read> = (0..15).map(|i| parse_read(&format!("r15+{i:x}")).unwrap()).collect();
        assert!(cave_code(cave, func, &[0x90; 32], RING_MAX, &fifteen).unwrap().len() as u64 <= IDX_AT);
        // SAFETY: our own block, no longer used.
        unsafe { VirtualFree(block as *mut _, 0, MEM_RELEASE) };
    }

    #[test]
    fn cave_stores_every_register_once() {
        let cave = 0x1_0000_0000;
        let c = cave_code(cave, cave + 0x10_0000, &[0x90; 7], RING, &[]).unwrap();
        // Every slot 0..15 written exactly once: `mov [rax+disp8], reg` ends in the slot offset.
        let mut slots: Vec<u8> = c.windows(4).filter(|w| (w[0] == 0x48 || w[0] == 0x4C) && w[1] == 0x89 && w[2] & 0xC7 == 0x40).map(|w| w[3]).collect();
        slots.sort_unstable();
        assert_eq!(slots, (0..16).map(|i| i * 8).collect::<Vec<u8>>());
        assert!(c.len() as u64 <= IDX_AT);
        // Ends with the jump back past the moved bytes.
        let back = i32::from_le_bytes(c[c.len() - 4..].try_into().unwrap()) as i64;
        assert_eq!(cave as i64 + c.len() as i64 + back, (cave + 0x10_0000 + 7) as i64);
    }
}
