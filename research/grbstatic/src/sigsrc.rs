//! Where `sigs` gets its patterns: every single-line `const NAME: &str = "<hex tokens>";` in the
//! mod's source (dll\src\*.rs), named "<file>.<NAME>" (e.g. "nvg.LOAD"), plus any `--pattern
//! NAME=HEX`. A string that is not a byte pattern (menu text) is skipped.

use std::path::{Path, PathBuf};

use aurora_core::pattern::Pattern;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Src {
    pub name: String,
    pub text: String,
}

pub fn parse_rs(stem: &str, text: &str) -> Vec<Src> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some((name, lit)) = const_str(line)
            && Pattern::parse(lit).is_ok()
        {
            out.push(Src { name: format!("{stem}.{name}"), text: lit.to_string() });
        }
    }
    out
}

/// `const NAME: &str = "text";` (optionally `pub` / `pub(crate)`) -> (NAME, text).
fn const_str(line: &str) -> Option<(&str, &str)> {
    let t = line.trim_start();
    let t = t.strip_prefix("pub(crate) ").or_else(|| t.strip_prefix("pub ")).unwrap_or(t);
    let (name, rest) = t.strip_prefix("const ")?.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("&str").or_else(|| rest.strip_prefix("&'static str"))?;
    let rest = rest.trim_start().strip_prefix('=')?.trim_start().strip_prefix('"')?;
    let (lit, tail) = rest.split_once('"')?;
    tail.trim_start().starts_with(';').then_some((name, lit))
}

/// dll\src, looked for above this exe and above the current folder.
pub fn find_src_dir() -> Option<PathBuf> {
    let mut starts = Vec::new();
    if let Ok(e) = std::env::current_exe() {
        starts.push(e);
    }
    if let Ok(d) = std::env::current_dir() {
        starts.push(d);
    }
    starts.iter().flat_map(|s| s.ancestors()).map(|a| a.join("aurora-dll").join("src")).find(|c| c.is_dir())
}

/// Every pattern in the.rs files of a folder, files in name order.
pub fn load_dir(dir: &Path) -> Result<Vec<Src>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "rs")).collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let stem = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        out.extend(parse_rs(&stem, &text));
    }
    Ok(out)
}

/// `NAME=HEX` from the command line.
pub fn parse_arg(s: &str) -> Result<Src, String> {
    let (name, text) = s.split_once('=').ok_or_else(|| format!("--pattern '{s}': want NAME=HEX"))?;
    Pattern::parse(text).map_err(|e| format!("--pattern {name}: {e}"))?;
    Ok(Src { name: name.trim().to_string(), text: text.trim().to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_patterns_and_skips_text() {
        let rs = r#"
/// doc
const FALL: &str = "48 89 5C 24 18 55 ?? ??";
pub const LOAD: &'static str = "41 88 85 0C 12 00 00";
    const ON_INJECT: &str = "Load On Inject";
const lower: &str = "48 8B";
const EMPTY: &str = "";
const NOSEMI: &str = "48 8B" + x
let x = "48 8B";
"#;
        let v = parse_rs("nvg", rs);
        assert_eq!(v, vec![
            Src { name: "nvg.FALL".into(), text: "48 89 5C 24 18 55 ?? ??".into() },
            Src { name: "nvg.LOAD".into(), text: "41 88 85 0C 12 00 00".into() },
        ]);
        assert_eq!(parse_arg("X=48 8B ??").unwrap().name, "X");
        assert!(parse_arg("X=zz").is_err());
        assert!(parse_arg("nothing").is_err());
    }
}
