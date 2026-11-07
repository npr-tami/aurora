//! The game data the mod reads at a fixed place: the player's body and camera roots, the gear
//! roots, four class tables and one return address. Each used to be a hard-coded exe offset of
//! Steam build 25349072. Now each is found from the code that uses it: a byte pattern that matches
//! once, then the address that code reads. A game update then makes the feature stop with a log
//! line, instead of reading or writing the wrong place.
//!
//! Found once per inject, first thing on the warm-up thread; the places are saved per game build in
//! `Bin\Cache\Offsets.bin` (see `memory::find_code`). Until then `rva` is `None`, and every user
//! already treats that like an unreadable pointer (skip and try again later).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::{log, memory};

/// How the address follows from the pattern's place.
enum How {
    /// The pattern starts with a 7-byte `mov reg, [rip+x]` or `lea reg, [rip+x]`: the address is
    /// that `x`.
    Rip,
    /// The pattern's own place plus this many bytes (a place in code).
    Plus(u64),
}

pub struct Global {
    name: &'static str,
    pattern: &'static str,
    how: How,
    /// Exe offset; 0 = not searched yet, `u64::MAX` = not found.
    rva: AtomicU64,
}

const MISSING: u64 = u64::MAX;

impl Global {
    const fn new(name: &'static str, pattern: &'static str, how: How) -> Self {
        Self { name, pattern, how, rva: AtomicU64::new(0) }
    }

    /// Exe offset, once found.
    pub fn rva(&self) -> Option<u64> {
        match self.rva.load(Ordering::Relaxed) {
            0 | MISSING => None,
            v => Some(v),
        }
    }

    fn find(&self, exe: &aurora_core::proto::ModuleInfo) -> Result<u64, String> {
        let at = memory::find_code(exe, &format!("global {}", self.name), self.pattern)?;
        let addr = match self.how {
            How::Rip => rip_target(at, &memory::read(at, 7)?).ok_or_else(|| format!("global {}: not a rip-relative instruction", self.name))?,
            How::Plus(n) => at + n,
        };
        if addr < exe.base || addr >= exe.base + exe.size {
            return Err(format!("global {}: {addr:#x} is outside the exe", self.name));
        }
        Ok(addr - exe.base)
    }
}

/// The address a 7-byte `REX.W 8B/8D modrm(rip) disp32` instruction at `at` reads.
fn rip_target(at: u64, insn: &[u8]) -> Option<u64> {
    let [rex, op, modrm, d0, d1, d2, d3, ..] = *insn else { return None };
    if rex & 0xF8 != 0x48 || !matches!(op, 0x8B | 0x8D) || modrm & 0xC7 != 0x05 {
        return None;
    }
    Some((at + 7).wrapping_add_signed(i64::from(i32::from_le_bytes([d0, d1, d2, d3]))))
}

/// `[exe+0x63EBEB0]` in build 25349072: the player's body (position at +0x20).
pub static BODY_ROOT: Global = Global::new("body root", "48 8B 15 ?? ?? ?? ?? 33 ED", How::Rip);
/// `[exe+0x62F24B0]`: the player root with the camera (vectors at +0x1c0…+0x200).
pub static CAM_ROOT: Global = Global::new("camera root", "48 8B 0D ?? ?? ?? ?? 48 8D 54 24 40 E8 ?? ?? ?? ?? F2 0F 10 48 18 F2 0F 10 40 10", How::Rip);
/// `[exe+0x63EB5D0]`: the gear slots.
pub static SLOTS_ROOT: Global = Global::new("gear slots root", "48 8B 0D ?? ?? ?? ?? 8B 7D", How::Rip);
/// `[exe+0x63A0D20]`: the gear level of new drops (+0x490).
pub static LEVEL_ROOT: Global = Global::new("gear level root", "48 8B 0D ?? ?? ?? ?? 48 85 C9 74 25 48 8B 01 41", How::Rip);
/// exe+0x4894BD0: the class table of a weapon item.
pub static WEAPON_VT: Global = Global::new("weapon table", "48 8D 05 ?? ?? ?? ?? 48 89 6C 24 38 48 8B F9 48 89 74 24 40 48 8B B1 B0", How::Rip);
/// exe+0x47B5130: the class table of a gear item.
pub static GEAR_VT: Global = Global::new("gear table", "48 8D 05 ?? ?? ?? ?? 48 89 07 48 8D B7 88 00 00 00 48", How::Rip);
/// exe+0x49BFD70: the class of an NPC's character object (its +8 is the body).
pub static CHAR_COMPONENT: Global = Global::new("npc tag", "48 8D 05 ?? ?? ?? ?? 48 89 07 48 8B C7 48 89 77 20 48 89 77 28 48 C7", How::Rip);
/// exe+0x4A4C5F0: the same for players.
pub static PLAYER_COMPONENT: Global = Global::new(
    "player tag",
    "48 8D 05 ?? ?? ?? ?? 48 89 03 48 8D 05 ?? ?? ?? ?? 48 89 43 20 48 8D 05 ?? ?? ?? ?? 48 89 83 A0 02 00 00 48 C7",
    How::Rip,
);
/// exe+0x273990E: the return address of the call that reads the player's own credits (`mov
/// rax,[rdx+60h]; call [rax]`, the pattern; the return is 6 bytes on).
pub static CREDITS_CALLER: Global = Global::new("credits caller", "48 8B 42 60 FF 10", How::Plus(6));

static ALL: [&Global; 9] = [&BODY_ROOT, &CAM_ROOT, &SLOTS_ROOT, &LEVEL_ROOT, &WEAPON_VT, &GEAR_VT, &CHAR_COMPONENT, &PLAYER_COMPONENT, &CREDITS_CALLER];

/// Find all of them (fast when `Offsets.bin` has this build). Logs each one that is missing.
pub fn find_all() {
    let Some(exe) = memory::find_module("") else {
        log("globals: exe not found");
        return;
    };
    for g in ALL {
        match g.find(&exe) {
            Ok(rva) => g.rva.store(rva, Ordering::Relaxed),
            Err(e) => {
                g.rva.store(MISSING, Ordering::Relaxed);
                log(&format!("{e}; what needs it stays off"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_parse() {
        for g in ALL {
            assert!(aurora_core::pattern::Pattern::parse(g.pattern).is_ok(), "{}", g.name);
            if matches!(g.how, How::Rip) {
                assert!(g.pattern.starts_with("48 8B ") || g.pattern.starts_with("48 8D "), "{}", g.name);
            }
        }
    }

    #[test]
    fn rip_targets() {
        // The body root's code in build 25349072: mov rdx,[rip+disp] at 0x1f4419a -> 0x63ebeb0.
        let disp = (0x63e_beb0i64 - (0x1f4_419a + 7)) as i32;
        let mut insn = vec![0x48, 0x8B, 0x15];
        insn.extend_from_slice(&disp.to_le_bytes());
        assert_eq!(rip_target(0x1f4_419a, &insn), Some(0x63e_beb0));
        // Lea rax,[rip-0x10]
        assert_eq!(rip_target(0x1000, &[0x48, 0x8D, 0x05, 0xF0, 0xFF, 0xFF, 0xFF]), Some(0x1000 + 7 - 0x10));
        // Not rip-relative (mov rcx,[rax+...]) or too short.
        assert_eq!(rip_target(0x1000, &[0x48, 0x8B, 0x88, 0, 0, 0, 0]), None);
        assert_eq!(rip_target(0x1000, &[0x48, 0x8B, 0x15]), None);
    }

    #[test]
    fn missing_reads_as_none() {
        let g = Global::new("t", "90", How::Plus(0));
        assert_eq!(g.rva(), None);
        g.rva.store(MISSING, Ordering::Relaxed);
        assert_eq!(g.rva(), None);
        g.rva.store(0x10, Ordering::Relaxed);
        assert_eq!(g.rva(), Some(0x10));
    }
}
