//! Pieces shared by the launcher (`launchpad`) and the in-game DLL (`aurora-dll`). Pure Rust, no
//! Win32, so everything here has unit tests.

pub mod card;
pub mod hello;
pub mod keys;
pub mod menuview;
pub mod paths;
pub mod pattern;
pub mod pe;
pub mod proto;
pub mod stamp;

/// The mod's name.
pub const MOD_NAME: &str = "Aurora";

/// Name of the named pipe the DLL listens on.
pub const PIPE_NAME: &str = r"\\.\pipe\aurora";

/// Protocol version. Bump when `proto` changes shape, or when the Launchpad's check on the DLL
/// changes.
pub const PROTO_VERSION: u32 = 5;

/// The old key file: `%APPDATA%\Aurora\keys.json`, read once to carry old menu keys over.
pub fn keys_path() -> std::path::PathBuf {
    paths::root().join("keys.json")
}

/// `%APPDATA%\Aurora\Hotkeys.txt`: the row hotkeys in Stand's tree, with the menu's own two keys on
/// top, the only keys there that are not a row's: `Open Menu: F1`, `Command Box: Y`. Fly's keys are
/// built in and not kept anywhere.
pub fn hotkeys_path() -> std::path::PathBuf {
    paths::root().join("Hotkeys.txt")
}

/// The names of the two lines on top of Hotkeys.txt.
pub const MENU_LINE: &str = "Open Menu";
pub const BOX_LINE: &str = "Command Box";

/// The line in the Launchpad's `Settings.txt` that says where the mod hooks its drawing (`Hook:
/// Auto`, `Driver` or `Off`): the Launchpad writes it, the mod reads it once at inject.
pub const HOOK_LINE: &str = "Hook";

/// The value of a `Key: Value` line in a settings text such as the Launchpad's `Settings.txt` (the
/// key in any case; the first line with it wins). Only reads.
pub fn line_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines().filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case(key)).map(|(_, v)| v.trim())
}

/// The mod's keys: [`read_keys`], and without those lines the old keys.json's once, written into
/// Hotkeys.txt. The mod's only: the Launchpad never writes Hotkeys.txt.
pub fn load_keys() -> keys::Keys {
    let (mut k, found) = read_keys();
    if !found {
        if let Some(old) = std::fs::read_to_string(keys_path()).ok().and_then(|s| serde_json::from_str::<keys::Keys>(&s).ok()) {
            k.menu = if old.menu != 0 { old.menu } else { k.menu };
            k.cmdbox = if old.cmdbox != 0 { old.cmdbox } else { k.cmdbox };
        }
        let _ = save_keys(&k);
    }
    k
}

/// The menu and command box keys as Hotkeys.txt has them (built-in ones for any missing), and
/// whether the file had either line. Only reads: the Launchpad uses it for its status line.
pub fn read_keys() -> (keys::Keys, bool) {
    let mut k = keys::Keys::default();
    let text = std::fs::read_to_string(hotkeys_path()).unwrap_or_default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut found = false;
    for line in text.lines().filter(|l| !l.starts_with('\t')) {
        let Some((name, key)) = line.split_once(": ") else { continue };
        // A key that is no key (or none) leaves the built-in one: the menu must open.
        let Some(vk) = keys::key_from_name(key.trim()).filter(|&vk| vk != 0) else { continue };
        match name.trim() {
            MENU_LINE => k.menu = vk,
            BOX_LINE => k.cmdbox = vk,
            _ => continue,
        }
        found = true;
    }
    (k, found)
}

/// The menu and command box keys into Hotkeys.txt, on top; the rest of the file stays.
pub fn save_keys(k: &keys::Keys) -> Result<(), String> {
    let p = hotkeys_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // Only a missing file starts empty: one that is there but unreadable (locked, not UTF-8) is
    // left alone, or its row hotkeys would be wiped.
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Hotkeys.txt could not be read ({e}), left as it is")),
    };
    paths::write_file(&p, with_menu_keys(&text, k)).map_err(|e| e.to_string())
}

/// A Hotkeys.txt text with the two menu key lines put on top (after the version line), in place of
/// any already there.
pub fn with_menu_keys(text: &str, k: &keys::Keys) -> String {

    // kept a key line it should have replaced.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let ours = |l: &&str| !l.starts_with('\t') && l.split_once(':').is_some_and(|(n, _)| n.trim() == MENU_LINE || n.trim() == BOX_LINE);
    let mut lines: Vec<&str> = text.lines().filter(|l| !ours(l)).collect();
    if !lines.first().is_some_and(|l| l.starts_with("Tree Compatibility Version")) {
        lines.insert(0, "Tree Compatibility Version: 1");
    }
    let (menu, cmdbox) = (format!("{MENU_LINE}: {}", keys::key_name(k.menu)), format!("{BOX_LINE}: {}", keys::key_name(k.cmdbox)));
    lines.insert(1, &menu);
    lines.insert(2, &cmdbox);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod hotkeys_file {
    use super::*;

    #[test]
    fn menu_keys_go_on_top_once() {
        let k = keys::Keys { menu: 0x70, ..keys::Keys::default() };
        let old = "Tree Compatibility Version: 1
Command Box: U
Self
	God Mode: /
";
        let text = with_menu_keys(old, &k);
        assert_eq!(text, "Tree Compatibility Version: 1
Open Menu: F1
Command Box: Y
Self
	God Mode: /
");
        assert_eq!(with_menu_keys(&text, &k), text);
        assert!(with_menu_keys("", &k).starts_with("Tree Compatibility Version: 1
Open Menu: F1
"));
        assert_eq!([keys::key_from_name("f1"), keys::key_from_name("none"), keys::key_from_name("nope")], [Some(0x70), Some(0), None]);
    }

    /// A Hotkeys.txt saved with a byte-order mark keeps one version line and one
    /// line per menu key; the mark goes.
    #[test]
    fn byte_order_mark_is_fine() {
        let k = keys::Keys::default();
        let text = with_menu_keys("\u{feff}Tree Compatibility Version: 1\r\nSelf\r\n\tGod Mode: /\r\n", &k);
        assert_eq!(text, with_menu_keys("Tree Compatibility Version: 1\nSelf\n\tGod Mode: /\n", &k));
        assert_eq!(text.matches("Tree Compatibility Version").count(), 1);
        assert!(!text.contains('\u{feff}'));
        assert_eq!(with_menu_keys("\u{feff}Open Menu: F2\n", &k).matches(MENU_LINE).count(), 1);
    }
}
