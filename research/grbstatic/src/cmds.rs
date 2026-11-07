//! The commands. Each opens the exe(s) read-only, prints aligned text and, with --json (info and
//! rtti always), writes its result as JSON into the output folder.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::Args;
use crate::disasm::{Disasm, check_code, quote, symbol_name};
use crate::image::{DIR_NAMES, DIR_SECURITY, Image, entropy};
use crate::makesig;
use crate::out;
use crate::rtti::{self, Rtti};
use crate::sigsrc;
use crate::unwind::Functions;
use crate::xrefs;

pub enum Fail {
    /// Bad arguments or no output folder (exit 2).
    Usage(String),
    /// Anything else (exit 1).
    Msg(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Fail {
        Fail::Msg(s)
    }
}

type R<T> = Result<T, Fail>;

fn hx(v: impl Into<u64>) -> String {
    format!("0x{:x}", v.into())
}

pub fn run(a: &Args) -> R<i32> {
    let cmd = a.words.first().ok_or_else(|| Fail::Usage("no command (grbstatic help)".into()))?;
    match cmd.as_str() {
        "info" => info(a),
        "sigs" => sigs(a),
        "func" => func(a),
        "dis" => dis(a),
        "xrefs" => xrefs_cmd(a),
        "rtti" => rtti_cmd(a),
        "vtable" => vtable(a),
        "makesig" => makesig_cmd(a),
        "strings" => strings(a),
        "compare" => compare(a),
        _ => Err(Fail::Usage(format!("unknown command '{cmd}' (grbstatic help)"))),
    }
}

fn word<'a>(a: &'a Args, i: usize, what: &str) -> R<&'a str> {
    a.words.get(i).map(String::as_str).ok_or_else(|| Fail::Usage(format!("{} needs {what}", a.words[0])))
}

/// Hex rva: "11bb332", "0x11bb332" or "exe+0x11bb332". Absolute addresses are refused so the
/// address kinds never mix.
pub fn parse_rva(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let t = t.strip_prefix("exe+").or_else(|| t.strip_prefix("EXE+")).unwrap_or(t);
    let t = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t);
    let v = u64::from_str_radix(t, 16).map_err(|_| format!("'{s}' is not a hex rva"))?;
    if v >= 0x1_0000_0000 {
        return Err(format!("'{s}' looks like an absolute address: pass the rva (va - 0x140000000, or a runtime address minus the game's base)"));
    }
    Ok(v as u32)
}

fn rva_arg(a: &Args, i: usize) -> R<u32> {
    parse_rva(word(a, i, "an rva")?).map_err(Fail::Usage)
}

fn open(p: &str) -> R<Image> {
    Ok(Image::open(Path::new(p))?)
}

/// A path without the `\\?\` prefix canonicalize adds.
fn show(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// The base output folder for these exes (never inside their folders).
fn base_dir(a: &Args, imgs: &[&Image]) -> R<PathBuf> {
    let dirs: Vec<PathBuf> = imgs.iter().filter_map(|i| std::fs::canonicalize(&i.path).ok()?.parent().map(Path::to_path_buf)).collect();
    let refs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    out::base(a.out.as_deref(), &refs).map_err(Fail::Usage)
}

fn exe_dir(a: &Args, img: &Image) -> R<PathBuf> {
    Ok(base_dir(a, &[img])?.join(img.build_id()))
}

/// The per-exe folder when --json asks for a file (checked before any work).
fn json_dir(a: &Args, img: &Image) -> R<Option<PathBuf>> {
    if a.json { exe_dir(a, img).map(Some) } else { Ok(None) }
}

fn save(dir: &Option<PathBuf>, name: &str, v: &Value) -> R<()> {
    if let Some(d) = dir {
        let p = out::write_json(d, name, v)?;
        out!("wrote {}", p.display());
    }
    Ok(())
}

fn load_funcs(img: &Image) -> R<Functions> {
    Ok(Functions::load(img)?)
}

/// Function starts, RTTI and a disassembler for annotated output.
struct Tools {
    funcs: Functions,
    rtti: Rtti,
    starts: Vec<u32>,
}

impl Tools {
    fn load(img: &Image) -> R<Tools> {
        let funcs = load_funcs(img)?;
        let starts = funcs.starts(img);
        Ok(Tools { funcs, rtti: Rtti::scan(img), starts })
    }

    fn disasm<'a>(&'a self, img: &'a Image) -> Disasm<'a> {
        Disasm::new(img, self.starts.clone(), Some(&self.rtti))
    }
}

// ---------------------------------------------------------------------------------------- info

fn days_to_civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// A PE timestamp as "YYYY-MM-DD hh:mm:ss UTC".
pub fn utc(ts: u32) -> String {
    let s = i64::from(ts);
    let (y, m, d) = days_to_civil(s.div_euclid(86_400));
    let t = s.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", t / 3600, t / 60 % 60, t % 60)
}

/// Steam's build id from steamapps\appmanifest_2231380.acf above the exe.
fn steam_build(exe: &Path) -> Option<(String, PathBuf)> {
    let abs = std::fs::canonicalize(exe).ok()?;
    let m = abs.ancestors().skip(1).map(|d| d.join("appmanifest_2231380.acf")).find(|m| m.is_file())?;
    let text = std::fs::read_to_string(&m).ok()?;
    text.lines().find_map(|l| {
        let p: Vec<&str> = l.split('"').collect();
        (p.len() >= 4 && p[1].eq_ignore_ascii_case("buildid")).then(|| (p[3].to_string(), m.clone()))
    })
}

fn info(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let dir = exe_dir(a, &img)?;
    let id = img.build_id();
    let sha: String = Sha256::digest(&img.data[..]).iter().map(|b| format!("{b:02x}")).collect();
    let imports = img.imports();
    let lower: Vec<String> = imports.dlls.iter().map(|d| d.to_lowercase()).collect();
    let has = |n: &str| lower.iter().any(|d| d == n || d.starts_with(&format!("{n} ")));
    let flavour = match (has("vulkan-1.dll"), has("d3d11.dll")) {
        (true, false) => "Vulkan (imports vulkan-1.dll)".to_string(),
        (false, true) => "DX11 (imports d3d11.dll)".to_string(),
        (true, true) => "unclear: imports both vulkan-1.dll and d3d11.dll".to_string(),
        _ => "unknown: imports neither vulkan-1.dll nor d3d11.dll".to_string(),
    };
    let steam = steam_build(&img.path);
    let funcs = Functions::load(&img);
    let rtti = Rtti::scan(&img);
    let relocs = img.relocs();

    out!("file          {}", show(&std::fs::canonicalize(&img.path).unwrap_or(img.path.clone())));
    out!("build id      {id}");
    out!("size          {} bytes", img.data.len());
    out!("sha256        {sha}");
    out!("pe timestamp  0x{:08x} ({})", img.timestamp, utc(img.timestamp));
    out!("checksum      0x{:08x}", img.checksum);
    out!("image base    va 0x{:x} (preferred on-disk base; the running game is moved by ASLR)", img.image_base);
    out!("entry         rva 0x{:x}", img.entry);
    out!("size of image 0x{:x}", img.size_of_image);
    out!("machine       0x{:x} (x64), subsystem {}", img.machine, img.subsystem);
    out!("flavour       {flavour}");
    match &steam {
        Some((b, m)) => out!("steam build   {b} ({})", show(m)),
        None => out!("steam build   not found (no appmanifest_2231380.acf above the exe)"),
    }
    out!();
    out!("sections (names are shuffled by the protection: the role comes from the flags)");
    out!("  #  name      rva          vsize        file         raw size     flags                       entropy  role");
    let mut secs = Vec::new();
    for s in &img.sections {
        let raw = img.raw(s);
        let e = entropy(&raw[..raw.len().min(4 << 20)]);
        out!(
            "  {:<2} {:<9} rva 0x{:<8x} 0x{:<10x} file 0x{:<7x} 0x{:<10x} 0x{:08x} {:<16} {:>5.2}    {}",
            s.index, s.name, s.rva, s.vsize, s.raw_off, s.raw_size, s.flags, s.flag_words(), e, s.role().label()
        );
        secs.push(json!({
            "index": s.index, "name": s.name, "rva": hx(s.rva), "vsize": hx(s.vsize), "file_offset": hx(s.raw_off),
            "raw_size": hx(s.raw_size), "flags": format!("0x{:08x}", s.flags), "flag_words": s.flag_words(),
            "entropy_first_4mb": (e * 1000.0).round() / 1000.0, "role": s.role().label(),
        }));
    }
    out!();
    out!("data directories");
    out!("  #  name           address             size        where");
    let mut dirs = Vec::new();
    for (i, d) in img.dirs.iter().enumerate() {
        if d.rva == 0 && d.size == 0 {
            continue;
        }
        let kind = if i == DIR_SECURITY { "file" } else { "rva" };
        out!("  {:<2} {:<14} {kind} 0x{:<13x} 0x{:<9x} {}", i, DIR_NAMES[i], d.rva, d.size, img.dir_place(i));
        dirs.push(json!({ "index": i, "name": DIR_NAMES[i], kind: hx(d.rva), "size": hx(d.size), "where": img.dir_place(i) }));
    }
    out!();
    let (ex_count, ex_chained) = match &funcs {
        Ok(f) => {
            let c = f.chained_count(&img);
            out!("exception entries  {} ({} primary functions, {} chained chunks)", f.len(), f.len() - c, c);
            (Some(f.len()), Some(c))
        }
        Err(e) => {
            out!("exception entries  not readable: {e}");
            (None, None)
        }
    };
    let with_vt = rtti.classes.iter().filter(|c| !c.vtable_rvas.is_empty()).count();
    out!("RTTI COLs          {} found ({} with a type name, {} with a vtable)", rtti.cols_found, rtti.classes.len(), with_vt);
    match &relocs {
        Ok(r) => out!("base relocations   {} fields", r.len()),
        Err(e) => out!("base relocations   not readable: {e}"),
    }
    out!("imports            {}", imports.dlls.len());
    for p in &imports.problems {
        out!("  note: {p}");
    }
    let v = json!({
        "build_id": id, "path": show(&img.path), "file_size": img.data.len(), "sha256": sha,
        "pe_timestamp": format!("0x{:08x}", img.timestamp), "pe_timestamp_utc": utc(img.timestamp),
        "checksum": format!("0x{:08x}", img.checksum), "image_base_va": hx(img.image_base), "entry_rva": hx(img.entry),
        "size_of_image": hx(img.size_of_image), "flavour": flavour,
        "steam_buildid": steam.as_ref().map(|s| s.0.clone()), "steam_manifest": steam.as_ref().map(|s| show(&s.1)),
        "sections": secs, "data_directories": dirs,
        "exception_entries": ex_count, "exception_chained": ex_chained,
        "rtti_cols": rtti.cols_found, "rtti_named": rtti.classes.len(), "rtti_with_vtable": with_vt,
        "relocations": relocs.as_ref().ok().map(Vec::len), "imports": imports.dlls,
    });
    let p = out::write_json(&dir, "build.json", &v)?;
    out!("\nwrote {}", p.display());
    Ok(0)
}

// ---------------------------------------------------------------------------------------- sigs

fn sigs(a: &Args) -> R<i32> {
    let paths = &a.words[1..];
    if paths.is_empty() {
        return Err(Fail::Usage("sigs needs at least one exe".into()));
    }
    let imgs: Vec<Image> = paths.iter().map(|p| open(p)).collect::<R<_>>()?;
    let refs: Vec<&Image> = imgs.iter().collect();
    let base = if a.json { Some(base_dir(a, &refs)?) } else { None };
    let (mut srcs, from) = match (&a.src, sigsrc::find_src_dir()) {
        (Some(d), _) => (sigsrc::load_dir(d)?, show(d)),
        (None, Some(d)) => (sigsrc::load_dir(&d)?, show(&d)),
        (None, None) if !a.patterns.is_empty() => (Vec::new(), "-".into()),
        (None, None) => return Err(Fail::Usage("dll\\src was not found above this exe or the current folder: pass --src DIR".into())),
    };
    let n_src = srcs.len();
    for p in &a.patterns {
        srcs.push(sigsrc::parse_arg(p).map_err(Fail::Usage)?);
    }
    let pats: Vec<aurora_core::pattern::Pattern> = srcs.iter().map(|s| aurora_core::pattern::Pattern::parse(&s.text)).collect::<Result<_, _>>()?;
    // One thread per pattern; each searches every exe's code sections.
    let hits: Vec<Vec<Vec<u32>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = pats.iter().map(|p| sc.spawn(|| imgs.iter().map(|img| makesig::find_code(img, p)).collect::<Vec<_>>())).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_default()).collect()
    });
    out!("{n_src} patterns from {from}, {} from --pattern", srcs.len() - n_src);
    let name_w = srcs.iter().map(|s| s.name.len()).max().unwrap_or(7).max(7);
    let ids: Vec<String> = imgs.iter().map(Image::build_id).collect();
    let mut head = format!("{:<name_w$}", "pattern");
    for id in &ids {
        head.push_str(&format!("  {id:<26}"));
    }
    out!("{}", head.trim_end());
    let mut bad = 0;
    let mut rows = Vec::new();
    let mut details = Vec::new();
    for (k, s) in srcs.iter().enumerate() {
        let mut line = format!("{:<name_w$}", s.name);
        let mut res = Vec::new();
        for (e, id) in ids.iter().enumerate() {
            let h = &hits[k][e];
            let cell = match h.len() {
                1 => format!("OK rva 0x{:x}", h[0]),
                0 => "MISSING".to_string(),
                n => format!("AMBIGUOUS({n})"),
            };
            if h.len() != 1 {
                bad += 1;
            }
            if h.len() > 1 {
                let list: Vec<String> = h.iter().take(8).map(|r| format!("rva 0x{r:x}")).collect();
                details.push(format!("  {} in {id}: {}{}", s.name, list.join(", "), if h.len() > 8 { ", ..." } else { "" }));
            }
            line.push_str(&format!("  {cell:<26}"));
            let status = match h.len() {
                1 => "OK".to_string(),
                0 => "MISSING".to_string(),
                n => format!("AMBIGUOUS({n})"),
            };
            res.push(json!({ "build_id": id, "status": status, "hits": h.len(), "rvas": h.iter().take(100).map(|&r| hx(r)).collect::<Vec<_>>() }));
        }
        out!("{}", line.trim_end());
        rows.push(json!({ "name": s.name, "pattern": s.text, "results": res }));
    }
    for d in &details {
        out!("{d}");
    }
    out!("{}", if bad == 0 { "all OK".to_string() } else { format!("{bad} not OK") });
    if let Some(b) = base {
        let exes: Vec<Value> = imgs.iter().map(|i| json!({ "path": show(&i.path), "build_id": i.build_id() })).collect();
        let p = out::write_json(&b, "sigs.json", &json!({ "source": from, "exes": exes, "patterns": rows }))?;
        out!("wrote {}", p.display());
    }
    Ok(if bad == 0 { 0 } else { 1 })
}

// ---------------------------------------------------------------------------------------- func

fn func(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let rva = rva_arg(a, 2)?;
    let dir = json_dir(a, &img)?;
    check_code(&img, rva)?;
    let funcs = load_funcs(&img)?;
    let f = funcs
        .containing(&img, rva)
        .ok_or_else(|| format!("no exception entry covers rva 0x{rva:x}: a leaf function (no unwind info), padding, or not code"))??;
    out!("function holding rva 0x{rva:x} in {}", img.build_id());
    out!("start   rva 0x{:x}  (sub_{:x}, va 0x{:x})", f.start, f.start, img.va(f.start));
    out!("size    0x{:x} bytes in {} chunk{}", f.size(), f.chunks.len(), if f.chunks.len() == 1 { "" } else { "s" });
    let mut chunks = Vec::new();
    for (i, c) in f.chunks.iter().enumerate() {
        let here = if c.begin <= rva && rva < c.end { "  <- holds the rva" } else { "" };
        out!(
            "chunk   rva 0x{:x} - rva 0x{:x}  size 0x{:x}  unwind info rva 0x{:x}  {}{here}",
            c.begin, c.end, c.end - c.begin, c.unwind, if i == 0 { "primary" } else { "chained" }
        );
        chunks.push(json!({ "begin_rva": hx(c.begin), "end_rva": hx(c.end), "unwind_info_rva": hx(c.unwind), "primary": i == 0 }));
    }
    if f.path.len() > 1 {
        let p: Vec<String> = f.path.iter().map(|c| format!("rva 0x{:x}", c.begin)).collect();
        out!("chain   {}", p.join(" -> "));
    }
    save(&dir, &format!("func-{rva:x}.json"), &json!({
        "build_id": img.build_id(), "rva": hx(rva), "start_rva": hx(f.start), "size": hx(f.size()), "chunks": chunks,
        "chain_rvas": f.path.iter().map(|c| hx(c.begin)).collect::<Vec<_>>(),
    }))?;
    Ok(0)
}

// ----------------------------------------------------------------------------------------- dis

fn print_line(l: &crate::disasm::Line, mark: bool) {
    let note = if l.note.is_empty() { String::new() } else { format!("  ; {}", l.note) };
    out!("{}rva 0x{:<8x} {:<31} {}{note}", if mark { "=> " } else { "   " }, l.rva, l.bytes, l.text);
}

fn dis(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let rva = rva_arg(a, 2)?;
    let dir = json_dir(a, &img)?;
    check_code(&img, rva)?;
    let t = Tools::load(&img)?;
    let mut d = t.disasm(&img);
    let mut lines = Vec::new();
    if a.func {
        let f = t.funcs.containing(&img, rva).ok_or_else(|| format!("no exception entry covers rva 0x{rva:x}: no function bounds (try without --func)"))??;
        out!("function sub_{:x} (rva 0x{:x}), 0x{:x} bytes in {} chunk(s), in {}", f.start, f.start, f.size(), f.chunks.len(), img.build_id());
        for (i, c) in f.chunks.iter().enumerate() {
            out!("-- chunk rva 0x{:x} - rva 0x{:x}{}", c.begin, c.end, if i == 0 { " (primary)" } else { " (chained)" });
            if let Err(e) = check_code(&img, c.begin) {
                out!("   {e}");
                continue;
            }
            for ins in d.decode(c.begin, Some(c.end), usize::MAX)? {
                let l = d.line(&ins);
                print_line(&l, l.rva == rva);
                lines.push(l);
            }
        }
    } else {
        let n = a.count.unwrap_or(20);
        let within = t.funcs.entry_at(rva).and_then(|e| crate::unwind::primary(&img, e).ok()).map(|(p, _)| p.begin);
        let where_ = within.map_or_else(|| "no function entry covers it".to_string(), |s| format!("in sub_{s:x}"));
        out!("{n} instructions from rva 0x{rva:x} ({where_}) in {}", img.build_id());
        for ins in d.decode(rva, None, n)? {
            let l = d.line(&ins);
            print_line(&l, false);
            lines.push(l);
        }
    }
    save(&dir, &format!("dis-{rva:x}.json"), &json!({ "build_id": img.build_id(), "rva": hx(rva), "lines": lines }))?;
    Ok(0)
}

// --------------------------------------------------------------------------------------- xrefs

/// Code and data references to one rva, printed; returns the JSON.
fn refs_for(img: &Image, t: &Tools, d: &mut Disasm, target: u32, limit: usize, indent: &str) -> Value {
    let code = xrefs::code_refs(img, &t.funcs, target);
    let data = xrefs::data_refs(img, target);
    out!("{indent}code: {}{}", code.len(), if code.len() > limit { format!(" (showing {limit})") } else { String::new() });
    let mut cj = Vec::new();
    for r in code.iter().take(limit) {
        let func = match (r.func, r.guessed) {
            (Some(f), _) => format!("sub_{f:x}"),
            (None, true) => "no entry (guess)".to_string(),
            (None, false) => "-".to_string(),
        };
        let text = d.text(&r.ins);
        out!("{indent}  rva 0x{:<8x} {:<10} {:<18} {text}", r.ins.ip(), r.kind, func);
        cj.push(json!({ "rva": hx(r.ins.ip()), "kind": r.kind, "function_rva": r.func.map(hx), "guessed_start": r.guessed, "text": text }));
    }
    out!("{indent}data pointers: {}{}", data.len(), if data.len() > limit { format!(" (showing {limit})") } else { String::new() });
    let mut dj = Vec::new();
    for &at in data.iter().take(limit) {
        let sec = img.section_of(at).map_or_else(|| "-".to_string(), |s| format!("section #{}", s.index));
        let ctx = match (t.rtti.slot_at(at), rtti::code_ptr_run(img, at)) {
            (Some((c, k)), _) => format!("vtable {} slot {k}", c.name),
            (None, Some((s, n, _))) => {
                // No RTTI: the table start is the nearest spot code takes the address of.
                match xrefs::lea_targets(img, &t.funcs, s, at).last() {
                    Some(&(vt, by)) => format!("slot {} of the pointer table at rva 0x{vt:x} (no RTTI; lea at rva 0x{by:x})", (at - vt) / 8),
                    None => format!("slot {} of a run of {n} code pointers from rva 0x{s:x} (no RTTI, no lea into it)", (at - s) / 8),
                }
            }
            (None, None) => String::new(),
        };
        out!("{indent}  rva 0x{at:<8x} {:<10} {sec:<18} {ctx}", "data-ptr");
        dj.push(json!({ "rva": hx(at), "kind": "data-ptr", "section": sec, "context": ctx }));
    }
    json!({ "target_rva": hx(target), "code": cj, "data": dj, "code_total": code.len(), "data_total": data.len() })
}

fn xrefs_cmd(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let rva = rva_arg(a, 2)?;
    let dir = json_dir(a, &img)?;
    if img.in_protected(rva) {
        return Err(Fail::Msg(format!("rva 0x{rva:x} is in the protected section (skipped)")));
    }
    let t = Tools::load(&img)?;
    let mut d = t.disasm(&img);
    let what = d.describe(rva);
    out!(
        "references to rva 0x{rva:x} ({}{}) in {}",
        symbol_name(&t.starts, u64::from(rva)),
        if what.is_empty() { String::new() } else { format!(", {what}") },
        img.build_id()
    );
    let v = refs_for(&img, &t, &mut d, rva, a.limit.unwrap_or(200), "");
    save(&dir, &format!("xrefs-{rva:x}.json"), &v)?;
    Ok(0)
}

// ---------------------------------------------------------------------------------------- rtti

fn rtti_line(c: &rtti::Class) -> String {
    let vt = match c.vtable_rvas.as_slice() {
        [] => "-".to_string(),
        [v] => format!("0x{v:x}"),
        [v, rest @ ..] => format!("0x{v:x}+{}", rest.len()),
    };
    let bases = if c.bases.is_empty() { String::new() } else { format!("  : {}", c.bases.join(", ")) };
    format!("{vt:<13} {:>5}  0x{:<9x} 0x{:<9x} {}{bases}", c.methods, c.col_rva, c.type_descriptor_rva, c.name)
}

const RTTI_HEAD: &str = "vtable rva    slots  COL rva     TD rva      class  : bases";

fn rtti_cmd(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let dir = exe_dir(a, &img)?;
    let r = Rtti::scan(&img);
    let mut order: Vec<usize> = (0..r.classes.len()).collect();
    order.sort_by(|&x, &y| r.classes[x].name.cmp(&r.classes[y].name).then(r.classes[x].col_rva.cmp(&r.classes[y].col_rva)));
    let with_vt = r.classes.iter().filter(|c| !c.vtable_rvas.is_empty()).count();
    let head = format!("RTTI in {}: {} COLs found, {} with a type name, {} with a vtable", img.build_id(), r.cols_found, r.classes.len(), with_vt);
    // Files first (all classes), then the filtered list on screen.
    let lines: Vec<String> = order.iter().map(|&i| rtti_line(&r.classes[i])).collect();
    let txt = format!("{head}\n{RTTI_HEAD}\n{}\n", lines.join("\n"));
    let classes: Vec<Value> = order
        .iter()
        .map(|&i| {
            let c = &r.classes[i];
            json!({
                "name": c.name, "raw": c.raw, "col_rva": hx(c.col_rva), "type_descriptor_rva": hx(c.type_descriptor_rva),
                "hierarchy_rva": hx(c.hierarchy_rva), "subobject_offset": hx(c.subobject_offset),
                "vtable_rvas": c.vtable_rvas.iter().map(|&v| hx(v)).collect::<Vec<_>>(), "methods": c.methods,
                "bases": c.bases, "attributes": c.attributes,
            })
        })
        .collect();
    let p1 = out::write(&dir, "rtti.txt", txt.as_bytes())?;
    let p2 = out::write_json(&dir, "rtti.json", &json!({ "build_id": img.build_id(), "cols_found": r.cols_found, "classes": classes }))?;
    out!("{head}");
    out!("wrote {} and {}", p1.display(), p2.display());
    let filt = a.filter.as_ref().map(|f| f.to_lowercase());
    out!("{RTTI_HEAD}");
    let mut shown = 0;
    for (k, &i) in order.iter().enumerate() {
        let c = &r.classes[i];
        if filt.as_ref().is_none_or(|f| c.name.to_lowercase().contains(f) || c.raw.to_lowercase().contains(f)) {
            out!("{}", lines[k]);
            shown += 1;
        }
    }
    if filt.is_some() {
        out!("{shown} of {} classes match", r.classes.len());
    }
    Ok(0)
}

// -------------------------------------------------------------------------------------- vtable

fn vtable(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let key = word(a, 2, "a class name or a vtable rva")?;
    let dir = json_dir(a, &img)?;
    let t = Tools::load(&img)?;
    let r = &t.rtti;
    // (class index or none, vtable rva)
    let mut picks: Vec<(Option<usize>, u32)> = Vec::new();
    let lk = key.to_lowercase();
    for (i, c) in r.classes.iter().enumerate() {
        if c.name.to_lowercase() == lk || c.raw.to_lowercase() == lk {
            picks.extend(c.vtable_rvas.iter().map(|&v| (Some(i), v)));
        }
    }
    if picks.is_empty() {
        if let Ok(v) = parse_rva(key) {
            picks.push((r.by_vtable.get(&v).copied(), v));
        } else {
            let near: Vec<&str> = r.classes.iter().filter(|c| c.name.to_lowercase().contains(&lk)).take(10).map(|c| c.name.as_str()).collect();
            let hint = if near.is_empty() { String::new() } else { format!(" Close: {}", near.join(" | ")) };
            return Err(Fail::Msg(format!("no RTTI class with a vtable is named '{key}' (exact name or raw name).{hint}")));
        }
    }
    let mut d = t.disasm(&img);
    let mut all = Vec::new();
    for (ci, vt) in picks {
        let mut slots = rtti::slots(&img, vt);
        let mut end_note = String::new();
        if ci.is_none() && slots.len() > 1 {
            // No COL to end it: stop at the next spot code takes the address of (the next vtable's
            // start, as constructors `lea` it).
            let last = vt + 8 * (slots.len() as u32 - 1);
            if let Some(&(next, by)) = xrefs::lea_targets(&img, &t.funcs, vt + 8, last).first() {
                slots.truncate(((next - vt) / 8) as usize);
                end_note = format!("; ends where the next table starts (rva 0x{next:x}, lea at rva 0x{by:x})");
            }
        }
        match ci {
            Some(i) => {
                let c = &r.classes[i];
                out!("vtable rva 0x{vt:x}  class {}  (COL rva 0x{:x}, sub-object offset 0x{:x}), {} slots", c.name, c.col_rva, c.subobject_offset, slots.len());
            }
            None => out!("vtable rva 0x{vt:x}  (no RTTI COL before it: a plain pointer table), {} slots{end_note}", slots.len()),
        }
        out!("slot  at rva        function          first instruction");
        let mut sj = Vec::new();
        for s in &slots {
            let first = if img.in_protected(s.target_rva) {
                "(protected section, skipped)".to_string()
            } else {
                match d.decode(s.target_rva, None, 1) {
                    Ok(v) if !v.is_empty() => d.line(&v[0]).text,
                    Ok(_) => "-".into(),
                    Err(e) => e,
                }
            };
            let name = symbol_name(&d.starts, u64::from(s.target_rva));
            out!("{:>4}  rva 0x{:<8x} {name:<17} {first}", s.index, s.at_rva);
            sj.push(json!({ "index": s.index, "at_rva": hx(s.at_rva), "function_rva": hx(s.target_rva), "first_instruction": first }));
        }
        all.push(json!({ "vtable_rva": hx(vt), "class": ci.map(|i| r.classes[i].name.clone()), "slots": sj }));
    }
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(60).collect();
    save(&dir, &format!("vtable-{safe}.json"), &json!({ "build_id": img.build_id(), "key": key, "vtables": all }))?;
    Ok(0)
}

// ------------------------------------------------------------------------------------- makesig

fn relocs_or_note(img: &Image) -> Vec<(u32, u8)> {
    match img.relocs() {
        Ok(r) => r,
        Err(e) => {
            out!("note: {e}; only address-looking values are wildcarded");
            Vec::new()
        }
    }
}

fn makesig_cmd(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let rva = rva_arg(a, 2)?;
    let other = a.other.as_ref().map(|p| Image::open(p)).transpose()?;
    let dir = json_dir(a, &img)?;
    let funcs = load_funcs(&img)?;
    let relocs = relocs_or_note(&img);
    let sig = makesig::make(&img, &funcs, &relocs, rva, other.as_ref())?;
    let fixed = sig.mask.iter().filter(|b| b.is_some()).count();
    out!("signature for rva 0x{rva:x} in {}", img.build_id());
    out!("pattern     {}", sig.text());
    out!("length      {} bytes ({fixed} fixed)", sig.mask.len());
    out!("starts at   rva 0x{:x}; the asked rva is at +{} in the pattern", sig.start, sig.target_off);
    out!("unique      yes (1 hit in the code sections{})", if sig.grown_for_other { "; grown past the shortest to tell the other build's hits apart" } else { "" });
    if sig.past_end(&funcs) {
        out!("note        the pattern runs past the end of its function: it leans on the neighbour's layout");
    }
    let mut oj = Value::Null;
    if let (Some(o), Some(h)) = (&other, &sig.other) {
        let targets: Vec<String> = h.pattern_rvas.iter().take(8).map(|p| format!("rva 0x{:x}", p + sig.target_off)).collect();
        let verdict = match h.pattern_rvas.len() {
            0 => "not found".to_string(),
            1 => format!("1 hit -> {}", targets[0]),
            n => format!("AMBIGUOUS({n}): {}", targets.join(", ")),
        };
        out!("other       {}: {verdict}", o.build_id());
        oj = json!({ "build_id": o.build_id(), "pattern_rvas": h.pattern_rvas.iter().map(|&p| hx(p)).collect::<Vec<_>>(),
            "target_rvas": h.pattern_rvas.iter().map(|&p| hx(p + sig.target_off)).collect::<Vec<_>>() });
    }
    save(&dir, &format!("makesig-{rva:x}.json"), &json!({
        "build_id": img.build_id(), "rva": hx(rva), "pattern": sig.text(), "length": sig.mask.len(), "start_rva": hx(sig.start),
        "target_offset": sig.target_off, "other": oj,
    }))?;
    Ok(0)
}

// ------------------------------------------------------------------------------------- strings

fn strings(a: &Args) -> R<i32> {
    let img = open(word(a, 1, "an exe")?)?;
    let text = word(a, 2, "the text to find")?;
    let dir = json_dir(a, &img)?;
    let ascii: Vec<u8> = text.as_bytes().to_vec();
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut hits: Vec<(u32, &str)> = Vec::new();
    for (enc, bytes) in [("ascii", &ascii), ("utf16", &utf16)] {
        let hexs: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
        let p = aurora_core::pattern::Pattern::parse(&hexs.join(" "))?;
        for s in img.open_sections() {
            hits.extend(p.find_all(img.raw(s), 1000).into_iter().map(|o| (s.rva + o as u32, enc)));
        }
    }
    hits.sort();
    out!("\"{text}\": {} hit(s) in {}", hits.len(), img.build_id());
    let t = if a.xrefs { Some(Tools::load(&img)?) } else { None };
    let mut d = t.as_ref().map(|t| t.disasm(&img));
    let mut out_v = Vec::new();
    for (k, &(at, enc)) in hits.iter().enumerate() {
        let start = string_start(&img, at, enc == "utf16");
        let full = crate::disasm::string_at(&img, start).unwrap_or_else(|| quote(text.as_bytes()));
        let sec = img.section_of(at).map_or_else(|| "-".to_string(), |s| format!("section #{} ({})", s.index, s.role().label()));
        out!("rva 0x{at:<8x} {enc:<5}  {sec}  string at rva 0x{start:x}: {full}");
        let mut v = json!({ "rva": hx(at), "encoding": enc, "section": sec, "string_rva": hx(start), "string": full });
        if let (Some(t), Some(d)) = (&t, &mut d) {
            if k < 50 {
                v["refs"] = refs_for(&img, t, d, start, a.limit.unwrap_or(50), "    ");
            } else if k == 50 {
                out!("    (refs shown for the first 50 hits only)");
            }
        }
        out_v.push(v);
    }
    let safe: String = text.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(40).collect();
    save(&dir, &format!("strings-{safe}.json"), &json!({ "build_id": img.build_id(), "text": text, "hits": out_v }))?;
    Ok(0)
}

/// Back to the start of the string holding `at` (previous NUL), at most 4 KB.
fn string_start(img: &Image, at: u32, wide: bool) -> u32 {
    let step = if wide { 2 } else { 1 };
    let mut s = at;
    for _ in 0..4096 {
        let Some(p) = s.checked_sub(step) else { break };
        let Some(b) = img.bytes(p, step as usize) else { break };
        let ok = if wide { (0x20..0x7F).contains(&b[0]) && b[1] == 0 } else { (0x20..0x7F).contains(&b[0]) };
        if !ok {
            break;
        }
        s = p;
    }
    s
}

// ------------------------------------------------------------------------------------- compare

fn compare(a: &Args) -> R<i32> {
    let ia = open(word(a, 1, "exe A")?)?;
    let ib = open(word(a, 2, "exe B")?)?;
    let rvas: Vec<u32> = a.words[3..].iter().map(|s| parse_rva(s).map_err(Fail::Usage)).collect::<R<_>>()?;
    if rvas.is_empty() {
        return Err(Fail::Usage("compare needs at least one rva".into()));
    }
    let base = if a.json { Some(base_dir(a, &[&ia, &ib])?) } else { None };
    let funcs = load_funcs(&ia)?;
    let relocs = relocs_or_note(&ia);
    let mut rtti_ab: Option<(Rtti, Rtti)> = None;
    out!("{} -> {}", ia.build_id(), ib.build_id());
    out!("rva A        method              confidence  rva B        detail");
    let mut rows = Vec::new();
    for &rva in &rvas {
        let (mut mapped, mut method, mut conf) = (None, "not mapped", "-");
        let mut detail = match makesig::make(&ia, &funcs, &relocs, rva, Some(&ib)) {
            Ok(sig) => {
                let hits = sig.other.as_ref().map(|o| o.pattern_rvas.clone()).unwrap_or_default();
                match hits.len() {
                    1 => {
                        mapped = Some(hits[0] + sig.target_off);
                        // A pattern that leans on the next function is less sure.
                        let past = sig.past_end(&funcs);
                        (method, conf) = ("unique signature", if past { "medium" } else { "high" });
                        format!("{} bytes{}: {}", sig.mask.len(), if past { ", runs past the function end" } else { "" }, sig.text())
                    }
                    0 => "signature not found in B".to_string(),
                    n => format!("signature matches {n} times in B"),
                }
            }
            Err(e) => e,
        };
        if mapped.is_none() {
            let (ra, rb) = rtti_ab.get_or_insert_with(|| (Rtti::scan(&ia), Rtti::scan(&ib)));
            let mut found: Vec<(u32, String)> = Vec::new();
            for (ci, _vt, slot) in ra.slots_pointing_at(&ia, rva) {
                let c = &ra.classes[ci];
                let Some(cb) = rb.classes.iter().find(|x| x.raw == c.raw && x.subobject_offset == c.subobject_offset) else { continue };
                let Some(&vtb) = cb.vtable_rvas.first() else { continue };
                if let Some(s) = rtti::slots(&ib, vtb).get(slot) {
                    found.push((s.target_rva, format!("vtable {} slot {slot}", c.name)));
                }
            }
            found.sort();
            found.dedup_by_key(|f| f.0);
            match found.len() {
                1 => {
                    mapped = Some(found[0].0);
                    (method, conf) = ("same vtable slot", "medium");
                    detail = format!("{} ({detail})", found[0].1);
                }
                0 => {}
                n => detail = format!("{n} different vtable-slot answers ({detail})"),
            }
        }
        let mb = mapped.map_or_else(|| "-".to_string(), |m| format!("0x{m:x}"));
        out!("0x{rva:<10x} {method:<19} {conf:<11} {mb:<12} {detail}");
        rows.push(json!({ "rva_a": hx(rva), "rva_b": mapped.map(hx), "method": method, "confidence": conf, "detail": detail }));
    }
    if let Some(b) = base {
        let name = format!("compare-{}-{}.json", ia.build_id(), ib.build_id());
        let p = out::write_json(&b, &name, &json!({ "a": ia.build_id(), "b": ib.build_id(), "rows": rows }))?;
        out!("wrote {}", p.display());
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rvas_and_dates() {
        assert_eq!(parse_rva("11bb332"), Ok(0x11bb332));
        assert_eq!(parse_rva("0x11BB332"), Ok(0x11bb332));
        assert_eq!(parse_rva("exe+0x11bb332"), Ok(0x11bb332));
        assert!(parse_rva("0x1411bb332").is_err());
        assert!(parse_rva("zz").is_err());
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(0x64ff3734), "2023-09-11 15:50:12 UTC");
    }
}
