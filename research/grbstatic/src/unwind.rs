//! Function bounds from the exception directory (data directory 3).
//!
//! It is an array of RUNTIME_FUNCTION {begin rva, end rva, unwind info rva}, sorted by begin. A
//! function the compiler split into pieces has one primary entry plus chained entries: the chained
//! entry's unwind info has UNW_FLAG_CHAININFO (flags = byte0 >> 3, bit 0x4) and, after its
//! unwind-code array (padded to an even count), a copy of its parent's RUNTIME_FUNCTION. Following
//! parents up to an entry without the flag gives the primary entry, whose begin is the function
//! start. Leaf functions (no stack use) have no entry at all.

use crate::image::Image;

/// One RUNTIME_FUNCTION entry (all rvas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct RtFn {
    pub begin: u32,
    pub end: u32,
    pub unwind: u32,
}

impl RtFn {
    pub fn from_bytes(b: &[u8]) -> Option<RtFn> {
        let g = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
        Some(RtFn { begin: g(0)?, end: g(4)?, unwind: g(8)? })
    }
}

/// Something that can hand out bytes at an rva (the exe, or a buffer in tests).
pub trait RvaRead {
    fn read(&self, rva: u32, len: usize) -> Option<&[u8]>;
}

impl RvaRead for Image {
    fn read(&self, rva: u32, len: usize) -> Option<&[u8]> {
        self.bytes(rva, len)
    }
}

/// What an entry's unwind info says about its parent.
#[derive(Debug, PartialEq, Eq)]
pub enum Parent {
    /// A primary entry: its begin is a function start.
    None,
    Chained(RtFn),
    /// The unwind info could not be read.
    Unreadable,
}

pub const UNW_FLAG_CHAININFO: u8 = 0x4;

pub fn parent_of<R: RvaRead + ?Sized>(mem: &R, f: &RtFn) -> Parent {
    // An odd unwind rva points straight at the parent RUNTIME_FUNCTION (older linkers).
    if f.unwind & 1 != 0 {
        return match mem.read(f.unwind & !1, 12).and_then(RtFn::from_bytes) {
            Some(p) => Parent::Chained(p),
            None => Parent::Unreadable,
        };
    }
    let Some(h) = mem.read(f.unwind, 4) else { return Parent::Unreadable };
    if (h[0] >> 3) & UNW_FLAG_CHAININFO == 0 {
        return Parent::None;
    }
    let codes = u32::from(h[2]);
    let at = f.unwind + 4 + 2 * ((codes + 1) & !1);
    match mem.read(at, 12).and_then(RtFn::from_bytes) {
        Some(p) => Parent::Chained(p),
        None => Parent::Unreadable,
    }
}

/// Follow parents to the primary entry. Returns the primary and the path taken (the entry itself
/// first). Gives up after 32 steps (a loop would mean broken data).
pub fn primary<R: RvaRead + ?Sized>(mem: &R, f: &RtFn) -> Result<(RtFn, Vec<RtFn>), String> {
    let mut cur = *f;
    let mut path = vec![cur];
    for _ in 0..32 {
        match parent_of(mem, &cur) {
            Parent::None => return Ok((cur, path)),
            Parent::Chained(p) => {
                cur = p;
                path.push(cur);
            }
            Parent::Unreadable => return Err(format!("unwind info at rva 0x{:x} is not readable", cur.unwind)),
        }
    }
    Err(format!("unwind chain from rva 0x{:x} is longer than 32 steps", f.begin))
}

/// The function around an rva: its start, primary entry and every chunk.
#[derive(Debug, Clone)]
pub struct Func {
    pub start: u32,
    /// Primary first, then the chained chunks by address.
    pub chunks: Vec<RtFn>,
    /// Entries walked from the one holding the asked rva to the primary.
    pub path: Vec<RtFn>,
}

impl Func {
    pub fn size(&self) -> u32 {
        self.chunks.iter().map(|c| c.end.saturating_sub(c.begin)).sum()
    }
}

pub struct Functions {
    pub entries: Vec<RtFn>,
}

impl Functions {
    /// Read the exception directory. Err when it is missing or unreadable.
    pub fn load(img: &Image) -> Result<Functions, String> {
        let d = img.dirs[crate::image::DIR_EXCEPTION];
        if d.rva == 0 || d.size < 12 {
            return Err("the exe has no exception directory: no function bounds".into());
        }
        let Some(all) = img.bytes(d.rva, d.size as usize - d.size as usize % 12) else {
            return Err(format!("exception directory (rva 0x{:x}) is not on disk", d.rva));
        };
        Ok(Functions::from_table(all))
    }

    pub fn from_table(all: &[u8]) -> Functions {
        let mut entries: Vec<RtFn> = all.as_chunks::<12>().0.iter().filter_map(|c| RtFn::from_bytes(c)).filter(|f| f.begin != 0 && f.end > f.begin).collect();
        // It should be sorted already; make sure, binary search depends on it.
        if !entries.windows(2).all(|w| w[0].begin <= w[1].begin) {
            entries.sort_by_key(|f| f.begin);
        }
        Functions { entries }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The entry whose [begin, end) holds rva.
    pub fn entry_at(&self, rva: u32) -> Option<&RtFn> {
        let i = self.entries.partition_point(|f| f.begin <= rva);
        let f = self.entries.get(i.checked_sub(1)?)?;
        (rva < f.end).then_some(f)
    }

    /// Begins of all primary entries (function starts), sorted.
    pub fn starts<R: RvaRead + ?Sized>(&self, mem: &R) -> Vec<u32> {
        let mut v: Vec<u32> = self.entries.iter().filter(|f| parent_of(mem, f) == Parent::None).map(|f| f.begin).collect();
        v.dedup();
        v
    }

    /// Count of chained (non-primary) entries.
    pub fn chained_count<R: RvaRead + ?Sized>(&self, mem: &R) -> usize {
        self.entries.iter().filter(|f| matches!(parent_of(mem, f), Parent::Chained(_))).count()
    }

    /// The whole function holding rva, or None when no entry covers it.
    pub fn containing<R: RvaRead + ?Sized>(&self, mem: &R, rva: u32) -> Option<Result<Func, String>> {
        let hit = *self.entry_at(rva)?;
        Some(self.func_of(mem, hit))
    }

    pub fn func_of<R: RvaRead + ?Sized>(&self, mem: &R, hit: RtFn) -> Result<Func, String> {
        let (prim, path) = primary(mem, &hit)?;
        // Chunks: every chained entry whose chain ends at this primary.
        let mut chunks: Vec<RtFn> = self
            .entries
            .iter()
            .filter(|f| matches!(parent_of(mem, f), Parent::Chained(_)))
            .filter(|f| primary(mem, f).is_ok_and(|(p, _)| p.begin == prim.begin))
            .copied()
            .collect();
        chunks.sort_by_key(|f| f.begin);
        chunks.insert(0, prim);
        Ok(Func { start: prim.begin, chunks, path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat memory: rva == index.
    struct Flat(Vec<u8>);
    impl RvaRead for Flat {
        fn read(&self, rva: u32, len: usize) -> Option<&[u8]> {
            self.0.get(rva as usize..rva as usize + len)
        }
    }

    fn put(m: &mut [u8], at: usize, b: &[u8]) {
        m[at..at + b.len()].copy_from_slice(b);
    }

    fn rf(begin: u32, end: u32, unwind: u32) -> Vec<u8> {
        [begin, end, unwind].iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn follows_chains_to_the_primary() {
        let mut m = vec![0u8; 0x400];
        // Primary unwind info at 0x100: version 1, no flags, 2 codes.
        put(&mut m, 0x100, &[0x01, 0x04, 0x02, 0x00, 0x04, 0x32, 0x00, 0x00]);
        // Chunk A unwind at 0x140: version 1 | CHAININFO << 3, 1 code (padded to 2), then the
        // parent RUNTIME_FUNCTION (the primary) at 0x140 + 4 + 4.
        put(&mut m, 0x140, &[0x01 | (UNW_FLAG_CHAININFO << 3), 0x00, 0x01, 0x00, 0xAA, 0xBB]);
        put(&mut m, 0x148, &rf(0x1000, 0x1080, 0x100));
        // Chunk B unwind at 0x180: chained, 0 codes, parent = chunk A.
        put(&mut m, 0x180, &[0x01 | (UNW_FLAG_CHAININFO << 3), 0x00, 0x00, 0x00]);
        put(&mut m, 0x184, &rf(0x2000, 0x2040, 0x140));
        let mem = Flat(m);
        let prim = RtFn { begin: 0x1000, end: 0x1080, unwind: 0x100 };
        let a = RtFn { begin: 0x2000, end: 0x2040, unwind: 0x140 };
        let b = RtFn { begin: 0x3000, end: 0x3010, unwind: 0x180 };
        assert_eq!(parent_of(&mem, &prim), Parent::None);
        assert_eq!(parent_of(&mem, &a), Parent::Chained(prim));
        let (p, path) = primary(&mem, &b).unwrap();
        assert_eq!(p, prim);
        assert_eq!(path, vec![b, a, prim]);

        let table: Vec<u8> = [rf(0x1000, 0x1080, 0x100), rf(0x2000, 0x2040, 0x140), rf(0x3000, 0x3010, 0x180)].concat();
        let funcs = Functions::from_table(&table);
        assert_eq!(funcs.entry_at(0x2010), Some(&a));
        assert_eq!(funcs.entry_at(0x2040), None);
        assert_eq!(funcs.entry_at(0xFFF), None);
        let f = funcs.containing(&mem, 0x3004).unwrap().unwrap();
        assert_eq!(f.start, 0x1000);
        assert_eq!(f.chunks, vec![prim, a, b]);
        assert_eq!(f.size(), 0x80 + 0x40 + 0x10);
        assert_eq!(funcs.starts(&mem), vec![0x1000]);
        assert_eq!(funcs.chained_count(&mem), 2);
    }

    #[test]
    fn odd_unwind_rva_points_at_the_parent_and_loops_stop() {
        let mut m = vec![0u8; 0x200];
        put(&mut m, 0x100, &[0x01, 0, 0, 0]);
        put(&mut m, 0x120, &rf(0x1000, 0x1010, 0x100));
        let mem = Flat(m.clone());
        let c = RtFn { begin: 0x5000, end: 0x5010, unwind: 0x121 };
        assert_eq!(primary(&mem, &c).unwrap().0.begin, 0x1000);
        // A chain that points at itself must stop with an error, not spin.
        put(&mut m, 0x150, &[0x01 | (UNW_FLAG_CHAININFO << 3), 0, 0, 0]);
        put(&mut m, 0x154, &rf(0x6000, 0x6010, 0x150));
        let mem = Flat(m);
        assert!(primary(&mem, &RtFn { begin: 0x6000, end: 0x6010, unwind: 0x150 }).is_err());
        assert_eq!(parent_of(&mem, &RtFn { begin: 1, end: 2, unwind: 0x1FE }), Parent::Unreadable);
    }
}
