//! The exe on disk: a read-only view of the file and its PE layout (headers, sections, data
//! directories, imports, base relocations).
//!
//! The file is opened with GENERIC_READ and mapped PAGE_READONLY, so nothing here can change it,
//! and only the pages a command touches are read from disk (a 0.5 GB exe opens at once). The game's
//! protection shuffles section names, so roles come from the section flags only.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};

pub const SCN_CODE: u32 = 0x20;
pub const SCN_IDATA: u32 = 0x40;
pub const SCN_UDATA: u32 = 0x80;
pub const SCN_EXEC: u32 = 0x2000_0000;
pub const SCN_READ: u32 = 0x4000_0000;
pub const SCN_WRITE: u32 = 0x8000_0000;

/// Data directory indexes we use.
pub const DIR_IMPORT: usize = 1;
pub const DIR_EXCEPTION: usize = 3;
pub const DIR_SECURITY: usize = 4;
pub const DIR_BASERELOC: usize = 5;
pub const DIR_DELAY_IMPORT: usize = 13;

pub const DIR_NAMES: [&str; 16] = [
    "export", "import", "resource", "exception", "security", "basereloc", "debug", "architecture", "globalptr", "tls",
    "load config", "bound import", "iat", "delay import", "clr", "reserved",
];

/// The file's bytes: a read-only map of the real file, or a plain buffer (unit tests).
pub enum Bytes {
    Mapped(Mapped),
    #[cfg_attr(not(test), allow(dead_code))]
    Owned(Vec<u8>),
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(m) => m,
            Bytes::Owned(v) => v,
        }
    }
}

/// A read-only memory map of a whole file.
pub struct Mapped {
    ptr: *const u8,
    len: usize,
    mapping: windows_sys::Win32::Foundation::HANDLE,
    file: windows_sys::Win32::Foundation::HANDLE,
}

// The view is read-only and lives until Drop, so sharing it between threads is fine.
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    pub fn open(path: &Path) -> Result<Mapped, String> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GetFileSizeEx, OPEN_EXISTING};
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, FILE_MAP_READ, MapViewOfFile, PAGE_READONLY};
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let err = |what: &str| format!("{}: {what}: {}", path.display(), std::io::Error::last_os_error());
        // SAFETY: plain Win32 calls with a NUL-terminated path; every handle is checked here and
        // Closed in Drop (or right away on failure).
        unsafe {
            let file = CreateFileW(wide.as_ptr(), GENERIC_READ, FILE_SHARE_READ, std::ptr::null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, std::ptr::null_mut());
            if file == INVALID_HANDLE_VALUE {
                return Err(err("cannot open"));
            }
            let mut size = 0i64;
            if GetFileSizeEx(file, &mut size) == 0 {
                let e = err("cannot get the size");
                CloseHandle(file);
                return Err(e);
            }
            if size <= 0 {
                CloseHandle(file);
                return Err(format!("{}: the file is empty", path.display()));
            }
            let mapping = CreateFileMappingW(file, std::ptr::null(), PAGE_READONLY, 0, 0, std::ptr::null());
            if mapping.is_null() {
                let e = err("cannot map");
                CloseHandle(file);
                return Err(e);
            }
            let view = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                let e = err("cannot map a view");
                CloseHandle(mapping);
                CloseHandle(file);
                return Err(e);
            }
            Ok(Mapped { ptr: view.Value as *const u8, len: size as usize, mapping, file })
        }
    }
}

impl Deref for Mapped {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: the view covers the whole file (len bytes) and stays mapped until Drop.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Memory::{MEMORY_MAPPED_VIEW_ADDRESS, UnmapViewOfFile};
        // SAFETY: the handles and view came from Mapped::open and are released once.
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr as *mut _ });
            CloseHandle(self.mapping);
            CloseHandle(self.file);
        }
    }
}

/// What a section is for, from its flags (names are meaningless in these exes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Executable, not writable: game code we analyse.
    Code,
    /// Writable and executable: the protection layer. Never decoded or analysed.
    Protected,
    /// Readable, not executable. (The "code" content flag means nothing here: the protection sets
    /// it on data sections too.)
    Data,
    Other,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Code => "code",
            Role::Protected => "protected (skipped)",
            Role::Data => "data",
            Role::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Section {
    pub index: usize,
    pub name: String,
    pub rva: u32,
    pub vsize: u32,
    pub raw_off: u32,
    pub raw_size: u32,
    pub flags: u32,
}

impl Section {
    pub fn exec(&self) -> bool {
        self.flags & SCN_EXEC != 0
    }

    pub fn write(&self) -> bool {
        self.flags & SCN_WRITE != 0
    }

    pub fn role(&self) -> Role {
        match (self.exec(), self.write()) {
            (true, true) => Role::Protected,
            (true, false) => Role::Code,
            _ if self.flags & SCN_READ != 0 => Role::Data,
            _ => Role::Other,
        }
    }

    /// Size in memory (VirtualSize, or the raw size when that is 0).
    pub fn extent(&self) -> u32 {
        if self.vsize != 0 { self.vsize } else { self.raw_size }
    }

    pub fn contains(&self, rva: u32) -> bool {
        rva >= self.rva && rva - self.rva < self.extent()
    }

    /// Bytes of the section that are on disk (the rest of its extent is zero-filled at load).
    pub fn disk_len(&self) -> u32 {
        self.raw_size.min(self.extent())
    }

    /// The flags as words: "code idata exec read write...".
    pub fn flag_words(&self) -> String {
        let mut w = Vec::new();
        for (bit, name) in [(SCN_CODE, "code"), (SCN_IDATA, "idata"), (SCN_UDATA, "udata"), (SCN_EXEC, "exec"), (SCN_READ, "read"), (SCN_WRITE, "write")] {
            if self.flags & bit != 0 {
                w.push(name);
            }
        }
        if self.flags & 0x0200_0000 != 0 {
            w.push("discard");
        }
        w.join(" ")
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DataDir {
    pub rva: u32,
    pub size: u32,
}

/// Names of imported DLLs and which IAT slot (rva) holds which import.
#[derive(Debug, Default)]
pub struct Imports {
    pub dlls: Vec<String>,
    pub slots: HashMap<u32, String>,
    /// Why some part could not be read, if so.
    pub problems: Vec<String>,
}

pub struct Image {
    pub path: PathBuf,
    pub data: Bytes,
    pub machine: u16,
    pub timestamp: u32,
    pub image_base: u64,
    pub entry: u32,
    pub size_of_image: u32,
    pub size_of_headers: u32,
    pub checksum: u32,
    pub subsystem: u16,
    pub sections: Vec<Section>,
    pub dirs: Vec<DataDir>,
}

fn rd_u16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}

fn rd_u32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn rd_u64(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

impl Image {
    /// Open and parse an exe (read-only map).
    pub fn open(path: &Path) -> Result<Image, String> {
        let data = Bytes::Mapped(Mapped::open(path)?);
        Image::parse(path.to_path_buf(), data)
    }

    pub fn parse(path: PathBuf, data: Bytes) -> Result<Image, String> {
        let bad = |what: &str| format!("{}: not a 64-bit PE file ({what})", path.display());
        let b: &[u8] = &data;
        if b.get(0..2) != Some(b"MZ") {
            return Err(bad("no MZ header"));
        }
        let pe = rd_u32(b, 0x3C).ok_or_else(|| bad("short file"))? as usize;
        if b.get(pe..pe + 4) != Some(b"PE\0\0") {
            return Err(bad("no PE signature"));
        }
        let fh = pe + 4;
        let machine = rd_u16(b, fh).ok_or_else(|| bad("short header"))?;
        if machine != 0x8664 {
            return Err(bad(&format!("machine 0x{machine:x}, not x64")));
        }
        let nsec = rd_u16(b, fh + 2).ok_or_else(|| bad("short header"))? as usize;
        let timestamp = rd_u32(b, fh + 4).ok_or_else(|| bad("short header"))?;
        let opt_size = rd_u16(b, fh + 16).ok_or_else(|| bad("short header"))? as usize;
        let oh = fh + 20;
        if rd_u16(b, oh) != Some(0x20B) {
            return Err(bad("optional header is not PE32+"));
        }
        let get32 = |o: usize| rd_u32(b, oh + o).ok_or_else(|| bad("short optional header"));
        let entry = get32(16)?;
        let image_base = rd_u64(b, oh + 24).ok_or_else(|| bad("short optional header"))?;
        let size_of_image = get32(56)?;
        let size_of_headers = get32(60)?;
        let checksum = get32(64)?;
        let subsystem = rd_u16(b, oh + 68).ok_or_else(|| bad("short optional header"))?;
        let ndirs = (get32(108)? as usize).min(16);
        let mut dirs = Vec::with_capacity(16);
        for i in 0..16 {
            let d = if i < ndirs { DataDir { rva: get32(112 + i * 8)?, size: get32(116 + i * 8)? } } else { DataDir::default() };
            dirs.push(d);
        }
        let st = oh + opt_size;
        let mut sections = Vec::with_capacity(nsec);
        for i in 0..nsec {
            let s = st + i * 40;
            let raw_name = b.get(s..s + 8).ok_or_else(|| bad("short section table"))?;
            let name = String::from_utf8_lossy(raw_name).trim_end_matches('\0').to_string();
            let g = |o: usize| rd_u32(b, s + o).ok_or_else(|| bad("short section table"));
            sections.push(Section { index: i, name, vsize: g(8)?, rva: g(12)?, raw_size: g(16)?, raw_off: g(20)?, flags: g(36)? });
        }
        Ok(Image { path, data, machine, timestamp, image_base, entry, size_of_image, size_of_headers, checksum, subsystem, sections, dirs })
    }

    /// "<exe stem>-<timestamp hex>", e.g. "GRB_vulkan-64ff3734".
    pub fn build_id(&self) -> String {
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "exe".into());
        format!("{stem}-{:08x}", self.timestamp)
    }

    /// The preferred (on-disk) absolute address of an rva. Not a runtime address.
    pub fn va(&self, rva: u32) -> u64 {
        self.image_base + u64::from(rva)
    }

    /// An 8-byte absolute pointer back to an rva, when it points into the image.
    pub fn ptr_to_rva(&self, p: u64) -> Option<u32> {
        let r = p.checked_sub(self.image_base)?;
        (r < u64::from(self.size_of_image)).then_some(r as u32)
    }

    pub fn section_of(&self, rva: u32) -> Option<&Section> {
        self.sections.iter().find(|s| s.contains(rva))
    }

    /// Executable, non-writable sections: the code we search and decode.
    pub fn code_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.role() == Role::Code)
    }

    /// Non-executable sections (data that can hold pointers, strings, RTTI).
    pub fn data_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| !s.exec())
    }

    /// Every section except the protected ones.
    pub fn open_sections(&self) -> impl Iterator<Item = &Section> {
        self.sections.iter().filter(|s| s.role() != Role::Protected)
    }

    pub fn in_code(&self, rva: u32) -> bool {
        self.section_of(rva).is_some_and(|s| s.role() == Role::Code)
    }

    pub fn in_protected(&self, rva: u32) -> bool {
        self.section_of(rva).is_some_and(|s| s.role() == Role::Protected)
    }

    /// The section's bytes that are on disk.
    pub fn raw(&self, s: &Section) -> &[u8] {
        let start = (s.raw_off as usize).min(self.data.len());
        let end = (start + s.disk_len() as usize).min(self.data.len());
        &self.data[start..end]
    }

    /// File offset of an rva, when that byte is on disk.
    pub fn file_off(&self, rva: u32) -> Option<usize> {
        if rva < self.size_of_headers {
            return Some(rva as usize);
        }
        let s = self.section_of(rva)?;
        let o = rva - s.rva;
        (o < s.disk_len()).then(|| s.raw_off as usize + o as usize)
    }

    /// Up to `max` bytes from rva, stopping at the end of its section's disk data.
    pub fn bytes_upto(&self, rva: u32, max: usize) -> Option<&[u8]> {
        let off = self.file_off(rva)?;
        let end = match self.section_of(rva) {
            Some(s) => s.raw_off as usize + s.disk_len() as usize,
            None => self.size_of_headers as usize,
        };
        let end = end.min(self.data.len()).min(off.saturating_add(max));
        self.data.get(off..end)
    }

    /// Exactly `len` bytes from rva (inside one section), or None.
    pub fn bytes(&self, rva: u32, len: usize) -> Option<&[u8]> {
        self.bytes_upto(rva, len).filter(|b| b.len() == len)
    }

    pub fn u32_at(&self, rva: u32) -> Option<u32> {
        self.bytes(rva, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap_or_default()))
    }

    pub fn u64_at(&self, rva: u32) -> Option<u64> {
        self.bytes(rva, 8).map(|b| u64::from_le_bytes(b.try_into().unwrap_or_default()))
    }

    /// A NUL-terminated byte string at rva (without the NUL), at most `max` bytes.
    pub fn cstr(&self, rva: u32, max: usize) -> Option<&[u8]> {
        let b = self.bytes_upto(rva, max)?;
        let n = b.iter().position(|&c| c == 0)?;
        Some(&b[..n])
    }

    /// Where a data directory lives, as text.
    pub fn dir_place(&self, i: usize) -> String {
        let d = self.dirs[i];
        if d.rva == 0 && d.size == 0 {
            return "-".into();
        }
        if i == DIR_SECURITY {
            // This one holds a file offset, not an rva.
            return "file data (after the sections)".into();
        }
        match self.section_of(d.rva) {
            Some(s) => format!("section #{} ({})", s.index, s.role().label()),
            None if d.rva < self.size_of_headers => "headers".into(),
            None => "outside every section".into(),
        }
    }

    /// Imported DLLs (normal and delay-loaded) and IAT slot names. Best effort: a part that cannot
    /// be read is noted in `problems`.
    pub fn imports(&self) -> Imports {
        let mut out = Imports::default();
        let d = self.dirs[DIR_IMPORT];
        if d.rva != 0 {
            let mut i = 0;
            loop {
                let at = d.rva + i * 20;
                let Some(desc) = self.bytes(at, 20) else {
                    out.problems.push(format!("import descriptor at rva 0x{at:x} is not on disk"));
                    break;
                };
                let int = rd_u32(desc, 0).unwrap_or(0);
                let name = rd_u32(desc, 12).unwrap_or(0);
                let iat = rd_u32(desc, 16).unwrap_or(0);
                if name == 0 && iat == 0 {
                    break;
                }
                let dll = self.cstr(name, 260).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_else(|| format!("? (name at rva 0x{name:x})"));
                self.thunks(&dll, if int != 0 { int } else { iat }, iat, &mut out.slots);
                out.dlls.push(dll);
                i += 1;
                if i > 4096 {
                    out.problems.push("too many import descriptors".into());
                    break;
                }
            }
        }
        let d = self.dirs[DIR_DELAY_IMPORT];
        if d.rva != 0 {
            let mut i = 0;
            loop {
                let at = d.rva + i * 32;
                let Some(desc) = self.bytes(at, 32) else {
                    out.problems.push(format!("delay import descriptor at rva 0x{at:x} is not on disk"));
                    break;
                };
                let attrs = rd_u32(desc, 0).unwrap_or(0);
                // Old-style descriptors hold VAs instead of rvas.
                let fix = |v: u32| if attrs & 1 == 0 && u64::from(v) >= self.image_base { (u64::from(v) - self.image_base) as u32 } else { v };
                let name = fix(rd_u32(desc, 4).unwrap_or(0));
                let iat = fix(rd_u32(desc, 12).unwrap_or(0));
                let int = fix(rd_u32(desc, 16).unwrap_or(0));
                if name == 0 {
                    break;
                }
                let dll = self.cstr(name, 260).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_else(|| format!("? (name at rva 0x{name:x})"));
                if int != 0 {
                    self.thunks(&dll, int, iat, &mut out.slots);
                }
                out.dlls.push(format!("{dll} (delay)"));
                i += 1;
                if i > 4096 {
                    break;
                }
            }
        }
        out
    }

    fn thunks(&self, dll: &str, names: u32, iat: u32, slots: &mut HashMap<u32, String>) {
        for k in 0..65_536u32 {
            let Some(t) = self.u64_at(names + k * 8) else { break };
            if t == 0 {
                break;
            }
            let what = if t & (1 << 63) != 0 {
                format!("#{}", t & 0xFFFF)
            } else {
                self.cstr((t as u32).wrapping_add(2), 512).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_else(|| "?".into())
            };
            slots.insert(iat + k * 8, format!("{dll}!{what}"));
        }
    }

    /// Base relocations: sorted (rva, width) of every relocated field (DIR64 = 8 bytes, HIGHLOW =
    /// 4). Err when the directory is missing or does not look sane.
    pub fn relocs(&self) -> Result<Vec<(u32, u8)>, String> {
        let d = self.dirs[DIR_BASERELOC];
        if d.rva == 0 || d.size == 0 {
            return Err("no base relocation directory".into());
        }
        let Some(all) = self.bytes(d.rva, d.size as usize) else {
            return Err(format!("base relocation directory (rva 0x{:x}) is not on disk", d.rva));
        };
        let mut out = Vec::new();
        let mut o = 0usize;
        while o + 8 <= all.len() {
            let page = rd_u32(all, o).unwrap_or(0);
            let size = rd_u32(all, o + 4).unwrap_or(0) as usize;
            if size == 0 && page == 0 {
                break;
            }
            if size < 8 || !size.is_multiple_of(2) || o + size > all.len() || page >= self.size_of_image {
                return Err(format!("base relocation block at rva 0x{:x} does not look sane", d.rva as usize + o));
            }
            for e in all[o + 8..o + size].as_chunks::<2>().0 {
                let e = u16::from_le_bytes(*e);
                match e >> 12 {
                    10 => out.push((page + u32::from(e & 0xFFF), 8)),
                    3 => out.push((page + u32::from(e & 0xFFF), 4)),
                    _ => {}
                }
            }
            o += size;
        }
        out.sort_unstable();
        Ok(out)
    }
}

/// Shannon entropy in bits per byte (0..8).
pub fn entropy(b: &[u8]) -> f64 {
    if b.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &x in b {
        counts[x as usize] += 1;
    }
    let n = b.len() as f64;
    counts.iter().filter(|&&c| c > 0).map(|&c| {
        let p = c as f64 / n;
        -p * p.log2()
    }).sum()
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A tiny PE32+ in memory: headers, one code section at rva 0x1000 (file 0x200) and one data
    /// section at rva 0x2000 (file 0x400). `code` and `data` fill them.
    pub fn tiny_pe(code: &[u8], data: &[u8]) -> Image {
        let mut b = vec![0u8; 0x600];
        b[0..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
        b[0x40..0x44].copy_from_slice(b"PE\0\0");
        let fh = 0x44;
        b[fh..fh + 2].copy_from_slice(&0x8664u16.to_le_bytes());
        b[fh + 2..fh + 4].copy_from_slice(&2u16.to_le_bytes());
        b[fh + 4..fh + 8].copy_from_slice(&0x64ff3734u32.to_le_bytes());
        b[fh + 16..fh + 18].copy_from_slice(&240u16.to_le_bytes());
        let oh = fh + 20;
        b[oh..oh + 2].copy_from_slice(&0x20Bu16.to_le_bytes());
        b[oh + 16..oh + 20].copy_from_slice(&0x1000u32.to_le_bytes());
        b[oh + 24..oh + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
        b[oh + 56..oh + 60].copy_from_slice(&0x3000u32.to_le_bytes());
        b[oh + 60..oh + 64].copy_from_slice(&0x200u32.to_le_bytes());
        b[oh + 108..oh + 112].copy_from_slice(&16u32.to_le_bytes());
        let st = oh + 240;
        for (i, (rva, off, flags)) in [(0x1000u32, 0x200u32, SCN_CODE | SCN_EXEC | SCN_READ), (0x2000, 0x400, SCN_IDATA | SCN_READ)].into_iter().enumerate() {
            let s = st + i * 40;
            b[s..s + 5].copy_from_slice(b".sect");
            b[s + 8..s + 12].copy_from_slice(&0x200u32.to_le_bytes());
            b[s + 12..s + 16].copy_from_slice(&rva.to_le_bytes());
            b[s + 16..s + 20].copy_from_slice(&0x200u32.to_le_bytes());
            b[s + 20..s + 24].copy_from_slice(&off.to_le_bytes());
            b[s + 36..s + 40].copy_from_slice(&flags.to_le_bytes());
        }
        b[0x200..0x200 + code.len()].copy_from_slice(code);
        b[0x400..0x400 + data.len()].copy_from_slice(data);
        Image::parse(PathBuf::from("tiny.exe"), Bytes::Owned(b)).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn parses_layout_and_maps_rvas() {
        let img = tiny_pe(&[0xC3], b"hello\0");
        assert_eq!(img.build_id(), "tiny-64ff3734");
        assert_eq!(img.sections.len(), 2);
        assert_eq!(img.sections[0].role(), Role::Code);
        assert_eq!(img.sections[1].role(), Role::Data);
        assert_eq!(img.file_off(0x1000), Some(0x200));
        assert_eq!(img.file_off(0x2005), Some(0x405));
        assert_eq!(img.file_off(0x2200), None);
        assert_eq!(img.cstr(0x2000, 16), Some(&b"hello"[..]));
        assert_eq!(img.va(0x1000), 0x1_4000_1000);
        assert_eq!(img.ptr_to_rva(0x1_4000_2000), Some(0x2000));
        assert_eq!(img.ptr_to_rva(0x1_3fff_ffff), None);
        assert!(img.bytes(0x21F0, 0x20).is_none());
        let rwx = Section { index: 0, name: String::new(), rva: 0, vsize: 1, raw_off: 0, raw_size: 1, flags: 0xE000_0020 };
        assert_eq!(rwx.role(), Role::Protected);
        assert!((entropy(&[1, 2, 3, 4]) - 2.0).abs() < 1e-9);
    }
}
