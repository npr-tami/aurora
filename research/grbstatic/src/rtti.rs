//! MSVC RTTI in the exe: Complete Object Locators (COL), type descriptors, class hierarchies and
//! the vtables that point at them.
//!
//! x64 COL: signature (1), offset of this vtable in the class, cd offset, type descriptor rva,
//! class hierarchy descriptor rva, rva of the COL itself. A COL is found by that self rva (+20)
//! matching its own position. The vtable sits right after an 8-byte pointer to the COL (imagebase +
//! COL rva) in a data section. Protected sections are never scanned.

use std::collections::HashMap;

use serde::Serialize;

use crate::demangle::demangle;
use crate::image::{Image, Role};

#[derive(Debug, Clone, Serialize)]
pub struct Class {
    pub name: String,
    pub raw: String,
    pub col_rva: u32,
    pub type_descriptor_rva: u32,
    pub hierarchy_rva: u32,
    /// Offset of this vtable's sub-object inside the complete class (COL offset field).
    pub subobject_offset: u32,
    /// Vtables that use this COL (normally one).
    pub vtable_rvas: Vec<u32>,
    /// Slots of the first vtable: consecutive pointers into executable sections.
    pub methods: usize,
    /// Base classes from the Base Class Array (all ancestors, nearest first as listed).
    pub bases: Vec<String>,
    /// Hierarchy attributes: 1 = multiple inheritance, 2 = virtual inheritance.
    pub attributes: u32,
}

pub struct Rtti {
    /// Positions passing the COL check (signature 1, self rva matches).
    pub cols_found: usize,
    /// COLs whose type descriptor name could be read (".?A...").
    pub classes: Vec<Class>,
    /// Vtable rva -> index into classes.
    pub by_vtable: HashMap<u32, usize>,
    pub by_col: HashMap<u32, usize>,
    pub by_td: HashMap<u32, usize>,
}

/// One vtable slot.
pub struct Slot {
    pub index: usize,
    pub at_rva: u32,
    pub target_rva: u32,
}

impl Rtti {
    pub fn scan(img: &Image) -> Rtti {
        // 1. COLs: 4-aligned positions with signature 1 whose self rva is their own rva.
        let mut cols = Vec::new();
        for s in img.open_sections() {
            let raw = img.raw(s);
            let mut o = 0usize;
            while o + 24 <= raw.len() {
                if raw[o..o + 4] == [1, 0, 0, 0] {
                    let me = s.rva + o as u32;
                    if u32::from_le_bytes([raw[o + 20], raw[o + 21], raw[o + 22], raw[o + 23]]) == me {
                        cols.push(me);
                    }
                }
                o += 4;
            }
        }
        let cols_found = cols.len();

        // 2. Read each COL's type descriptor and hierarchy.
        let mut classes = Vec::new();
        for &col in &cols {
            let (Some(off), Some(td), Some(chd)) = (img.u32_at(col + 4), img.u32_at(col + 12), img.u32_at(col + 16)) else { continue };
            let Some(raw) = img.cstr(td.wrapping_add(16), 2048) else { continue };
            if !raw.starts_with(b".?A") {
                continue;
            }
            let raw = String::from_utf8_lossy(raw).into_owned();
            let (attributes, bases) = hierarchy(img, chd);
            classes.push(Class {
                name: demangle(&raw),
                raw,
                col_rva: col,
                type_descriptor_rva: td,
                hierarchy_rva: chd,
                subobject_offset: off,
                vtable_rvas: Vec::new(),
                methods: 0,
                bases,
                attributes,
            });
        }

        // 3. Vtables: 8-aligned pointers to a COL in non-executable sections, +8.
        let by_col: HashMap<u32, usize> = classes.iter().enumerate().map(|(i, c)| (c.col_rva, i)).collect();
        if let (Some(lo), Some(hi)) = (classes.iter().map(|c| c.col_rva).min(), classes.iter().map(|c| c.col_rva).max()) {
            let (lo, hi) = (img.va(lo), img.va(hi));
            for s in img.data_sections() {
                let raw = img.raw(s);
                let mut o = (8 - s.rva as usize % 8) % 8;
                while o + 8 <= raw.len() {
                    let q = u64::from_le_bytes(raw[o..o + 8].try_into().unwrap_or_default());
                    if q >= lo && q <= hi
                        && let Some(&i) = by_col.get(&((q - img.image_base) as u32))
                    {
                        classes[i].vtable_rvas.push(s.rva + o as u32 + 8);
                    }
                    o += 8;
                }
            }
        }
        for c in &mut classes {
            if let Some(&vt) = c.vtable_rvas.first() {
                c.methods = slots(img, vt).len();
            }
        }
        let mut by_vtable = HashMap::new();
        for (i, c) in classes.iter().enumerate() {
            for &v in &c.vtable_rvas {
                by_vtable.insert(v, i);
            }
        }
        let by_td = classes.iter().enumerate().map(|(i, c)| (c.type_descriptor_rva, i)).collect();
        Rtti { cols_found, classes, by_vtable, by_col, by_td }
    }

    /// The class and slot index whose vtable slot sits at `at` (an rva inside a vtable).
    pub fn slot_at(&self, at: u32) -> Option<(&Class, usize)> {
        // Vtables are short; look back at most 4096 slots for a vtable start.
        for k in 0..4096u32 {
            let start = at.checked_sub(k * 8)?;
            if let Some(&i) = self.by_vtable.get(&start) {
                // `methods` counts the first vtable; other vtables of one COL are copies.
                let c = &self.classes[i];
                return ((k as usize) < c.methods).then_some((c, k as usize));
            }
        }
        None
    }

    /// Every (class, vtable, slot) whose slot points at `target`.
    pub fn slots_pointing_at(&self, img: &Image, target: u32) -> Vec<(usize, u32, usize)> {
        let mut out = Vec::new();
        for (i, c) in self.classes.iter().enumerate() {
            for &vt in &c.vtable_rvas {
                for s in slots(img, vt) {
                    if s.target_rva == target {
                        out.push((i, vt, s.index));
                    }
                }
            }
        }
        out
    }
}

/// Hierarchy attributes and base class names (the Base Class Array without its first entry, which
/// is the class itself).
fn hierarchy(img: &Image, chd: u32) -> (u32, Vec<String>) {
    let (Some(attrs), Some(n), Some(bca)) = (img.u32_at(chd + 4), img.u32_at(chd + 8), img.u32_at(chd + 12)) else {
        return (0, Vec::new());
    };
    let mut bases = Vec::new();
    for k in 1..n.min(256) {
        let Some(bcd) = img.u32_at(bca + k * 4) else { break };
        let Some(td) = img.u32_at(bcd) else { break };
        match img.cstr(td.wrapping_add(16), 2048) {
            Some(raw) if raw.starts_with(b".?A") => bases.push(demangle(&String::from_utf8_lossy(raw))),
            _ => bases.push(format!("? (type descriptor rva 0x{td:x})")),
        }
    }
    (attrs, bases)
}

fn is_code_ptr(img: &Image, at: u32) -> bool {
    img.u64_at(at).and_then(|q| img.ptr_to_rva(q)).and_then(|t| img.section_of(t)).is_some_and(|s| matches!(s.role(), Role::Code | Role::Protected))
}

/// The run of consecutive 8-byte code pointers holding `at`: (first rva, length, index of `at`).
/// Without RTTI this is the best guess at a vtable (most classes here have no COL).
pub fn code_ptr_run(img: &Image, at: u32) -> Option<(u32, usize, usize)> {
    if !is_code_ptr(img, at) {
        return None;
    }
    let mut s = at;
    while s >= 8 && at - s < 4096 * 8 && is_code_ptr(img, s - 8) {
        s -= 8;
    }
    let mut e = at + 8;
    while e - s < 4096 * 8 && is_code_ptr(img, e) {
        e += 8;
    }
    Some((s, ((e - s) / 8) as usize, ((at - s) / 8) as usize))
}

/// Slots of a vtable: consecutive 8-byte pointers into executable sections (code or protected; the
/// protected ones are listed but never decoded). At most 4096.
pub fn slots(img: &Image, vtable: u32) -> Vec<Slot> {
    let mut out = Vec::new();
    for k in 0..4096u32 {
        let at = vtable + k * 8;
        let Some(q) = img.u64_at(at) else { break };
        let Some(t) = img.ptr_to_rva(q) else { break };
        if !img.section_of(t).is_some_and(|s| matches!(s.role(), Role::Code | Role::Protected)) {
            break;
        }
        out.push(Slot { index: k as usize, at_rva: at, target_rva: t });
    }
    out
}
