//! Key bindings: which key does what. Saved by the in-game menu, used by the DLL. Keys are Windows
//! virtual-key codes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub god: u32,
    pub fly: u32,
    pub spot1: u32,
    pub spot2: u32,
    pub spot3: u32,
    pub forward: u32,
    pub back: u32,
    pub left: u32,
    pub right: u32,
    pub up: u32,
    pub down: u32,
    pub fast: u32,
    pub target: u32,
    pub yeet_target: u32,
    pub bring_target: u32,
    /// Opens and closes the in-game menu.
    pub menu: u32,
    /// Back to where the last teleport started.
    pub undo: u32,
    /// Opens the command box.
    pub cmdbox: u32,
    /// Which defaults this file was saved under: 0 = before when God, Fly, Target, Yeet and Bring
    /// had keys from the start. A file without it is that old one.
    #[serde(default)]
    pub version: u32,
}

/// Defaults now: no command has a key until the player gives it one; only the menu and the command
/// box have theirs.
pub const KEYS_VERSION: u32 = 2;

impl Default for Keys {
    fn default() -> Self {
        Keys {
            // No key until the player sets one.
            god: 0,
            fly: 0,
            // Spot keys live on each saved spot now (spots.json); these are only read once to move
            // old bindings over.
            spot1: 0,
            spot2: 0,
            spot3: 0,
            forward: 0x57, // W
            back: 0x53,    // S
            left: 0x41,    // A
            right: 0x44,   // D
            up: 0x10,      // Shift
            down: 0x11,    // Ctrl
            fast: 0x20,    // Space
            target: 0,
            yeet_target: 0,
            bring_target: 0,
            menu: 0x73,         // F4
            undo: 0,            // None: F1, F2 and F4 are the game's
            cmdbox: 0x59,       // Y (U is one of the game's keys)
            version: KEYS_VERSION,
        }
    }
}

/// Every action, with what it does.
pub const ACTIONS: [(&str, &str); 15] = [
    ("menu", "Open / close the in-game menu"),
    ("cmdbox", "Command box"),
    ("god", "God mode on / off"),
    ("fly", "Fly on / off"),
    ("undo", "Undo the last teleport"),
    ("forward", "Fly forward"),
    ("back", "Fly back"),
    ("left", "Fly left"),
    ("right", "Fly right"),
    ("up", "Fly up"),
    ("down", "Fly down"),
    ("fast", "Fly fast (hold)"),
    ("target", "Target the NPC you aim at"),
    ("yeet_target", "Yeet the target"),
    ("bring_target", "Bring the target to you"),
];

impl Keys {
    /// An old file kept the old default keys as if the player had picked them. Those go; a key the
    /// player changed stays. True when something changed (then it gets saved).
    pub fn migrate(&mut self) -> bool {
        if self.version >= KEYS_VERSION {
            return false;
        }
        for (action, old) in [("god", 0x74), ("fly", 0x75), ("target", 0x54), ("yeet_target", 0x59), ("bring_target", 0x47)] {
            if self.get(action) == old {
                self.set(action, 0);
            }
        }
        self.version = KEYS_VERSION;
        true
    }

    pub fn get(&self, action: &str) -> u32 {
        match action {
            "god" => self.god,
            "fly" => self.fly,
            "spot1" => self.spot1,
            "spot2" => self.spot2,
            "spot3" => self.spot3,
            "forward" => self.forward,
            "back" => self.back,
            "left" => self.left,
            "right" => self.right,
            "up" => self.up,
            "down" => self.down,
            "fast" => self.fast,
            "target" => self.target,
            "yeet_target" => self.yeet_target,
            "bring_target" => self.bring_target,
            "menu" => self.menu,
            "undo" => self.undo,
            "cmdbox" => self.cmdbox,
            _ => 0,
        }
    }

    pub fn set(&mut self, action: &str, vk: u32) {
        match action {
            "god" => self.god = vk,
            "fly" => self.fly = vk,
            "spot1" => self.spot1 = vk,
            "spot2" => self.spot2 = vk,
            "spot3" => self.spot3 = vk,
            "forward" => self.forward = vk,
            "back" => self.back = vk,
            "left" => self.left = vk,
            "right" => self.right = vk,
            "up" => self.up = vk,
            "down" => self.down = vk,
            "fast" => self.fast = vk,
            "target" => self.target = vk,
            "yeet_target" => self.yeet_target = vk,
            "bring_target" => self.bring_target = vk,
            "menu" => self.menu = vk,
            "undo" => self.undo = vk,
            "cmdbox" => self.cmdbox = vk,
            _ => {}
        }
    }
}

/// Human name of a virtual-key code. A key by the name `key_name` gives it, any case (`F1`, `numpad
/// 2`, `none` is 0).
pub fn key_from_name(name: &str) -> Option<u32> {
    (0..=255u32).find(|&vk| key_name(vk).eq_ignore_ascii_case(name.trim()))
}

pub fn key_name(vk: u32) -> String {
    match vk {
        0 => "none".into(),
        0x01 => "Mouse left".into(),
        0x02 => "Mouse right".into(),
        0x04 => "Mouse middle".into(),
        0x05 => "Mouse 4".into(),
        0x06 => "Mouse 5".into(),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x10 => "Shift".into(),
        0x11 => "Ctrl".into(),
        0x12 => "Alt".into(),
        0x14 => "Caps Lock".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x30..=0x39 => char::from(vk as u8).to_string(),
        0x41..=0x5A => char::from(vk as u8).to_string(),
        0x60..=0x69 => format!("Numpad {}", vk - 0x60),
        0x6A => "Numpad *".into(),
        0x6B => "Numpad +".into(),
        0x6D => "Numpad -".into(),
        0x6E => "Numpad .".into(),
        0x6F => "Numpad /".into(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0xA0 => "Left Shift".into(),
        0xA1 => "Right Shift".into(),
        0xA2 => "Left Ctrl".into(),
        0xA3 => "Right Ctrl".into(),
        0xA4 => "Left Alt".into(),
        0xA5 => "Right Alt".into(),
        0xBA => ";".into(),
        0xBB => "=".into(),
        0xBC => ",".into(),
        0xBD => "-".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xC0 => "`".into(),
        0xDB => "[".into(),
        0xDC => "\\".into(),
        0xDD => "]".into(),
        0xDE => "'".into(),
        _ => format!("key {vk:#x}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_names() {
        let k = Keys::default();
        assert_eq!(key_name(k.god), "none");
        assert_eq!(key_name(k.menu), "F4");
        assert_eq!(key_name(k.cmdbox), "Y");
        assert_eq!(key_name(k.forward), "W");
        assert_eq!(key_name(k.fast), "Space");
        assert_eq!(key_name(0x62), "Numpad 2");
        assert_eq!(key_name(0), "none");
    }

    #[test]
    fn old_files_still_load() {
        let old = r#"{"god":116,"fly":117,"spot1":112,"spot2":113,"spot3":114,"forward":87,"back":83,"left":65,"right":68,"up":32,"down":17,"fast":16}"#;
        let mut k: Keys = serde_json::from_str(old).unwrap();
        assert_eq!(k.version, 0);
        assert_eq!(k.god, 116);
        assert!(k.migrate());
        assert_eq!(k.god, 0, "F5 was the old default");
        assert_eq!(k.spot1, 112);
        assert!(!k.migrate());
    }

    #[test]
    fn migrate_keeps_her_own_keys() {
        let old = r#"{"god":116,"fly":118,"target":71,"yeet_target":89,"bring_target":71,"menu":115,"cmdbox":89}"#;
        let mut k: Keys = serde_json::from_str(old).unwrap();
        k.migrate();
        assert_eq!((k.god, k.fly, k.target, k.yeet_target, k.bring_target), (0, 118, 71, 0, 0));
        assert_eq!((k.menu, k.cmdbox), (115, 89));
        let again: Keys = serde_json::from_str(&serde_json::to_string(&k).unwrap()).unwrap();
        assert_eq!(again.version, KEYS_VERSION);
    }

    #[test]
    fn get_set_roundtrip() {
        let mut k = Keys::default();
        for (action, _) in ACTIONS {
            k.set(action, 0x42);
            assert_eq!(k.get(action), 0x42, "{action}");
        }
        let s = serde_json::to_string(&k).unwrap();
        let back: Keys = serde_json::from_str(&s).unwrap();
        assert_eq!(back, k);
    }
}
