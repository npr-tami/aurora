//! x64 decoding and Intel-syntax text (iced-x86) with names for what an instruction points at.
//!
//! Instructions are decoded with ip = rva, so every address iced computes (branch targets,
//! RIP-relative operands) is an rva. In the text a function start shows as `sub_<rva>` and any
//! other target as `rva 0x...`; the note column says what the target holds: a string, an RTTI
//! vtable, a COL or type descriptor, an import slot, or the protected section. Only code sections
//! are ever decoded.

use std::collections::HashMap;
use std::rc::Rc;

use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, MemorySizeOptions, OpKind, SymbolResolver, SymbolResult};

use crate::image::{Image, Role};
use crate::rtti::Rtti;

/// One decoded instruction, ready to print.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Line {
    pub rva: u32,
    pub bytes: String,
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Names branch targets and RIP-relative operands: `sub_<rva>` for function starts, `rva 0x...` for
/// anything else.
struct Names {
    starts: Rc<Vec<u32>>,
}

impl SymbolResolver for Names {
    fn symbol(&mut self, ins: &Instruction, _operand: u32, ins_operand: Option<u32>, address: u64, _size: u32) -> Option<SymbolResult<'_>> {
        let k = ins.op_kind(ins_operand?);
        let wanted = matches!(k, OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) || (k == OpKind::Memory && ins.is_ip_rel_memory_operand());
        if !wanted {
            return None;
        }
        Some(SymbolResult::with_string(address, symbol_name(&self.starts, address)))
    }
}

pub fn symbol_name(starts: &[u32], address: u64) -> String {
    if address <= u64::from(u32::MAX) && starts.binary_search(&(address as u32)).is_ok() {
        format!("sub_{address:x}")
    } else {
        format!("rva 0x{address:x}")
    }
}

pub struct Disasm<'a> {
    pub img: &'a Image,
    pub rtti: Option<&'a Rtti>,
    pub imports: HashMap<u32, String>,
    pub starts: Rc<Vec<u32>>,
    fmt: IntelFormatter,
}

impl<'a> Disasm<'a> {
    pub fn new(img: &'a Image, starts: Vec<u32>, rtti: Option<&'a Rtti>) -> Disasm<'a> {
        let starts = Rc::new(starts);
        let mut fmt = IntelFormatter::with_options(Some(Box::new(Names { starts: starts.clone() })), None);
        fmt.options_mut().set_memory_size_options(MemorySizeOptions::Always);
        Disasm { img, rtti, imports: img.imports().slots, starts, fmt }
    }

    /// Decode from `start` until `end` (exclusive) or `count` instructions, whichever comes first.
    /// Only code sections; the protected section is refused.
    pub fn decode(&self, start: u32, end: Option<u32>, count: usize) -> Result<Vec<Instruction>, String> {
        let s = check_code(self.img, start)?;
        let stop = end.unwrap_or(u32::MAX).min(s.rva + s.disk_len());
        let max = stop.saturating_sub(start) as usize;
        let bytes = self.img.bytes_upto(start, max).ok_or_else(|| format!("rva 0x{start:x} is not on disk"))?;
        let mut d = Decoder::with_ip(64, bytes, u64::from(start), DecoderOptions::NONE);
        let mut out = Vec::new();
        let mut ins = Instruction::default();
        while d.can_decode() && out.len() < count {
            d.decode_out(&mut ins);
            out.push(ins);
        }
        Ok(out)
    }

    pub fn text(&mut self, ins: &Instruction) -> String {
        let mut t = String::new();
        self.fmt.format(ins, &mut t);
        t
    }

    pub fn line(&mut self, ins: &Instruction) -> Line {
        let rva = ins.ip() as u32;
        let raw = self.img.bytes(rva, ins.len()).unwrap_or_default();
        let mut bytes: Vec<String> = raw.iter().take(10).map(|b| format!("{b:02X}")).collect();
        if raw.len() > 10 {
            bytes.push("..".into());
        }
        let text = if ins.is_invalid() { "(bad)".to_string() } else { self.text(ins) };
        Line { rva, bytes: bytes.join(" "), text, note: self.note(ins) }
    }

    /// What the instruction's target holds, if anything worth saying.
    pub fn note(&self, ins: &Instruction) -> String {
        let k0 = if ins.op_count() > 0 { ins.op0_kind() } else { OpKind::Register };
        if matches!(k0, OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64) {
            let t = ins.near_branch_target();
            return if t <= u64::from(u32::MAX) && self.img.in_protected(t as u32) { "into the protected section".into() } else { String::new() };
        }
        if ins.is_ip_rel_memory_operand() {
            let t = ins.ip_rel_memory_address();
            if t <= u64::from(u32::MAX) {
                return self.describe(t as u32);
            }
        }
        String::new()
    }

    /// What lives at an rva: import slot, RTTI piece, string, protected section.
    pub fn describe(&self, t: u32) -> String {
        if let Some(n) = self.imports.get(&t) {
            return format!("import {n}");
        }
        if let Some(r) = self.rtti {
            if let Some(&i) = r.by_vtable.get(&t) {
                return format!("vtable {}", r.classes[i].name);
            }
            if let Some(&i) = r.by_col.get(&t) {
                return format!("RTTI COL {}", r.classes[i].name);
            }
            if let Some(&i) = r.by_td.get(&t) {
                return format!("type descriptor {}", r.classes[i].name);
            }
        }
        if self.img.in_protected(t) {
            return "protected section".into();
        }
        if let Some(s) = string_at(self.img, t) {
            return s;
        }
        // A pointer to code (a function pointer, or the first slot of a table without RTTI).
        if !self.img.in_code(t)
            && let Some(p) = self.img.u64_at(t).and_then(|q| self.img.ptr_to_rva(q))
            && self.img.section_of(p).is_some_and(|s| s.exec())
        {
            return format!("holds a pointer to {}", symbol_name(&self.starts, u64::from(p)));
        }
        String::new()
    }
}

/// The section of rva when it is code we may decode; a message otherwise.
pub fn check_code(img: &Image, rva: u32) -> Result<&crate::image::Section, String> {
    match img.section_of(rva) {
        Some(s) if s.role() == Role::Code => Ok(s),
        Some(s) if s.role() == Role::Protected => Err(format!("rva 0x{rva:x} is in section #{} (writable + executable: protected, skipped)", s.index)),
        Some(s) => Err(format!("rva 0x{rva:x} is in section #{} ({}), not code", s.index, s.role().label())),
        None => Err(format!("rva 0x{rva:x} is outside every section")),
    }
}

fn printable(c: u8) -> bool {
    (0x20..=0x7E).contains(&c) || matches!(c, b'\t' | b'\n' | b'\r')
}

/// Quoted text for a byte string: escapes, cut at 80 characters.
pub fn quote(b: &[u8]) -> String {
    let mut s = String::from("\"");
    for &c in b.iter().take(80) {
        match c {
            b'"' => s.push_str("\\\""),
            b'\\' => s.push_str("\\\\"),
            b'\n' => s.push_str("\\n"),
            b'\r' => s.push_str("\\r"),
            b'\t' => s.push_str("\\t"),
            c => s.push(c as char),
        }
    }
    if b.len() > 80 {
        s.push_str("...");
    }
    s.push('"');
    s
}

/// A string literal at rva: ASCII (at least 4 printable bytes, then NUL) or UTF-16LE (at least 4
/// printable characters, then a 0 character). UTF-16 shows as u"...".
pub fn string_at(img: &Image, rva: u32) -> Option<String> {
    let b = img.bytes_upto(rva, 512)?;
    string_in(b)
}

pub fn string_in(b: &[u8]) -> Option<String> {
    let n = b.iter().take_while(|&&c| printable(c)).count();
    if n >= 4 && (b.get(n) == Some(&0) || n >= 256) {
        return Some(quote(&b[..n]));
    }
    let mut k = 0;
    while k + 1 < b.len() && printable(b[k]) && b[k + 1] == 0 {
        k += 2;
    }
    if k / 2 >= 4 && ((b.get(k) == Some(&0) && b.get(k + 1) == Some(&0)) || k / 2 >= 200) {
        let ascii: Vec<u8> = b[..k].iter().step_by(2).copied().collect();
        return Some(format!("u{}", quote(&ascii)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_one(bytes: &[u8], ip: u64) -> (Instruction, iced_x86::ConstantOffsets) {
        let mut d = Decoder::with_ip(64, bytes, ip, DecoderOptions::NONE);
        let ins = d.decode();
        let co = d.get_constant_offsets(&ins);
        (ins, co)
    }

    #[test]
    fn rip_targets_are_rvas() {
        // Mov rax,[rip+0x100] at rva 0x1000: 7 bytes, target = 0x1007 + 0x100.
        let (ins, co) = decode_one(&[0x48, 0x8B, 0x05, 0x00, 0x01, 0x00, 0x00], 0x1000);
        assert!(ins.is_ip_rel_memory_operand());
        assert_eq!(ins.ip_rel_memory_address(), 0x1107);
        assert_eq!((co.displacement_offset(), co.displacement_size()), (3, 4));
        // RIP-relative with an immediate after the displacement: cmp byte [rip+0x10],1.
        let (ins, _) = decode_one(&[0x80, 0x3D, 0x10, 0x00, 0x00, 0x00, 0x01], 0x2000);
        assert_eq!(ins.ip_rel_memory_address(), 0x2007 + 0x10);
        // Call rel32 backwards: E8 FB FF FF FF at 0x3000 -> 0x3000.
        let (ins, co) = decode_one(&[0xE8, 0xFB, 0xFF, 0xFF, 0xFF], 0x3000);
        assert_eq!(ins.near_branch_target(), 0x3000);
        assert_eq!((co.immediate_offset(), co.immediate_size()), (1, 4));
    }

    #[test]
    fn formats_like_the_logs_and_names_targets() {
        let (ins, _) = decode_one(&[0x45, 0x88, 0xBD, 0x99, 0x02, 0x00, 0x00], 0x11bb332);
        let mut f = IntelFormatter::with_options(Some(Box::new(Names { starts: Rc::new(vec![0x3000]) })), None);
        f.options_mut().set_memory_size_options(MemorySizeOptions::Always);
        let mut t = String::new();
        f.format(&ins, &mut t);
        assert_eq!(t, "mov byte ptr [r13+299h],r15b");
        let (ins, _) = decode_one(&[0xE8, 0xFB, 0xFF, 0xFF, 0xFF], 0x3000);
        t.clear();
        f.format(&ins, &mut t);
        assert_eq!(t, "call sub_3000");
        let (ins, _) = decode_one(&[0x48, 0x8D, 0x0D, 0x00, 0x01, 0x00, 0x00], 0x1000);
        t.clear();
        f.format(&ins, &mut t);
        assert_eq!(t, "lea rcx,[rva 0x1107]");
    }

    #[test]
    fn finds_string_literals() {
        assert_eq!(string_in(b"Hello\0junk").as_deref(), Some("\"Hello\""));
        assert_eq!(string_in(b"Hi\0").as_deref(), None);
        assert_eq!(string_in(b"H\0e\0l\0l\0o\0\0\0").as_deref(), Some("u\"Hello\""));
        assert_eq!(string_in(b"a\"b\nc\0").as_deref(), Some("\"a\\\"b\\nc\""));
        assert_eq!(string_in(&[0x90, 0x90, 0xC3]), None);
    }
}
