//! The commands. Each prints plain text that a person or a script can read. Every address is
//! printed in both forms: runtime and static (module+rva or a region note).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aurora_core::pattern::Pattern;

use crate::db::{self, Db, Entry, SECTIONS, Status};
use crate::diff::{self, As};
use crate::expr::{self, Env, EvalError, Step};
use crate::proc::{self, ReadError, Target};
use crate::scan::{self, Area, NextMode, Needle, Opts, Stats};
use crate::store::{self, Hit, SavedScan, Snapshot};
use crate::types::{self, VType};
use crate::view;

/// Why a command stopped.
#[derive(Debug)]
pub enum Fail {
    /// The target process ended: stop everything.
    Gone,
    Msg(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Msg(s)
    }
}

impl From<&str> for Fail {
    fn from(s: &str) -> Fail {
        Fail::Msg(s.to_string())
    }
}

pub type Res = Result<(), Fail>;

const VALUE_FLAGS: &[(&str, usize)] = &[
    ("pid", 1),
    ("exe", 1),
    ("dir", 1),
    ("prot", 1),
    ("range", 2),
    ("module", 1),
    ("reuse", 1),
    ("limit", 1),
    ("save", 1),
    ("as", 1),
    ("ms", 1),
    ("secs", 1),
    ("evidence", 1),
    ("out", 1),
    ("within", 1),
    ("type", 1),
    ("back", 1),
];
const BOOL_FLAGS: &[&str] = &["committed", "full", "all", "unaligned", "resident", "paged-out", "help"];

/// Positional words plus --flags.
#[derive(Debug, Default)]
pub struct Args {
    pub pos: Vec<String>,
    flags: BTreeMap<String, Vec<String>>,
}

impl Args {
    pub fn parse(words: &[String]) -> Result<Args, String> {
        let mut a = Args::default();
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            i += 1;
            let Some(name) = w.strip_prefix("--").filter(|n| !n.is_empty()) else {
                a.pos.push(w.clone());
                continue;
            };
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (name, None),
            };
            if let Some(&(_, n)) = VALUE_FLAGS.iter().find(|(f, _)| *f == name) {
                let mut vals: Vec<String> = inline.into_iter().collect();
                while vals.len() < n {
                    let v = words.get(i).ok_or_else(|| format!("--{name} needs {n} value(s)"))?;
                    vals.push(v.clone());
                    i += 1;
                }
                a.flags.insert(name.to_string(), vals);
            } else if BOOL_FLAGS.contains(&name) {
                a.flags.insert(name.to_string(), Vec::new());
            } else {
                return Err(format!("unknown flag --{name}"));
            }
        }
        Ok(a)
    }

    pub fn get(&self, n: &str) -> Option<&str> {
        self.flags.get(n).and_then(|v| v.first()).map(String::as_str)
    }

    pub fn get2(&self, n: &str) -> Option<(&str, &str)> {
        self.flags.get(n).filter(|v| v.len() == 2).map(|v| (v[0].as_str(), v[1].as_str()))
    }

    pub fn has(&self, n: &str) -> bool {
        self.flags.contains_key(n)
    }

    pub fn num<T: std::str::FromStr>(&self, n: &str, default: T) -> Result<T, String> {
        match self.get(n) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("--{n}: '{v}' is not a number")),
        }
    }

    pub fn need(&self, i: usize, what: &str) -> Result<&str, String> {
        self.pos.get(i).map(String::as_str).ok_or_else(|| format!("missing {what}"))
    }
}

/// Lengths and counts: hex with 0x, else decimal.
fn parse_len(s: &str) -> Result<usize, String> {
    let v = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => usize::from_str_radix(h, 16),
        None => s.parse(),
    };
    v.map_err(|_| format!("bad length '{s}' (decimal, or hex with 0x)"))
}

/// Long-lived state: the research folder, the opened target, remembered names.
pub struct Ctx {
    pub dir: PathBuf,
    pub pid: Option<u32>,
    pub exe: String,
    pub target: Option<Target>,
    pub vars: BTreeMap<String, u64>,
}

impl Ctx {
    pub fn new(dir: PathBuf, pid: Option<u32>, exe: String) -> Ctx {
        Ctx { dir, pid, exe, target: None, vars: BTreeMap::new() }
    }

    /// Open the target once: --pid, or the only process with the exe name.
    pub fn open(&mut self) -> Res {
        if self.target.is_some() {
            return Ok(());
        }
        let pid = match self.pid {
            Some(p) => p,
            None => {
                let found: Vec<(u32, String)> =
                    proc::list_processes().into_iter().filter(|(_, n)| n.eq_ignore_ascii_case(&self.exe)).collect();
                match found.as_slice() {
                    [] => return Err(format!("{} is not running (start the game, or use --exe NAME / --pid N)", self.exe).into()),
                    [(p, _)] => *p,
                    many => {
                        let list: Vec<String> = many.iter().map(|(p, n)| format!("  pid {p}  {n}")).collect();
                        return Err(format!("{} processes named {}:\n{}\nPick one with --pid N", many.len(), self.exe, list.join("\n")).into());
                    }
                }
            }
        };
        self.target = Some(Target::open(pid)?);
        Ok(())
    }
}

/// What a command works with once the target is open.
pub struct Session<'a> {
    pub t: &'a Target,
    pub vars: &'a mut BTreeMap<String, u64>,
    pub dir: &'a Path,
}

struct TEnv<'a> {
    t: &'a Target,
    vars: &'a BTreeMap<String, u64>,
}

impl Env for TEnv<'_> {
    fn module(&self, name: &str) -> Option<(String, u64)> {
        self.t.module_by_name(name).map(|m| (m.name.clone(), m.base))
    }
    fn var(&self, name: &str) -> Option<u64> {
        self.vars.get(name).copied()
    }
    fn read_ptr(&self, addr: u64) -> Result<u64, ReadError> {
        self.t.read_u64(addr)
    }
}

/// Evaluate an address expression against a target.
pub fn eval_in(t: &Target, vars: &BTreeMap<String, u64>, text: &str) -> Result<(u64, Vec<Step>), Fail> {
    let e = expr::parse(text)?;
    let mut steps = Vec::new();
    match expr::eval(&e, &TEnv { t, vars }, &mut steps) {
        Ok(v) => Ok((v, steps)),
        Err(EvalError::Read(s)) if s.result == Err(ReadError::ProcessGone) => Err(Fail::Gone),
        Err(e) => Err(Fail::Msg(e.to_string())),
    }
}

impl Session<'_> {
    pub fn addr(&self, text: &str) -> Result<u64, Fail> {
        eval_in(self.t, self.vars, text).map(|(v, _)| v)
    }

    /// "0x7FF6... (GRB_vulkan.exe+0x123)".
    pub fn both(&self, addr: u64) -> String {
        format!("0x{addr:X} ({})", self.t.describe(addr))
    }

    fn ptr_label(&self, v: u64) -> String {
        view::points_to(self.t, v).map(|p| p.label).unwrap_or_else(|| "not a pointer".into())
    }

    fn opened_db(&self) -> Result<Db, Fail> {
        open_db(self.dir)
    }
}

fn db_path(dir: &Path) -> PathBuf {
    dir.join("research.json")
}

fn open_db(dir: &Path) -> Result<Db, Fail> {
    store::ensure(dir)?;
    Ok(Db::open(&db_path(dir), &store::today())?)
}

/// Commands that need the target open.
const TARGET_CMDS: &[&str] = &[
    "info", "modules", "regions", "read", "dump", "view", "chain", "inspect", "scan", "refs", "next", "snap", "watch", "chars",
];

pub fn run(ctx: &mut Ctx, words: &[String]) -> Res {
    let Some(cmd) = words.first() else { return Ok(()) };
    let args = Args::parse(&words[1..])?;
    match cmd.as_str() {
        "db" => return db_cmd(&ctx.dir, &args),
        "export" => return export(&ctx.dir, &args),
        "diff" => return diff_cmd(&ctx.dir, &args),
        c if !TARGET_CMDS.contains(&c) => return Err(format!("unknown command '{c}' (try --help)").into()),
        _ => {}
    }
    ctx.open()?;
    let Some(t) = ctx.target.as_ref() else { return Err("target not open".into()) };
    let mut s = Session { t, vars: &mut ctx.vars, dir: &ctx.dir };
    let r = match cmd.as_str() {
        "info" => info(&s),
        "modules" => modules(&s),
        "regions" => regions(&s, &args),
        "read" => read(&mut s, &args),
        "dump" => dump(&mut s, &args),
        "view" => view_cmd(&mut s, &args),
        "chain" => chain(&mut s, &args),
        "inspect" => inspect(&mut s, &args),
        "scan" => scan_cmd(&s, &args, words),
        "refs" => refs(&s, &args),
        "next" => next(&s, &args, words),
        "snap" => snap(&s, &args),
        "watch" => watch(&s, &args),
        "chars" => crate::chars::chars(&s, &args),
        _ => Ok(()),
    };
    // A failed read may mean the game closed: say so plainly.
    match r {
        Err(Fail::Msg(m)) if !t.alive() => {
            eprintln!("{m}");
            Err(Fail::Gone)
        }
        r => r,
    }
}

fn info(s: &Session) -> Res {
    let t = s.t;
    let b = t.build();
    println!("pid {}  {}", t.pid, t.exe);
    println!("path {}", t.path);
    if let Some(m) = t.exe_module() {
        println!("image base 0x{:X}  size 0x{:X} ({:.1} MB)", m.base, m.size, m.size as f64 / 1_048_576.0);
    }
    let ts = b.pe_timestamp.map(|t| format!("0x{t:08X} ({})", date_of(t as u64))).unwrap_or_else(|| "unknown".into());
    println!("build: file version {}, PE timestamp {ts}, key {}", b.file_version, b.key());
    println!("access: read-only (PROCESS_QUERY_INFORMATION | PROCESS_VM_READ)");
    if let Ok(db) = s.opened_db() {
        for p in db.profiles.iter().filter(|p| p.exe.eq_ignore_ascii_case(&t.exe)) {
            let same = p.build == b.key();
            println!(
                "profile {} [{}] build {}{}",
                p.id,
                p.status.name(),
                p.build,
                if same { " (this build)" } else { "  <- not this build: addresses may differ; record the build in research.json" }
            );
        }
    }
    Ok(())
}

fn date_of(secs: u64) -> String {
    let (y, m, d) = store::civil(secs / 86_400);
    format!("{y:04}-{m:02}-{d:02} UTC")
}

fn modules(s: &Session) -> Res {
    for m in &s.t.modules {
        println!("0x{:016X}  0x{:08X}  {:<28} {}", m.base, m.size, m.name, m.path);
    }
    println!("{} modules", s.t.modules.len());
    Ok(())
}

fn regions(s: &Session, a: &Args) -> Res {
    let (from, to) = match a.get2("range") {
        Some((x, y)) => (s.addr(x)?, s.addr(y)?),
        None => (0, proc::USER_MAX),
    };
    let want = a.get("prot").unwrap_or("");
    let (mut n, mut committed) = (0usize, 0u64);
    for r in s.t.regions(from, to) {
        if a.has("committed") && !r.committed() {
            continue;
        }
        let (pr, pw, px) = (r.readable(), r.writable(), r.executable());
        if (want.contains('r') && !pr) || (want.contains('w') && !pw) || (want.contains('x') && !px) {
            continue;
        }
        let module = s.t.module_at(r.base).map(|m| m.name.as_str()).unwrap_or("");
        println!(
            "0x{:016X}-0x{:016X} {:>12X}  {:<7} {:<7} {:<11} alloc 0x{:X} {module}",
            r.base,
            r.end(),
            r.size,
            r.state_str(),
            r.kind_str(),
            r.prot_str(),
            r.alloc_base
        );
        n += 1;
        if r.committed() {
            committed += r.size;
        }
    }
    println!("{n} regions, {:.1} MB committed", committed as f64 / 1_048_576.0);
    Ok(())
}

fn read(s: &mut Session, a: &Args) -> Res {
    let addr = s.addr(a.need(0, "address")?)?;
    let t = VType::parse(a.need(1, "type")?)?;
    let count = match a.pos.get(2) {
        Some(c) => parse_len(c)?,
        None if matches!(t, VType::Str | VType::WStr) => 64,
        None if t == VType::Bytes => 16,
        None => 1,
    };
    let len = count.checked_mul(t.size()).ok_or("count too big")?;
    s.vars.insert("_".into(), addr);
    println!("{}  {} x{count}", s.both(addr), t.name());
    let (data, err) = s.t.read_some(addr, len);
    let strings = matches!(t, VType::Str | VType::WStr);
    // Strings may end early at a region edge; numbers must be complete.
    if let Some(e) = err
        && (e == ReadError::ProcessGone || !strings || data.is_empty())
    {
        return Err(match e {
            ReadError::ProcessGone => Fail::Gone,
            e => Fail::Msg(format!("read failed: {e}")),
        });
    }
    let vals = types::format_values(t, &data);
    if t == VType::Ptr {
        for (i, v) in vals.iter().enumerate() {
            let raw = types::raw_of(&data[i * 8..i * 8 + 8]);
            println!("  +0x{:X}  {v}  {}", i * 8, s.ptr_label(raw));
        }
    } else if vals.len() == 1 {
        println!("  {}", vals[0]);
    } else {
        for (row, chunk) in vals.chunks(8).enumerate() {
            println!("  +0x{:04X}  {}", row * 8 * t.size(), chunk.join("  "));
        }
    }
    Ok(())
}

fn dump(s: &mut Session, a: &Args) -> Res {
    let addr = s.addr(a.need(0, "address")?)?;
    let len = parse_len(a.need(1, "length")?)?;
    s.vars.insert("_".into(), addr);
    println!("{}  {len} bytes", s.both(addr));
    let (data, err) = s.t.read_some(addr, len);
    for l in view::dump_lines(addr, &data, len) {
        println!("{l}");
    }
    match err {
        Some(ReadError::ProcessGone) => Err(Fail::Gone),
        Some(e) => Err(format!("stopped after {} bytes: {e}", data.len()).into()),
        None => Ok(()),
    }
}

fn view_cmd(s: &mut Session, a: &Args) -> Res {
    let addr = s.addr(a.need(0, "address")?)?;
    let len = a.pos.get(1).map(|l| parse_len(l)).transpose()?.unwrap_or(0x80).div_ceil(8) * 8;
    s.vars.insert("_".into(), addr);
    println!("{}  {len} bytes", s.both(addr));
    let (data, err) = s.t.read_some(addr, len);
    for (i, b) in data.chunks(8).enumerate() {
        let raw = types::raw_of(b);
        let p = if b.len() == 8 { s.ptr_label(raw) } else { String::new() };
        println!("{}", view::view_row(i * 8, b, &p));
    }
    match err {
        Some(ReadError::ProcessGone) => Err(Fail::Gone),
        Some(e) => Err(format!("stopped after {} bytes: {e}", data.len()).into()),
        None => Ok(()),
    }
}

fn chain(s: &mut Session, a: &Args) -> Res {
    let text = a.need(0, "address")?;
    let e = expr::parse(text)?;
    println!("chain {e}");
    let mut steps = Vec::new();
    let r = expr::eval(&e, &TEnv { t: s.t, vars: s.vars }, &mut steps);
    for (i, st) in steps.iter().enumerate() {
        match st.result {
            Ok(v) => println!("  {}. {}  read at {} -> 0x{v:X} ({})", i + 1, st.text, s.both(st.addr), s.ptr_label(v)),
            Err(err) => println!("  {}. {}  read at {} FAILED: {err}", i + 1, st.text, s.both(st.addr)),
        }
    }
    match r {
        Ok(v) => {
            s.vars.insert("_".into(), v);
            println!("result {}", s.both(v));
            Ok(())
        }
        Err(EvalError::Read(st)) if st.result == Err(ReadError::ProcessGone) => Err(Fail::Gone),
        Err(EvalError::Read(_)) => Err(format!("chain stops at step {}", steps.len()).into()),
        Err(e) => Err(e.to_string().into()),
    }
}

/// The db entries that name an object type for this vtable address.
fn vtable_entries<'d>(t: &Target, db: &'d Db, vt: u64) -> Vec<&'d Entry> {
    let vars = BTreeMap::new();
    db.vtables
        .iter()
        .filter(|e| {
            expr::parse(&e.module_rva).ok().filter(|x| !expr::has_deref(x)).is_some_and(|x| {
                let mut st = Vec::new();
                expr::eval(&x, &TEnv { t, vars: &vars }, &mut st) == Ok(vt)
            })
        })
        .collect()
}

fn inspect(s: &mut Session, a: &Args) -> Res {
    let addr = s.addr(a.need(0, "address")?)?;
    let len = a.pos.get(1).map(|l| parse_len(l)).transpose()?.unwrap_or(0x100).div_ceil(8) * 8;
    s.vars.insert("_".into(), addr);
    println!("object at {}", s.both(addr));
    match s.t.query(addr) {
        Some(r) => println!(
            "region: {} {}, allocation 0x{:X} (object at +0x{:X}), 0x{:X} bytes with this protection from here",
            r.state_str(),
            r.note(),
            r.alloc_base,
            addr.saturating_sub(r.alloc_base),
            r.end().saturating_sub(addr)
        ),
        None => println!("region: none"),
    }
    let db = s.opened_db()?;
    let (data, err) = s.t.read_some(addr, len);
    let mut otype = a.get("type").map(str::to_string);
    if data.len() >= 8 {
        let vt = types::raw_of(&data[..8]);
        match s.t.module_at(vt) {
            Some(m) => {
                let ents = vtable_entries(s.t, &db, vt);
                let known: Vec<String> =
                    ents.iter().map(|e| format!("db {} [{}] {}", e.id, e.status.name(), e.object_type)).collect();
                println!(
                    "vtable: [obj] = 0x{vt:X} -> {}+0x{:X}  {}",
                    m.name,
                    vt - m.base,
                    if known.is_empty() { "unknown type".to_string() } else { known.join(", ") }
                );
                if otype.is_none() {
                    otype = ents.first().map(|e| e.object_type.clone());
                }
            }
            None => println!("vtable: none ([obj] does not point into a module)"),
        }
    }
    let labels: Vec<&Entry> = match &otype {
        Some(ty) => db.offsets.iter().filter(|e| e.object_type.eq_ignore_ascii_case(ty)).collect(),
        None => Vec::new(),
    };
    if let Some(ty) = &otype {
        println!("labels from research.json for type {ty}: {} entries", labels.len());
    }
    for (i, b) in data.chunks(8).enumerate() {
        let off = i * 8;
        let raw = types::raw_of(b);
        let kind = if b.len() < 8 { "unreadable".to_string() } else { view::qword_kind(raw, view::points_to(s.t, raw).as_ref()) };
        let lab: Vec<String> = labels
            .iter()
            .filter(|e| db::offset_value(&e.object_offset).is_some_and(|o| o / 8 == off as u64 / 8))
            .map(|e| format!("  <- {} [{}] {} {}", e.id, e.status.name(), e.object_offset, e.name))
            .collect();
        println!("+0x{off:04X}  {:016X}  {kind}{}", raw, lab.join(""));
    }
    if data.len() < len {
        println!("+0x{:04X}  unreadable from here: {}", data.len(), err.map(|e| e.to_string()).unwrap_or_default());
    }
    match err {
        Some(ReadError::ProcessGone) => Err(Fail::Gone),
        _ => Ok(()),
    }
}

/// --range / --module / --reuse / --full, or `default` when none is given.
enum Where {
    Area(Area),
    Points(Box<SavedScan>),
}

/// What a scan returns: hits, stats, and the spans it covered.
struct Found {
    hits: Vec<Hit>,
    st: Stats,
    spans: Vec<[u64; 2]>,
}

fn pick_area(s: &Session, a: &Args, default: Option<Area>, images: bool) -> Result<Where, Fail> {
    if let Some((x, y)) = a.get2("range") {
        return Ok(Where::Area(Area::Range(s.addr(x)?, s.addr(y)?)));
    }
    if let Some(m) = a.get("module") {
        return Ok(Where::Area(Area::Module(m.to_string())));
    }
    if let Some(name) = a.get("reuse") {
        let old = store::load_scan(s.dir, name)?;
        if old.pid != s.t.pid {
            println!("note: scan '{name}' came from pid {}, this is pid {}", old.pid, s.t.pid);
        }
        if old.hits.is_empty() {
            return Ok(Where::Area(Area::Spans(old.spans)));
        }
        return Ok(Where::Points(Box::new(old)));
    }
    if a.has("full") {
        return Ok(Where::Area(Area::Full { images }));
    }
    default.map(Where::Area).ok_or_else(|| {
        "say where: --range A B, --module NAME, --reuse SCAN, or --full (all committed memory; slower)".into()
    })
}

fn opts(a: &Args, full: bool, default_limit: usize) -> Result<Opts, Fail> {
    Ok(Opts {
        limit: a.num("limit", default_limit)?,
        resident_only: if full { !a.has("paged-out") } else { a.has("resident") },
        aligned: !a.has("unaligned"),
    })
}

/// Run a needle over an area or saved points. Returns hits, stats and the spans scanned.
fn run_scan(s: &Session, a: &Args, needle: &Needle, default: Option<Area>, images: bool, limit: usize) -> Result<Found, Fail> {
    scan::arm_cancel();
    let mut st = Stats::default();
    let w = pick_area(s, a, default, images)?;
    match w {
        Where::Points(old) => {
            let start = Instant::now();
            let addrs: Vec<u64> = old.hits.iter().map(|h| h.a).collect();
            let n = needle.len();
            let now = scan::reread(s.t, &addrs, n);
            let mut hits = Vec::new();
            for (a, b) in addrs.iter().zip(now) {
                match b {
                    Some(b) if needle.test(&b) => hits.push(Hit { a: *a, v: types::raw_of(&b[..n.min(8)]) }),
                    Some(_) => {}
                    None => *st.skipped.entry("gone").or_insert(0) += 1,
                }
            }
            st.bytes = (addrs.len() * n) as u64;
            st.elapsed = start.elapsed();
            Ok(Found { hits, st, spans: old.spans })
        }
        Where::Area(area) => {
            let full = matches!(area, Area::Full { .. });
            let o = opts(a, full, limit)?;
            let spans = scan::spans(s.t, &area, &mut st)?;
            let hits = scan::find(s.t, &spans, needle, &o, &mut st);
            Ok(Found { hits, st, spans })
        }
    }
}

fn save_or_last(s: &Session, a: &Args, mut sc: SavedScan) -> Res {
    let name = a.get("save").unwrap_or("_last").to_string();
    sc.name = name.clone();
    let p = store::save_scan(s.dir, &sc)?;
    if name == "_last" {
        println!("kept as scan '_last' (use --save NAME to keep it under a name)");
    } else {
        println!("saved scan '{name}' -> {}", p.display());
    }
    Ok(())
}

fn new_saved(s: &Session, kind: &str, vtype: &str, tol: f64, words: &[String], f: Found) -> SavedScan {
    let Found { hits, st, spans } = f;
    SavedScan {
        kind: kind.into(),
        vtype: vtype.into(),
        tol,
        query: words.join(" "),
        exe: s.t.exe.clone(),
        build: s.t.build().key(),
        pid: s.t.pid,
        date: store::today(),
        history: vec![words.join(" ")],
        spans,
        truncated: st.truncated,
        cancelled: st.cancelled,
        hits,
        ..Default::default()
    }
}

fn show_hits(s: &Session, hits: &[Hit], a: &Args, fmt: impl Fn(&Hit) -> String) {
    let shown = if a.has("all") { hits.len() } else { hits.len().min(20) };
    for h in &hits[..shown] {
        println!("  {}  {}", s.both(h.a), fmt(h));
    }
    if shown < hits.len() {
        println!("  ... {} more (use --all, or narrow with next)", hits.len() - shown);
    }
}

fn scan_cmd(s: &Session, a: &Args, words: &[String]) -> Res {
    let kind = a.need(0, "scan kind: aob | value | ptr | vtable")?;
    match kind {
        "aob" => {
            let p = Pattern::parse(a.need(1, "pattern")?)?;
            let f = run_scan(s, a, &Needle::Aob(p), Some(Area::Module("exe".into())), true, 10_000)?;
            println!("{} hits ({})", f.hits.len(), f.st.summary());
            show_hits(s, &f.hits, a, |_| String::new());
            save_or_last(s, a, new_saved(s, "aob", "", 0.0, words, f))
        }
        "value" => {
            let t = VType::parse(a.need(1, "type")?)?;
            let text = a.need(2, "value")?;
            let (needle, tol) = match t {
                VType::Str => (Needle::Bytes(text.as_bytes().to_vec()), 0.0),
                VType::WStr => (Needle::Bytes(text.encode_utf16().flat_map(u16::to_le_bytes).collect()), 0.0),
                VType::Bytes => return Err("use scan aob for byte patterns".into()),
                _ => {
                    let target = types::parse_value(t, text)?;
                    (Needle::Value { t, target }, target.tol)
                }
            };
            let f = run_scan(s, a, &needle, None, false, 1_000_000)?;
            println!("{} hits ({})", f.hits.len(), f.st.summary());
            show_hits(s, &f.hits, a, |h| if t.numeric() { types::format_one(t, h.v) } else { String::new() });
            save_or_last(s, a, new_saved(s, "value", t.name(), tol, words, f))
        }
        "ptr" | "vtable" => {
            let target = s.addr(a.need(1, "address")?)?;
            let within = if kind == "ptr" { a.get("within").map(parse_len).transpose()?.unwrap_or(0) as u64 } else { 0 };
            let needle = Needle::Qword { lo: target.saturating_sub(within), hi: target };
            let f = run_scan(s, a, &needle, None, true, 10_000)?;
            let what = if kind == "ptr" { "references" } else { "objects whose first qword is it" };
            println!("{} {what} to {} ({})", f.hits.len(), s.both(target), f.st.summary());
            show_hits(s, &f.hits, a, |h| if h.v == target { String::new() } else { format!("-> target-0x{:X}", target - h.v) });
            save_or_last(s, a, new_saved(s, kind, "ptr", 0.0, words, f))
        }
        _ => Err(format!("unknown scan kind '{kind}' (aob | value | ptr | vtable)").into()),
    }
}

/// If `hit` looks like a field of an object, (object start, vtable address). Walks back up to
/// `back` bytes for a qword pointing into a module whose own first qword points at code: that is
/// how a vtable looks.
pub fn containing_object(t: &Target, hit: u64, back: u64) -> Option<(u64, u64)> {
    let r = t.query(hit)?;
    let start = hit.saturating_sub(back).max(r.base) & !7;
    let data = t.read(start, (hit - start) as usize + 8).ok()?;
    for off in (0..=(hit - start) as usize).rev().step_by(8) {
        let q = types::raw_of(&data[off..off + 8]);
        if t.module_at(q).is_none() || t.query(q).is_some_and(|r| r.executable()) {
            continue;
        }
        let f = t.read_u64(q).ok()?;
        if t.module_at(f).is_some() && t.query(f).is_some_and(|r| r.executable()) {
            return Some((start + off as u64, q));
        }
    }
    None
}

fn refs(s: &Session, a: &Args) -> Res {
    let target = s.addr(a.need(0, "address")?)?;
    let back = a.get("back").map(parse_len).transpose()?.unwrap_or(0x200) as u64;
    let within = a.get("within").map(parse_len).transpose()?.unwrap_or(0) as u64;
    let needle = Needle::Qword { lo: target.saturating_sub(within), hi: target };
    let f = run_scan(s, a, &needle, None, true, 10_000)?;
    println!("{} references to {} ({})", f.hits.len(), s.both(target), f.st.summary());
    let (img, heap): (Vec<&Hit>, Vec<&Hit>) = f.hits.iter().partition(|h| s.t.module_at(h.a).is_some());
    let max = if a.has("all") { usize::MAX } else { 20 };
    println!("inside images ({}): static roots, usable as [module+rva]", img.len());
    for h in img.iter().take(max) {
        let off = if h.v == target { String::new() } else { format!(" (points to target-0x{:X})", target - h.v) };
        println!("  {}{off}", s.both(h.a));
    }
    println!("heap and other ({}):", heap.len());
    let db = s.opened_db().unwrap_or_default();
    for h in heap.iter().take(max) {
        let obj = match containing_object(s.t, h.a, back) {
            Some((o, vt)) => {
                let names: Vec<String> = vtable_entries(s.t, &db, vt).iter().map(|e| format!(" db {} [{}]", e.id, e.status.name())).collect();
                format!("  field +0x{:X} of object 0x{o:X}, vtable {}{} (guess)", h.a - o, s.t.describe(vt), names.join(""))
            }
            None => "  no object start found".into(),
        };
        println!("  {}{obj}", s.both(h.a));
    }
    if max < f.hits.len() {
        println!("  (lists cut at 20 each: use --all)");
    }
    let mut words = vec!["refs".to_string()];
    words.extend(a.pos.iter().cloned());
    save_or_last(s, a, new_saved(s, "ptr", "ptr", 0.0, &words, f))
}

fn next(s: &Session, a: &Args, words: &[String]) -> Res {
    let name = a.need(0, "scan name")?;
    let mut sc = store::load_scan(s.dir, name)?;
    let t = VType::parse(&sc.vtype).map_err(|_| format!("scan '{name}' is not a value scan"))?;
    if sc.kind != "value" || !t.numeric() {
        return Err(format!("next works on numeric value scans; '{name}' is a {} scan", sc.kind).into());
    }
    let mode = NextMode::parse(t, &a.pos[1..])?;
    let before = sc.hits.len();
    let addrs: Vec<u64> = sc.hits.iter().map(|h| h.a).collect();
    let now: Vec<Option<u64>> = scan::reread(s.t, &addrs, t.size()).into_iter().map(|b| b.map(|b| types::raw_of(&b))).collect();
    let gone = now.iter().filter(|v| v.is_none()).count();
    let old: BTreeMap<u64, u64> = sc.hits.iter().map(|h| (h.a, h.v)).collect();
    sc.hits = scan::narrow(&sc.hits, &now, t, mode);
    sc.history.push(words.join(" "));
    println!("{before} -> {} hits ({} unreadable now)", sc.hits.len(), gone);
    show_hits(s, &sc.hits, a, |h| {
        format!("{} -> {}", types::format_one(t, old.get(&h.a).copied().unwrap_or(0)), types::format_one(t, h.v))
    });
    store::save_scan(s.dir, &sc)?;
    println!("scan '{name}' updated (step {})", sc.history.len());
    Ok(())
}

fn snap(s: &Session, a: &Args) -> Res {
    let name = a.need(0, "snapshot name")?;
    store::check_name(name)?;
    let text = a.need(1, "address")?;
    let addr = s.addr(text)?;
    let len = parse_len(a.need(2, "length")?)?;
    let (data, err) = s.t.read_some(addr, len);
    if err == Some(ReadError::ProcessGone) {
        return Err(Fail::Gone);
    }
    let sn = Snapshot {
        name: name.into(),
        expr: text.into(),
        addr,
        len,
        exe: s.t.exe.clone(),
        build: s.t.build().key(),
        pid: s.t.pid,
        date: store::today(),
        time_ms: store::now_ms(),
        bytes: store::to_hex(&data),
        error: err.map(|e| e.to_string()).unwrap_or_default(),
    };
    let p = store::save_snap(s.dir, &sn)?;
    println!("snap '{name}': {} of {len} bytes at {} -> {}", data.len(), s.both(addr), p.display());
    if let Some(e) = err {
        println!("  read stopped early: {e}");
    }
    Ok(())
}

fn diff_cmd(dir: &Path, a: &Args) -> Res {
    let x = store::load_snap(dir, a.need(0, "first snapshot")?)?;
    let y = store::load_snap(dir, a.need(1, "second snapshot")?)?;
    let as_ = As::parse(a.get("as").unwrap_or("auto"))?;
    if x.addr != y.addr {
        println!("note: different addresses 0x{:X} vs 0x{:X}; comparing by offset", x.addr, y.addr);
    }
    let (da, db_) = (x.data(), y.data());
    let ch = diff::diff(&da, &db_, as_.width());
    let dt = y.time_ms as i64 - x.time_ms as i64;
    println!(
        "{} -> {}: {} changed fields of {} bytes (as {}, {:+.1} s apart, {})",
        x.name,
        y.name,
        ch.len(),
        da.len().min(db_.len()),
        match as_ {
            As::Auto => "auto",
            As::Type(t) => t.name(),
        },
        dt as f64 / 1000.0,
        x.expr
    );
    let max = if a.has("all") { ch.len() } else { ch.len().min(200) };
    for c in &ch[..max] {
        println!("{}", diff::line(c, as_));
    }
    if max < ch.len() {
        println!("... {} more (use --all)", ch.len() - max);
    }
    Ok(())
}

fn watch(s: &Session, a: &Args) -> Res {
    let addr = s.addr(a.need(0, "address")?)?;
    let len = parse_len(a.need(1, "length")?)?;
    let ms = a.num("ms", 100u64)?.max(1);
    let secs = a.num("secs", 10f64)?;
    let as_ = As::parse(a.get("as").unwrap_or("auto"))?;
    println!(
        "watching {} for {len} bytes every {ms} ms for {secs} s (address fixed at start; Ctrl+C stops). \
         Polling misses changes faster than {ms} ms and cannot tell who wrote.",
        s.both(addr)
    );
    scan::arm_cancel();
    let start = Instant::now();
    let mut prev = s.t.read_some(addr, len);
    if prev.1 == Some(ReadError::ProcessGone) {
        return Err(Fail::Gone);
    }
    let (mut ticks, mut changes) = (0u64, 0usize);
    while start.elapsed().as_secs_f64() < secs && !scan::cancelled() {
        std::thread::sleep(Duration::from_millis(ms));
        let now = s.t.read_some(addr, len);
        if now.1 == Some(ReadError::ProcessGone) {
            return Err(Fail::Gone);
        }
        ticks += 1;
        if now.0.len() != prev.0.len() {
            println!("t+{:.3}s readable bytes {} -> {} ({})", start.elapsed().as_secs_f64(), prev.0.len(), now.0.len(), now.1.map(|e| e.to_string()).unwrap_or_default());
        }
        for c in diff::diff(&prev.0, &now.0, as_.width()) {
            changes += 1;
            println!("t+{:.3}s {}", start.elapsed().as_secs_f64(), diff::line(&c, as_));
        }
        prev = now;
    }
    println!("{ticks} polls, {changes} changes{}", if scan::cancelled() { " (stopped by Ctrl+C)" } else { "" });
    Ok(())
}

fn db_cmd(dir: &Path, a: &Args) -> Res {
    let sub = a.pos.first().map(String::as_str).unwrap_or("list");
    let mut db = open_db(dir)?;
    match sub {
        "list" => {
            match a.pos.get(1) {
                Some(sec) => {
                    let list = db.section(sec).ok_or_else(|| format!("no section '{sec}' ({})", SECTIONS.join(" ")))?;
                    for e in list {
                        println!("{}", db::line(sec, e));
                        if a.has("all") {
                            db::details(e).iter().for_each(|l| println!("{l}"));
                        }
                    }
                    println!("{} entries", list.len());
                }
                None => {
                    for sec in SECTIONS {
                        println!("{sec:<14} {}", db.section(sec).map(Vec::len).unwrap_or(0));
                    }
                    let c: Vec<String> = db.count_by_status().iter().map(|(k, v)| format!("{} {v}", k.name())).collect();
                    println!("by status: {}", c.join(", "));
                    println!("file: {}", db_path(dir).display());
                }
            }
            Ok(())
        }
        "find" => {
            let text = a.pos[1..].join(" ");
            if text.is_empty() {
                return Err("db find <text>".into());
            }
            let hits = db.find(&text);
            for (sec, e) in &hits {
                println!("{}", db::line(sec, e));
                db::details(e).iter().for_each(|l| println!("{l}"));
            }
            let scans: Vec<String> = store::list(dir, "scans").into_iter().filter(|n| n.to_ascii_lowercase().contains(&text.to_ascii_lowercase())).collect();
            let snaps: Vec<String> = store::list(dir, "snaps").into_iter().filter(|n| n.to_ascii_lowercase().contains(&text.to_ascii_lowercase())).collect();
            if !scans.is_empty() || !snaps.is_empty() {
                println!("saved scans: {}  snapshots: {}", scans.join(" "), snaps.join(" "));
            }
            println!("{} entries match '{text}'", hits.len());
            Ok(())
        }
        "add" => {
            let sec = a.need(1, "section")?.to_string();
            let mut e = Entry { date: store::today(), exe: "GRB_vulkan.exe".into(), ..Default::default() };
            for kv in &a.pos[2..] {
                let (k, v) = kv.split_once('=').ok_or_else(|| format!("'{kv}' is not key=value"))?;
                db::set_field(&mut e, k, v)?;
            }
            if e.status == Status::Verified && e.evidence.is_empty() {
                return Err("verified needs evidence=...".into());
            }
            let list = db.section_mut(&sec).ok_or_else(|| format!("no section '{sec}' ({})", SECTIONS.join(" ")))?;
            if e.id.is_empty() {
                e.id = format!("{sec}.{}", list.len() + 1);
            }
            let id = e.id.clone();
            let line = db::line(&sec, &e);
            list.push(e);
            if db.all().iter().filter(|(_, x)| x.id.eq_ignore_ascii_case(&id)).count() > 1 {
                return Err(format!("id '{id}' already exists; pick another id=").into());
            }
            db.save(&db_path(dir))?;
            println!("added {line}");
            Ok(())
        }
        "set-status" => {
            let id = a.need(1, "id")?;
            let st = Status::parse(a.need(2, "status")?)?;
            let ev = a.get("evidence").ok_or("--evidence \"...\" is required")?;
            let today = store::today();
            let e = db.find_id_mut(id).ok_or_else(|| format!("no entry '{id}' (db find ...)"))?;
            let old = e.status;
            e.status = st;
            let note = format!("{today} {} -> {}: {ev}", old.name(), st.name());
            e.evidence = if e.evidence.is_empty() { note } else { format!("{} | {note}", e.evidence) };
            e.date = today;
            println!("{id}: {} -> {}", old.name(), st.name());
            db.save(&db_path(dir))?;
            Ok(())
        }
        _ => Err("db list [section] | db find <text> | db add <section> key=value ... | db set-status <id> <status> --evidence \"...\"".into()),
    }
}

fn export(dir: &Path, a: &Args) -> Res {
    let db = open_db(dir)?;
    let scans: Vec<serde_json::Value> = store::list(dir, "scans")
        .iter()
        .filter_map(|n| store::load_scan(dir, n).ok())
        .map(|s| {
            serde_json::json!({
                "name": s.name, "kind": s.kind, "vtype": s.vtype, "query": s.query, "exe": s.exe,
                "build": s.build, "date": s.date, "hits": s.hits.len(), "history": s.history,
                "first_hits": s.hits.iter().take(20).map(|h| format!("0x{:X}", h.a)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let snaps: Vec<serde_json::Value> = store::list(dir, "snaps")
        .iter()
        .filter_map(|n| store::load_snap(dir, n).ok())
        .map(|s| {
            serde_json::json!({
                "name": s.name, "expr": s.expr, "addr": format!("0x{:X}", s.addr), "len": s.len,
                "exe": s.exe, "build": s.build, "date": s.date, "error": s.error,
            })
        })
        .collect();
    let out = serde_json::json!({ "exported": store::today(), "research": db, "scans": scans, "snapshots": snaps });
    let path = a.get("out").map(PathBuf::from).unwrap_or_else(|| dir.join(format!("export-{}.json", store::today())));
    let text = serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("exported {} entries, {} scans, {} snapshots -> {}", db.all().len(), scans.len(), snaps.len(), path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_args() {
        let a = Args::parse(&w("value f32 100 --range 0x1000 0x2000 --save hp --full --limit=5")).unwrap();
        assert_eq!(a.pos, w("value f32 100"));
        assert_eq!(a.get2("range"), Some(("0x1000", "0x2000")));
        assert_eq!(a.get("save"), Some("hp"));
        assert!(a.has("full"));
        assert_eq!(a.num("limit", 0usize).unwrap(), 5);
        assert!(Args::parse(&w("--range 1")).is_err());
        assert!(Args::parse(&w("--bogus")).is_err());
        let neg = Args::parse(&w("eq -5")).unwrap();
        assert_eq!(neg.pos, w("eq -5"));
        assert_eq!(parse_len("0x10").unwrap(), 16);
        assert_eq!(parse_len("10").unwrap(), 10);
        assert!(parse_len("zz").is_err());
        assert!(date_of(0).starts_with("1970-01-01"));
    }
}
