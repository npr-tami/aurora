//! Value types for read, scan, diff and watch, and how each one prints.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VType {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    Ptr,
    Bytes,
    Str,
    WStr,
}

pub const TYPE_NAMES: &str = "u8 i8 u16 i16 u32 i32 u64 i64 f32 f64 ptr bytes str wstr";

impl VType {
    pub fn parse(s: &str) -> Result<VType, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "u8" => VType::U8,
            "i8" => VType::I8,
            "u16" => VType::U16,
            "i16" => VType::I16,
            "u32" => VType::U32,
            "i32" => VType::I32,
            "u64" => VType::U64,
            "i64" => VType::I64,
            "f32" | "float" => VType::F32,
            "f64" | "double" => VType::F64,
            "ptr" | "pointer" => VType::Ptr,
            "bytes" => VType::Bytes,
            "str" => VType::Str,
            "wstr" => VType::WStr,
            _ => return Err(format!("unknown type '{s}' (want one of: {TYPE_NAMES})")),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            VType::U8 => "u8",
            VType::I8 => "i8",
            VType::U16 => "u16",
            VType::I16 => "i16",
            VType::U32 => "u32",
            VType::I32 => "i32",
            VType::U64 => "u64",
            VType::I64 => "i64",
            VType::F32 => "f32",
            VType::F64 => "f64",
            VType::Ptr => "ptr",
            VType::Bytes => "bytes",
            VType::Str => "str",
            VType::WStr => "wstr",
        }
    }

    /// Size of one element in bytes (one character for strings).
    pub fn size(self) -> usize {
        match self {
            VType::U8 | VType::I8 | VType::Bytes | VType::Str => 1,
            VType::U16 | VType::I16 | VType::WStr => 2,
            VType::U32 | VType::I32 | VType::F32 => 4,
            VType::U64 | VType::I64 | VType::F64 | VType::Ptr => 8,
        }
    }

    /// Numbers (and pointers): the types value scans and `next` work with.
    pub fn numeric(self) -> bool {
        !matches!(self, VType::Bytes | VType::Str | VType::WStr)
    }

    pub fn float(self) -> bool {
        matches!(self, VType::F32 | VType::F64)
    }
}

/// Little-endian bytes (up to 8) as a zero-extended u64.
pub fn raw_of(b: &[u8]) -> u64 {
    let mut a = [0u8; 8];
    let n = b.len().min(8);
    a[..n].copy_from_slice(&b[..n]);
    u64::from_le_bytes(a)
}

/// A number decoded from raw bits, for comparing.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Num {
    I(i128),
    F(f64),
}

pub fn num(t: VType, raw: u64) -> Num {
    match t {
        VType::U8 => Num::I((raw as u8) as i128),
        VType::I8 => Num::I((raw as u8 as i8) as i128),
        VType::U16 => Num::I((raw as u16) as i128),
        VType::I16 => Num::I((raw as u16 as i16) as i128),
        VType::U32 => Num::I((raw as u32) as i128),
        VType::I32 => Num::I((raw as u32 as i32) as i128),
        VType::I64 => Num::I((raw as i64) as i128),
        VType::F32 => Num::F(f32::from_bits(raw as u32) as f64),
        VType::F64 => Num::F(f64::from_bits(raw)),
        _ => Num::I(raw as i128),
    }
}

/// One element as text.
pub fn format_one(t: VType, raw: u64) -> String {
    match t {
        VType::F32 => fmt_f32(f32::from_bits(raw as u32)),
        VType::F64 => fmt_f64(f64::from_bits(raw)),
        VType::Ptr => format!("0x{raw:X}"),
        VType::Bytes => format!("{:02X}", raw as u8),
        _ => match num(t, raw) {
            Num::I(i) => i.to_string(),
            Num::F(f) => f.to_string(),
        },
    }
}

pub fn fmt_f32(v: f32) -> String {
    if v.is_finite() && v != 0.0 && (v.abs() >= 1e7 || v.abs() < 1e-4) { format!("{v:e}") } else { format!("{v}") }
}

pub fn fmt_f64(v: f64) -> String {
    if v.is_finite() && v != 0.0 && (v.abs() >= 1e12 || v.abs() < 1e-6) { format!("{v:e}") } else { format!("{v}") }
}

/// Bytes as "48 8B 05".
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

/// Strings stop at the first NUL; everything else prints `count` elements.
pub fn format_values(t: VType, b: &[u8]) -> Vec<String> {
    match t {
        VType::Bytes => vec![hex(b)],
        VType::Str => {
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            vec![format!("{:?}", String::from_utf8_lossy(&b[..end]))]
        }
        VType::WStr => {
            let w: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
            vec![format!("{:?}", String::from_utf16_lossy(&w[..end]))]
        }
        _ => b.chunks_exact(t.size()).map(|c| format_one(t, raw_of(c))).collect(),
    }
}

/// A scan or compare target: raw bits plus, for floats, how close counts as equal.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Target {
    pub raw: u64,
    pub tol: f64,
}

/// Parse a value the user typed. Integers are decimal, or hex with 0x. Floats match within half a
/// unit of the last typed digit ("12.5" matches 12.45..12.55), like CE.
pub fn parse_value(t: VType, s: &str) -> Result<Target, String> {
    let bad = || format!("'{s}' is not a {}", t.name());
    if t.float() {
        let v: f64 = s.parse().map_err(|_| bad())?;
        let decimals = s.split_once('.').map(|(_, d)| d.trim_end_matches(|c: char| !c.is_ascii_digit()).len());
        let tol = 0.5 * 10f64.powi(-(decimals.unwrap_or(0) as i32));
        let raw = if t == VType::F32 { (v as f32).to_bits() as u64 } else { v.to_bits() };
        return Ok(Target { raw, tol });
    }
    if !t.numeric() {
        return Err(format!("{} is not a number type", t.name()));
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mag: u128 = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(h) => u128::from_str_radix(h, 16).map_err(|_| bad())?,
        None => body.parse().map_err(|_| bad())?,
    };
    let v: i128 = if neg { -(mag as i128) } else { mag as i128 };
    let bits = t.size() * 8;
    let (min, max): (i128, i128) = match t {
        VType::I8 | VType::I16 | VType::I32 | VType::I64 => (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1),
        _ => (if neg { -(1i128 << (bits - 1)) } else { 0 }, (1i128 << bits) - 1),
    };
    if v < min || v > max {
        return Err(format!("{s} does not fit in {}", t.name()));
    }
    let mask: u128 = if bits == 64 { u64::MAX as u128 } else { (1u128 << bits) - 1 };
    Ok(Target { raw: ((v as u128) & mask) as u64, tol: 0.0 })
}

/// Does `raw` (of type t) equal the target? Floats use the tolerance.
pub fn matches(t: VType, raw: u64, target: &Target) -> bool {
    match (num(t, raw), num(t, target.raw)) {
        (Num::F(a), Num::F(b)) => a.is_finite() && (a - b).abs() <= target.tol,
        _ => {
            let mask = if t.size() >= 8 { u64::MAX } else { (1u64 << (t.size() * 8)) - 1 };
            raw & mask == target.raw & mask
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_values(VType::I32, &(-5i32).to_le_bytes()), vec!["-5"]);
        assert_eq!(format_values(VType::U16, &[1, 0, 2, 0]), vec!["1", "2"]);
        assert_eq!(format_values(VType::F32, &1.5f32.to_le_bytes()), vec!["1.5"]);
        assert_eq!(format_values(VType::F64, &(-2.25f64).to_le_bytes()), vec!["-2.25"]);
        assert_eq!(format_values(VType::Ptr, &0x7FF6_1234u64.to_le_bytes()), vec!["0x7FF61234"]);
        assert_eq!(format_values(VType::Bytes, &[0x48, 0x8B]), vec!["48 8B"]);
        assert_eq!(format_values(VType::Str, b"hi\0junk"), vec!["\"hi\""]);
        assert_eq!(format_values(VType::WStr, &[b'o', 0, b'k', 0, 0, 0]), vec!["\"ok\""]);
        assert_eq!(format_values(VType::I8, &[0xFF]), vec!["-1"]);
        assert_eq!(fmt_f32(1e9), "1e9");
        assert!(VType::parse("F32").is_ok() && VType::parse("x").is_err());
    }

    #[test]
    fn values_and_matching() {
        let t = parse_value(VType::I32, "-1").unwrap();
        assert_eq!(t.raw, 0xFFFF_FFFF);
        assert!(matches(VType::I32, 0xFFFF_FFFF, &t));
        assert!(parse_value(VType::U8, "256").is_err());
        assert!(parse_value(VType::I8, "-129").is_err());
        assert_eq!(parse_value(VType::U32, "0x10").unwrap().raw, 16);
        assert_eq!(parse_value(VType::U64, "18446744073709551615").unwrap().raw, u64::MAX);
        let f = parse_value(VType::F32, "12.5").unwrap();
        assert!((f.tol - 0.05).abs() < 1e-9);
        assert!(matches(VType::F32, 12.53f32.to_bits() as u64, &f));
        assert!(!matches(VType::F32, 12.6f32.to_bits() as u64, &f));
        assert!(!matches(VType::F32, f32::NAN.to_bits() as u64, &f));
        let d = parse_value(VType::F64, "100").unwrap();
        assert!(matches(VType::F64, 100.4f64.to_bits(), &d));
        assert!(parse_value(VType::Str, "1").is_err());
    }
}
