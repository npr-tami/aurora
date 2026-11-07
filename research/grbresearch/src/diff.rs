//! Compare two byte blocks field by field: which offsets changed, old and new raw bytes, and what
//! they mean as numbers (floats shown beside raw).

use crate::types::{self, VType, fmt_f32, hex, raw_of};

/// How to cut the bytes into fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum As {
    /// 4-byte fields, shown as i32 and f32.
    Auto,
    Type(VType),
}

impl As {
    pub fn parse(s: &str) -> Result<As, String> {
        if s.eq_ignore_ascii_case("auto") {
            return Ok(As::Auto);
        }
        let t = VType::parse(s)?;
        if !t.numeric() {
            return Err("--as wants a number type or auto".into());
        }
        Ok(As::Type(t))
    }

    pub fn width(self) -> usize {
        match self {
            As::Auto => 4,
            As::Type(t) => t.size(),
        }
    }

    /// One value as text.
    pub fn show(self, b: &[u8]) -> String {
        let raw = raw_of(b);
        match self {
            As::Auto => {
                let i = raw as u32 as i32;
                let f = f32::from_bits(raw as u32);
                if float_like(f as f64) { format!("f32 {} (i32 {i})", fmt_f32(f)) } else { format!("i32 {i} (f32 {})", fmt_f32(f)) }
            }
            As::Type(t) => types::format_one(t, raw),
        }
    }
}

/// A float that looks like real data (not tiny garbage or a huge exponent).
pub fn float_like(v: f64) -> bool {
    v.is_finite() && (v == 0.0 || (1e-4..1e7).contains(&v.abs()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub off: usize,
    pub old: Vec<u8>,
    pub new: Vec<u8>,
}

/// Changed fields of `width` bytes, comparing only where both blocks have data.
pub fn diff(a: &[u8], b: &[u8], width: usize) -> Vec<Change> {
    let n = a.len().min(b.len());
    let mut out = Vec::new();
    let mut off = 0;
    while off + width <= n {
        if a[off..off + width] != b[off..off + width] {
            out.push(Change { off, old: a[off..off + width].to_vec(), new: b[off..off + width].to_vec() });
        }
        off += width;
    }
    out
}

/// "+0x20  00 00 C8 42 -> 00 00 CA 42   f32 100 -> 101".
pub fn line(c: &Change, as_: As) -> String {
    format!("+0x{:04X}  {} -> {}   {} -> {}", c.off, hex(&c.old), hex(&c.new), as_.show(&c.old), as_.show(&c.new))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_changed_fields() {
        let mut a = vec![0u8; 16];
        a[4..8].copy_from_slice(&100f32.to_le_bytes());
        let mut b = a.clone();
        b[4..8].copy_from_slice(&101f32.to_le_bytes());
        b[12] = 7;
        let c = diff(&a, &b, 4);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].off, 4);
        assert_eq!(c[1].off, 12);
        let l = line(&c[0], As::Auto);
        assert!(l.contains("f32 100 ") && l.contains("f32 101 "), "{l}");
        assert!(line(&c[1], As::Auto).contains("i32 0") && line(&c[1], As::Auto).contains("i32 7"));
        assert_eq!(diff(&a, &b, 8).len(), 2);
        assert_eq!(diff(&a, &b[..8], 4).len(), 1, "only where both have data");
        assert!(diff(&a, &a, 4).is_empty());
        assert_eq!(As::parse("f64").unwrap().width(), 8);
        assert!(As::parse("str").is_err());
    }
}
