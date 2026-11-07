//! The hello and the go: once the DLL is in the game, the Launchpad asks the copy there to answer a
//! fresh random challenge with what the checked file says (`aurora_core::hello`), and only then
//! tells it to go. Both are exports of the DLL, found in the checked file's export table
//! (`aurora_core::pe`) and run in the game the way LoadLibraryW is (`Proc::call_export`). The DLL
//! waits 15 s for the go and unloads itself without one. No secret anywhere: the challenge is new
//! each time.
//!
//! Before the hello, the module in the game is compared with the checked file.

use std::time::Duration;

use aurora_core::card::{CARD_LABEL, CARD_SIZE, CardInfo};
use aurora_core::hello::{Answer, CHALLENGE_LEN, Expected, HELLO_OK, HELLO_SIZE, Hello};
use aurora_core::pe;

use crate::process::Proc;

/// How long one call into the game may take.
const CALL_WAIT: Duration = Duration::from_secs(5);

/// The module called `name` in the game against the checked file's bytes (`signed`): its headers,
/// as the loader maps them, and its card at the card's own address must be the file's; its size
/// must be the file's image size. Err: what differs, for the log.
pub fn matches_file(p: &Proc, name: &str, signed: &[u8]) -> Result<u64, String> {
    let (base, size) = p.modules().iter().find(|m| m.name.eq_ignore_ascii_case(name)).map(|m| (m.base, m.size)).ok_or_else(|| format!("the game has no module {name}"))?;
    let (headers, image) = pe::sizes(signed).map_err(|e| format!("{name}: {e}"))?;
    if size != u64::from(image) {
        return Err(format!("the module in the game is {size:#x} bytes, the checked file's image {image:#x}"));
    }
    let mut in_game = p.read_memory(base, headers as usize)?;
    let mut from_file = signed[..headers as usize].to_vec();
    // The loader writes the real load address into ImageBase; the rest of the headers stay as in
    // the file.
    let field = pe::image_base_field(signed).map_err(|e| format!("{name}: {e}"))?;
    if field.end <= in_game.len() {
        in_game[field.clone()].fill(0);
        from_file[field].fill(0);
    }
    if in_game != from_file {
        return Err("the module's headers in the game are not the checked file's".into());
    }
    let at = signed.windows(CARD_LABEL.len()).position(|w| w == CARD_LABEL).ok_or("the checked file has no card")?;
    let rva = pe::rva_of_offset(signed, at).ok_or("the card is outside every section")?;
    let card = p.read_memory(base + u64::from(rva), CARD_SIZE)?;
    if card != signed[at..at + CARD_SIZE] {
        return Err("the card in the game is not the checked file's".into());
    }
    Ok(base)
}

/// One hello to the module called `name`: the answer, checked against the file's card, the game's
/// pid and where the game has the module. Err: why not, for the log.
pub fn ask(p: &Proc, name: &str, card: &CardInfo, signed: &[u8]) -> Result<Answer, String> {
    let rva = pe::export_rva(signed, "aurora_hello").map_err(|e| format!("{name} has no aurora_hello export ({e}): a build from before the hello"))?;
    let base = p.module_base(name).ok_or_else(|| format!("the game no longer has {name}"))?;
    let challenge = random()?;
    let ask = Hello::ask(aurora_core::PROTO_VERSION, std::process::id(), &challenge);
    let (back, code) = p.call_export(base + u64::from(rva), &ask.0, CALL_WAIT)?;
    if code != HELLO_OK {
        return Err(format!("aurora_hello returned {code}, not {HELLO_OK}"));
    }
    let back: [u8; HELLO_SIZE] = back.try_into().map_err(|_| "the hello came back the wrong size".to_string())?;
    let answer = Hello(back).read_answer().ok_or_else(|| "the hello was not answered".to_string())?;
    answer.check(&Expected { challenge: &challenge, proto: aurora_core::PROTO_VERSION, card, pid: p.pid, module: base })?;
    Ok(answer)
}

/// The go, after a hello that checked out.
pub fn go(p: &Proc, name: &str, signed: &[u8]) -> Result<(), String> {
    let rva = pe::export_rva(signed, "aurora_go").map_err(|e| format!("{name} has no aurora_go export ({e})"))?;
    let base = p.module_base(name).ok_or_else(|| format!("the game no longer has {name}"))?;
    let (_, code) = p.call_export(base + u64::from(rva), &[], CALL_WAIT)?;
    if code != 1 {
        return Err(format!("aurora_go returned {code}, not 1"));
    }
    Ok(())
}

/// 32 bytes from Windows' own random generator (CNG).
fn random() -> Result<[u8; CHALLENGE_LEN], String> {
    use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut out = [0u8; CHALLENGE_LEN];
    // SAFETY: a buffer of the size we say, filled by CNG.
    let status = unsafe { BCryptGenRandom(std::ptr::null_mut(), out.as_mut_ptr(), out.len() as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status != 0 {
        return Err(format!("BCryptGenRandom failed ({status:#x})"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    /// Two challenges are never the same, and never all zero.
    #[test]
    fn challenges_are_fresh() {
        let (a, b) = (super::random().unwrap(), super::random().unwrap());
        assert_ne!(a, b);
        assert!(a.iter().any(|&x| x != 0) && b.iter().any(|&x| x != 0));
    }
}
