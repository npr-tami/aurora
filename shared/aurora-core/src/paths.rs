//! Every file and folder Aurora uses, in one place, with who makes it and who writes it.
//!
//! `%APPDATA%\Aurora\`:
//!
//! | Path | Made by | Written by | Read by |
//! |---|---|---|---|
//! | `Aurora\`, `Bin\`, `Bin\Temp\`, `Bin\Cache\`, `Places\`, `Profiles\`, `Dumper\` | Launchpad (when missing) | - | both |
//! | `Bin\Temp\*.tmp` | whoever saves a file | same, then renamed into place | nobody |
//! | `Bin\aurora.dll` | Launchpad (install) | Launchpad | the game loads it |
//! | `Bin\hook.txt` | mod | mod (at inject, when it hooks) | Launchpad (Options) |
//! | `Bin\Launchpad.lock` | Launchpad (at start) | nobody (empty) | a second Launchpad: held means one is open. Windows deletes it when the Launchpad closes (as Stand's) |
//! | `Bin\Cache\Offsets.bin` | mod | mod (after a code search) | mod (before searching; per game build) |
//! | `Hotkeys.txt` | mod | mod | mod; Launchpad reads the menu key for its status line |
//! | `Saved Commands.txt` | mod | mod | mod |
//! | `Meta State.txt` | mod | mod | mod (profiles' Load On Inject and Automatically Save) |
//! | `Places\<name>.txt` | mod | mod | mod |
//! | `Profiles\...` | mod | mod | mod: every setting lives here, menu look included (as Stand's) |
//! | `unload.request` | test scripts | scripts | mod deletes it and unloads itself (as menu > Unload) |
//! | `quit.request` | test scripts | scripts | Launchpad deletes it and closes (the mod keeps running) |
//! | `command.request` | test scripts | scripts | mod runs it and deletes it |
//! | `keys.json`, `row_keys.json`, `saved.json`, `teleports.json`, `spots.json` | old builds | nobody now | the mod, once, to carry old data over |
//! | `settings.json`, `menu.json` | old builds | nobody now | the mod, once at inject: into the Load On Inject profile, then moved to `Dumper\old` |
//! | `Offsets.json` | old builds | nobody now | nobody: the mod moves it to `Dumper\old` (the cache rebuilds) |
//! | `launchpad.json` | old Launchpad | nobody now | the Launchpad, once: into its `Settings.txt`, then moved to `Dumper\old` |
//! | `Dumper\old\` | mod or Launchpad (when moving an old file) | the moves | people |
//! | `Dumper\dll.log` | mod | mod (appends) | people: start here when something goes wrong |
//! | `Dumper\flytrace.log` | mod | mod (appends; stops growing at 10 MB) | people: what the game did to the player around a Fly off |
//! | `Dumper\target.txt` | mod | mod (Target > Pick) | mod |
//! | `Dumper\research\`, `Dumper\research-dll.log` | research tools | grbresearch, grbresearch-dll | research tools |
//! | `Dumper\dumps\` (was `grbmod\dumps`) | old launcher | nobody now | people |
//! | `Dumper\*` (old probes, watch lists, bone dumps) | old builds | nobody now | people |
//!
//! `%LOCALAPPDATA%\Borealis\Aurora.Launchpad\`:
//!
//! | Path | Made by | Written by | Read by |
//! |---|---|---|---|
//! | `Settings.txt` | Launchpad (when it saves) | Launchpad, through `Settings.txt.tmp` beside it | Launchpad; the mod reads `Hook` once at inject |
//!
//! `Dumper\` was `%LOCALAPPDATA%\grbmod\` before; the Launchpad moves that one in at its start too,
//! see [`dumper`]. Nothing writes there before `Aurora` exists (see [`dumper_for_writing`]).
//!
//! Files the mod rewrites go through [`write_file`], so a crash mid-write never leaves half a file;
//! the half-written copy lives in `Bin\Temp`. Folder names start with a capital letter, except the
//! ones inside `Dumper` (`research`, `dumps`, `bones_dump`): those stay lowercase.

use std::path::{Path, PathBuf};

fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

fn localappdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// The vendor folder in `%LOCALAPPDATA%` that holds the Launchpad's own folder, as Stand's
/// `Calamity, Inc.` does. A working name the player may still change: change it here only, every
/// path below follows.
pub const VENDOR: &str = "Borealis";

/// `%APPDATA%\Aurora`: every file of the mod and the Launchpad.
pub fn root() -> PathBuf {
    appdata().join("Aurora")
}

/// `%APPDATA%\grbmod`: where everything lived before.
pub fn old_root() -> PathBuf {
    appdata().join("grbmod")
}

/// What [`move_root`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Moved {
    /// No old folder: nothing to do.
    Nothing,
    /// The old folder is the new one now, with everything in it.
    Moved,
    /// Both are there: the new one is used, the old one is left alone (never merged or deleted).
    BothThere,
}

/// Move the old `grbmod` folder to `Aurora` when only the old one is there: one rename, so every
/// file (profiles, hotkeys, places, settings) moves together or nothing moves. The Launchpad calls
/// it before making any folder, so it never makes an empty `Aurora` beside the player's old files.
/// Fails while something holds a file in the old folder open (the game with the old DLL loaded, a
/// folder window): then the caller must stop and say so.
pub fn move_root(old: &Path, new: &Path) -> std::io::Result<Moved> {
    if !old.is_dir() {
        return Ok(Moved::Nothing);
    }
    if new.exists() {
        return Ok(Moved::BothThere);
    }
    std::fs::rename(old, new)?;
    Ok(Moved::Moved)
}

/// The folders the Launchpad makes when they are missing.
pub fn folders() -> [PathBuf; 7] {
    let r = root();
    [r.clone(), r.join("Bin"), temp(), cache(), r.join("Places"), r.join("Profiles"), dumper()]
}

/// `Aurora\Dumper`: logs and research files: the mod's `dll.log` and `target.txt`, the research
/// tools' `research\` and `research-dll.log`, old dumps.
pub fn dumper() -> PathBuf {
    root().join("Dumper")
}

/// `Dumper\dumps`: the old launcher's dumps, kept for reading. Was `grbmod\dumps`; moved here,
/// lowercase like the other folders inside `Dumper`.
pub fn dumps() -> PathBuf {
    dumper().join("dumps")
}

/// `Aurora\dumps`: where the old dumps land after the `grbmod` folder moves; the Launchpad then
/// moves them to [`dumps`].
pub fn old_dumps() -> PathBuf {
    root().join("dumps")
}

/// `%LOCALAPPDATA%\grbmod`: where the logs and research files lived before. The Launchpad moves it
/// to [`dumper`] with [`move_root`].
pub fn old_dumper() -> PathBuf {
    localappdata().join("grbmod")
}

/// `Dumper\old`: old files that are not used any more, moved here instead of deleted. See
/// [`move_aside`].
pub fn old_files() -> PathBuf {
    dumper().join("old")
}

/// Move a file into `dir` (made when missing) under its own name, or `name 2.ext`, `name 3.ext`
/// when that is taken: nothing already there is ever replaced. Gives where it went. Fails while the
/// file is in use; then it stays where it was.
pub fn move_aside(file: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = file.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let to = (1..1000)
        .map(|n| if n == 1 { dir.join(format!("{stem}{ext}")) } else { dir.join(format!("{stem} {n}{ext}")) })
        .find(|p| !p.exists())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} is full of old copies", dir.display())))?;
    std::fs::rename(file, &to)?;
    Ok(to)
}

/// [`dumper`], made when missing, but only inside an `Aurora` that is already there. `Aurora`
/// itself is the Launchpad's to make, after it has moved the old folders over: a log line written
/// first (a unit test, a research tool) would make an `Aurora` that stops that move and hides the
/// player's old files. `None` then; callers skip logging.
pub fn dumper_for_writing() -> Option<PathBuf> {
    if !root().is_dir() {
        return None;
    }
    let d = dumper();
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// `Bin\Temp`: files being written, before they are renamed into place.
pub fn temp() -> PathBuf {
    root().join("Bin").join("Temp")
}

/// The mod's DLL file name.
pub const DLL_FILE: &str = "aurora.dll";

/// `Bin\aurora.dll`: the only place the mod is loaded from.
pub fn dll() -> PathBuf {
    root().join("Bin").join(DLL_FILE)
}

/// `Bin\Launchpad.lock`: there only while a Launchpad is open, as Stand's. The open Launchpad holds
/// it alone; Windows deletes it when that one closes.
pub fn launchpad_lock() -> PathBuf {
    root().join("Bin").join("Launchpad.lock")
}

/// `Bin\hook.txt`: the mod's one line about where its drawing hooked.
pub fn hook_note() -> PathBuf {
    root().join("Bin").join("hook.txt")
}

/// `Bin\Cache`: what the mod keeps only to be faster (its found code offsets). Deleting it costs
/// one slow start, nothing more.
pub fn cache() -> PathBuf {
    root().join("Bin").join("Cache")
}

/// `Bin\Cache\Offsets.bin`: where each code search found its code, per game build.
pub fn offsets_cache() -> PathBuf {
    cache().join("Offsets.bin")
}

/// `Aurora\Offsets.json`: the cache before; the mod moves it to [`old_files`].
pub fn old_offsets() -> PathBuf {
    root().join("Offsets.json")
}

/// `%LOCALAPPDATA%\Borealis\Aurora.Launchpad`: the Launchpad's own folder, made when it saves.
pub fn launchpad_dir() -> PathBuf {
    localappdata().join(VENDOR).join("Aurora.Launchpad")
}

/// The Launchpad's own settings: `Settings.txt`, one `Key: Value` per line, as `Meta State.txt`.
pub fn launchpad_settings() -> PathBuf {
    launchpad_dir().join("Settings.txt")
}

/// `Aurora\launchpad.json`: the Launchpad's settings until. The Launchpad turns it into
/// [`launchpad_settings`] at its start and moves it to [`old_files`].
pub fn old_launchpad_settings() -> PathBuf {
    root().join("launchpad.json")
}

/// `Dumper\launchpad.log`: every message the Launchpad shows in its window, also written here, so
/// they can be read when the window can't be seen. Over 1 MB at start it becomes
/// `launchpad.old.log`.
pub fn launchpad_log() -> PathBuf {
    dumper().join("launchpad.log")
}

/// `Aurora\settings.json`: the mod's settings until (gear, fly, ESP, coordinates). They live in
/// profiles now, as Stand's; the mod carries them over once and moves the file to [`old_files`].
pub fn old_mod_settings() -> PathBuf {
    root().join("settings.json")
}

/// `Aurora\menu.json`: the menu's look and place until. Carried into profiles like
/// [`old_mod_settings`].
pub fn old_menu_settings() -> PathBuf {
    root().join("menu.json")
}

pub fn places() -> PathBuf {
    root().join("Places")
}

pub fn profiles() -> PathBuf {
    root().join("Profiles")
}

pub fn unload_request() -> PathBuf {
    root().join("unload.request")
}

pub fn quit_request() -> PathBuf {
    root().join("quit.request")
}

/// Write a whole file: first to `Bin\Temp`, then renamed into place, so the old file stays whole
/// until the new one is complete. Refused while `Aurora` is not there: making `Bin\Temp` would make
/// `Aurora` before the Launchpad has moved the old folder over (see [`dumper_for_writing`]).
pub fn write_file(path: &std::path::Path, text: impl AsRef<[u8]>) -> std::io::Result<()> {
    if !root().is_dir() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} is not there yet (the Launchpad makes it)", root().display())));
    }
    write_via(&temp(), path, text.as_ref())
}

/// [`write_file`] with its temp folder given (the tests use their own). The folder is made when
/// missing: a save must never be skipped because `Bin\Temp` was deleted.
fn write_via(temp: &std::path::Path, path: &std::path::Path, text: &[u8]) -> std::io::Result<()> {
    use std::hash::{Hash, Hasher};
    std::fs::create_dir_all(temp)?;
    // Same file name in two folders (a place and a profile) must not share one temp file.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = temp.join(format!("{name}.{:016x}.tmp", h.finish()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    /// Test files go to AURORA_TEST_DIR when set, else the temp folder.
    fn test_dir(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join(format!("aurora-{name}-{}", std::process::id()))
    }

    #[test]
    fn write_replaces_whole_and_leaves_no_temp() {
        let dir = test_dir("paths");
        let temp = dir.join("Bin").join("Temp");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Hotkeys.txt");
        super::write_via(&temp, &p, b"one").unwrap();
        super::write_via(&temp, &p, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        assert_eq!(std::fs::read_dir(&temp).unwrap().count(), 0, "temp folder left empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_start_with_a_capital() {
        for f in super::folders() {
            let name = f.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(|c: char| c.is_ascii_uppercase()), "{name}");
        }
        assert!(super::root().ends_with("Aurora"));
        assert!(super::temp().ends_with("Aurora\\Bin\\Temp"));
        assert!(super::dumper().ends_with("Aurora\\Dumper"));
        assert!(super::folders().contains(&super::dumper()));
        // The offsets cache's folder is made with the others.
        assert!(super::folders().contains(&super::cache()));
        assert!(super::offsets_cache().ends_with("Aurora\\Bin\\Cache\\Offsets.bin"));
        assert!(super::launchpad_settings().ends_with("Borealis\\Aurora.Launchpad\\Settings.txt"));
        assert!(super::old_files().ends_with("Aurora\\Dumper\\old"));
        assert!(super::launchpad_lock().ends_with("Aurora\\Bin\\Launchpad.lock"));
    }

    #[test]
    fn move_aside_never_replaces() {
        use super::move_aside;
        let dir = test_dir("aside");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old");
        let f = dir.join("settings.json");
        std::fs::write(&f, "one").unwrap();
        assert_eq!(move_aside(&f, &old).unwrap(), old.join("settings.json"));
        assert!(!f.exists());
        // A second one with the same name gets its own name; the first stays as it was.
        std::fs::write(&f, "two").unwrap();
        assert_eq!(move_aside(&f, &old).unwrap(), old.join("settings 2.json"));
        assert_eq!(std::fs::read_to_string(old.join("settings.json")).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(old.join("settings 2.json")).unwrap(), "two");
        // A file that is not there: an error, nothing made in its place.
        assert!(move_aside(&f, &old).is_err());
        assert!(!old.join("settings 3.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_folder_moves_whole_once() {
        use super::{Moved, move_root};
        let dir = test_dir("move");
        let _ = std::fs::remove_dir_all(&dir);
        let (old, new) = (dir.join("grbmod"), dir.join("Aurora"));
        // No old folder: nothing happens, nothing is made.
        assert_eq!(move_root(&old, &new).unwrap(), Moved::Nothing);
        assert!(!new.exists());
        // Only the old one: it becomes the new one, files and subfolders with it.
        std::fs::create_dir_all(old.join("Profiles")).unwrap();
        std::fs::write(old.join("Profiles").join("Main.txt"), "Self\n").unwrap();
        std::fs::write(old.join("Hotkeys.txt"), "keys").unwrap();
        assert_eq!(move_root(&old, &new).unwrap(), Moved::Moved);
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("Profiles").join("Main.txt")).unwrap(), "Self\n");
        assert_eq!(std::fs::read_to_string(new.join("Hotkeys.txt")).unwrap(), "keys");
        // Both there: neither is touched.
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("stray.txt"), "x").unwrap();
        assert_eq!(move_root(&old, &new).unwrap(), Moved::BothThere);
        assert!(old.join("stray.txt").is_file());
        assert!(new.join("Hotkeys.txt").is_file());
        assert!(!new.join("stray.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
