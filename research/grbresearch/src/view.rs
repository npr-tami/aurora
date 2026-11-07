//! Ways to look at a block of memory: hex dump, the 8-byte row view, and the object view that
//! labels each qword (pointer into an image, pointer to heap, small int, float).

use crate::diff::float_like;
use crate::proc::Target;
use crate::types::{fmt_f32, fmt_f64, hex, raw_of};

/// Where a pointer value lands, if it lands in committed memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointsTo {
    pub image: bool,
    pub code: bool,
    pub label: String,
}

pub fn points_to(t: &Target, v: u64) -> Option<PointsTo> {
    let label = t.points_to(v)?;
    let image = t.module_at(v).is_some();
    let code = t.query(v).is_some_and(|r| r.executable());
    Some(PointsTo { image, code, label })
}

/// What a qword most likely is. Plain heuristics: "unknown" when nothing fits.
pub fn qword_kind(raw: u64, p: Option<&PointsTo>) -> String {
    if raw == 0 {
        return "zero".into();
    }
    if let Some(p) = p {
        let what = if p.image { "ptr-to-image" } else { "ptr-to-heap" };
        let code = if p.code { " (code)" } else { "" };
        let unaligned = if !raw.is_multiple_of(8) { " (unaligned)" } else { "" };
        return format!("{what} {}{code}{unaligned}", p.label);
    }
    let i = raw as i64;
    if i.unsigned_abs() < 0x10000 {
        return format!("small-int {i}");
    }
    let (lo, hi) = (raw as u32 as i32, (raw >> 32) as u32 as i32);
    if lo.unsigned_abs() < 0x10000 && hi.unsigned_abs() < 0x10000 {
        return format!("small-ints {lo}, {hi}");
    }
    // A pair of f32 also reads as some f64; a zero low half points to a real f64 (1234.5).
    let d = f64::from_bits(raw);
    let f64_ok = float_like(d) && d.abs() >= 1e-3;
    let (a, b) = (f32::from_bits(raw as u32), f32::from_bits((raw >> 32) as u32));
    let real = |x: f32| x == 0.0 || (1e-3..1e7).contains(&x.abs());
    if raw as u32 != 0 && real(a) && real(b) {
        return format!("float-like f32 {}, {}", fmt_f32(a), fmt_f32(b));
    }
    if f64_ok {
        return format!("float-like f64 {}", fmt_f64(d));
    }
    "unknown".into()
}

/// Classic 16-per-row hex + ASCII. Bytes that could not be read show as "??".
pub fn dump_lines(addr: u64, data: &[u8], len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut off = 0;
    while off < len {
        let mut hexs = String::new();
        let mut ascii = String::new();
        for i in off..(off + 16).min(len) {
            match data.get(i) {
                Some(&b) => {
                    hexs.push_str(&format!("{b:02X} "));
                    ascii.push(if (0x20..0x7F).contains(&b) { b as char } else { '.' });
                }
                None => {
                    hexs.push_str("?? ");
                    ascii.push(' ');
                }
            }
            if i % 16 == 7 {
                hexs.push(' ');
            }
        }
        out.push(format!("{:016X}  +{off:04X}  {hexs:<49} {ascii}", addr + off as u64));
        off += 16;
    }
    out
}

/// One `view` row for 8 bytes.
pub fn view_row(off: usize, b: &[u8], ptr: &str) -> String {
    if b.len() < 8 {
        return format!("+{off:04X}  {:<23}  (only {} bytes)", hex(b), b.len());
    }
    let raw = raw_of(b);
    format!(
        "+{off:04X}  {}  {:<28} u64 {:<20} i32 {} {}  f32 {} {}  f64 {}",
        hex(b),
        ptr,
        raw,
        raw as u32 as i32,
        (raw >> 32) as u32 as i32,
        fmt_f32(f32::from_bits(raw as u32)),
        fmt_f32(f32::from_bits((raw >> 32) as u32)),
        fmt_f64(f64::from_bits(raw)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_qwords() {
        let img = PointsTo { image: true, code: false, label: "GRB_vulkan.exe+0x49BFD70".into() };
        assert_eq!(qword_kind(0x7FF6_0000_0000, Some(&img)), "ptr-to-image GRB_vulkan.exe+0x49BFD70");
        let heap = PointsTo { image: false, code: false, label: "heap rw-".into() };
        assert_eq!(qword_kind(0x2000_0000, Some(&heap)), "ptr-to-heap heap rw-");
        assert_eq!(qword_kind(0x2000_0004, Some(&heap)), "ptr-to-heap heap rw- (unaligned)");
        assert_eq!(qword_kind(0, None), "zero");
        assert_eq!(qword_kind(42, None), "small-int 42");
        assert_eq!(qword_kind((-3i64) as u64, None), "small-int -3");
        assert_eq!(qword_kind(0x0001_0000_0001, None), "small-ints 1, 1");
        assert_eq!(qword_kind(1234.5f64.to_bits(), None), "float-like f64 1234.5");
        let two = (1.5f32.to_bits() as u64) | ((2.0f32.to_bits() as u64) << 32);
        assert_eq!(qword_kind(two, None), "float-like f32 1.5, 2");
        assert_eq!(qword_kind(0xDEAD_BEEF_DEAD_BEEF, None), "unknown");
    }

    #[test]
    fn dump_marks_unread() {
        let l = dump_lines(0x1000, b"AB", 4);
        assert_eq!(l.len(), 1);
        assert!(l[0].contains("41 42 ?? ??"), "{}", l[0]);
        assert!(l[0].ends_with("AB  "));
        assert!(view_row(0, &[1, 0, 0, 0, 0, 0, 0, 0], "not a pointer").contains("u64 1 "));
    }
}
