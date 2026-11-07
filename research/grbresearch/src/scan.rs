//! Memory scans: pick regions, read them in 1 MB chunks, and test each spot. Chunks that vanish
//! mid-scan are skipped and counted, never an error. Ctrl+C sets a flag; the scan stops at the next
//! chunk and keeps what it found.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aurora_core::pattern::Pattern;

use crate::proc::{PAGE, Region, Target, USER_MAX, USER_MIN};
use crate::store::Hit;
use crate::types::{self, Num, VType};

pub const CHUNK: u64 = 1 << 20;

static CANCEL: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_ctrl(kind: u32) -> windows_sys::core::BOOL {
    // CTRL_C_EVENT = 0, CTRL_BREAK_EVENT = 1: stop the running scan, keep the process.
    if kind <= 1 {
        CANCEL.store(true, Ordering::SeqCst);
        return 1;
    }
    0
}

/// Install the Ctrl+C handler (once) and clear the flag.
pub fn arm_cancel() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: a plain handler that only stores to an atomic.
        unsafe { windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(on_ctrl), 1) };
    });
    CANCEL.store(false, Ordering::SeqCst);
}

pub fn cancelled() -> bool {
    CANCEL.load(Ordering::SeqCst)
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub bytes: u64,
    pub regions: usize,
    pub skipped: BTreeMap<&'static str, usize>,
    pub elapsed: Duration,
    pub cancelled: bool,
    pub truncated: bool,
}

impl Stats {
    fn skip(&mut self, why: &'static str) {
        *self.skipped.entry(why).or_insert(0) += 1;
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "{:.2} s, {:.1} MB read, {} regions",
            self.elapsed.as_secs_f64(),
            self.bytes as f64 / 1_048_576.0,
            self.regions
        );
        if !self.skipped.is_empty() {
            let parts: Vec<String> = self.skipped.iter().map(|(k, v)| format!("{k} {v}")).collect();
            s.push_str(&format!(", skipped: {}", parts.join(", ")));
        }
        if self.cancelled {
            s.push_str(", CANCELLED (partial results kept)");
        }
        if self.truncated {
            s.push_str(", stopped at --limit");
        }
        s
    }
}

/// Where to scan.
#[derive(Debug, Clone)]
pub enum Area {
    /// [start, end).
    Range(u64, u64),
    /// One module's image.
    Module(String),
    /// All committed readable private + mapped memory (+ images if asked).
    Full { images: bool },
    /// Saved spans from an earlier scan.
    Spans(Vec<[u64; 2]>),
}

#[derive(Debug, Clone, Copy)]
pub struct Opts {
    pub limit: usize,
    /// Read only pages that are in RAM now (QueryWorkingSetEx).
    pub resident_only: bool,
    /// Value scans: only addresses aligned to the type size.
    pub aligned: bool,
}

/// The spans to read for an area, region-checked. Unreadable regions are counted by reason.
pub fn spans(t: &Target, area: &Area, st: &mut Stats) -> Result<Vec<[u64; 2]>, String> {
    let (from, to, full_images) = match area {
        Area::Range(a, b) => (*a, *b, None),
        Area::Module(name) => {
            let m = t.module_by_name(name).ok_or_else(|| format!("no module '{name}'"))?;
            (m.base, m.base + m.size, None)
        }
        Area::Full { images } => (USER_MIN, USER_MAX, Some(*images)),
        Area::Spans(s) => {
            let mut out = Vec::new();
            for sp in s {
                out.extend(spans(t, &Area::Range(sp[0], sp[1]), st)?);
            }
            return Ok(out);
        }
    };
    if from >= to {
        return Err("empty range".into());
    }
    let mut out = Vec::new();
    for r in t.regions(from, to) {
        if !keep(&r, full_images, st) {
            continue;
        }
        let a = r.base.max(from);
        let b = r.end().min(to);
        if a < b {
            out.push([a, b]);
        }
    }
    Ok(out)
}

fn keep(r: &Region, full_images: Option<bool>, st: &mut Stats) -> bool {
    if !r.committed() {
        return false;
    }
    if r.guard() {
        st.skip("guard");
        return false;
    }
    if !r.readable() {
        st.skip("noaccess");
        return false;
    }
    if r.gpu() {
        st.skip("gpu");
        return false;
    }
    if full_images == Some(false) && r.kind_str() == "image" {
        return false;
    }
    true
}

/// Read every span chunk by chunk. `f(addr, data, owned)` sees `data` read at `addr`; only matches
/// starting before `owned` belong to this chunk (the rest is overlap). `f` returns false to stop.
pub fn walk(
    t: &Target,
    spans: &[[u64; 2]],
    overlap: usize,
    opts: &Opts,
    st: &mut Stats,
    mut f: impl FnMut(u64, &[u8], usize) -> bool,
) {
    let start = Instant::now();
    let mut buf = vec![0u8; CHUNK as usize + overlap];
    'outer: for sp in spans {
        st.regions += 1;
        let mut pos = sp[0];
        while pos < sp[1] {
            if cancelled() {
                st.cancelled = true;
                break 'outer;
            }
            let own_end = (pos + CHUNK).min(sp[1]);
            let read_end = (own_end + overlap as u64).min(sp[1]);
            // Runs of pages to read inside [pos, own_end).
            let runs: Vec<(u64, u64)> = if opts.resident_only {
                resident_runs(t, pos, own_end, st)
            } else {
                vec![(pos, own_end)]
            };
            for (a, b) in runs {
                let end = if b == own_end { read_end } else { b };
                let n = (end - a) as usize;
                let got = t.read_raw(a, &mut buf[..n]);
                if got == n {
                    st.bytes += n as u64;
                    if !f(a, &buf[..n], (b - a) as usize) {
                        break 'outer;
                    }
                    continue;
                }
                // Pages changed under us: try page by page, skip what is gone.
                st.skip("vanished");
                let mut p = a;
                while p < b {
                    let pe = (p + PAGE).min(b);
                    let e2 = if pe == own_end { read_end } else { pe };
                    let m = (e2 - p) as usize;
                    if t.read_raw(p, &mut buf[..m]) == m {
                        st.bytes += m as u64;
                        if !f(p, &buf[..m], (pe - p) as usize) {
                            break 'outer;
                        }
                    }
                    p = pe;
                }
            }
            pos = own_end;
        }
    }
    st.elapsed += start.elapsed();
}

fn resident_runs(t: &Target, from: u64, to: u64, st: &mut Stats) -> Vec<(u64, u64)> {
    let a = from & !(PAGE - 1);
    let pages = (to - a).div_ceil(PAGE) as usize;
    let Some(flags) = t.resident(a, pages) else {
        return vec![(from, to)];
    };
    let mut out = Vec::new();
    let mut run: Option<u64> = None;
    for (i, &res) in flags.iter().enumerate() {
        let p = (a + i as u64 * PAGE).max(from);
        match (res, run) {
            (true, None) => run = Some(p),
            (false, Some(s)) => {
                out.push((s, p));
                run = None;
            }
            _ => {}
        }
        if !res {
            st.skip("not in RAM (pages)");
        }
    }
    if let Some(s) = run {
        out.push((s, to));
    }
    out
}

/// What a scan looks for.
#[derive(Debug, Clone)]
pub enum Needle {
    Value { t: VType, target: types::Target },
    Bytes(Vec<u8>),
    Aob(Pattern),
    /// 8-aligned qwords in [lo, hi]: pointers to an address (or into an object).
    Qword { lo: u64, hi: u64 },
}

impl Needle {
    pub fn len(&self) -> usize {
        match self {
            Needle::Value { t, .. } => t.size(),
            Needle::Bytes(b) => b.len(),
            Needle::Aob(p) => p.len(),
            Needle::Qword { .. } => 8,
        }
    }

    fn step(&self, aligned: bool) -> usize {
        match self {
            Needle::Value { t, .. } if aligned => t.size(),
            Needle::Qword { .. } => 8,
            _ => 1,
        }
    }

    /// Does it match at the start of `b` (b is at least len long)?
    pub fn test(&self, b: &[u8]) -> bool {
        match self {
            Needle::Value { t, target } => types::matches(*t, types::raw_of(&b[..t.size()]), target),
            Needle::Bytes(x) => b.starts_with(x),
            Needle::Aob(p) => p.matches_at(b),
            Needle::Qword { lo, hi } => {
                let v = types::raw_of(&b[..8]);
                v >= *lo && v <= *hi
            }
        }
    }
}

/// Scan spans for the needle. Hits carry the raw value (up to 8 bytes) seen there.
pub fn find(t: &Target, spans: &[[u64; 2]], needle: &Needle, opts: &Opts, st: &mut Stats) -> Vec<Hit> {
    let n = needle.len();
    let step = needle.step(opts.aligned);
    let mut hits = Vec::new();
    walk(t, spans, n.saturating_sub(1), opts, st, |addr, data, owned| {
        if data.len() < n {
            return true;
        }
        let last = (data.len() - n).min(owned.saturating_sub(1));
        let mut push = |off: usize| {
            hits.push(Hit { a: addr + off as u64, v: types::raw_of(&data[off..off + n.min(8)]) });
            opts.limit == 0 || hits.len() < opts.limit
        };
        if let Needle::Aob(p) = needle {
            for off in p.find_all(&data[..(last + n).min(data.len())], 0) {
                if off <= last && !push(off) {
                    return false;
                }
            }
            return true;
        }
        // First offset aligned in absolute terms.
        let mut off = ((step as u64 - addr % step as u64) % step as u64) as usize;
        while off <= last {
            if needle.test(&data[off..]) && !push(off) {
                return false;
            }
            off += step;
        }
        true
    });
    if opts.limit != 0 && hits.len() >= opts.limit {
        st.truncated = true;
    }
    hits
}

/// Re-read each address (n bytes), page by page. None where it cannot be read now.
pub fn reread(t: &Target, addrs: &[u64], n: usize) -> Vec<Option<Vec<u8>>> {
    let mut cache: BTreeMap<u64, Option<Vec<u8>>> = BTreeMap::new();
    addrs
        .iter()
        .map(|&a| {
            let page = a & !(PAGE - 1);
            let off = (a - page) as usize;
            if off + n > PAGE as usize {
                return t.read(a, n).ok();
            }
            let data = cache.entry(page).or_insert_with(|| t.read(page, PAGE as usize).ok());
            data.as_ref().map(|d| d[off..off + n].to_vec())
        })
        .collect()
}

/// How `next` narrows a value scan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NextMode {
    Changed,
    Unchanged,
    Increased,
    Decreased,
    Eq(types::Target),
    Ne(types::Target),
}

impl NextMode {
    pub fn parse(t: VType, words: &[String]) -> Result<NextMode, String> {
        let w = words.first().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let val = || {
            words.get(1).ok_or_else(|| format!("'{w}' needs a value")).and_then(|v| types::parse_value(t, v))
        };
        Ok(match w.as_str() {
            "changed" => NextMode::Changed,
            "unchanged" => NextMode::Unchanged,
            "increased" | "inc" => NextMode::Increased,
            "decreased" | "dec" => NextMode::Decreased,
            "eq" | "=" => NextMode::Eq(val()?),
            "ne" | "!=" => NextMode::Ne(val()?),
            _ => return Err("want changed | unchanged | increased | decreased | eq <v> | ne <v>".into()),
        })
    }
}

/// Keep the hits that pass, with their new values. Unreadable ones drop out.
pub fn narrow(hits: &[Hit], now: &[Option<u64>], t: VType, mode: NextMode) -> Vec<Hit> {
    let ord = |a: u64, b: u64| match (types::num(t, a), types::num(t, b)) {
        (Num::I(x), Num::I(y)) => x.partial_cmp(&y),
        (Num::F(x), Num::F(y)) => x.partial_cmp(&y),
        _ => None,
    };
    hits.iter()
        .zip(now)
        .filter_map(|(h, n)| {
            let v = (*n)?;
            let keep = match mode {
                NextMode::Changed => v != h.v,
                NextMode::Unchanged => v == h.v,
                NextMode::Increased => ord(v, h.v) == Some(std::cmp::Ordering::Greater),
                NextMode::Decreased => ord(v, h.v) == Some(std::cmp::Ordering::Less),
                NextMode::Eq(tg) => types::matches(t, v, &tg),
                NextMode::Ne(tg) => !types::matches(t, v, &tg),
            };
            keep.then_some(Hit { a: h.a, v })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(a: u64, v: u64) -> Hit {
        Hit { a, v }
    }

    #[test]
    fn narrowing_like_cheat_engine() {
        let hits = vec![h(0x10, 100), h(0x20, 100), h(0x30, 100), h(0x40, 100)];
        let now = vec![Some(100), Some(90), Some(110), None];
        let t = VType::I32;
        assert_eq!(narrow(&hits, &now, t, NextMode::Changed), vec![h(0x20, 90), h(0x30, 110)]);
        assert_eq!(narrow(&hits, &now, t, NextMode::Unchanged), vec![h(0x10, 100)]);
        assert_eq!(narrow(&hits, &now, t, NextMode::Decreased), vec![h(0x20, 90)]);
        assert_eq!(narrow(&hits, &now, t, NextMode::Increased), vec![h(0x30, 110)]);
        let eq = NextMode::parse(t, &["eq".into(), "110".into()]).unwrap();
        assert_eq!(narrow(&hits, &now, t, eq), vec![h(0x30, 110)]);
        let ne = NextMode::parse(t, &["ne".into(), "100".into()]).unwrap();
        assert_eq!(narrow(&hits, &now, t, ne).len(), 2);
        assert!(NextMode::parse(t, &["eq".into()]).is_err());
        assert!(NextMode::parse(t, &["bigger".into()]).is_err());
    }

    #[test]
    fn signed_and_float_order() {
        let neg = (-5i32) as u32 as u64;
        let hits = vec![h(0x10, 1)];
        assert_eq!(narrow(&hits, &[Some(neg)], VType::I32, NextMode::Decreased).len(), 1);
        assert_eq!(narrow(&hits, &[Some(neg)], VType::U32, NextMode::Increased).len(), 1);
        let f = |x: f32| x.to_bits() as u64;
        let hits = vec![h(0x10, f(100.0)), h(0x14, f(100.0))];
        let now = vec![Some(f(99.5)), Some(f32::NAN.to_bits() as u64)];
        assert_eq!(narrow(&hits, &now, VType::F32, NextMode::Decreased), vec![h(0x10, f(99.5))]);
        let eq = NextMode::parse(VType::F32, &["eq".into(), "99.5".into()]).unwrap();
        assert_eq!(narrow(&hits, &now, VType::F32, eq).len(), 1);
    }

    #[test]
    fn needles() {
        let v = Needle::Value { t: VType::U16, target: types::parse_value(VType::U16, "513").unwrap() };
        assert!(v.test(&[1, 2, 9]));
        assert!(!v.test(&[2, 1]));
        let q = Needle::Qword { lo: 0x1000, hi: 0x1010 };
        assert!(q.test(&0x1008u64.to_le_bytes()));
        assert!(!q.test(&0x1011u64.to_le_bytes()));
        assert!(Needle::Bytes(b"ab".to_vec()).test(b"abc"));
    }
}
