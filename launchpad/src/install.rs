//! The Launchpad's only files: the folders under `%APPDATA%\Aurora`, made when missing. The mod's
//! DLL is always loaded from `Aurora\Bin` and only read here: the exe carries no DLL and never
//! writes Bin's. An old `%APPDATA%\grbmod` is moved to `Aurora` first.

use std::path::Path;

use aurora_core::paths;

/// Move the old folders in (`%APPDATA%\grbmod` to `Aurora`, then `%LOCALAPPDATA%\grbmod` to
/// `Aurora\Dumper`, then the old `dumps` to `Dumper\dumps`), then make the folders that are
/// missing. Returns what it did, one line each. When a move fails nothing more is made (an empty
/// `Aurora` or `Dumper` would hide the player's files and stop the move for good), and the caller
/// must not inject.
pub fn ensure_folders() -> Result<Vec<String>, String> {
    // In this order: each later one lives inside an earlier one's new place.
    let moves = [
        (paths::old_root(), paths::root()),
        (paths::old_dumper(), paths::dumper()),
        (paths::old_dumps(), paths::dumps()),
    ];
    move_and_make(&moves, &paths::folders())
}

/// [`ensure_folders`] with its folders given (the tests use their own). Each move's new folder's
/// parent is made first when the old one is there (`Dumper` goes inside `Aurora`; safe then, as
/// every earlier move has been dealt with).
fn move_and_make(moves: &[(std::path::PathBuf, std::path::PathBuf)], folders: &[std::path::PathBuf]) -> Result<Vec<String>, String> {
    let mut made = Vec::new();
    for (old, new) in moves {
        if old.is_dir()
            && let Some(parent) = new.parent()
        {
            std::fs::create_dir_all(parent).map_err(|e| format!("could not make {}: {e}", parent.display()))?;
        }
        match paths::move_root(old, new) {
            Ok(paths::Moved::Nothing) => {}
            Ok(paths::Moved::Moved) => made.push(format!("moved {} to {}", old.display(), new.display())),
            Ok(paths::Moved::BothThere) => made.push(format!("{} is left alone: {} is used (move anything you still need by hand)", old.display(), new.display())),
            Err(e) => {
                return Err(format!(
                    "could not move {} to {}: {e}. Close the game and any window showing that folder, then start the Launchpad again",
                    old.display(),
                    new.display()
                ));
            }
        }
    }
    let mut names = Vec::new();
    for dir in folders {
        if dir.is_dir() {
            continue;
        }
        std::fs::create_dir_all(dir).map_err(|e| format!("could not make {}: {e}", dir.display()))?;
        names.push(dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    }
    if !names.is_empty() {
        made.push(format!("made folders: {}", names.join(", ")));
    }
    Ok(made)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The player's PC, in a test folder: `Roaming\grbmod` (with its lowercase `dumps`) and
    /// `Local\grbmod`; the moves in the same order as `ensure_folders`.
    fn setup(name: &str) -> (PathBuf, [(PathBuf, PathBuf); 3], Vec<PathBuf>) {
        // AURORA_TEST_DIR when set, else the temp folder.
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("aurora-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (roaming, local) = (dir.join("Roaming"), dir.join("Local"));
        std::fs::create_dir_all(roaming.join("grbmod").join("Profiles")).unwrap();
        std::fs::write(roaming.join("grbmod").join("Profiles").join("Main.txt"), "Self\n").unwrap();
        std::fs::write(roaming.join("grbmod").join("Hotkeys.txt"), "keys").unwrap();
        std::fs::create_dir_all(roaming.join("grbmod").join("dumps").join("run1")).unwrap();
        std::fs::write(roaming.join("grbmod").join("dumps").join("run1").join("a.bin"), "dump").unwrap();
        std::fs::create_dir_all(local.join("grbmod").join("research")).unwrap();
        std::fs::write(local.join("grbmod").join("dll.log"), "log").unwrap();
        std::fs::write(local.join("grbmod").join("research").join("research.json"), "{}").unwrap();
        let root = roaming.join("Aurora");
        let moves = [
            (roaming.join("grbmod"), root.clone()),
            (local.join("grbmod"), root.join("Dumper")),
            (root.join("dumps"), root.join("Dumper").join("dumps")),
        ];
        let folders = ["", "Bin", "Bin\\Temp", "Places", "Profiles", "Dumper"].iter().map(|f| if f.is_empty() { root.clone() } else { root.join(f) }).collect();
        (dir, moves, folders)
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap_or_default()
    }

    #[test]
    fn both_old_folders_move_into_aurora() {
        let (dir, moves, folders) = setup("move");
        let done = super::move_and_make(&moves, &folders).unwrap();
        let root = &moves[0].1;
        assert_eq!(read(&root.join("Profiles").join("Main.txt")), "Self\n");
        assert_eq!(read(&root.join("Hotkeys.txt")), "keys");
        assert_eq!(read(&root.join("Dumper").join("dll.log")), "log");
        assert_eq!(read(&root.join("Dumper").join("research").join("research.json")), "{}");
        assert_eq!(read(&root.join("Dumper").join("dumps").join("run1").join("a.bin")), "dump");
        assert!(moves.iter().all(|m| !m.0.exists()), "old folders gone: {done:?}");
        // The old dumps are inside Dumper now, not beside it (Windows paths ignore case, so look at
        // the listings).
        let names = |p: &Path| -> Vec<String> { std::fs::read_dir(p).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect() };
        assert!(names(&root.join("Dumper")).contains(&"dumps".to_string()), "{:?}", names(&root.join("Dumper")));
        assert!(!names(root).iter().any(|n| n.eq_ignore_ascii_case("dumps")), "{:?}", names(root));
        assert!(folders.iter().all(|f| f.is_dir()));
        // A second start: nothing to move, nothing to make.
        assert!(super::move_and_make(&moves, &folders).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_locked_old_folder_makes_nothing() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, moves, folders) = setup("locked");
        // As the game holds a file in it: open with no sharing, so the folder can't be renamed.
        let lock = std::fs::OpenOptions::new().read(true).share_mode(0).open(moves[0].0.join("Hotkeys.txt")).unwrap();
        assert!(super::move_and_make(&moves, &folders).is_err());
        assert!(!moves[0].1.exists(), "no Aurora made beside the old folder");
        assert!(moves[1].0.is_dir(), "the log folder stays until the first move worked");
        drop(lock);
        // Once it is free, the next start moves both.
        super::move_and_make(&moves, &folders).unwrap();
        assert_eq!(read(&moves[0].1.join("Hotkeys.txt")), "keys");
        assert_eq!(read(&moves[1].1.join("dll.log")), "log");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Bin's DLL as the Launchpad finds it. The Launchpad never writes it.
#[derive(Debug, PartialEq, Eq)]
pub enum Dll {
    /// There, and how long ago it was written.
    There(Option<std::time::Duration>),
    /// Not in Bin: nothing to inject.
    Missing,
}

/// Look at Bin's DLL. Reads only.
pub fn check_dll() -> Dll {
    check_at(&paths::dll())
}

/// [`check_dll`] for a given path (the tests use their own).
fn check_at(p: &Path) -> Dll {
    match std::fs::metadata(p) {
        Ok(m) if m.is_file() => Dll::There(m.modified().ok().and_then(|t| t.elapsed().ok())),
        _ => Dll::Missing,
    }
}

/// The status line's short form of [`missing_text`] (room for about 44 characters; the whole text
/// is its tooltip).
pub fn missing_short() -> String {
    format!("No {} in Aurora\\Bin.", paths::DLL_FILE)
}

/// What the Launchpad says when Bin has no DLL.
pub fn missing_text() -> String {
    format!("no {}: {}", paths::DLL_FILE, copy_hint(&paths::dll()))
}

/// What to do when Bin's DLL is missing or refused, in plain words:
/// the one stamped DLL from the build folder, into Bin (its stamp is inside it since Gatekeeper
/// step 1, no.sig). On the build PC `tools\swap-dll.ps1` does that.
pub fn copy_hint(dll: &Path) -> String {
    let name = dll.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bin = dll.parent().map(|d| d.display().to_string()).unwrap_or_default();
    format!("copy {name} from the build folder into {bin}")
}

#[cfg(test)]
mod dll_tests {
    use super::{Dll, check_at};

    /// Test files go to AURORA_TEST_DIR when set, else the temp folder.
    fn dir(name: &str) -> std::path::PathBuf {
        let base = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("aurora-bin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_missing_dll_is_missing_and_nothing_is_made() {
        let d = dir("missing");
        let p = d.join("aurora.dll");
        assert_eq!(check_at(&p), Dll::Missing);
        assert!(!p.exists(), "the check never writes");
        // A folder with the DLL's name is not a DLL.
        std::fs::create_dir_all(&p).unwrap();
        assert_eq!(check_at(&p), Dll::Missing);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bins_dll_is_used_as_it_is() {
        let d = dir("there");
        let p = d.join("aurora.dll");
        std::fs::write(&p, b"the player's build").unwrap();
        assert!(matches!(check_at(&p), Dll::There(_)));
        assert_eq!(std::fs::read(&p).unwrap(), b"the player's build", "left exactly as it was");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The exe has no DLL inside any more (no build script, no embedded bytes).
    #[test]
    fn the_exe_carries_no_dll() {
        let manifest = env!("CARGO_MANIFEST_DIR");
        assert!(!std::path::Path::new(manifest).join("build.rs").exists());
        assert!(option_env!("AURORA_EMBED_DLL").is_none());
    }
}
