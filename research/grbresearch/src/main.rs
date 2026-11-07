//! grbresearch: a read-only memory inspector for Ghost Recon Breakpoint research. Attaches to the
//! running game from outside with PROCESS_QUERY_INFORMATION | PROCESS_VM_READ only, so research
//! never needs the mod rebuilt or reinjected.

mod chars;
mod cmd;
mod db;
mod diff;
mod expr;
mod proc;
mod scan;
mod selftest;
mod store;
mod types;
mod view;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use cmd::{Ctx, Fail};

const HELP: &str = r#"grbresearch - read-only memory inspector for Ghost Recon Breakpoint

READ-ONLY: opens the game with PROCESS_QUERY_INFORMATION | PROCESS_VM_READ only.
It never writes memory, never changes page protection, never creates threads
(no WriteProcessMemory, VirtualProtectEx or CreateRemoteThread anywhere).
The game and Aurora keep running; nothing needs rebuilding for a new offset.

usage: grbresearch [--exe NAME | --pid N] [--dir FOLDER] <command> ...
  --exe NAME   process to attach to (default GRB_vulkan.exe); if several run, use --pid
  --dir PATH   research folder (default %APPDATA%\Aurora\Dumper\research)

ADDRESSES (numbers are hex):  7FF6A000  0x1234  exe+0x1234  ntdll.dll+0x10
  [exe+0x63A0D20]+0x490   [[exe+0x63EDAA0]+0x1F0]+0x18   $name (shell)   a+b-c
  [x] reads the pointer at x. Output always shows runtime and static form.

COMMANDS
  info                              pid, path, image base/size, build (file version + PE time)
  modules                           loaded modules
  regions [--prot rwx] [--range A B] [--committed]
  read <addr> <type> [count]        types: u8 i8 u16 i16 u32 i32 u64 i64 f32 f64 ptr bytes str wstr
  dump <addr> <len>                 hex + ascii (len decimal, or hex with 0x)
  view <addr> [len]                 one row per 8 bytes: hex, pointer target, u64, i32x2, f32x2, f64
  chain <addr>                      every pointer step, and where/why it fails
  inspect <addr> [len] [--type T]   object view: region, vtable, each qword labelled; db labels
  scan aob "<pattern>" [--module exe]
  scan value <type> <value>         ints decimal or 0x; floats match to the typed digits
  scan ptr <addr> [--within N]      references to an address (or into the N bytes before it)
  scan vtable <addr>                objects whose first qword is that address
      where: --range A B | --module M | --reuse SCAN (rescan its hits) | --full (all memory)
      --limit N (0 = none)  --all (print every hit)  --save NAME (else kept as _last)
      --unaligned  --resident (only pages in RAM; default for --full)  --paged-out
      Ctrl+C stops a scan cleanly and keeps the partial result.
  next <scan> changed|unchanged|increased|decreased|eq <v>|ne <v> [--all]
                                    narrow a saved value scan (prints count + 20 hits)
  refs <addr> [where] [--within N] [--back 0x200]
                                    references grouped: inside images (static roots) vs heap,
                                    with the vtable of the object holding a heap hit (a guess)
  snap <name> <addr> <len>          save bytes to the research folder
  diff <a> <b> [--as f32|i32|u64|f64|auto] [--all]
  watch <addr> <len> [--ms 100] [--secs 10] [--as auto]
                                    print fields that change, with time. Polling can miss
                                    changes faster than --ms and cannot tell WHO wrote:
                                    that needs grbresearch-dll.
  chars [--all] [--paged-out]       NPC + player character components (ESP checks)
  db list [section] [--all] | db find <text> | db add <section> key=value ..
  db set-status <id> verified|strong|unverified|rejected --evidence "..."
  export [--out file.json]          research db + saved scans + snapshots index
  shell                             keep the process open; `name = <addr>` remembers $name
  selftest [--dir D]                read-only checks against this tool's own process

Exit codes: 0 ok, 1 error, 3 the target process ended.
"#;

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // Pull out the global flags; everything else goes to the command.
    let mut pid = None;
    let mut exe = "GRB_vulkan.exe".to_string();
    let mut dir: Option<PathBuf> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let w = raw[i].as_str();
        let val = raw.get(i + 1).cloned();
        match (w, val) {
            ("--pid", Some(v)) => {
                match v.parse() {
                    Ok(p) => pid = Some(p),
                    Err(_) => {
                        eprintln!("--pid: '{v}' is not a number");
                        std::process::exit(1);
                    }
                }
                i += 2;
            }
            ("--exe", Some(v)) => {
                exe = v;
                i += 2;
            }
            ("--dir", Some(v)) => {
                dir = Some(PathBuf::from(v));
                i += 2;
            }
            _ => {
                rest.push(raw[i].clone());
                i += 1;
            }
        }
    }
    if rest.is_empty() || matches!(rest[0].as_str(), "help" | "--help" | "-h" | "/?") {
        print!("{HELP}");
        return;
    }
    if rest[0] == "selftest" {
        std::process::exit(selftest::run(dir));
    }
    let dir = match dir.map(Ok).unwrap_or_else(store::default_dir) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut ctx = Ctx::new(dir, pid, exe);
    if rest[0] == "shell" {
        std::process::exit(shell(&mut ctx));
    }
    std::process::exit(report(cmd::run(&mut ctx, &rest)));
}

fn report(r: Result<(), Fail>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(Fail::Gone) => {
            eprintln!("the target process has ended: stopping");
            3
        }
        Err(Fail::Msg(m)) => {
            eprintln!("error: {m}");
            1
        }
    }
}

/// Split a line into words; "double quotes" keep spaces.
fn split_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// A small REPL: the process stays open and `name = <addr>` results are remembered as $name.
fn shell(ctx: &mut Ctx) -> i32 {
    if let Err(f) = ctx.open() {
        return report(Err(f));
    }
    if let Some(t) = &ctx.target {
        println!("attached read-only to {} (pid {}). `help`, `vars`, `name = <addr>`, `quit`.", t.exe, t.pid);
    }
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        print!("grb> ");
        let _ = std::io::stdout().flush();
        line.clear();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 {
            return 0;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        if let Some((name, e)) = text.split_once('=').filter(|(n, _)| {
            let n = n.trim();
            !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }) {
            let name = name.trim().to_string();
            let Some(t) = &ctx.target else { return 1 };
            match cmd::eval_in(t, &ctx.vars, e.trim()) {
                Ok((v, _)) => {
                    println!("${name} = 0x{v:X} ({})", t.describe(v));
                    ctx.vars.insert(name, v);
                }
                Err(Fail::Gone) => return report(Err(Fail::Gone)),
                Err(Fail::Msg(m)) => println!("error: {m}"),
            }
            continue;
        }
        let words = split_line(text);
        match words[0].as_str() {
            "quit" | "exit" | "q" => return 0,
            "help" | "?" => {
                print!("{HELP}");
                continue;
            }
            "vars" => {
                for (k, v) in &ctx.vars {
                    println!("${k} = 0x{v:X}");
                }
                continue;
            }
            "shell" | "selftest" => {
                println!("not inside the shell");
                continue;
            }
            _ => {}
        }
        match cmd::run(ctx, &words) {
            Ok(()) => {}
            Err(Fail::Gone) => return report(Err(Fail::Gone)),
            Err(Fail::Msg(m)) => println!("error: {m}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_words() {
        assert_eq!(split_line(r#"scan aob "48 8B ??" --save x"#), vec!["scan", "aob", "48 8B ??", "--save", "x"]);
        assert_eq!(split_line("  read  exe+10 u32 "), vec!["read", "exe+10", "u32"]);
        assert_eq!(split_line(r#"db find """#), vec!["db", "find", ""]);
    }
}
