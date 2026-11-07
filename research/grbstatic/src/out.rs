//! Where result files go, and the guard that keeps them out of the game folder.
//!
//! Default: `%APPDATA%\Aurora\Dumper\research\static\` (per-exe files in a `<build-id>\` folder
//! below it), only when `Aurora` is already there: making it here would stop the Launchpad from
//! moving the player's old folders over (same rule as grbresearch). `--out DIR` replaces the base
//! folder. A folder inside the game folder is always refused: this tool never writes next to the
//! game.

use std::path::{Component, Path, PathBuf};

pub fn default_base() -> Result<PathBuf, String> {
    let root = aurora_core::paths::root();
    if !root.is_dir() {
        return Err(format!("{} is not there yet: start the Aurora Launchpad once, or pass --out FOLDER", root.display()));
    }
    Ok(aurora_core::paths::dumper().join("research").join("static"))
}

/// The base folder for this run: `--out` or the default, never inside a game folder.
pub fn base(out: Option<&Path>, game_dirs: &[&Path]) -> Result<PathBuf, String> {
    let b = match out {
        Some(p) => p.to_path_buf(),
        None => default_base()?,
    };
    for g in game_dirs {
        if inside(&b, g) {
            let g = g.display().to_string();
            let g = g.strip_prefix(r"\\?\").unwrap_or(&g);
            return Err(format!("refusing to write into {}: it is inside the game folder {g}; pick a folder elsewhere", b.display()));
        }
    }
    Ok(b)
}

/// Lower-case path parts after resolving what exists on disk (links, 8.3 names, `..`).
fn parts(p: &Path) -> Vec<String> {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    // Canonicalize the longest existing ancestor, then add the rest lexically.
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    let real = loop {
        if let Ok(c) = cur.canonicalize() {
            break c;
        }
        match (cur.file_name(), cur.parent()) {
            (Some(n), Some(parent)) => {
                tail.push(n.to_os_string());
                cur = parent;
            }
            _ => break cur.to_path_buf(),
        }
    };
    let mut full = real;
    for n in tail.iter().rev() {
        full.push(n);
    }
    lexical(&full)
}

/// Path parts with `.` and `..` resolved, lower case, `\\?\` prefix dropped.
fn lexical(p: &Path) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for c in p.components() {
        match c {
            Component::Prefix(x) => {
                let s = x.as_os_str().to_string_lossy().to_lowercase();
                v.push(s.trim_start_matches(r"\\?\").trim_start_matches("unc\\").to_string());
            }
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if v.len() > 1 {
                    v.pop();
                }
            }
            Component::Normal(n) => v.push(n.to_string_lossy().to_lowercase()),
        }
    }
    v
}

/// Is `child` the folder `parent` or below it?
pub fn inside(child: &Path, parent: &Path) -> bool {
    let (c, p) = (parts(child), parts(parent));
    !p.is_empty() && c.len() >= p.len() && c[..p.len()] == p[..]
}

pub fn write(dir: &Path, name: &str, data: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join(name);
    std::fs::write(&p, data).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(p)
}

pub fn write_json(dir: &Path, name: &str, v: &serde_json::Value) -> Result<PathBuf, String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    write(dir, name, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_parts_ignore_case_and_dots() {
        assert_eq!(lexical(Path::new(r"C:\Games\GRB\..\GRB\x")), vec!["c:", "games", "grb", "x"]);
        assert_eq!(lexical(Path::new(r"\\?\C:\A\b")), vec!["c:", "a", "b"]);
    }

    #[test]
    fn refuses_folders_inside_the_game_folder() {
        // Paths that do not exist are compared lexically: no files are touched.
        let game = Path::new(r"Q:\nowhere-grbstatic\steamapps\common\Ghost Recon Breakpoint");
        assert!(inside(Path::new(r"Q:\nowhere-grbstatic\steamapps\common\ghost recon breakpoint\out"), game));
        assert!(inside(Path::new(r"Q:\nowhere-grbstatic\steamapps\common\Ghost Recon Breakpoint"), game));
        assert!(inside(Path::new(r"Q:\nowhere-grbstatic\steamapps\common\Ghost Recon Breakpoint\a\..\b"), game));
        assert!(!inside(Path::new(r"Q:\nowhere-grbstatic\steamapps\common\Ghost Recon Breakpoint\..\x"), game));
        assert!(!inside(Path::new(r"C:\research\static"), game));
        assert!(base(Some(Path::new(r"Q:\nowhere-grbstatic\steamapps\common\Ghost Recon Breakpoint\r")), &[game]).is_err());
        assert!(base(Some(Path::new(r"Q:\nowhere-grbstatic\r")), &[game]).is_ok());
    }
}
