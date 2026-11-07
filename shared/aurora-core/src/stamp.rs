//! The stamp inside `aurora.dll`: the signature lives at the end of the DLL itself, so anyone
//! gets one file, not a DLL and a `.sig`.
//!
//! Layout, at the very end of the file (the "trailer", [`STAMP_SIZE`] bytes in format 1):
//!
//! ```text
//! [signature: 64 bytes, r then s] [format: u16 LE] [size: u16 LE] [label: "AURORA-STAMP"]
//! ```
//!
//! The signed bytes are everything before the trailer: exactly the DLL as cargo built it. Windows'
//! loader ignores bytes after the last section, so the stamped DLL loads as before. The label sits
//! last so a reader looks at the end of the file first; `size` says how long the whole trailer is,
//! so a later format can grow and an old reader still finds where the signed bytes end (and says
//! "needs a newer Launchpad" instead of guessing). `tools\sign-dll.ps1` writes this trailer; the
//! Launchpad reads it (`launchpad\src\sign.rs`). Pure Rust:here, used by the
//! Launchpad; the DLL never reads its own stamp.

/// The 12 bytes a stamped DLL ends with.
pub const STAMP_LABEL: &[u8; 12] = b"AURORA-STAMP";
/// The trailer format this code writes and reads.
pub const STAMP_FORMAT: u16 = 1;
/// The trailer's whole size in format 1: signature, format, size, label.
pub const STAMP_SIZE: usize = 64 + 2 + 2 + STAMP_LABEL.len();

/// What the trailer carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    /// ECDSA P-256 over SHA-256 of the signed bytes: r then s, 32 bytes each.
    pub signature: [u8; 64],
    pub format: u16,
}

/// Why a file's stamp could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StampError {
    /// The file does not end with the label: not stamped (an unsigned or old build).
    NoStamp,
    /// The label is there, but the trailer is from a later format than this reader knows.
    NewerFormat(u16),
    /// The label is there, but the trailer's size does not fit its format or the file.
    BadSize(u16),
}

impl std::fmt::Display for StampError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StampError::NoStamp => write!(f, "no Aurora stamp"),
            StampError::NewerFormat(n) => write!(f, "stamp format {n} is newer than this reader ({STAMP_FORMAT})"),
            StampError::BadSize(n) => write!(f, "stamp size {n} does not fit"),
        }
    }
}

/// True when the file ends with the stamp label (whatever the format): the cheap test the
/// Add-button rule uses to tell an Aurora DLL from any other.
pub fn is_stamped(file: &[u8]) -> bool {
    file.len() >= STAMP_LABEL.len() && file.ends_with(STAMP_LABEL)
}

/// The signed bytes and the stamp, split off a stamped file.
pub fn split(file: &[u8]) -> Result<(&[u8], Stamp), StampError> {
    if !is_stamped(file) {
        return Err(StampError::NoStamp);
    }
    let before_label = file.len() - STAMP_LABEL.len();
    if before_label < 4 {
        return Err(StampError::BadSize(0));
    }
    let format = u16::from_le_bytes([file[before_label - 4], file[before_label - 3]]);
    let size = u16::from_le_bytes([file[before_label - 2], file[before_label - 1]]);
    if format > STAMP_FORMAT {
        return Err(StampError::NewerFormat(format));
    }
    // Format 1 (the only one): the size is fixed, and the trailer must fit in the file.
    if format != STAMP_FORMAT || usize::from(size) != STAMP_SIZE || file.len() < STAMP_SIZE {
        return Err(StampError::BadSize(size));
    }
    let start = file.len() - STAMP_SIZE;
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&file[start..start + 64]);
    Ok((&file[..start], Stamp { signature, format }))
}

/// A file with the trailer on: the signed bytes, then the stamp (what `tools\sign-dll.ps1` writes;
/// here for the tests and for anything else that stamps).
pub fn attach(signed: &[u8], signature: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(signed.len() + STAMP_SIZE);
    out.extend_from_slice(signed);
    out.extend_from_slice(signature);
    out.extend_from_slice(&STAMP_FORMAT.to_le_bytes());
    out.extend_from_slice(&(STAMP_SIZE as u16).to_le_bytes());
    out.extend_from_slice(STAMP_LABEL);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIG: [u8; 64] = {
        let mut s = [0u8; 64];
        let mut i = 0;
        while i < 64 {
            s[i] = i as u8 ^ 0x5a;
            i += 1;
        }
        s
    };

    /// The trailer is 80 bytes, label last; what `attach` writes, `split` reads back, and the
    /// signed bytes are exactly what went in.
    #[test]
    fn attach_then_split_gives_the_same_bytes() {
        assert_eq!(STAMP_SIZE, 80);
        let dll = b"MZ this stands for the built DLL".to_vec();
        let file = attach(&dll, &SIG);
        assert_eq!(file.len(), dll.len() + STAMP_SIZE);
        assert!(file.ends_with(STAMP_LABEL));
        assert!(is_stamped(&file));
        let (signed, stamp) = split(&file).unwrap();
        assert_eq!(signed, &dll[..]);
        assert_eq!(stamp, Stamp { signature: SIG, format: 1 });
        // The signed part never includes any of the trailer.
        assert!(!is_stamped(signed));
    }

    /// A plain file (no label) is not stamped; so is an empty one or one shorter than the label.
    #[test]
    fn a_plain_file_has_no_stamp() {
        assert_eq!(split(b"MZ plain build"), Err(StampError::NoStamp));
        assert_eq!(split(b""), Err(StampError::NoStamp));
        assert_eq!(split(b"STAMP"), Err(StampError::NoStamp));
        assert!(!is_stamped(b"AURORA-STAM"));
        // The label alone, nothing before it: stamped, but nothing fits.
        assert_eq!(split(STAMP_LABEL), Err(StampError::BadSize(0)));
    }

    /// A later format says so (an old Launchpad then says "needs a newer Launchpad", and never
    /// guesses where the signed bytes end); a wrong size, or a short file, is refused.
    #[test]
    fn a_newer_format_or_a_wrong_size_is_refused() {
        let dll = b"MZ build".to_vec();
        let mut file = attach(&dll, &SIG);
        let n = file.len();
        // Format 2, same size: newer.
        file[n - 16] = 2;
        assert_eq!(split(&file), Err(StampError::NewerFormat(2)));
        // Format 0: not one this reader knows; size 80 is right but the format is not.
        file[n - 16] = 0;
        assert_eq!(split(&file), Err(StampError::BadSize(80)));
        file[n - 16] = 1;
        // Size 81 in a format-1 trailer.
        file[n - 14] = 81;
        assert_eq!(split(&file), Err(StampError::BadSize(81)));
        // A trailer alone, with nothing before it, is a stamped file of 80 bytes: signed part
        // empty. One byte short of that is refused.
        let bare = attach(b"", &SIG);
        assert_eq!(split(&bare).unwrap().0, b"");
        assert_eq!(split(&bare[1..]), Err(StampError::BadSize(80)));
    }

    /// Changing any signed byte, or any signature byte, changes what the reader hands over (the
    /// crypto check then fails; that check lives in the Launchpad).
    #[test]
    fn every_byte_counts() {
        let file = attach(b"MZ build", &SIG);
        let (signed, stamp) = split(&file).unwrap();
        let mut changed = file.clone();
        changed[0] ^= 1;
        assert_ne!(split(&changed).unwrap().0, signed);
        let mut changed = file.clone();
        changed[file.len() - STAMP_SIZE] ^= 1;
        assert_ne!(split(&changed).unwrap().1, stamp);
    }
}
