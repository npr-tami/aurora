//! Just enough of the PE file format to find an export by name in a 64-bit DLL's bytes: the
//! Launchpad looks `aurora_hello` and `aurora_go` up in the file it has checked, and calls them in
//! the game at the module's base plus that offset. Reads only; pure Rust,on a small
//! made-up DLL.

/// Why an export could not be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeError {
    /// Not a 64-bit DLL this reader understands (no MZ, no PE, not PE32+, cut short).
    NotAPe,
    /// A valid DLL with no export table at all.
    NoExports,
    /// A valid export table without that name.
    NoSuchExport,
}

impl std::fmt::Display for PeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeError::NotAPe => write!(f, "not a 64-bit DLL"),
            PeError::NoExports => write!(f, "no export table"),
            PeError::NoSuchExport => write!(f, "no such export"),
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// A section: where it is in memory (RVA) and in the file.
struct Section {
    rva: u32,
    vsize: u32,
    raw: u32,
    rsize: u32,
}

/// The sections of a PE32+ file, and the export directory's RVA and size (zero when none).
fn parse(file: &[u8]) -> Result<(Vec<Section>, u32, u32), PeError> {
    if file.get(..2) != Some(b"MZ") {
        return Err(PeError::NotAPe);
    }
    let pe = u32_at(file, 0x3c).ok_or(PeError::NotAPe)? as usize;
    if file.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err(PeError::NotAPe);
    }
    let coff = pe + 4;
    let sections = u16_at(file, coff + 2).ok_or(PeError::NotAPe)? as usize;
    let opt_size = u16_at(file, coff + 16).ok_or(PeError::NotAPe)? as usize;
    let opt = coff + 20;
    // PE32+ only (the game and the mod are 64-bit); its data directories start at +112.
    if u16_at(file, opt) != Some(0x20b) || opt_size < 112 + 8 {
        return Err(PeError::NotAPe);
    }
    let export_rva = u32_at(file, opt + 112).ok_or(PeError::NotAPe)?;
    let export_size = u32_at(file, opt + 116).ok_or(PeError::NotAPe)?;
    let mut out = Vec::with_capacity(sections);
    for i in 0..sections {
        let s = opt + opt_size + i * 40;
        out.push(Section {
            vsize: u32_at(file, s + 8).ok_or(PeError::NotAPe)?,
            rva: u32_at(file, s + 12).ok_or(PeError::NotAPe)?,
            rsize: u32_at(file, s + 16).ok_or(PeError::NotAPe)?,
            raw: u32_at(file, s + 20).ok_or(PeError::NotAPe)?,
        });
    }
    Ok((out, export_rva, export_size))
}

/// Where an RVA is in the file, through the section table.
fn offset(sections: &[Section], rva: u32) -> Option<usize> {
    let s = sections.iter().find(|s| rva >= s.rva && rva < s.rva + s.vsize.max(s.rsize))?;
    let within = rva - s.rva;
    (within < s.rsize).then_some(s.raw as usize + within as usize)
}

/// The RVA of a file offset, through the section table (None outside every section's raw data). For
/// the after-load check: where the card from the file sits in the game's memory.
pub fn rva_of_offset(file: &[u8], offset: usize) -> Option<u32> {
    let (sections, _, _) = parse(file).ok()?;
    let s = sections.iter().find(|s| offset >= s.raw as usize && offset < (s.raw + s.rsize) as usize)?;
    Some(s.rva + (offset - s.raw as usize) as u32)
}

/// The headers' size (the bytes the loader maps first, as they are in the file) and the whole
/// image's size in memory, from the optional header.
pub fn sizes(file: &[u8]) -> Result<(u32, u32), PeError> {
    parse(file)?;
    let pe = u32_at(file, 0x3c).ok_or(PeError::NotAPe)? as usize;
    let opt = pe + 4 + 20;
    let image = u32_at(file, opt + 56).ok_or(PeError::NotAPe)?;
    let headers = u32_at(file, opt + 60).ok_or(PeError::NotAPe)?;
    if headers == 0 || headers as usize > file.len() || image < headers {
        return Err(PeError::NotAPe);
    }
    Ok((headers, image))
}

/// Where the optional header's ImageBase field is in the file (8 bytes). Windows writes the real
/// load address there once the DLL is in memory, so a header comparison must skip it.
pub fn image_base_field(file: &[u8]) -> Result<std::ops::Range<usize>, PeError> {
    parse(file)?;
    let pe = u32_at(file, 0x3c).ok_or(PeError::NotAPe)? as usize;
    let at = pe + 4 + 20 + 24;
    Ok(at..at + 8)
}

/// The RVA (offset from the module's base once loaded) of the export called `name`.
pub fn export_rva(file: &[u8], name: &str) -> Result<u32, PeError> {
    let (sections, dir_rva, dir_size) = parse(file)?;
    if dir_rva == 0 || dir_size == 0 {
        return Err(PeError::NoExports);
    }
    let dir = offset(&sections, dir_rva).ok_or(PeError::NotAPe)?;
    let names = u32_at(file, dir + 24).ok_or(PeError::NotAPe)? as usize;
    let functions = offset(&sections, u32_at(file, dir + 28).ok_or(PeError::NotAPe)?).ok_or(PeError::NotAPe)?;
    let name_table = offset(&sections, u32_at(file, dir + 32).ok_or(PeError::NotAPe)?).ok_or(PeError::NotAPe)?;
    let ordinals = offset(&sections, u32_at(file, dir + 36).ok_or(PeError::NotAPe)?).ok_or(PeError::NotAPe)?;
    for i in 0..names {
        let name_at = offset(&sections, u32_at(file, name_table + i * 4).ok_or(PeError::NotAPe)?).ok_or(PeError::NotAPe)?;
        let rest = file.get(name_at..).ok_or(PeError::NotAPe)?;
        let end = rest.iter().position(|&c| c == 0).ok_or(PeError::NotAPe)?;
        if &rest[..end] != name.as_bytes() {
            continue;
        }
        let ordinal = u16_at(file, ordinals + i * 2).ok_or(PeError::NotAPe)? as usize;
        let rva = u32_at(file, functions + ordinal * 4).ok_or(PeError::NotAPe)?;
        // An RVA inside the export directory is a forwarder ("other.dll.fn"), not code.
        if rva >= dir_rva && rva < dir_rva + dir_size {
            return Err(PeError::NoSuchExport);
        }
        return Ok(rva);
    }
    Err(PeError::NoSuchExport)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny 64-bit DLL with one section at RVA 0x1000 (file offset 0x200) holding an export
    /// directory with two names: `aurora_hello` at RVA 0x1100 and `aurora_go` at 0x1200.
    fn tiny_dll(with_exports: bool) -> Vec<u8> {
        let mut f = vec![0u8; 0x200 + 0x300];
        f[..2].copy_from_slice(b"MZ");
        f[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        f[0x80..0x84].copy_from_slice(b"PE\0\0");
        let coff = 0x84;
        f[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes()); // One section
        f[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes()); // Optional header size
        let opt = coff + 20;
        f[opt..opt + 2].copy_from_slice(&0x20bu16.to_le_bytes());
        f[opt + 56..opt + 60].copy_from_slice(&0x2000u32.to_le_bytes()); // SizeOfImage
        f[opt + 60..opt + 64].copy_from_slice(&0x200u32.to_le_bytes()); // SizeOfHeaders
        if with_exports {
            f[opt + 112..opt + 116].copy_from_slice(&0x1000u32.to_le_bytes());
            f[opt + 116..opt + 120].copy_from_slice(&0x100u32.to_le_bytes());
        }
        let s = opt + 240;
        f[s..s + 8].copy_from_slice(b".rdata\0\0");
        f[s + 8..s + 12].copy_from_slice(&0x300u32.to_le_bytes()); // Virtual size
        f[s + 12..s + 16].copy_from_slice(&0x1000u32.to_le_bytes()); // RVA
        f[s + 16..s + 20].copy_from_slice(&0x300u32.to_le_bytes()); // Raw size
        f[s + 20..s + 24].copy_from_slice(&0x200u32.to_le_bytes()); // Raw offset
        // The export directory at RVA 0x1000: 2 functions, 2 names; tables right after it.
        let d = 0x200;
        let put = |f: &mut Vec<u8>, at: usize, v: u32| f[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut f, d + 20, 2); // functions
        put(&mut f, d + 24, 2); // names
        put(&mut f, d + 28, 0x1040); // AddressOfFunctions
        put(&mut f, d + 32, 0x1050); // AddressOfNames
        put(&mut f, d + 36, 0x1060); // AddressOfNameOrdinals
        put(&mut f, d + 0x40, 0x1200); // Ordinal 0: aurora_go
        put(&mut f, d + 0x44, 0x1100); // Ordinal 1: aurora_hello
        put(&mut f, d + 0x50, 0x1070); // Name 0
        put(&mut f, d + 0x54, 0x1080); // Name 1
        f[d + 0x60..d + 0x62].copy_from_slice(&1u16.to_le_bytes()); // Name 0 -> ordinal 1
        f[d + 0x62..d + 0x64].copy_from_slice(&0u16.to_le_bytes()); // Name 1 -> ordinal 0
        f[d + 0x70..d + 0x70 + 13].copy_from_slice(b"aurora_hello\0");
        f[d + 0x80..d + 0x80 + 10].copy_from_slice(b"aurora_go\0");
        f
    }

    #[test]
    fn exports_are_found_by_name() {
        let dll = tiny_dll(true);
        assert_eq!(export_rva(&dll, "aurora_hello"), Ok(0x1100));
        assert_eq!(export_rva(&dll, "aurora_go"), Ok(0x1200));
        assert_eq!(export_rva(&dll, "aurora_nope"), Err(PeError::NoSuchExport));
        assert_eq!(export_rva(&dll, "aurora_hell"), Err(PeError::NoSuchExport), "a prefix is not the name");
    }

    /// A file offset inside the section maps to its RVA; the headers and the image have their
    /// sizes; outside any section there is no RVA.
    #[test]
    fn offsets_map_to_rvas_and_sizes_read() {
        let dll = tiny_dll(true);
        assert_eq!(rva_of_offset(&dll, 0x200), Some(0x1000));
        assert_eq!(rva_of_offset(&dll, 0x270), Some(0x1070));
        assert_eq!(rva_of_offset(&dll, 0x100), None, "in the headers, not a section");
        assert_eq!(rva_of_offset(&dll, 0x500), None, "past the section");
        assert_eq!(sizes(&dll), Ok((0x200, 0x2000)));
        // ImageBase sits 24 bytes into the optional header: e_lfanew 0x80 + 4 + 20 + 24.
        assert_eq!(image_base_field(&dll), Ok(0xb0..0xb8));
        assert_eq!(image_base_field(b"MZ"), Err(PeError::NotAPe));
        assert_eq!(sizes(b"MZ"), Err(PeError::NotAPe));
    }

    #[test]
    fn what_is_not_a_dll_with_exports_says_so() {
        assert_eq!(export_rva(b"not a dll", "aurora_hello"), Err(PeError::NotAPe));
        assert_eq!(export_rva(&tiny_dll(false), "aurora_hello"), Err(PeError::NoExports));
        let mut cut = tiny_dll(true);
        cut.truncate(0x200 + 0x60);
        assert_eq!(export_rva(&cut, "aurora_hello"), Err(PeError::NotAPe));
        // A 32-bit DLL is refused.
        let mut dll32 = tiny_dll(true);
        dll32[0x84 + 20..0x84 + 22].copy_from_slice(&0x10bu16.to_le_bytes());
        assert_eq!(export_rva(&dll32, "aurora_hello"), Err(PeError::NotAPe));
        // A forwarder (RVA inside the export directory) is not code to call.
        let mut fwd = tiny_dll(true);
        fwd[0x244..0x248].copy_from_slice(&0x1090u32.to_le_bytes());
        assert_eq!(export_rva(&fwd, "aurora_hello"), Err(PeError::NoSuchExport));
    }
}
