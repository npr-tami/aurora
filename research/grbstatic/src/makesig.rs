//! Shortest unique byte signature for an instruction, and finding it again in another build.
//!
//! Decode forward from a start instruction and turn each instruction into pattern bytes,
//! wildcarding what changes between builds: rel32 branch offsets, RIP-relative displacements, bytes
//! covered by base relocations, and 4-byte displacements or immediates whose value looks like an
//! rva (at least 1 MB and inside the image: table addresses). Grow by whole instructions until the
//! pattern matches once in the code sections, then trim bytes off the end while it still matches
//! once. When no pattern starting at the rva is unique within 96 bytes, earlier instruction starts
//! (up to 64 bytes back, same function chunk) are tried, and the target offset says where the rva
//! sits inside the pattern.

use aurora_core::pattern::Pattern;
use iced_x86::{ConstantOffsets, Decoder, DecoderOptions, Instruction, OpKind};

use crate::image::Image;
use crate::unwind::Functions;

pub const MAX_LEN: usize = 96;
const MAX_BACK: u32 = 64;

/// Pattern bytes; None is a wildcard.
pub type Mask = Vec<Option<u8>>;

pub fn mask_text(m: &[Option<u8>]) -> String {
    m.iter().map(|b| b.map_or_else(|| "??".to_string(), |b| format!("{b:02X}"))).collect::<Vec<_>>().join(" ")
}

pub fn to_pattern(m: &[Option<u8>]) -> Option<Pattern> {
    Pattern::parse(&mask_text(m)).ok()
}

fn is_branch(ins: &Instruction) -> bool {
    (0..ins.op_count()).any(|i| matches!(ins.op_kind(i), OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64))
}

/// Pattern bytes for one instruction with the build-specific parts wildcarded. `relocs` is the
/// sorted (rva, width) list, empty when the directory is unreadable.
pub fn wildcard(bytes: &[u8], ins: &Instruction, co: &ConstantOffsets, relocs: &[(u32, u8)], image_base: u64, size_of_image: u32) -> Mask {
    let mut m: Mask = bytes.iter().map(|&b| Some(b)).collect();
    let mut wipe = |o: usize, n: usize| {
        for x in m.iter_mut().skip(o).take(n) {
            *x = None;
        }
    };
    let rd = |o: usize, n: usize| -> u64 { bytes.get(o..o + n).map_or(0, |b| b.iter().rev().fold(0u64, |a, &x| a << 8 | u64::from(x))) };
    let rva_like = |v: u64| (0x10_0000..u64::from(size_of_image)).contains(&v);
    let branch = is_branch(ins);
    if co.has_immediate() {
        let (o, n) = (co.immediate_offset(), co.immediate_size());
        let v = rd(o, n);
        let wild = match n {
            4 if branch => true,
            4 => rva_like(v),
            8 => v.checked_sub(image_base).is_some_and(|r| r < u64::from(size_of_image)),
            _ => false,
        };
        if wild {
            wipe(o, n);
        }
    }
    if co.has_displacement() {
        let (o, n) = (co.displacement_offset(), co.displacement_size());
        if ins.is_ip_rel_memory_operand() || (n == 4 && rva_like(rd(o, 4))) {
            wipe(o, n);
        }
    }
    // Relocated fields that overlap this instruction.
    let start = ins.ip() as u32;
    let end = start + bytes.len() as u32;
    let i = relocs.partition_point(|&(r, _)| r < end);
    for &(r, w) in relocs[..i].iter().rev().take_while(|&&(r, _)| r + 8 > start) {
        let (a, b) = (r.max(start), (r + u32::from(w)).min(end));
        if a < b {
            wipe((a - start) as usize, (b - a) as usize);
        }
    }
    m
}

/// Rvas of every match in the code sections (executable, not writable).
pub fn find_code(img: &Image, p: &Pattern) -> Vec<u32> {
    let mut out = Vec::new();
    for s in img.code_sections() {
        out.extend(p.find_all(img.raw(s), 0).into_iter().map(|o| s.rva + o as u32));
    }
    out
}

fn filter(img: &Image, cands: &[u32], p: &Pattern) -> Vec<u32> {
    cands.iter().copied().filter(|&c| img.bytes_upto(c, p.len()).is_some_and(|b| p.matches_at(b))).collect()
}

/// Masks of the instructions from `start` on, at most MAX_LEN bytes in all.
fn masks_from(img: &Image, start: u32, relocs: &[(u32, u8)]) -> Result<Vec<Mask>, String> {
    let bytes = img.bytes_upto(start, MAX_LEN + 15).ok_or_else(|| format!("rva 0x{start:x} is not on disk"))?;
    let mut d = Decoder::with_ip(64, bytes, u64::from(start), DecoderOptions::NONE);
    let mut out = Vec::new();
    let mut total = 0;
    let mut ins = Instruction::default();
    while d.can_decode() && total < MAX_LEN {
        d.decode_out(&mut ins);
        if ins.is_invalid() {
            break;
        }
        let co = d.get_constant_offsets(&ins);
        let o = (ins.ip() as u32 - start) as usize;
        let m = wildcard(&bytes[o..o + ins.len()], &ins, &co, relocs, img.image_base, img.size_of_image);
        total += m.len();
        out.push(m);
    }
    Ok(out)
}

/// What another exe says about a pattern.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OtherHits {
    /// Pattern starts (rvas) in the other exe.
    pub pattern_rvas: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Sig {
    /// Where the pattern starts (rva).
    pub start: u32,
    /// The asked rva minus `start`.
    pub target_off: u32,
    pub mask: Mask,
    pub other: Option<OtherHits>,
    /// Pattern grown past the shortest one so the other exe has one hit.
    pub grown_for_other: bool,
}

impl Sig {
    pub fn text(&self) -> String {
        mask_text(&self.mask)
    }

    /// Does the pattern reach past the end of its function into padding or the next function? (Past
    /// the exception entry's end; without an entry, two int3 bytes in a row.) Such a pattern
    /// depends on the neighbour's layout, so a match elsewhere is less sure.
    pub fn past_end(&self, funcs: &Functions) -> bool {
        let end = self.start + self.mask.len() as u32;
        match funcs.entry_at(self.start) {
            Some(e) => end > e.end,
            None => self.mask.windows(2).any(|w| w == [Some(0xCC), Some(0xCC)]),
        }
    }
}

/// Grow from `start` until unique in `img`; then, with `other`, keep growing while the other exe
/// has several hits. Err(count) when even MAX_LEN bytes match several times.
fn from_start(img: &Image, start: u32, relocs: &[(u32, u8)], other: Option<&Image>) -> Result<Result<Sig, usize>, String> {
    let masks = masks_from(img, start, relocs)?;
    let mut pat: Mask = Vec::new();
    let mut cands: Option<Vec<u32>> = None;
    let mut prev: Option<Vec<u32>> = None;
    let mut used = 0;
    for m in &masks {
        if pat.len() + m.len() > MAX_LEN {
            break;
        }
        pat.extend(m);
        used += 1;
        let Some(p) = to_pattern(&pat) else { continue };
        let next = match &cands {
            None => find_code(img, &p),
            Some(c) => filter(img, c, &p),
        };
        prev = cands.replace(next);
        if cands.as_ref().is_some_and(|c| c.len() <= 1) {
            break;
        }
    }
    let hits = cands.as_ref().map_or(0, Vec::len);
    if hits != 1 {
        return Ok(Err(hits));
    }
    // Trim: the shortest prefix that still matches once. Shorter prefixes match more, so stop at
    // the first one that is not unique.
    let full_len = pat.len();
    let last_len = masks[used - 1].len();
    let mut best = full_len;
    for len in (full_len - last_len + 1..full_len).rev() {
        if pat[len - 1].is_none() {
            continue; // Same matches as the next shorter one
        }
        let Some(p) = to_pattern(&pat[..len]) else { break };
        let n = match &prev {
            Some(c) => filter(img, c, &p).len(),
            None => find_code(img, &p).len(),
        };
        if n == 1 {
            best = len;
        } else {
            break;
        }
    }
    let mut mask: Mask = pat[..best].to_vec();
    while mask.last() == Some(&None) {
        mask.pop();
    }
    let mut sig = Sig { start, target_off: 0, mask, other: None, grown_for_other: false };
    if let Some(b) = other {
        let p = to_pattern(&sig.mask).ok_or("empty pattern")?;
        let mut hits = find_code(b, &p);
        if hits.len() > 1 {
            // Grow on with the following instructions while the other exe stays ambiguous.
            let mut grown: Mask = pat.clone();
            for m in &masks[used..] {
                if grown.len() + m.len() > MAX_LEN {
                    break;
                }
                grown.extend(m);
                let Some(p) = to_pattern(&grown) else { continue };
                let h = filter(b, &hits, &p);
                if h.is_empty() {
                    break; // The builds differ from here on: keep the last answer
                }
                hits = h;
                sig.mask = grown.clone();
                sig.grown_for_other = true;
                if hits.len() == 1 {
                    break;
                }
            }
            while sig.mask.last() == Some(&None) {
                sig.mask.pop();
            }
        }
        sig.other = Some(OtherHits { pattern_rvas: hits });
    }
    Ok(Ok(sig))
}

/// Instruction starts in the chunk holding rva, and whether rva is one of them.
fn boundaries(img: &Image, funcs: &Functions, rva: u32) -> Option<(Vec<u32>, bool)> {
    let e = funcs.entry_at(rva)?;
    let bytes = img.bytes_upto(e.begin, (e.end - e.begin) as usize)?;
    let mut d = Decoder::with_ip(64, bytes, u64::from(e.begin), DecoderOptions::NONE);
    let mut v = Vec::new();
    let mut ins = Instruction::default();
    while d.can_decode() {
        let ip = d.ip() as u32;
        if ip > rva {
            break;
        }
        v.push(ip);
        d.decode_out(&mut ins);
    }
    let ok = v.last() == Some(&rva);
    Some((v, ok))
}

/// The shortest unique pattern for the instruction at rva (see the module docs).
pub fn make(img: &Image, funcs: &Functions, relocs: &[(u32, u8)], rva: u32, other: Option<&Image>) -> Result<Sig, String> {
    crate::disasm::check_code(img, rva)?;
    let bounds = boundaries(img, funcs, rva);
    if let Some((v, false)) = &bounds {
        let around = v.last().copied().unwrap_or(rva);
        return Err(format!("rva 0x{rva:x} is not an instruction start (decoding its function chunk gives one at rva 0x{around:x})"));
    }
    let first = match from_start(img, rva, relocs, other)? {
        Ok(s) => return Ok(s),
        Err(n) => n,
    };
    // Earlier starts in the same chunk, nearest first.
    if let Some((v, _)) = bounds {
        for &s in v.iter().rev().skip(1).take_while(|&&s| rva - s <= MAX_BACK) {
            if let Ok(mut sig) = from_start(img, s, relocs, other)? {
                // The pattern must still reach the asked instruction.
                if sig.mask.len() as u32 > rva - s {
                    sig.target_off = rva - s;
                    return Ok(sig);
                }
            }
        }
    }
    Err(format!("no unique pattern within {MAX_LEN} bytes from rva 0x{rva:x} or up to {MAX_BACK} bytes before it ({first} matches from the rva itself)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(bytes: &[u8], ip: u64, relocs: &[(u32, u8)]) -> String {
        let mut d = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
        let ins = d.decode();
        let co = d.get_constant_offsets(&ins);
        mask_text(&wildcard(&bytes[..ins.len()], &ins, &co, relocs, 0x1_4000_0000, 0x2000_0000))
    }

    #[test]
    fn wildcards_what_moves_between_builds() {
        // Field offsets stay; rel32 calls and RIP-relative displacements go.
        assert_eq!(one(&[0x45, 0x88, 0xBD, 0x99, 0x02, 0x00, 0x00], 0x1000, &[]), "45 88 BD 99 02 00 00");
        assert_eq!(one(&[0xE8, 0x10, 0x20, 0x30, 0x00], 0x1000, &[]), "E8 ?? ?? ?? ??");
        assert_eq!(one(&[0x48, 0x8B, 0x05, 0x11, 0x22, 0x33, 0x00], 0x1000, &[]), "48 8B 05 ?? ?? ?? ??");
        // Short jumps stay.
        assert_eq!(one(&[0x75, 0x0C], 0x1000, &[]), "75 0C");
        // Cmp byte [rip+x],1: displacement goes, the immediate stays.
        assert_eq!(one(&[0x80, 0x3D, 0x10, 0x00, 0x00, 0x00, 0x01], 0x1000, &[]), "80 3D ?? ?? ?? ?? 01");
        // Mov eax,[r8+rcx*4+2D00000h]: a table rva in a disp32 goes; a small one stays.
        assert_eq!(one(&[0x41, 0x8B, 0x84, 0x88, 0x00, 0x00, 0xD0, 0x02], 0x1000, &[]), "41 8B 84 88 ?? ?? ?? ??");
        assert_eq!(one(&[0x8B, 0x81, 0x50, 0x02, 0x00, 0x00], 0x1000, &[]), "8B 81 50 02 00 00");
        // Mov rax, imm64 that is an address in the image (relocated): all 8 bytes go.
        assert_eq!(one(&[0x48, 0xB8, 0x00, 0x10, 0x00, 0x40, 0x01, 0x00, 0x00, 0x00], 0x1000, &[]), "48 B8 ?? ?? ?? ?? ?? ?? ?? ??");
        // A relocation over the last 4 bytes of an instruction.
        assert_eq!(one(&[0xC7, 0x01, 0x05, 0x00, 0x00, 0x00], 0x1000, &[(0x1002, 4)]), "C7 01 ?? ?? ?? ??");
    }

    #[test]
    fn finds_shortest_unique_and_maps_to_another_build() {
        // Two copies of "mov [r13+299h],r15b" in A; the one followed by "ret" is the target.
        let mut a = vec![0x45, 0x88, 0xBD, 0x99, 0x02, 0x00, 0x00, 0x90];
        a.extend([0x45, 0x88, 0xBD, 0x99, 0x02, 0x00, 0x00, 0xC3]);
        let img_a = crate::image::tests::tiny_pe(&a, &[]);
        let funcs = Functions { entries: Vec::new() };
        let sig = make(&img_a, &funcs, &[], 0x1008, None).unwrap();
        assert_eq!(sig.text(), "45 88 BD 99 02 00 00 C3");
        assert_eq!(sig.target_off, 0);
        // B has the same code 4 bytes later.
        let mut b = vec![0xCC; 4];
        b.extend(&a);
        let img_b = crate::image::tests::tiny_pe(&b, &[]);
        let sig = make(&img_a, &funcs, &[], 0x1008, Some(&img_b)).unwrap();
        assert_eq!(sig.other.unwrap().pattern_rvas, vec![0x100C]);
    }
}
