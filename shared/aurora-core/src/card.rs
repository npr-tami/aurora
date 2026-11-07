//! The build card inside `aurora.dll`: a small fixed block of bytes the DLL carries, saying which
//! Launchpad protocols it speaks, its version and its build name. The Launchpad reads it from the
//! stamped file before loading (this step) and, later, from the game's memory after loading (step 2
//! of the plan).
//!
//! Layout ([`CARD_SIZE`] bytes, every number little-endian):
//!
//! ```text
//! [label "AURORA-CARD\0": 12] [format: u16] [size: u16] [proto_min: u32] [proto_max: u32]
//! [version: 16, NUL-padded] [build: 56, NUL-padded]
//! ```
//!
//! "The launchpad should be able to use future versions without having to change the launchpad
//! itself": the Launchpad accepts a DLL when its own protocol lies in `proto_min..=proto_max`, and
//! a later card format can add fields after these (a reader skips what it does not know by `size`),
//! so only a change to the check itself needs a new Launchpad. Pure Rust: the DLL builds its card
//! at compile time (`Card::new`), the Launchpad finds and reads it (`find`).

/// The 12 bytes a card starts with.
pub const CARD_LABEL: &[u8; 12] = b"AURORA-CARD\0";
/// The card format this code writes and reads.
pub const CARD_FORMAT: u16 = 1;
/// Room for the version text and the build name.
pub const VERSION_LEN: usize = 16;
pub const BUILD_LEN: usize = 56;
/// The card's whole size in format 1.
pub const CARD_SIZE: usize = CARD_LABEL.len() + 2 + 2 + 4 + 4 + VERSION_LEN + BUILD_LEN;

/// The card as the DLL carries it: plain bytes, so the same thing is in the file and in the game's
/// memory, with no padding or endianness to think about.
#[repr(C)]
pub struct Card(pub [u8; CARD_SIZE]);

impl Card {
    /// A card for a DLL that speaks the Launchpad protocols `proto_min` to `proto_max`, at compile
    /// time. Texts longer than their room are cut (the build name says enough in 56).
    pub const fn new(proto_min: u32, proto_max: u32, version: &str, build: &str) -> Card {
        let mut b = [0u8; CARD_SIZE];
        let mut at = 0;
        at = put(&mut b, at, CARD_LABEL);
        at = put(&mut b, at, &CARD_FORMAT.to_le_bytes());
        at = put(&mut b, at, &(CARD_SIZE as u16).to_le_bytes());
        at = put(&mut b, at, &proto_min.to_le_bytes());
        at = put(&mut b, at, &proto_max.to_le_bytes());
        at = put_text(&mut b, at, VERSION_LEN, version.as_bytes());
        put_text(&mut b, at, BUILD_LEN, build.as_bytes());
        Card(b)
    }
}

/// `src` into `b` at `at`; the index after it.
const fn put(b: &mut [u8; CARD_SIZE], at: usize, src: &[u8]) -> usize {
    let mut i = 0;
    while i < src.len() {
        b[at + i] = src[i];
        i += 1;
    }
    at + src.len()
}

/// `text` into `room` bytes of `b` at `at`, cut to fit, NUL-padded; the index after the room.
const fn put_text(b: &mut [u8; CARD_SIZE], at: usize, room: usize, text: &[u8]) -> usize {
    let mut i = 0;
    while i < text.len() && i < room {
        b[at + i] = text[i];
        i += 1;
    }
    at + room
}

/// What a card says, read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    pub format: u16,
    pub proto_min: u32,
    pub proto_max: u32,
    pub version: String,
    pub build: String,
}

impl CardInfo {
    /// True when a Launchpad with protocol `proto` may load this DLL.
    pub fn speaks(&self, proto: u32) -> bool {
        self.proto_min <= proto && proto <= self.proto_max
    }

    /// "0.1.0 (release, commit 1a2b3c4), protocol 5" or "..., protocols 5 to 6": for the log.
    pub fn describe(&self) -> String {
        let protos = if self.proto_min == self.proto_max { format!("protocol {}", self.proto_min) } else { format!("protocols {} to {}", self.proto_min, self.proto_max) };
        let build = if self.build.is_empty() { String::new() } else { format!(" ({})", self.build) };
        format!("{}{build}, {protos}", self.version)
    }
}

/// Why no card could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardError {
    /// No card label in the bytes: a build from before the cards.
    NoCard,
    /// A card label, but the card after it is from a later format than this reader knows.
    NewerFormat(u16),
    /// A card label, but the bytes after it do not make a card (cut short, wrong size).
    Broken,
    /// More than one card, and they differ: not one build.
    Several,
}

impl std::fmt::Display for CardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CardError::NoCard => write!(f, "no build card"),
            CardError::NewerFormat(n) => write!(f, "build card format {n} is newer than this reader ({CARD_FORMAT})"),
            CardError::Broken => write!(f, "the build card is broken"),
            CardError::Several => write!(f, "more than one build card"),
        }
    }
}

/// A card read from bytes that start with its label.
pub fn parse(bytes: &[u8]) -> Result<CardInfo, CardError> {
    if !bytes.starts_with(CARD_LABEL) {
        return Err(CardError::NoCard);
    }
    let at = CARD_LABEL.len();
    if bytes.len() < at + 4 {
        return Err(CardError::Broken);
    }
    let format = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let size = usize::from(u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]));
    if format > CARD_FORMAT {
        return Err(CardError::NewerFormat(format));
    }
    if format != CARD_FORMAT || size != CARD_SIZE || bytes.len() < CARD_SIZE {
        return Err(CardError::Broken);
    }
    let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let text = |i: usize, room: usize| {
        let field = &bytes[i..i + room];
        let end = field.iter().position(|&b| b == 0).unwrap_or(room);
        String::from_utf8_lossy(&field[..end]).into_owned()
    };
    let (proto_min, proto_max) = (u32_at(at + 4), u32_at(at + 8));
    if proto_min > proto_max {
        return Err(CardError::Broken);
    }
    Ok(CardInfo { format, proto_min, proto_max, version: text(at + 12, VERSION_LEN), build: text(at + 12 + VERSION_LEN, BUILD_LEN) })
}

/// The one card in a DLL's bytes (the signed part of a stamped file, or the module in the game's
/// memory). Every place the label shows up must read as the same card.
pub fn find(data: &[u8]) -> Result<CardInfo, CardError> {
    let mut found: Option<CardInfo> = None;
    let mut from = 0;
    while let Some(i) = data[from..].windows(CARD_LABEL.len()).position(|w| w == CARD_LABEL) {
        let at = from + i;
        let card = parse(&data[at..])?;
        match &found {
            Some(seen) if *seen != card => return Err(CardError::Several),
            _ => found = Some(card),
        }
        from = at + CARD_LABEL.len();
    }
    found.ok_or(CardError::NoCard)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card is 96 bytes; what `new` writes, `parse` and `find` read back.
    #[test]
    fn a_card_reads_back() {
        assert_eq!(CARD_SIZE, 96);
        let card = Card::new(5, 5, "0.1.0", "release, commit 1a2b3c4");
        let info = parse(&card.0).unwrap();
        assert_eq!(info, CardInfo { format: 1, proto_min: 5, proto_max: 5, version: "0.1.0".into(), build: "release, commit 1a2b3c4".into() });
        assert_eq!(info.describe(), "0.1.0 (release, commit 1a2b3c4), protocol 5");
        assert!(info.speaks(5));
        assert!(!info.speaks(4) && !info.speaks(6));
        // Inside a bigger file, anywhere.
        let mut file = b"MZ some code ".to_vec();
        file.extend_from_slice(&card.0);
        file.extend_from_slice(b" more code");
        assert_eq!(find(&file).unwrap(), info);
        // The same card twice (as when the reader's label is in the file too) is fine.
        file.extend_from_slice(&card.0);
        assert_eq!(find(&file).unwrap(), info);
    }

    /// A DLL that speaks a range of protocols: the Launchpad takes it when its own is in it; an
    /// empty build name shows no brackets.
    #[test]
    fn a_range_of_protocols() {
        let info = parse(&Card::new(5, 7, "0.2.0", "").0).unwrap();
        assert_eq!(info.describe(), "0.2.0, protocols 5 to 7");
        assert!([5, 6, 7].iter().all(|&p| info.speaks(p)));
        assert!(![4, 8].iter().any(|&p| info.speaks(p)));
    }

    /// Texts that do not fit are cut, never overflow; a build name may have 56 characters.
    #[test]
    fn long_texts_are_cut() {
        let long = "x".repeat(100);
        let info = parse(&Card::new(1, 1, &long, &long).0).unwrap();
        assert_eq!(info.version.len(), VERSION_LEN);
        assert_eq!(info.build.len(), BUILD_LEN);
        let exact = "y".repeat(BUILD_LEN);
        assert_eq!(parse(&Card::new(1, 1, "v", &exact).0).unwrap().build, exact);
    }

    /// No label: no card. A later format says so. A card cut short, with a wrong size, or with min
    /// over max is broken. Two different cards are not one build.
    #[test]
    fn broken_and_newer_cards_are_told_apart() {
        assert_eq!(find(b"MZ no card here"), Err(CardError::NoCard));
        let card = Card::new(5, 5, "0.1.0", "b");
        let mut newer = card.0;
        newer[12] = 2;
        assert_eq!(parse(&newer), Err(CardError::NewerFormat(2)));
        assert_eq!(parse(&card.0[..50]), Err(CardError::Broken));
        let mut size = card.0;
        size[14] = 95;
        assert_eq!(parse(&size), Err(CardError::Broken));
        assert_eq!(parse(&Card::new(6, 5, "0.1.0", "b").0), Err(CardError::Broken));
        let mut two = card.0.to_vec();
        two.extend_from_slice(&Card::new(5, 6, "0.1.0", "b").0);
        assert_eq!(find(&two), Err(CardError::Several));
        // A broken card anywhere in the file is refused, even next to a good one.
        let mut mixed = card.0.to_vec();
        mixed.extend_from_slice(&card.0[..20]);
        assert_eq!(find(&mixed), Err(CardError::Broken));
    }
}
