//! Grbstatic: an offline, read-only analyzer for Ghost Recon Breakpoint's exes on disk.
//!
//! It reads the exe files only (a read-only memory map): no process is opened, nothing in the game
//! folder is written, no file it reads is changed. Writable + executable sections (the game's
//! protection layer) are listed as protected and never decoded or analysed. Every address it prints
//! says its kind: `rva 0x...` (module-relative), `va 0x...` (imagebase + rva, the preferred on-disk
//! address, not a runtime one) or `file 0x...`.

/// Println! That ends the program quietly when stdout is closed (`| head`), instead of panicking.
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

mod cmds;
mod demangle;
mod disasm;
mod image;
mod makesig;
mod out;
mod rtti;
mod sigsrc;
mod unwind;
mod xrefs;

use std::path::PathBuf;

const HELP: &str = r#"grbstatic - offline, read-only analyzer for Ghost Recon Breakpoint's exes

READ-ONLY: reads the exe files on disk (read-only memory map). It never opens a process,
never writes into the game folder and never changes a file it reads. Writable + executable
sections (the protection layer) are listed as "protected, skipped" and never decoded.

usage: grbstatic [--out DIR] [--json] <command> ...
  --out DIR   base folder for result files (default %APPDATA%\Aurora\Dumper\research\static,
              only when Aurora exists). Per-exe files go to <DIR>\<build-id>\. Never the game folder.
  --json      also write the command's result as JSON there (info and rtti always write).

ADDRESSES: numbers are hex rvas: 11bb332, 0x11bb332, exe+0x11bb332 (as the mod logs them).
  Output: "rva 0x..." module-relative; "va 0x..." = 0x140000000 + rva (on-disk preferred
  address, NOT the runtime one: ASLR moves the game); "file 0x..." file offset.
  sub_<rva> names a function start (exception table); "rva 0x..." in code is any other target.

COMMANDS
  info <exe>                         build id, size, SHA-256, PE timestamp, flavour (Vulkan/DX11),
                                     Steam build, sections (role, entropy), data directories,
                                     exception entries, RTTI COLs. Writes build.json.
  sigs <exe>... [--src DIR] [--pattern NAME=HEX]...
                                     the mod's code patterns (dll\src\*.rs) in each exe:
                                     OK (1 hit) / MISSING / AMBIGUOUS(n). Exit 1 if any is not OK.
  func <exe> <rva>                   the function holding rva: start, chunks, size, unwind info
  dis <exe> <rva> [--count N] [--func]
                                     disassembly (Intel); --func: the whole function
  xrefs <exe> <rva> [--limit N]      code that calls/jumps to/reads/writes/leas rva, and
                                     8-byte data pointers to it (vtables, tables)
  rtti <exe> [--filter TEXT]         RTTI classes: name, COL, type descriptor, vtable, methods,
                                     bases. Writes rtti.json and rtti.txt (all classes).
  vtable <exe> <class name | vtable rva>
                                     the slots: index, function, first instruction
  makesig <exe> <rva> [--other EXE2] shortest unique pattern at rva; --other finds it in EXE2
  strings <exe> <text> [--xrefs]     ASCII and UTF-16 occurrences; --xrefs: who uses each one
  compare <exeA> <exeB> <rva>...     map rvas of A to B: unique signature (high), same RTTI
                                     class + vtable slot (medium), or not mapped

Exit codes: 0 ok, 1 error (or a sig not OK), 2 bad arguments or no output folder.
"#;

/// Parsed command line: positional words and flags.
#[derive(Debug, Default)]
pub struct Args {
    pub words: Vec<String>,
    pub out: Option<PathBuf>,
    pub json: bool,
    pub count: Option<usize>,
    pub func: bool,
    pub limit: Option<usize>,
    pub filter: Option<String>,
    pub other: Option<PathBuf>,
    pub src: Option<PathBuf>,
    pub patterns: Vec<String>,
    pub xrefs: bool,
}

fn parse_args(raw: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0;
    while i < raw.len() {
        let w = raw[i].as_str();
        let takes_value = matches!(w, "--out" | "--count" | "--limit" | "--filter" | "--other" | "--src" | "--pattern");
        let v = if takes_value {
            i += 1;
            Some(raw.get(i).cloned().ok_or_else(|| format!("{w} needs a value"))?)
        } else {
            None
        };
        let num = |v: &str| v.parse::<usize>().map_err(|_| format!("{w}: '{v}' is not a number"));
        match (w, v) {
            ("--out", Some(v)) => a.out = Some(PathBuf::from(v)),
            ("--count", Some(v)) => a.count = Some(num(&v)?),
            ("--limit", Some(v)) => a.limit = Some(num(&v)?),
            ("--filter", Some(v)) => a.filter = Some(v),
            ("--other", Some(v)) => a.other = Some(PathBuf::from(v)),
            ("--src", Some(v)) => a.src = Some(PathBuf::from(v)),
            ("--pattern", Some(v)) => a.patterns.push(v),
            ("--json", _) => a.json = true,
            ("--func", _) => a.func = true,
            ("--xrefs", _) => a.xrefs = true,
            _ if w.starts_with("--") => return Err(format!("unknown flag {w}")),
            _ => a.words.push(w.to_string()),
        }
        i += 1;
    }
    Ok(a)
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || matches!(raw[0].as_str(), "help" | "--help" | "-h" | "/?") {
        print!("{HELP}");
        return;
    }
    let args = match parse_args(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    std::process::exit(match cmds::run(&args) {
        Ok(code) => code,
        Err(cmds::Fail::Usage(m)) => {
            eprintln!("error: {m}");
            2
        }
        Err(cmds::Fail::Msg(m)) => {
            eprintln!("error: {m}");
            1
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_split_flags_and_words() {
        let raw: Vec<String> = ["--json", "dis", "a.exe", "11bb332", "--count", "5", "--out", "C:\\out"].iter().map(|s| s.to_string()).collect();
        let a = parse_args(&raw).unwrap();
        assert_eq!(a.words, vec!["dis", "a.exe", "11bb332"]);
        assert_eq!(a.count, Some(5));
        assert!(a.json);
        assert_eq!(a.out, Some(PathBuf::from("C:\\out")));
        assert!(parse_args(&["--count".to_string()]).is_err());
        assert!(parse_args(&["--nope".to_string()]).is_err());
    }
}
