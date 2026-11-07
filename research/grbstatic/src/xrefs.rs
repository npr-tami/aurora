//! Who references an rva (or any rva in a range).
//!
//! Code: one raw pass over the code sections finds every position whose 4 bytes, read as a rel32 /
//! disp32, would land on the target (next ip = position + 4, plus 0, 1, 2 or 4 bytes of immediate
//! after it). Each candidate is then checked by decoding: inside a function (an exception-table
//! entry) the whole chunk is decoded from its start, so instruction boundaries are real. Outside
//! every entry (leaf functions sit between entries) the gap is decoded from the end of the entry
//! before it; failing that, the instruction is decoded at the starts that put the offset exactly at
//! the candidate. Both are marked as found without an entry. Short (rel8) jumps can only come from
//! within 128 bytes, so the chunks around a code target are decoded as well.
//!
//! Data: 8-byte absolute pointers (imagebase + rva) at 8-aligned spots in non-executable sections
//! (vtables, tables). The protected section is never read.

use std::collections::{BTreeSet, HashMap};

use iced_x86::{Decoder, DecoderOptions, FlowControl, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind, Register};

use crate::image::Image;
use crate::unwind::{Functions, RtFn, primary};

pub struct CodeRef {
    pub ins: Instruction,
    /// The referenced rva.
    pub target: u32,
    pub kind: &'static str,
    /// Start of the containing function (primary entry), when an entry covers it.
    pub func: Option<u32>,
    /// True when no exception entry covers the spot: the instruction was found by decoding from the
    /// end of the previous entry, or (last resort) at a guessed start.
    pub guessed: bool,
}

/// The rva the instruction branches to or addresses RIP-relative, when it is in [lo, hi].
pub fn target_in(ins: &Instruction, lo: u32, hi: u32) -> Option<u32> {
    (0..ins.op_count()).find_map(|i| {
        let t = match ins.op_kind(i) {
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => ins.near_branch_target(),
            OpKind::Memory if ins.is_ip_rel_memory_operand() => ins.ip_rel_memory_address(),
            _ => return None,
        };
        (u64::from(lo)..=u64::from(hi)).contains(&t).then_some(t as u32)
    })
}

/// Call / jmp / jcc for direct branches; call-ptr / jmp-ptr through memory; lea; otherwise how the
/// memory operand is used: read, write, read+write.
pub fn kind(ins: &Instruction, info: &mut InstructionInfoFactory) -> &'static str {
    let branch = (0..ins.op_count()).any(|i| matches!(ins.op_kind(i), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64));
    match ins.flow_control() {
        FlowControl::Call if branch => return "call",
        FlowControl::UnconditionalBranch if branch => return "jmp",
        FlowControl::ConditionalBranch if branch => return "jcc",
        FlowControl::IndirectCall => return "call-ptr",
        FlowControl::IndirectBranch => return "jmp-ptr",
        _ => {}
    }
    if ins.mnemonic() == Mnemonic::Lea {
        return "lea";
    }
    let target = ins.ip_rel_memory_address();
    let used = info.info(ins).used_memory();
    let m = used.iter().find(|m| m.base() == Register::RIP || (m.base() == Register::None && m.index() == Register::None && m.displacement() == target));
    match m.map(|m| m.access()) {
        Some(OpAccess::Read | OpAccess::CondRead) => "read",
        Some(OpAccess::Write | OpAccess::CondWrite) => "write",
        Some(OpAccess::ReadWrite | OpAccess::ReadCondWrite) => "read+write",
        _ => "ref",
    }
}

/// Positions in the code sections whose rel32 / disp32 could resolve into [lo, hi].
pub fn candidates(img: &Image, lo: u32, hi: u32) -> Vec<u32> {
    let mut out = Vec::new();
    // Next ip + d lands in [lo - 4, hi]: the immediate after the offset is 0, 1, 2 or 4 bytes.
    let (lo, hi) = (i64::from(lo) - 4, i64::from(hi));
    for s in img.code_sections() {
        let raw = img.raw(s);
        let base = i64::from(s.rva) + 4;
        for i in 0..raw.len().saturating_sub(3) {
            let d = i32::from_le_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
            let t = base + i as i64 + i64::from(d);
            if t >= lo && t <= hi {
                out.push(s.rva + i as u32);
            }
        }
    }
    out
}

/// Every instruction in code that references `target`, sorted by rva.
pub fn code_refs(img: &Image, funcs: &Functions, target: u32) -> Vec<CodeRef> {
    code_refs_in(img, funcs, target, target)
}

/// Every instruction in code that references an rva in [lo, hi], sorted by rva.
pub fn code_refs_in(img: &Image, funcs: &Functions, lo: u32, hi: u32) -> Vec<CodeRef> {
    let cands = candidates(img, lo, hi);
    let mut chunks: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    let mut loose = Vec::new();
    for &c in &cands {
        match funcs.entry_at(c) {
            Some(e) => {
                chunks.insert((e.begin, e.end, e.unwind));
            }
            None => loose.push(c),
        }
    }
    // Short jumps: chunks within 128 bytes of a code target.
    if img.in_code(lo) {
        let from = lo.saturating_sub(128);
        let first = funcs.entries.partition_point(|f| f.end <= from);
        for e in funcs.entries[first..].iter().take_while(|f| f.begin <= hi.saturating_add(128)) {
            chunks.insert((e.begin, e.end, e.unwind));
        }
    }

    let mut info = InstructionInfoFactory::new();
    let mut out = Vec::new();
    let mut prim_cache: HashMap<u32, Option<u32>> = HashMap::new();
    for (begin, end, unwind) in chunks {
        if !img.in_code(begin) {
            continue;
        }
        let Some(bytes) = img.bytes_upto(begin, (end - begin) as usize) else { continue };
        let mut d = Decoder::with_ip(64, bytes, u64::from(begin), DecoderOptions::NONE);
        let mut ins = Instruction::default();
        while d.can_decode() {
            d.decode_out(&mut ins);
            if let Some(target) = target_in(&ins, lo, hi) {
                let func = *prim_cache.entry(begin).or_insert_with(|| primary(img, &RtFn { begin, end, unwind }).ok().map(|(p, _)| p.begin));
                out.push(CodeRef { ins, target, kind: kind(&ins, &mut info), func, guessed: false });
            }
        }
    }
    for c in loose {
        if let Some((ins, target)) = sweep_gap(img, funcs, c, lo, hi).or_else(|| guess_at(img, c, lo, hi)) {
            out.push(CodeRef { ins, target, kind: kind(&ins, &mut info), func: None, guessed: true });
        }
    }
    out.sort_by_key(|r| r.ins.ip());
    out.dedup_by_key(|r| r.ins.ip());
    out
}

/// Does `ins` hold its 4-byte branch offset or displacement at rva `c`?
fn constant_at(d: &Decoder, ins: &Instruction, c: u32) -> bool {
    let co = d.get_constant_offsets(ins);
    let Some(at) = c.checked_sub(ins.ip() as u32).map(|x| x as usize) else { return false };
    (co.has_displacement() && co.displacement_offset() == at && co.displacement_size() == 4)
        || (co.has_immediate() && co.immediate_offset() == at && co.immediate_size() == 4)
}

/// Code outside every exception entry (leaf functions sit between entries, after int3 padding):
/// decode linearly from the end of the entry before `c` (at most 64 KB back) and keep the
/// instruction that holds the offset at `c`, if decoding lands on it.
fn sweep_gap(img: &Image, funcs: &Functions, c: u32, lo: u32, hi: u32) -> Option<(Instruction, u32)> {
    let s = img.section_of(c)?;
    let i = funcs.entries.partition_point(|f| f.begin <= c);
    let from = if i > 0 { funcs.entries[i - 1].end.max(s.rva) } else { s.rva };
    if c < from || c - from > 0x1_0000 {
        return None;
    }
    let bytes = img.bytes_upto(from, (c - from) as usize + 16)?;
    let mut d = Decoder::with_ip(64, bytes, u64::from(from), DecoderOptions::NONE);
    let mut ins = Instruction::default();
    while d.can_decode() {
        d.decode_out(&mut ins);
        if ins.ip() as u32 > c {
            return None;
        }
        if ins.next_ip() as u32 > c {
            let t = target_in(&ins, lo, hi)?;
            return constant_at(&d, &ins, c).then_some((ins, t));
        }
    }
    None
}

/// Last resort: decode at the starts (1..=10 bytes before `c`) that put a 4-byte branch offset or
/// RIP-relative displacement exactly at `c`. Shortest wins (it may miss a REX or legacy prefix; the
/// target is right either way).
fn guess_at(img: &Image, c: u32, lo: u32, hi: u32) -> Option<(Instruction, u32)> {
    for k in 1..=10u32 {
        let start = c.checked_sub(k)?;
        if !img.in_code(start) {
            return None;
        }
        let bytes = img.bytes_upto(start, 16)?;
        let mut d = Decoder::with_ip(64, bytes, u64::from(start), DecoderOptions::NONE);
        let ins = d.decode();
        if ins.is_invalid() {
            continue;
        }
        if let Some(t) = target_in(&ins, lo, hi)
            && constant_at(&d, &ins, c)
        {
            return Some((ins, t));
        }
    }
    None
}

/// 8-aligned spots in non-executable sections holding imagebase + `target`.
pub fn data_refs(img: &Image, target: u32) -> Vec<u32> {
    let want = img.va(target).to_le_bytes();
    let mut out = Vec::new();
    for s in img.data_sections() {
        let raw = img.raw(s);
        let mut o = (8 - s.rva as usize % 8) % 8;
        while o + 8 <= raw.len() {
            if raw[o..o + 8] == want {
                out.push(s.rva + o as u32);
            }
            o += 8;
        }
    }
    out
}

/// Spots in [lo, hi] (8-aligned from lo) whose address code takes with `lea`, with one such
/// instruction each, sorted. In a run of code pointers without RTTI these are the likely vtable
/// starts: constructors `lea` the vtable and store it in the object.
pub fn lea_targets(img: &Image, funcs: &Functions, lo: u32, hi: u32) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> =
        code_refs_in(img, funcs, lo, hi).iter().filter(|r| r.kind == "lea" && (r.target - lo).is_multiple_of(8)).map(|r| (r.target, r.ins.ip() as u32)).collect();
    v.sort_unstable();
    v.dedup_by_key(|x| x.0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_math_and_kinds() {
        // Code at rva 0x1000: call rel32 to 0x1100 (E8 FB 00 00 00), then mov [rip+disp],eax to
        // 0x1100 (89 05 disp) and lea rcx,[rip+disp] to 0x1100.
        let mut code = vec![0xE8, 0xFB, 0x00, 0x00, 0x00];
        let next = 0x1005u32 + 6;
        code.extend([0x89, 0x05]);
        code.extend((0x1100i32 - next as i32).to_le_bytes());
        let next = 0x100Bu32 + 7;
        code.extend([0x48, 0x8D, 0x0D]);
        code.extend((0x1100i32 - next as i32).to_le_bytes());
        code.push(0xC3);
        let img = crate::image::tests::tiny_pe(&code, &[]);
        // (The zero bytes after the code make a few more candidates; decoding drops them.)
        let c = candidates(&img, 0x1100, 0x1100);
        assert!(c.starts_with(&[0x1001, 0x1007, 0x100E]), "{c:x?}");
        // No exception table in the tiny exe: decoded from the section start, marked as such.
        let funcs = Functions { entries: Vec::new() };
        let refs = code_refs(&img, &funcs, 0x1100);
        let kinds: Vec<(u64, &str, bool)> = refs.iter().map(|r| (r.ins.ip(), r.kind, r.guessed)).collect();
        assert_eq!(kinds, vec![(0x1000, "call", true), (0x1005, "write", true), (0x100B, "lea", true)]);
        // The same with a function entry covering the code: decoded linearly, not guessed.
        let funcs = Functions { entries: vec![RtFn { begin: 0x1000, end: 0x1013, unwind: 0x2000 }] };
        let refs = code_refs(&img, &funcs, 0x1100);
        assert_eq!(refs.len(), 3);
        assert!(refs.iter().all(|r| !r.guessed && r.func == Some(0x1000) && r.target == 0x1100));
        // A range finds them too.
        assert_eq!(code_refs_in(&img, &funcs, 0x10F0, 0x1108).len(), 3);
        assert!(code_refs_in(&img, &funcs, 0x1101, 0x1108).is_empty());
    }

    #[test]
    fn data_pointers_are_aligned_absolute() {
        let mut data = vec![0u8; 0x20];
        data[8..16].copy_from_slice(&0x1_4000_1000u64.to_le_bytes());
        data[0x11..0x19].copy_from_slice(&0x1_4000_1000u64.to_le_bytes()); // Unaligned: ignored
        let img = crate::image::tests::tiny_pe(&[0xC3], &data);
        assert_eq!(data_refs(&img, 0x1000), vec![0x2008]);
    }
}
