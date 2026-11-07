//! Steam and Ubisoft Connect: find each store's copy of Breakpoint and start it the store's way.
//! Found from the stores' own records, never a fixed folder; a folder the player picks by hand is
//! the fallback.
//!
//! Steam: `HKCU\Software\Valve\Steam` `SteamPath`, then `steamapps\libraryfolders.vdf` lists the
//! libraries, a library holding the game has `steamapps\appmanifest_2231380.acf` whose `installdir`
//! names `steamapps\common\<dir>`. Launch: `steam://rungameid/2231380` (Steam then starts Ubisoft
//! Connect by itself).
//!
//! Ubisoft Connect: `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\<id>` `InstallDir` per
//! installed game. The id is read, not assumed: the one whose folder holds the game. Launch:
//! `uplay://launch/<id>/0`; without an id, the game's exe itself (it hands over to Ubisoft
//! Connect).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const STEAM_APP: u32 = 2231380;

/// The game's exe names: DX11 and Vulkan, and the Ubisoft+ builds.
pub const GAME_EXES: [&str; 4] = ["GRB.exe", "GRB_vulkan.exe", "GRB_UPP.exe", "GRB_UPP_vulkan.exe"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Steam,
    Ubisoft,
}

impl Platform {
    pub const ALL: [Platform; 2] = [Platform::Steam, Platform::Ubisoft];

    pub fn name(self) -> &'static str {
        match self {
            Platform::Steam => "Steam",
            Platform::Ubisoft => "Ubisoft Connect",
        }
    }
}

/// One copy of the game: its folder, and the Ubisoft id when the store gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub dir: PathBuf,
    pub ubisoft_id: Option<String>,
}

/// Does this folder hold the game?
pub fn is_game_dir(dir: &Path) -> bool {
    GAME_EXES.iter().any(|e| dir.join(e).is_file())
}

/// The store's copy: what the store records say, else the folder the player picked (if it still
/// holds the game).
pub fn find(p: Platform, picked: Option<&PathBuf>) -> Option<Install> {
    let found = match p {
        Platform::Steam => steam_dir().map(|dir| Install { dir, ubisoft_id: None }),
        Platform::Ubisoft => ubisoft_install(),
    };
    found.or_else(|| {
        let dir = picked?;
        is_game_dir(dir).then(|| Install { dir: dir.clone(), ubisoft_id: ubisoft_id_for(dir) })
    })
}

/// Start the game through the store. `Err` says what is missing.
pub fn launch(p: Platform, install: Option<&Install>) -> Result<String, String> {
    let open = |what: &str| -> Result<(), String> {
        // `explorer <uri>` hands the link to its registered app, as a double-click would.
        std::process::Command::new("explorer").arg(what).spawn().map(|_| ()).map_err(|e| e.to_string())
    };
    match p {
        Platform::Steam => {
            if steam_root().is_none() {
                return Err("Steam is not installed (no Steam in the registry)".into());
            }
            open(&format!("steam://rungameid/{STEAM_APP}"))?;
            Ok("Steam is starting the game.".into())
        }
        Platform::Ubisoft => {
            if ubisoft_root().is_none() {
                return Err("Ubisoft Connect is not installed".into());
            }
            let install = install.ok_or("Breakpoint not found in Ubisoft Connect: pick its folder in Advanced (Locate...)")?;
            if let Some(id) = &install.ubisoft_id {
                open(&format!("uplay://launch/{id}/0"))?;
                return Ok("Ubisoft Connect is starting the game.".into());
            }
            let exe = GAME_EXES.iter().map(|e| install.dir.join(e)).find(|e| e.is_file()).ok_or("the game folder has no game exe")?;
            std::process::Command::new(&exe).current_dir(&install.dir).spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
            Ok("Starting the game; it hands over to Ubisoft Connect.".into())
        }
    }
}

// ---- Steam ----

fn steam_root() -> Option<PathBuf> {
    let from_reg = reg::string(reg::HKCU, r"Software\Valve\Steam", "SteamPath")
        .or_else(|| reg::string(reg::HKLM, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"))
        .or_else(|| reg::string(reg::HKLM, r"SOFTWARE\Valve\Steam", "InstallPath"))
        .map(PathBuf::from);
    from_reg.filter(|p| p.is_dir())
}

/// Every Steam library folder, the main one first.
fn steam_libraries(root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![root.to_path_buf()];
    for file in [root.join("steamapps").join("libraryfolders.vdf"), root.join("config").join("libraryfolders.vdf")] {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        for p in vdf_values(&text, "path") {
            let p = PathBuf::from(p);
            if !libs.iter().any(|l| same_dir(l, &p)) {
                libs.push(p);
            }
        }
    }
    libs
}

fn steam_dir() -> Option<PathBuf> {
    let root = steam_root()?;
    steam_libraries(&root).into_iter().find_map(|lib| {
        let apps = lib.join("steamapps");
        let manifest = std::fs::read_to_string(apps.join(format!("appmanifest_{STEAM_APP}.acf"))).ok()?;
        let dir = apps.join("common").join(vdf_values(&manifest, "installdir").into_iter().next()?);
        is_game_dir(&dir).then_some(dir)
    })
}

/// All values of `"key" "value"` pairs in a Valve KeyValues text, unescaped (`\\` to `\`).
pub fn vdf_values(text: &str, key: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                s.push(n);
                            }
                        }
                        '"' => break,
                        c => s.push(c),
                    }
                }
                tokens.push(Some(s));
            }
            '{' | '}' => tokens.push(None),
            _ => {}
        }
    }
    tokens
        .windows(2)
        .filter_map(|w| match w {
            [Some(k), Some(v)] if k.eq_ignore_ascii_case(key) => Some(v.clone()),
            _ => None,
        })
        .collect()
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

// ---- Ubisoft Connect ----

const UBI_LAUNCHER: &str = r"SOFTWARE\WOW6432Node\Ubisoft\Launcher";

fn ubisoft_root() -> Option<PathBuf> {
    reg::string(reg::HKLM, UBI_LAUNCHER, "InstallDir").map(PathBuf::from).filter(|p| p.is_dir())
}

/// (id, folder) for every game Ubisoft Connect lists with a folder.
fn ubisoft_installs() -> Vec<(String, PathBuf)> {
    let key = format!(r"{UBI_LAUNCHER}\Installs");
    reg::subkeys(reg::HKLM, &key)
        .into_iter()
        .filter_map(|id| {
            let dir = reg::string(reg::HKLM, &format!(r"{key}\{id}"), "InstallDir")?;
            Some((id, PathBuf::from(dir)))
        })
        .collect()
}

fn ubisoft_install() -> Option<Install> {
    ubisoft_installs()
        .into_iter()
        .find(|(_, dir)| is_game_dir(dir))
        .map(|(id, dir)| Install { dir, ubisoft_id: Some(id) })
}

/// The Ubisoft id of a folder the player picked, if Ubisoft Connect lists that folder.
fn ubisoft_id_for(dir: &Path) -> Option<String> {
    ubisoft_installs().into_iter().find(|(_, d)| same_dir(d, dir)).map(|(id, _)| id)
}

/// Windows' own folder picker; None if the player cancels.
pub fn pick_folder() -> Option<PathBuf> {
    use windows_sys::Win32::UI::Shell::{BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW, SHGetPathFromIDListW};
    let title: Vec<u16> = "Pick the Ghost Recon Breakpoint folder (the one with GRB.exe)\0".encode_utf16().collect();
    // SAFETY: plain struct of numbers and pointers; zero is its documented empty state.
    let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
    bi.lpszTitle = title.as_ptr();
    bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    // SAFETY: `bi` and `title` live for the call.
    let list = unsafe { SHBrowseForFolderW(&bi) };
    if list.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 1024];
    // SAFETY: `list` came from SHBrowseForFolderW; the buffer is MAX_PATH or more.
    let ok = unsafe { SHGetPathFromIDListW(list, buf.as_mut_ptr()) } != 0;
    // SAFETY: the shell allocated `list` with the COM allocator.
    unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(list as _) };
    if !ok {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..end])))
}

/// Registry reads, nothing else.
mod reg {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    };

    pub const HKCU: HKEY = HKEY_CURRENT_USER;
    pub const HKLM: HKEY = HKEY_LOCAL_MACHINE;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A string value, `None` if missing or empty.
    pub fn string(root: HKEY, key: &str, name: &str) -> Option<String> {
        let (k, n) = (wide(key), wide(name));
        let mut buf = vec![0u16; 2048];
        let mut bytes = (buf.len() * 2) as u32;
        // SAFETY: buffers sized as told; the type flags only accept strings.
        let r = unsafe {
            RegGetValueW(root, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut bytes)
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let s = String::from_utf16_lossy(&buf[..end]);
        (!s.trim().is_empty()).then_some(s)
    }

    /// The names of a key's subkeys.
    pub fn subkeys(root: HKEY, key: &str) -> Vec<String> {
        let k = wide(key);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: opens for reading; closed below.
        if unsafe { RegOpenKeyExW(root, k.as_ptr(), 0, KEY_READ, &mut h) } != ERROR_SUCCESS {
            return Vec::new();
        }
        let mut out = Vec::new();
        for i in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: name buffer sized as told; the rest are optional outputs left null.
            let r = unsafe {
                RegEnumKeyExW(h, i, name.as_mut_ptr(), &mut len, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
            };
            if r != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        // SAFETY: opened above.
        unsafe { RegCloseKey(h) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_paths_from_vdf() {
        let text = r#""libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps"
		{
			"2231380"		"71164415687"
		}
	}
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}"#;
        assert_eq!(vdf_values(text, "path"), vec![r"C:\Program Files (x86)\Steam".to_string(), r"D:\SteamLibrary".to_string()]);
        let acf = "\"AppState\"\n{\n\t\"appid\"\t\t\"2231380\"\n\t\"installdir\"\t\t\"Ghost Recon Breakpoint\"\n}";
        assert_eq!(vdf_values(acf, "installdir"), vec!["Ghost Recon Breakpoint".to_string()]);
        // A key's name as a value is not taken for the key.
        assert!(vdf_values(r#""a" "path" "b" "x""#, "path").len() <= 1);
    }

    #[test]
    fn same_dir_ignores_case_and_slashes() {
        assert!(same_dir(Path::new("c:/program files (x86)/steam"), Path::new(r"C:\Program Files (x86)\Steam\")));
    }
}
