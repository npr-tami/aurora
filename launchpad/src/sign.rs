//! Bin's DLL loads only with Aurora's stamp: the DLL's last 80 bytes hold an ECDSA P-256 signature
//! over the rest of its bytes, made by `tools\sign-dll.ps1` with the key that stays on the player's
//! PC (not in the repo); layout in `aurora_core::stamp`. No stamp, or changed bytes, and the
//! Launchpad does not inject it. DLLs from Advanced's Add button need none. Checked with Windows'
//! own crypto (CNG). An `aurora.dll.sig` beside the DLL (the way before the stamp) is no longer
//! read. After the stamp, the build card (`aurora_core::card`): the signed bytes must carry one,
//! and it must say the DLL speaks this Launchpad's protocol; else not injected ("needs a newer
//! Launchpad" when the DLL is the newer one).

use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use aurora_core::card::{self, CardError, CardInfo};
use aurora_core::stamp::{self, StampError};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Security::Cryptography::{
    BCRYPT_ALG_HANDLE, BCRYPT_ECCPUBLIC_BLOB, BCRYPT_ECDSA_P256_ALGORITHM, BCRYPT_ECDSA_PUBLIC_P256_MAGIC, BCRYPT_KEY_HANDLE, BCryptCloseAlgorithmProvider, BCryptDestroyKey,
    BCryptImportKeyPair, BCryptOpenAlgorithmProvider, BCryptVerifySignature,
};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

use crate::sign_key::PUBLIC_KEY;

/// Why Bin's DLL was not injected: a short line for the window's status line (room for about 44
/// characters) and the whole message, with what to copy where, for the log and the status line's
/// tooltip.
#[derive(Debug)]
pub struct Refused {
    pub short: String,
    pub full: String,
}

/// The Add rule: a DLL from Advanced's Add button is Aurora, and gets the full check, when its name
/// is aurora.dll (any case), or its bytes carry the card's label or the stamp. (never two Auroras,
/// under any name) also: a name that starts with "aurora" (any case, as aurora.sa.dll), or the
/// mod's pipe name inside (ASCII or UTF-16LE): older builds with no stamp and no card. Those then
/// fail the stamp check, so they never load. Any other Add DLL is loaded unchecked, as before.
/// Reads the file once; an unreadable file counts as not Aurora (the load then says what is wrong).
pub fn is_aurora(dll: &Path) -> bool {
    let name = dll_name(dll);
    if name.eq_ignore_ascii_case(aurora_core::paths::DLL_FILE) || name.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("aurora")) {
        return true;
    }
    let Ok(data) = std::fs::read(dll) else { return false };
    let pipe = aurora_core::PIPE_NAME.as_bytes();
    let pipe16: Vec<u8> = aurora_core::PIPE_NAME.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let has = |needle: &[u8]| data.windows(needle.len()).any(|w| w == needle);
    stamp::is_stamped(&data) || has(card::CARD_LABEL) || has(pipe) || has(&pipe16)
}

/// Bin's DLL, checked and held.
#[derive(Debug)]
pub struct Checked {
    /// The file, open for reading, with no one else allowed to write, rename or delete it while
    /// this lives: keep it until the game has loaded the DLL, so nothing can swap it in between.
    pub file: std::fs::File,
    /// What its build card says.
    pub card: CardInfo,
    /// The verified bytes (the DLL without its stamp): the hello looks its exports up here.
    pub signed: Vec<u8>,
}

/// Bin's DLL, checked against its stamp and its build card. Err: what to say; inject nothing.
pub fn check(dll: &Path) -> Result<Checked, Refused> {
    check_with(dll, &PUBLIC_KEY, aurora_core::PROTO_VERSION)
}

fn check_with(dll: &Path, key: &[u8; 64], proto: u32) -> Result<Checked, Refused> {
    let name = dll_name(dll);
    let (file, data) = open_held(dll)?;
    let (signed, stamp) = match stamp::split(&data) {
        Ok(x) => x,
        Err(StampError::NoStamp) => {
            return Err(Refused { short: format!("Not injected: {name} has no stamp."), full: format!("not injected: {name} carries no Aurora stamp (an unsigned or older build); {}", crate::install::copy_hint(dll)) });
        }
        Err(StampError::NewerFormat(n)) => {
            return Err(Refused { short: format!("Not injected: {name} needs a newer Launchpad."), full: format!("not injected: {name} carries a stamp of format {n}, newer than this Launchpad knows (format {}); get the Launchpad from the same build", stamp::STAMP_FORMAT) });
        }
        Err(e @ StampError::BadSize(_)) => {
            return Err(Refused { short: format!("Not injected: {name}'s stamp is broken."), full: format!("not injected: {name}'s stamp can't be read ({e}); {}", crate::install::copy_hint(dll)) });
        }
    };
    if !verify(signed, &stamp.signature, key) {
        return Err(Refused { short: format!("Not injected: {name}'s stamp doesn't match."), full: format!("not injected: {name} was changed or isn't an official Aurora build; {}", crate::install::copy_hint(dll)) });
    }
    let card = check_card(signed, dll, proto)?;
    Ok(Checked { file, card, signed: signed.to_vec() })
}

fn dll_name(dll: &Path) -> String {
    dll.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The DLL open for reading and read whole, held as [`Checked::file`] says.
fn open_held(dll: &Path) -> Result<(std::fs::File, Vec<u8>), Refused> {
    let name = dll_name(dll);
    let unreadable = |e: std::io::Error| Refused { short: format!("Not injected: {name} can't be read."), full: format!("not injected: {name} could not be read: {e}") };
    let mut f = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(dll).map_err(unreadable)?;
    let mut data = Vec::new();
    f.read_to_end(&mut data).map_err(unreadable)?;
    Ok((f, data))
}

/// The build card in the verified bytes, and that it speaks this Launchpad's protocol (`proto`).
/// Gatekeeper idea 2: no card, a broken one, or another protocol, and nothing is injected.
fn check_card(signed: &[u8], dll: &Path, proto: u32) -> Result<CardInfo, Refused> {
    let name = dll_name(dll);
    let card = match card::find(signed) {
        Ok(c) => c,
        Err(CardError::NoCard) => {
            return Err(Refused { short: format!("Not injected: {name} has no build card."), full: format!("not injected: {name} carries no build card (a build from before the cards); {}", crate::install::copy_hint(dll)) });
        }
        Err(CardError::NewerFormat(n)) => {
            return Err(Refused { short: format!("Not injected: {name} needs a newer Launchpad."), full: format!("not injected: {name} carries a build card of format {n}, newer than this Launchpad knows (format {}); get the Launchpad from the same build", card::CARD_FORMAT) });
        }
        Err(e @ (CardError::Broken | CardError::Several)) => {
            return Err(Refused { short: format!("Not injected: {name}'s build card is broken."), full: format!("not injected: {name}'s build card can't be read ({e}); {}", crate::install::copy_hint(dll)) });
        }
    };
    if !card.speaks(proto) {
        let (short, what) = if card.proto_min > proto {
            (format!("Not injected: {name} needs a newer Launchpad."), "get the Launchpad from the same build".to_string())
        } else {
            (format!("Not injected: {name} is too old for this Launchpad."), crate::install::copy_hint(dll))
        };
        return Err(Refused { short, full: format!("not injected: {name} {} speaks {}, this Launchpad protocol {proto}; {what}", card.version, protocols(&card)) });
    }
    Ok(card)
}

/// "protocol 5" or "protocols 5 to 6".
fn protocols(card: &CardInfo) -> String {
    if card.proto_min == card.proto_max { format!("protocol {}", card.proto_min) } else { format!("protocols {} to {}", card.proto_min, card.proto_max) }
}

/// True when `sig` (r and s, 32 bytes each) is the key's signature over `data` (SHA-256).
fn verify(data: &[u8], sig: &[u8], key: &[u8; 64]) -> bool {
    if sig.len() != 64 {
        return false;
    }
    let hash = Sha256::digest(data);
    // BCRYPT_ECCKEY_BLOB: its magic and the size of X and Y, then X and Y.
    let mut blob = Vec::with_capacity(72);
    blob.extend_from_slice(&BCRYPT_ECDSA_PUBLIC_P256_MAGIC.to_le_bytes());
    blob.extend_from_slice(&32u32.to_le_bytes());
    blob.extend_from_slice(key);
    // SAFETY: plain CNG calls on buffers that live through them; both handles are closed.
    unsafe {
        let mut alg: BCRYPT_ALG_HANDLE = std::ptr::null_mut();
        if BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_ECDSA_P256_ALGORITHM, std::ptr::null(), 0) != 0 {
            return false;
        }
        let mut k: BCRYPT_KEY_HANDLE = std::ptr::null_mut();
        let ok = BCryptImportKeyPair(alg, std::ptr::null_mut(), BCRYPT_ECCPUBLIC_BLOB, &mut k, blob.as_ptr(), blob.len() as u32, 0) == 0
            && BCryptVerifySignature(k, std::ptr::null(), hash.as_ptr(), hash.len() as u32, sig.as_ptr(), sig.len() as u32, 0) == 0;
        if !k.is_null() {
            BCryptDestroyKey(k);
        }
        BCryptCloseAlgorithmProvider(alg, 0);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::{PUBLIC_KEY, check_card, check_with, open_held, verify};
    use crate::sign_key::{TEST_MESSAGE, TEST_SIG};

    /// What.NET signed when the key was made checks here; any change does not.
    #[test]
    fn dotnet_signatures_check_and_changes_do_not() {
        assert!(verify(TEST_MESSAGE, &TEST_SIG, &PUBLIC_KEY));
        let mut text = TEST_MESSAGE.to_vec();
        text[0] ^= 1;
        assert!(!verify(&text, &TEST_SIG, &PUBLIC_KEY), "changed bytes");
        let mut sig = TEST_SIG;
        sig[10] ^= 1;
        assert!(!verify(TEST_MESSAGE, &sig, &PUBLIC_KEY), "changed signature");
        assert!(!verify(TEST_MESSAGE, &TEST_SIG[..63], &PUBLIC_KEY), "short signature");
        let mut key = PUBLIC_KEY;
        key[0] ^= 1;
        assert!(!verify(TEST_MESSAGE, &TEST_SIG, &key), "another key");
    }

    /// Test files go to AURORA_TEST_DIR when set, else the temp folder.
    fn dir(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("aurora-sign-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A stamped DLL is held while the handle lives; no stamp, a newer stamp, a broken one or
    /// changed bytes are refused with what to do, in plain words. The stamp here is
    /// the key's own test signature, over TEST_MESSAGE, which carries no build card: so a good
    /// stamp ends at "no build card", the check after it.
    #[test]
    fn bins_dll_needs_its_stamp() {
        let d = dir("check");
        let dll = d.join("aurora.dll");
        let hint = format!("; copy aurora.dll from the build folder into {}", d.display());
        std::fs::write(&dll, TEST_MESSAGE).unwrap();
        let e = check_with(&dll, &PUBLIC_KEY, 5).unwrap_err();
        assert!(e.full.starts_with("not injected: aurora.dll carries no Aurora stamp") && e.full.ends_with(&hint), "{e:?}");
        assert_eq!(e.short, "Not injected: aurora.dll has no stamp.");
        // An old-style.sig beside it changes nothing: it is no longer read.
        std::fs::write(d.join("aurora.dll.sig"), TEST_SIG).unwrap();
        assert_eq!(check_with(&dll, &PUBLIC_KEY, 5).unwrap_err().short, "Not injected: aurora.dll has no stamp.");
        let stamped = aurora_core::stamp::attach(TEST_MESSAGE, &TEST_SIG);
        std::fs::write(&dll, &stamped).unwrap();
        // The stamp checks; what is missing is the card.
        let e = check_with(&dll, &PUBLIC_KEY, 5).unwrap_err();
        assert_eq!(e.short, "Not injected: aurora.dll has no build card.");
        assert!(e.full.starts_with("not injected: aurora.dll carries no build card") && e.full.ends_with(&hint), "{e:?}");
        // Held: still readable (the game's loader reads it), but not writable or movable.
        let (held, data) = open_held(&dll).unwrap();
        assert_eq!(data, stamped);
        assert!(std::fs::OpenOptions::new().write(true).open(&dll).is_err(), "no writes while held");
        assert!(std::fs::rename(&dll, d.join("moved.dll")).is_err(), "no rename while held");
        drop(held);
        // A changed signed byte.
        let mut changed = stamped.clone();
        changed[0] ^= 1;
        std::fs::write(&dll, &changed).unwrap();
        let e = check_with(&dll, &PUBLIC_KEY, 5).unwrap_err();
        assert!(e.full.starts_with("not injected: aurora.dll was changed or isn't an official Aurora build") && e.full.ends_with(&hint), "{e:?}");
        assert_eq!(e.short, "Not injected: aurora.dll's stamp doesn't match.");
        // A changed signature byte.
        let mut changed = stamped.clone();
        changed[TEST_MESSAGE.len()] ^= 1;
        std::fs::write(&dll, &changed).unwrap();
        assert_eq!(check_with(&dll, &PUBLIC_KEY, 5).unwrap_err().short, "Not injected: aurora.dll's stamp doesn't match.");
        // A stamp of a later format: this Launchpad is too old for it.
        let mut newer = stamped.clone();
        let n = newer.len();
        newer[n - 16] = 2;
        std::fs::write(&dll, &newer).unwrap();
        let e = check_with(&dll, &PUBLIC_KEY, 5).unwrap_err();
        assert_eq!(e.short, "Not injected: aurora.dll needs a newer Launchpad.");
        assert!(e.full.contains("format 2"), "{e:?}");
        // A broken stamp (wrong size).
        let mut broken = stamped.clone();
        broken[n - 14] = 81;
        std::fs::write(&dll, &broken).unwrap();
        assert_eq!(check_with(&dll, &PUBLIC_KEY, 5).unwrap_err().short, "Not injected: aurora.dll's stamp is broken.");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The Add rule: aurora.dll by name, any name with a card or a stamp inside; other DLLs are not
    /// Aurora.
    #[test]
    fn add_dlls_that_are_aurora_are_told() {
        use aurora_core::card::Card;
        let d = dir("add");
        assert!(super::is_aurora(&d.join("Aurora.DLL")), "by name, any case, even missing");
        let plain = d.join("other.dll");
        std::fs::write(&plain, b"MZ some other mod").unwrap();
        assert!(!super::is_aurora(&plain));
        let carded = d.join("aurora.sa.dll");
        let mut v = b"MZ ".to_vec();
        v.extend_from_slice(&Card::new(5, 5, "0.1.0", "").0);
        std::fs::write(&carded, &v).unwrap();
        assert!(super::is_aurora(&carded), "a card inside");
        let stamped = d.join("renamed.dll");
        std::fs::write(&stamped, aurora_core::stamp::attach(b"MZ", &TEST_SIG)).unwrap();
        assert!(super::is_aurora(&stamped), "a stamp inside");
        assert!(!super::is_aurora(&d.join("missing.dll")));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Never two Auroras, under any name: older builds with no stamp and no card count too, by a
    /// name that starts with "aurora" or the pipe name inside.
    #[test]
    fn old_auroras_under_any_name_are_told() {
        let d = dir("old");
        assert!(super::is_aurora(&d.join("aurora.sa.dll")), "by name, even missing");
        assert!(super::is_aurora(&d.join("AURORA.mb.dll")), "any case");
        let ascii = d.join("x1.dll");
        std::fs::write(&ascii, [b"MZ ".as_slice(), aurora_core::PIPE_NAME.as_bytes(), b" more"].concat()).unwrap();
        assert!(super::is_aurora(&ascii), "the pipe name in ASCII");
        let wide = d.join("x2.dll");
        let mut v = b"MZ \0".to_vec();
        v.extend(aurora_core::PIPE_NAME.encode_utf16().flat_map(u16::to_le_bytes));
        std::fs::write(&wide, &v).unwrap();
        assert!(super::is_aurora(&wide), "the pipe name in UTF-16LE");
        let debugger = d.join("grbresearch_dll.dll");
        std::fs::write(&debugger, b"MZ \\\\.\\pipe\\grbresearch").unwrap();
        assert!(!super::is_aurora(&debugger), "another DLL still loads");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The card check on its own: the DLL must speak this Launchpad's protocol; a newer one says
    /// "needs a newer Launchpad", an older one "too old"; a broken or later-format card is refused.
    #[test]
    fn the_build_card_must_fit() {
        use aurora_core::card::Card;
        let dll = std::path::Path::new("C:\\Aurora\\Bin\\aurora.dll");
        let file = |card: &Card| {
            let mut v = b"MZ code ".to_vec();
            v.extend_from_slice(&card.0);
            v
        };
        let c = check_card(&file(&Card::new(5, 5, "0.1.0", "release, commit 1a2b3c4")), dll, 5).unwrap();
        assert_eq!(c.describe(), "0.1.0 (release, commit 1a2b3c4), protocol 5");
        assert!(check_card(&file(&Card::new(4, 6, "0.1.0", "")), dll, 5).is_ok());
        let e = check_card(&file(&Card::new(6, 7, "0.2.0", "")), dll, 5).unwrap_err();
        assert_eq!(e.short, "Not injected: aurora.dll needs a newer Launchpad.");
        assert_eq!(e.full, "not injected: aurora.dll 0.2.0 speaks protocols 6 to 7, this Launchpad protocol 5; get the Launchpad from the same build");
        let e = check_card(&file(&Card::new(4, 4, "0.0.9", "")), dll, 5).unwrap_err();
        assert_eq!(e.short, "Not injected: aurora.dll is too old for this Launchpad.");
        assert_eq!(e.full, "not injected: aurora.dll 0.0.9 speaks protocol 4, this Launchpad protocol 5; copy aurora.dll from the build folder into C:\\Aurora\\Bin");
        let mut newer = Card::new(5, 5, "0.1.0", "");
        newer.0[12] = 2;
        let e = check_card(&file(&newer), dll, 5).unwrap_err();
        assert_eq!(e.short, "Not injected: aurora.dll needs a newer Launchpad.");
        assert!(e.full.contains("build card of format 2"), "{e:?}");
        let mut cut = file(&Card::new(5, 5, "0.1.0", ""));
        cut.truncate(cut.len() - 10);
        assert_eq!(check_card(&cut, dll, 5).unwrap_err().short, "Not injected: aurora.dll's build card is broken.");
        assert_eq!(check_card(b"MZ no card", dll, 5).unwrap_err().short, "Not injected: aurora.dll has no build card.");
        // Every short line fits the status line (room for about 48 characters).
        for s in [
            "Not injected: aurora.dll has no stamp.",
            "Not injected: aurora.dll's stamp doesn't match.",
            "Not injected: aurora.dll needs a newer Launchpad.",
            "Not injected: aurora.dll's stamp is broken.",
            "Not injected: aurora.dll can't be read.",
            "Not injected: aurora.dll has no build card.",
            "Not injected: aurora.dll's build card is broken.",
            "Not injected: aurora.dll is too old for this Launchpad.",
            &crate::install::missing_short(),
        ] {
            assert!(s.chars().count() <= 56, "{s}");
        }
    }
}
