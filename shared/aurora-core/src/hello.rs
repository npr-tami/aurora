//! The hello: after the load, the Launchpad asks the DLL in the game "is the checked copy the one
//! answering?", with a fresh random challenge, and only on a right answer tells it to go. Nothing
//! to keep secret: the challenge is new every time, so an old answer is of no use, and every field
//! must match what the Launchpad read from the file it checked.
//!
//! One buffer, written by the Launchpad into the game's memory and handed to the DLL's
//! `aurora_hello` export; the DLL fills the answer part in place ([`HELLO_SIZE`] bytes, every
//! number little-endian):
//!
//! ```text
//! ask:    [label "AURORA-HELLO": 12] [format: u16] [size: u16] [launchpad proto: u32]
//!         [launchpad pid: u32] [challenge: 32]
//! answer: [answered: u32] [challenge back: 32] [proto: u32] [version: 16] [build: 56]
//!         [pid: u32] [module: u64]
//! ```
//!
//! The DLL answers in the Launchpad's protocol (it says which in the ask), so a later DLL can still
//! answer an older Launchpad in the older shape. Pure Rust,here; the DLL and the
//! Launchpad only hand bytes over.

use crate::card::{BUILD_LEN, CardInfo, VERSION_LEN};

/// The 12 bytes a hello starts with.
pub const HELLO_LABEL: &[u8; 12] = b"AURORA-HELLO";
/// The hello format this code writes and reads.
pub const HELLO_FORMAT: u16 = 1;
/// A challenge's size: 32 random bytes from the system's random generator.
pub const CHALLENGE_LEN: usize = 32;
/// The buffer's whole size in format 1.
pub const HELLO_SIZE: usize = ASK_LEN + ANSWER_LEN;
const ASK_LEN: usize = 12 + 2 + 2 + 4 + 4 + CHALLENGE_LEN;
const ANSWER_LEN: usize = 4 + CHALLENGE_LEN + 4 + VERSION_LEN + BUILD_LEN + 4 + 8;

/// Where each field sits.
const AT_FORMAT: usize = 12;
const AT_SIZE: usize = 14;
const AT_PROTO: usize = 16;
const AT_PID: usize = 20;
const AT_CHALLENGE: usize = 24;
const AT_ANSWERED: usize = ASK_LEN;
const AT_BACK: usize = AT_ANSWERED + 4;
const AT_A_PROTO: usize = AT_BACK + CHALLENGE_LEN;
const AT_A_VERSION: usize = AT_A_PROTO + 4;
const AT_A_BUILD: usize = AT_A_VERSION + VERSION_LEN;
const AT_A_PID: usize = AT_A_BUILD + BUILD_LEN;
const AT_A_MODULE: usize = AT_A_PID + 4;

/// What `aurora_hello` returns: 0 is answered; the rest say why not (the Launchpad logs the number;
/// it never trusts a buffer whose `answered` field is not 1).
pub const HELLO_OK: u32 = 0;
pub const HELLO_NULL: u32 = 1;
pub const HELLO_NOT_A_HELLO: u32 = 2;
pub const HELLO_NEWER_FORMAT: u32 = 3;

/// The hello buffer, as plain bytes.
#[repr(C)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hello(pub [u8; HELLO_SIZE]);

impl Hello {
    /// The Launchpad's ask: its protocol, its pid and a fresh challenge. The answer part is zero
    /// (not answered).
    pub fn ask(proto: u32, pid: u32, challenge: &[u8; CHALLENGE_LEN]) -> Hello {
        let mut b = [0u8; HELLO_SIZE];
        b[..12].copy_from_slice(HELLO_LABEL);
        b[AT_FORMAT..AT_FORMAT + 2].copy_from_slice(&HELLO_FORMAT.to_le_bytes());
        b[AT_SIZE..AT_SIZE + 2].copy_from_slice(&(HELLO_SIZE as u16).to_le_bytes());
        b[AT_PROTO..AT_PROTO + 4].copy_from_slice(&proto.to_le_bytes());
        b[AT_PID..AT_PID + 4].copy_from_slice(&pid.to_le_bytes());
        b[AT_CHALLENGE..AT_CHALLENGE + CHALLENGE_LEN].copy_from_slice(challenge);
        Hello(b)
    }

    /// The DLL's side, on raw bytes the Launchpad wrote: is this a hello this code can answer?
    /// Returns the protocol the Launchpad speaks, or the `HELLO_*` code to return instead.
    pub fn check_ask(bytes: &[u8]) -> Result<u32, u32> {
        if bytes.len() < ASK_LEN || !bytes.starts_with(HELLO_LABEL) {
            return Err(HELLO_NOT_A_HELLO);
        }
        let format = u16::from_le_bytes([bytes[AT_FORMAT], bytes[AT_FORMAT + 1]]);
        let size = usize::from(u16::from_le_bytes([bytes[AT_SIZE], bytes[AT_SIZE + 1]]));
        if format > HELLO_FORMAT {
            return Err(HELLO_NEWER_FORMAT);
        }
        if format != HELLO_FORMAT || size != HELLO_SIZE {
            return Err(HELLO_NOT_A_HELLO);
        }
        Ok(u32_at(bytes, AT_PROTO))
    }

    /// The DLL's answer, written into the buffer: the challenge back, the protocol it answers in,
    /// its version, build, pid and module address; `answered` last.
    pub fn answer(&mut self, proto: u32, version: &str, build: &str, pid: u32, module: u64) {
        let b = &mut self.0;
        let challenge: [u8; CHALLENGE_LEN] = b[AT_CHALLENGE..AT_CHALLENGE + CHALLENGE_LEN].try_into().unwrap_or([0; CHALLENGE_LEN]);
        b[AT_BACK..AT_BACK + CHALLENGE_LEN].copy_from_slice(&challenge);
        b[AT_A_PROTO..AT_A_PROTO + 4].copy_from_slice(&proto.to_le_bytes());
        put_text(&mut b[AT_A_VERSION..AT_A_VERSION + VERSION_LEN], version);
        put_text(&mut b[AT_A_BUILD..AT_A_BUILD + BUILD_LEN], build);
        b[AT_A_PID..AT_A_PID + 4].copy_from_slice(&pid.to_le_bytes());
        b[AT_A_MODULE..AT_A_MODULE + 8].copy_from_slice(&module.to_le_bytes());
        b[AT_ANSWERED..AT_ANSWERED + 4].copy_from_slice(&1u32.to_le_bytes());
    }

    /// The answer as read back by the Launchpad.
    pub fn read_answer(&self) -> Option<Answer> {
        let b = &self.0;
        if u32_at(b, AT_ANSWERED) != 1 {
            return None;
        }
        Some(Answer {
            challenge: b[AT_BACK..AT_BACK + CHALLENGE_LEN].try_into().ok()?,
            proto: u32_at(b, AT_A_PROTO),
            version: text_at(b, AT_A_VERSION, VERSION_LEN),
            build: text_at(b, AT_A_BUILD, BUILD_LEN),
            pid: u32_at(b, AT_A_PID),
            module: u64::from_le_bytes(b[AT_A_MODULE..AT_A_MODULE + 8].try_into().ok()?),
        })
    }
}

/// What the DLL answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub challenge: [u8; CHALLENGE_LEN],
    pub proto: u32,
    pub version: String,
    pub build: String,
    pub pid: u32,
    pub module: u64,
}

/// What the Launchpad expects the answer to say: the challenge it sent, its protocol, the card it
/// read from the checked file, the game's pid and where the game has the DLL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected<'a> {
    pub challenge: &'a [u8; CHALLENGE_LEN],
    pub proto: u32,
    pub card: &'a CardInfo,
    pub pid: u32,
    pub module: u64,
}

impl Answer {
    /// Every field against what was expected: Ok, or the first thing that differs, in words for the
    /// log.
    pub fn check(&self, want: &Expected<'_>) -> Result<(), String> {
        if self.challenge != *want.challenge {
            return Err("the challenge came back changed (an old or faked answer)".into());
        }
        if self.proto != want.proto {
            return Err(format!("it answered in protocol {}, not {}", self.proto, want.proto));
        }
        if self.version != want.card.version {
            return Err(format!("it says version {}, the file's card says {}", self.version, want.card.version));
        }
        if self.build != want.card.build {
            return Err(format!("it says build \"{}\", the file's card says \"{}\"", self.build, want.card.build));
        }
        if self.pid != want.pid {
            return Err(format!("it says pid {}, the game is {}", self.pid, want.pid));
        }
        if self.module != want.module {
            return Err(format!("it says it is at {:#x}, the game has the DLL at {:#x}", self.module, want.module));
        }
        Ok(())
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn text_at(b: &[u8], at: usize, room: usize) -> String {
    let field = &b[at..at + room];
    let end = field.iter().position(|&c| c == 0).unwrap_or(room);
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn put_text(field: &mut [u8], text: &str) {
    field.fill(0);
    let n = text.len().min(field.len());
    field[..n].copy_from_slice(&text.as_bytes()[..n]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::Card;

    const CHALLENGE: [u8; CHALLENGE_LEN] = [7; CHALLENGE_LEN];

    fn card() -> CardInfo {
        crate::card::parse(&Card::new(5, 5, "0.1.0", "release, commit 1a2b3c4").0).unwrap()
    }

    /// The buffer is 180 bytes; the ask reads as one on the DLL's side; the answer written there
    /// reads back on the Launchpad's side and checks against the file's card.
    #[test]
    fn ask_answer_check() {
        assert_eq!(HELLO_SIZE, 180);
        let mut h = Hello::ask(5, 4242, &CHALLENGE);
        assert_eq!(h.read_answer(), None, "not answered yet");
        assert_eq!(Hello::check_ask(&h.0), Ok(5));
        h.answer(5, "0.1.0", "release, commit 1a2b3c4", 9001, 0x7ff8_1234_0000);
        let a = h.read_answer().unwrap();
        assert_eq!(a, Answer { challenge: CHALLENGE, proto: 5, version: "0.1.0".into(), build: "release, commit 1a2b3c4".into(), pid: 9001, module: 0x7ff8_1234_0000 });
        let card = card();
        let want = Expected { challenge: &CHALLENGE, proto: 5, card: &card, pid: 9001, module: 0x7ff8_1234_0000 };
        assert_eq!(a.check(&want), Ok(()));
        // The ask part is left as it was.
        assert_eq!(&h.0[..ASK_LEN], &Hello::ask(5, 4242, &CHALLENGE).0[..ASK_LEN]);
    }

    /// Each field that differs is refused, with the first difference named.
    #[test]
    fn every_field_must_match() {
        let card = card();
        let mut h = Hello::ask(5, 1, &CHALLENGE);
        h.answer(5, "0.1.0", "release, commit 1a2b3c4", 9001, 0x1000);
        let a = h.read_answer().unwrap();
        let want = Expected { challenge: &CHALLENGE, proto: 5, card: &card, pid: 9001, module: 0x1000 };
        let other = [8; CHALLENGE_LEN];
        assert!(a.check(&Expected { challenge: &other, ..want.clone() }).unwrap_err().contains("challenge"));
        assert!(a.check(&Expected { proto: 6, ..want.clone() }).unwrap_err().contains("protocol 5, not 6"));
        let c2 = crate::card::parse(&Card::new(5, 5, "0.2.0", "release, commit 1a2b3c4").0).unwrap();
        assert!(a.check(&Expected { card: &c2, ..want.clone() }).unwrap_err().contains("version 0.1.0"));
        let c3 = crate::card::parse(&Card::new(5, 5, "0.1.0", "release 2").0).unwrap();
        assert!(a.check(&Expected { card: &c3, ..want.clone() }).unwrap_err().contains("build"));
        assert!(a.check(&Expected { pid: 9002, ..want.clone() }).unwrap_err().contains("pid 9001"));
        assert!(a.check(&Expected { module: 0x2000, ..want.clone() }).unwrap_err().contains("0x1000"));
        // An answer that only echoes an old challenge fails the new one.
        let mut old = Hello::ask(5, 1, &other);
        old.answer(5, "0.1.0", "release, commit 1a2b3c4", 9001, 0x1000);
        assert!(old.read_answer().unwrap().check(&want).is_err());
    }

    /// The DLL's side refuses what is not a hello it knows, with the code it should return.
    #[test]
    fn the_dll_checks_the_ask() {
        assert_eq!(Hello::check_ask(b"MZ not a hello"), Err(HELLO_NOT_A_HELLO));
        assert_eq!(Hello::check_ask(&HELLO_LABEL[..]), Err(HELLO_NOT_A_HELLO));
        let mut h = Hello::ask(5, 1, &CHALLENGE);
        h.0[AT_FORMAT] = 2;
        assert_eq!(Hello::check_ask(&h.0), Err(HELLO_NEWER_FORMAT));
        h.0[AT_FORMAT] = 1;
        h.0[AT_SIZE] = 100;
        assert_eq!(Hello::check_ask(&h.0), Err(HELLO_NOT_A_HELLO));
        // Texts that do not fit are cut; an unanswered buffer stays unanswered.
        let mut h = Hello::ask(5, 1, &CHALLENGE);
        h.answer(5, &"v".repeat(40), &"b".repeat(100), 1, 1);
        let a = h.read_answer().unwrap();
        assert_eq!((a.version.len(), a.build.len()), (VERSION_LEN, BUILD_LEN));
    }
}
