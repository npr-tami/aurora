//! The Launchpad's own settings: `%LOCALAPPDATA%\Borealis\Aurora.Launchpad\Settings.txt`, one `Key:
//! Value` per line as the mod's `Meta State.txt`. The Launchpad is the only writer; the mod reads
//! `Hook` once at inject. See `aurora_core::paths`.
//!
//! ```text
//! Automatically Inject: On
//! Inject Delay: 0
//! Load Aurora: On
//! Hook: Auto
//! Platform: Steam
//! Steam Game Folder: D:\Games\Ghost Recon Breakpoint
//! Extra DLL: On, C:\Mods\other.dll
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::launchpad::ExtraDll;
use crate::platform::Platform;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Saved {
    /// Inject by itself after Launch.
    pub auto_inject: bool,
    /// Seconds to wait after the game starts before auto inject.
    pub inject_delay: u32,
    /// Other DLLs to load after the mod's (Advanced list).
    pub extra_dlls: Vec<ExtraDll>,
    /// The mod's own row in that list: off loads only the others. Saved as `grbmod_on` before the
    /// rename to Aurora; the old key still loads from an old launchpad.json.
    #[serde(alias = "grbmod_on")]
    pub aurora_on: bool,
    /// Where the mod hooks its drawing. Read by the mod once, at inject.
    #[serde(deserialize_with = "any_hook")]
    pub hook: Hook,
    /// Which store's copy of the game Launch starts.
    pub platform: Platform,
    /// The game folder the player picked by hand, when finding it failed (per platform).
    pub game_dir_steam: Option<PathBuf>,
    pub game_dir_ubisoft: Option<PathBuf>,
}

/// Where the in-game drawing hooks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Hook {
    /// The first function in the chain: the Steam overlay if it's on, else the GPU driver.
    #[default]
    Auto,
    /// The GPU driver's own functions, past every layer.
    Driver,
    /// Never hook: no in-game menu or ESP.
    Off,
}

impl Hook {
    /// As `Settings.txt` has it, and as the mod reads it.
    fn name(self) -> &'static str {
        match self {
            Hook::Auto => "Auto",
            Hook::Driver => "Driver",
            Hook::Off => "Off",
        }
    }

    /// Any other text is Auto, not a reset of every setting.
    fn from_name(t: &str) -> Hook {
        match t.trim().to_ascii_lowercase().as_str() {
            "driver" => Hook::Driver,
            "off" => Hook::Off,
            _ => Hook::Auto,
        }
    }
}

/// An odd value in the old JSON file means Auto, not a reset of every setting.
fn any_hook<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Hook, D::Error> {
    Ok(Hook::from_name(serde_json::Value::deserialize(d)?.as_str().unwrap_or("")))
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            auto_inject: true,
            inject_delay: 0,
            extra_dlls: Vec::new(),
            aurora_on: true,
            hook: Hook::Auto,
            platform: Platform::Steam,
            game_dir_steam: None,
            game_dir_ubisoft: None,
        }
    }
}

impl Saved {
    pub fn game_dir(&self, p: Platform) -> Option<&PathBuf> {
        match p {
            Platform::Steam => self.game_dir_steam.as_ref(),
            Platform::Ubisoft => self.game_dir_ubisoft.as_ref(),
        }
    }

    pub fn set_game_dir(&mut self, p: Platform, dir: Option<PathBuf>) {
        match p {
            Platform::Steam => self.game_dir_steam = dir,
            Platform::Ubisoft => self.game_dir_ubisoft = dir,
        }
    }
}

// The keys of Settings.txt. `Hook` is `aurora_core::HOOK_LINE`: the mod reads that one.
const AUTO_INJECT: &str = "Automatically Inject";
const INJECT_DELAY: &str = "Inject Delay";
const LOAD_AURORA: &str = "Load Aurora";
const PLATFORM: &str = "Platform";
const STEAM_DIR: &str = "Steam Game Folder";
const UBISOFT_DIR: &str = "Ubisoft Connect Game Folder";
/// One line per DLL, in the list's order: `Extra DLL: On, C:\Mods\other.dll`.
const EXTRA_DLL: &str = "Extra DLL";

fn on_off(on: bool) -> &'static str {
    if on { "On" } else { "Off" }
}

/// The settings as `Settings.txt` text. Folders the player never picked are left out.
pub fn to_text(s: &Saved) -> String {
    use std::fmt::Write as _;
    let mut t = String::new();
    let _ = writeln!(t, "{AUTO_INJECT}: {}", on_off(s.auto_inject));
    let _ = writeln!(t, "{INJECT_DELAY}: {}", s.inject_delay);
    let _ = writeln!(t, "{LOAD_AURORA}: {}", on_off(s.aurora_on));
    let _ = writeln!(t, "{}: {}", aurora_core::HOOK_LINE, s.hook.name());
    let _ = writeln!(t, "{PLATFORM}: {}", s.platform.name());
    for (key, dir) in [(STEAM_DIR, &s.game_dir_steam), (UBISOFT_DIR, &s.game_dir_ubisoft)] {
        if let Some(d) = dir {
            let _ = writeln!(t, "{key}: {}", d.display());
        }
    }
    for d in &s.extra_dlls {
        let _ = writeln!(t, "{EXTRA_DLL}: {}, {}", on_off(d.on), d.path);
    }
    t
}

/// `Settings.txt` text back into settings. Keys in any case; a line it does not know, or a value
/// that is not one it takes, leaves that one setting at its default.
pub fn from_text(text: &str) -> Saved {
    let mut s = Saved::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let flag = |v: &str, d: bool| match v.to_ascii_lowercase().as_str() {
        "on" | "true" | "1" => true,
        "off" | "false" | "0" => false,
        _ => d,
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        let is = |k: &str| key.eq_ignore_ascii_case(k);
        if is(AUTO_INJECT) {
            s.auto_inject = flag(value, s.auto_inject);
        } else if is(INJECT_DELAY) {
            s.inject_delay = value.parse::<u32>().map_or(s.inject_delay, |d| d.min(120));
        } else if is(LOAD_AURORA) {
            s.aurora_on = flag(value, s.aurora_on);
        } else if is(aurora_core::HOOK_LINE) {
            s.hook = Hook::from_name(value);
        } else if is(PLATFORM) {
            s.platform = Platform::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(value) || (*p == Platform::Ubisoft && value.eq_ignore_ascii_case("ubisoft"))).unwrap_or(s.platform);
        } else if is(STEAM_DIR) && !value.is_empty() {
            s.game_dir_steam = Some(PathBuf::from(value));
        } else if is(UBISOFT_DIR) && !value.is_empty() {
            s.game_dir_ubisoft = Some(PathBuf::from(value));
        } else if is(EXTRA_DLL) {
            // `On, C:\path`; a line with only a path counts as on.
            let (on, path) = match value.split_once(',') {
                Some((o, p)) if ["on", "off"].contains(&o.trim().to_ascii_lowercase().as_str()) => (flag(o.trim(), true), p.trim()),
                _ => (true, value),
            };
            if !path.is_empty() && !s.extra_dlls.iter().any(|d| d.path.eq_ignore_ascii_case(path)) {
                s.extra_dlls.push(ExtraDll { path: path.to_string(), on });
            }
        }
    }
    s
}

/// The old JSON files' text (launchpad.json, or the Launchpad's fields in the even older shared
/// settings.json) into settings; broken JSON gives the defaults.
fn from_json(text: &str) -> Saved {
    serde_json::from_str(text).unwrap_or_default()
}

/// Settings.txt. Not there yet (the old folder could not move, so the carry-over did not run): the
/// old launchpad.json, else the Launchpad's fields in the old shared settings.json; both only read.
pub fn load() -> Saved {
    if let Ok(text) = std::fs::read_to_string(aurora_core::paths::launchpad_settings()) {
        return from_text(&text);
    }
    [aurora_core::paths::old_launchpad_settings(), aurora_core::paths::old_mod_settings()]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|t| from_json(&t))
        .unwrap_or_default()
}

pub fn save(s: &Saved) -> Result<(), String> {
    write_text(&aurora_core::paths::launchpad_settings(), &to_text(s))
}

/// Its folder is made first (`%LOCALAPPDATA%\Borealis\Aurora.Launchpad`), then the file is written
/// whole to `Settings.txt.tmp` beside it and renamed into place. Not through `Aurora\Bin\Temp` as
/// the mod's saves: `%LOCALAPPDATA%` and `%APPDATA%` can be on different drives, and a rename can't
/// cross drives.
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// At start, once the folders are there: the old `Aurora\launchpad.json` becomes Settings.txt (only
/// when that is not there yet), then moves into `Dumper\old`. Never deletes: a file that can't be
/// written or moved stays where it is. Gives what it did, one line each, for the window and the
/// log.
pub fn carry_over() -> Vec<String> {
    carry_over_in(&aurora_core::paths::launchpad_settings(), &aurora_core::paths::old_launchpad_settings(), &aurora_core::paths::old_files(), &write_text)
}

/// [`carry_over`] with its files given (the tests use their own, and their own writer: the real one
/// saves through `%APPDATA%\Aurora\Bin\Temp`).
fn carry_over_in(new: &Path, old: &Path, old_dir: &Path, write: &dyn Fn(&Path, &str) -> Result<(), String>) -> Vec<String> {
    if !old.is_file() {
        return Vec::new();
    }
    let mut done = Vec::new();
    if !new.exists() {
        let text = match std::fs::read_to_string(old) {
            Ok(t) => t,
            Err(e) => return vec![format!("{} could not be read ({e}): left where it is", old.display())],
        };
        if serde_json::from_str::<serde_json::Value>(&text).is_err() {
            done.push(format!("{} is not valid JSON: Launchpad settings start at their defaults", old.display()));
        }
        if let Err(e) = write(new, &to_text(&from_json(&text))) {
            done.push(format!("could not write {} ({e}): {} left where it is", new.display(), old.display()));
            return done;
        }
        done.push(format!("Launchpad settings carried over from {} to {}", old.display(), new.display()));
    }
    match aurora_core::paths::move_aside(old, old_dir) {
        Ok(to) => done.push(format!("moved {} to {}", old.display(), to.display())),
        Err(e) => done.push(format!("could not move {} to {} ({e}): left where it is", old.display(), old_dir.display())),
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_shared_file_gives_the_launchpad_fields() {
        // A settings.json from before with the mod's fields mixed in.
        let old = r#"{"gear_value":500,"auto_inject":false,"inject_delay":7,"grbmod_on":true,"hook":"driver","esp_on":true}"#;
        let s = from_json(old);
        assert_eq!((s.auto_inject, s.inject_delay, s.hook, s.platform), (false, 7, Hook::Driver, Platform::Steam));
        let odd = from_json(r#"{"hook":"nope","inject_delay":3}"#);
        assert_eq!((odd.hook, odd.inject_delay), (Hook::Auto, 3));
        // The key before the rename to Aurora still loads.
        assert!(!from_json(r#"{"grbmod_on":false}"#).aurora_on);
        assert_eq!(from_json("not json"), Saved::default());
    }

    fn sample() -> Saved {
        Saved {
            auto_inject: false,
            inject_delay: 12,
            extra_dlls: vec![ExtraDll { path: r"C:\Mods\a, b.dll".into(), on: true }, ExtraDll { path: r"D:\x.dll".into(), on: false }],
            aurora_on: false,
            hook: Hook::Driver,
            platform: Platform::Ubisoft,
            game_dir_steam: None,
            game_dir_ubisoft: Some(PathBuf::from(r"E:\Ubisoft\Ghost Recon Breakpoint")),
        }
    }

    #[test]
    fn settings_text_reads_back() {
        let s = sample();
        let text = to_text(&s);
        assert_eq!(
            text,
            "Automatically Inject: Off\nInject Delay: 12\nLoad Aurora: Off\nHook: Driver\nPlatform: Ubisoft Connect\n\
             Ubisoft Connect Game Folder: E:\\Ubisoft\\Ghost Recon Breakpoint\nExtra DLL: On, C:\\Mods\\a, b.dll\nExtra DLL: Off, D:\\x.dll\n"
        );
        assert_eq!(from_text(&text), s);
        assert_eq!(from_text(&to_text(&Saved::default())), Saved::default());
        // The mod finds its line in the same text.
        assert_eq!(aurora_core::line_value(&text, aurora_core::HOOK_LINE), Some("Driver"));
        // Typed by hand: any case, CRLF, a byte-order mark, odd values only reset their own line.
        let hand = "\u{feff}hook: OFF\r\nINJECT DELAY: soon\r\nplatform: steam\r\nAutomatically Inject: nope\r\nNonsense line\r\nExtra DLL: C:\\y.dll\r\n";
        let s = from_text(hand);
        assert_eq!((s.hook, s.inject_delay, s.platform, s.auto_inject), (Hook::Off, 0, Platform::Steam, true));
        assert_eq!(s.extra_dlls, vec![ExtraDll { path: r"C:\y.dll".into(), on: true }]);
        assert_eq!(from_text(""), Saved::default());
    }

    #[test]
    fn old_json_is_carried_over_and_kept() {
        // AURORA_TEST_DIR when set, else the temp folder.
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("aurora-lpsettings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (new, old, old_dir) = (dir.join("Borealis").join("Aurora.Launchpad").join("Settings.txt"), dir.join("Aurora").join("launchpad.json"), dir.join("Aurora").join("Dumper").join("old"));
        let write = |p: &Path, t: &str| -> Result<(), String> {
            std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(p, t).map_err(|e| e.to_string())
        };
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        // Nothing old: nothing happens, nothing is made.
        assert!(carry_over_in(&new, &old, &old_dir, &write).is_empty());
        assert!(!new.exists() && !old_dir.exists());
        // Only the old file: its values go into Settings.txt, it moves into Dumper\old.
        let json = r#"{"auto_inject":false,"inject_delay":5,"extra_dlls":[{"path":"C:\\m.dll","on":false}],"aurora_on":true,"hook":"off","platform":"ubisoft","game_dir_steam":"D:\\GRB","game_dir_ubisoft":null}"#;
        std::fs::write(&old, json).unwrap();
        let done = carry_over_in(&new, &old, &old_dir, &write);
        assert_eq!(done.len(), 2, "{done:?}");
        let s = from_text(&std::fs::read_to_string(&new).unwrap());
        assert_eq!((s.auto_inject, s.inject_delay, s.hook, s.platform), (false, 5, Hook::Off, Platform::Ubisoft));
        assert_eq!(s.game_dir_steam, Some(PathBuf::from(r"D:\GRB")));
        assert_eq!(s.extra_dlls, vec![ExtraDll { path: r"C:\m.dll".into(), on: false }]);
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(old_dir.join("launchpad.json")).unwrap(), json);
        // Both there (an old Launchpad ran again): Settings.txt stays as it is, the old file just
        // moves, beside the first one.
        let before = std::fs::read_to_string(&new).unwrap();
        std::fs::write(&old, r#"{"inject_delay":99}"#).unwrap();
        carry_over_in(&new, &old, &old_dir, &write);
        assert_eq!(std::fs::read_to_string(&new).unwrap(), before);
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(old_dir.join("launchpad 2.json")).unwrap(), r#"{"inject_delay":99}"#);
        assert_eq!(std::fs::read_to_string(old_dir.join("launchpad.json")).unwrap(), json);
        // A write that fails: the old file stays where it was.
        std::fs::remove_file(&new).unwrap();
        std::fs::write(&old, json).unwrap();
        let fail = |_: &Path, _: &str| -> Result<(), String> { Err("disk full".into()) };
        carry_over_in(&new, &old, &old_dir, &fail);
        assert!(old.is_file() && !new.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
