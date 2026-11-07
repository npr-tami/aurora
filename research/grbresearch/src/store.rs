//! The research folder: research.json, saved scans in scans\, snapshots in snaps\. All plain JSON.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// `Aurora\Dumper\research`. Refused while `Aurora` is not there: making it here would stop the
/// Launchpad from moving the player's old folders over (see
/// `aurora_core::paths::dumper_for_writing`).
pub fn default_dir() -> Result<PathBuf, String> {
    if !aurora_core::paths::root().is_dir() {
        return Err(format!(
            "{} is not there yet: start the Aurora Launchpad once (it moves the old grbmod folders there), or pass --dir",
            aurora_core::paths::root().display()
        ));
    }
    Ok(aurora_core::paths::dumper().join("research"))
}

/// Make the folder and its sub folders on first use.
pub fn ensure(dir: &Path) -> Result<(), String> {
    for d in [dir.to_path_buf(), dir.join("scans"), dir.join("snaps")] {
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    Ok(())
}

/// Names become file names: keep them simple.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) {
        return Err(format!("bad name '{name}': use letters, digits, _ - . (max 64)"));
    }
    Ok(())
}

/// Today as YYYY-MM-DD (UTC).
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to (year, month, day). Howard Hinnant's algorithm.
pub fn civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// One scan hit: address and the raw value seen there (numeric scans only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub a: u64,
    #[serde(default)]
    pub v: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedScan {
    pub name: String,
    /// Value | aob | ptr | vtable
    pub kind: String,
    /// Value type for value scans (u32, f32...).
    pub vtype: String,
    /// Float tolerance used for eq.
    pub tol: f64,
    /// What was searched for, as typed.
    pub query: String,
    pub exe: String,
    pub build: String,
    pub pid: u32,
    pub date: String,
    /// Steps taken so far: "scan value f32 100", "next decreased", ...
    pub history: Vec<String>,
    /// Address spans that were scanned, [start, end).
    pub spans: Vec<[u64; 2]>,
    pub truncated: bool,
    pub cancelled: bool,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Snapshot {
    pub name: String,
    pub expr: String,
    pub addr: u64,
    pub len: usize,
    pub exe: String,
    pub build: String,
    pub pid: u32,
    pub date: String,
    /// Milliseconds since 1970, to order snapshots.
    pub time_ms: u64,
    /// Hex of the bytes that could be read (may be shorter than len).
    pub bytes: String,
    /// Why the read stopped early, if it did.
    pub error: String,
}

impl Snapshot {
    pub fn data(&self) -> Vec<u8> {
        (0..self.bytes.len() / 2)
            .filter_map(|i| u8::from_str_radix(&self.bytes[i * 2..i * 2 + 2], 16).ok())
            .collect()
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn save_json<T: Serialize>(path: &Path, v: &T) -> Result<(), String> {
    let text = serde_json::to_string(v).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn save_scan(dir: &Path, s: &SavedScan) -> Result<PathBuf, String> {
    check_name(&s.name)?;
    ensure(dir)?;
    let p = dir.join("scans").join(format!("{}.json", s.name));
    save_json(&p, s)?;
    Ok(p)
}

pub fn load_scan(dir: &Path, name: &str) -> Result<SavedScan, String> {
    check_name(name)?;
    load_json(&dir.join("scans").join(format!("{name}.json")))
}

pub fn save_snap(dir: &Path, s: &Snapshot) -> Result<PathBuf, String> {
    check_name(&s.name)?;
    ensure(dir)?;
    let p = dir.join("snaps").join(format!("{}.json", s.name));
    save_json(&p, s)?;
    Ok(p)
}

pub fn load_snap(dir: &Path, name: &str) -> Result<Snapshot, String> {
    check_name(name)?;
    load_json(&dir.join("snaps").join(format!("{name}.json")))
}

/// Names of saved items in a sub folder ("scans" or "snaps").
pub fn list(dir: &Path, sub: &str) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir.join(sub))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string))
        .collect();
    out.sort();
    out
}

/// A folder for test files: under AURORA_TEST_DIR when set, else the temp folder.
pub fn test_dir(what: &str) -> PathBuf {
    let base = std::env::var_os("AURORA_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("grbresearch-tests");
    let d = base.join(format!("{what}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_395), (2025, 11, 3));
        assert_eq!(civil(11_016), (2000, 2, 29));
        assert_eq!(today().len(), 10);
    }

    #[test]
    fn names_and_files() {
        assert!(check_name("hp_1").is_ok());
        assert!(check_name("../x").is_err());
        assert!(check_name("").is_err());
        let dir = test_dir("store");
        let s = Snapshot { name: "a".into(), bytes: to_hex(&[1, 0xAB, 0xFF]), ..Default::default() };
        save_snap(&dir, &s).unwrap();
        assert_eq!(load_snap(&dir, "a").unwrap().data(), vec![1, 0xAB, 0xFF]);
        let sc = SavedScan { name: "s".into(), hits: vec![Hit { a: 0x10, v: 5 }], ..Default::default() };
        save_scan(&dir, &sc).unwrap();
        assert_eq!(load_scan(&dir, "s").unwrap(), sc);
        assert_eq!(list(&dir, "scans"), vec!["s"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
