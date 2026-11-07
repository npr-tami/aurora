//! `selftest`: attach to OUR OWN process (never another one) and prove the read paths, pointer
//! chains, scans, next, snapshots, diff and refs against data we placed ourselves.

use std::ffi::c_void;
use std::path::PathBuf;

use aurora_core::pattern::Pattern;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_GUARD, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};

use crate::cmd::{self, Ctx};
use crate::proc::{ReadError, Target};
use crate::scan::{self, Area, NextMode, Needle, Opts, Stats};
use crate::store;
use crate::types::{self, VType};

#[repr(C)]
struct Inner {
    tag: u64,
    hp: f32,
    pad: u32,
    pos: [f64; 3],
}

fn dummy() {}

/// A fake vtable in our image: first entry points at code.
static FAKE_VT: [fn(); 2] = [dummy, dummy];
/// A byte signature that sits in our image's data.
static SIG: [u8; 16] = [0x5E, 0x1F, 0x7E, 0x57, 0xC0, 0xDE, 0x01, 0x02, 0x03, 0x04, 0xA5, 0x5A, 0x99, 0x77, 0x13, 0x37];

const MARK: u32 = 0x5EED_1234;
const REGION: usize = 0x10000;

struct Report {
    pass: usize,
    fail: usize,
}

impl Report {
    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.pass += 1;
            println!("ok   {name}");
        } else {
            self.fail += 1;
            println!("FAIL {name}: {detail}");
        }
    }
}

unsafe fn put<T>(base: *mut u8, off: usize, v: T) {
    // SAFETY: caller passes an offset inside our own committed region.
    unsafe { std::ptr::write_volatile(base.add(off) as *mut T, v) };
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

pub fn run(dir: Option<PathBuf>) -> i32 {
    let dir = dir.unwrap_or_else(|| store::test_dir("selftest"));
    let pid = std::process::id();
    println!("selftest: reading only our own process (pid {pid}); files in {}", dir.display());
    let mut r = Report { pass: 0, fail: 0 };

    // Data we own: a heap object, and a private region with known values.
    let mut inner = Box::new(Inner { tag: 0x1122_3344_5566_7788, hp: 12.5, pad: 0, pos: [1.0, 2.0, 3.0] });
    let ip = &*inner as *const Inner as u64;
    // SAFETY: plain allocations in our own process; freed at the end.
    let region = unsafe { VirtualAlloc(std::ptr::null(), REGION, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE) } as *mut u8;
    if region.is_null() {
        println!("FAIL could not allocate the test region");
        return 1;
    }
    let rb = region as u64;
    let vt = FAKE_VT.as_ptr() as u64;
    // SAFETY: all offsets are inside the 64 KB region.
    unsafe {
        put(region, 0x100, 0xABCD_u64); // outer.magic
        put(region, 0x108, ip); // outer.inner
        put(region, 0x200, rb + 0x100); // Pointer to outer
        put(region, 0x1000, MARK);
        put(region, 0x1010, MARK);
        put(region, 0x2004, MARK);
        put(region, 0x3000, ip); // A second reference
        put(region, 0x5000, vt); // Object with a vtable...
        put(region, 0x5018, ip); // ...holding a field that points at inner
    }

    let mut ctx = Ctx::new(dir.clone(), Some(pid), String::new());
    let t = match Target::open(pid) {
        Ok(t) => t,
        Err(e) => {
            println!("FAIL open own process read-only: {e}");
            return 1;
        }
    };
    // SAFETY: our own module handle.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as u64;
    r.check("open own pid read-only, exe module first", t.exe_module().map(|m| m.base) == Some(base), format!("{:?}", t.exe_module()));
    r.check("alive", t.alive(), String::new());

    // Reads.
    r.check("read u64 from heap", t.read_u64(ip) == Ok(0x1122_3344_5566_7788), format!("{:?}", t.read_u64(ip)));
    let ev = |text: &str| cmd::eval_in(&t, &ctx.vars, text).map(|(v, s)| (v, s.len())).map_err(|e| format!("{e:?}"));
    r.check("exe+0 is the image base", ev("exe+0") == Ok((base, 0)), format!("{:?}", ev("exe+0")));
    let sig_desc = t.describe(SIG.as_ptr() as u64);
    r.check("static form is module+rva", sig_desc.starts_with(&t.exe) && sig_desc.contains("+0x"), sig_desc.clone());
    let hp = ev(&format!("[0x{:X}+8]+8", rb + 0x100));
    r.check("chain into a heap Box", hp == Ok((ip + 8, 1)), format!("{hp:?}"));
    let hpv = t.read(ip + 8, 4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    r.check("read f32 through the chain", hpv == Ok(12.5), format!("{hpv:?}"));
    let pos = ev(&format!("[[0x{:X}]+8]+0x10", rb + 0x200));
    r.check("nested chain", pos == Ok((ip + 0x10, 2)), format!("{pos:?}"));
    // [[outer]+0x20] reads a zero, so the third read is at 0x10: the null page.
    let bad = ev(&format!("[[[0x{:X}]+0x20]+0x10]", rb + 0x200));
    r.check(
        "chain reports the failing step",
        bad.as_ref().is_err_and(|e| e.contains("InvalidAddress") && e.contains("0x10 failed")),
        format!("{bad:?}"),
    );

    // Read error kinds, on our own allocations.
    r.check("null page -> InvalidAddress", t.read(0x10, 8) == Err(ReadError::InvalidAddress), format!("{:?}", t.read(0x10, 8)));
    r.check("kernel address -> InvalidAddress", t.read(0xFFFF_8000_0000_0000, 8) == Err(ReadError::InvalidAddress), String::new());
    // SAFETY: our own reservations, released below.
    let (resv, noacc, guard, part) = unsafe {
        let resv = VirtualAlloc(std::ptr::null(), 0x10000, MEM_RESERVE, PAGE_NOACCESS);
        let noacc = VirtualAlloc(std::ptr::null(), 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_NOACCESS);
        let guard = VirtualAlloc(std::ptr::null(), 0x1000, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE | PAGE_GUARD);
        let part = VirtualAlloc(std::ptr::null(), 0x2000, MEM_RESERVE, PAGE_NOACCESS);
        VirtualAlloc(part, 0x1000, MEM_COMMIT, PAGE_READWRITE);
        (resv, noacc, guard, part)
    };
    r.check("reserved -> Reserved", t.read(resv as u64, 8) == Err(ReadError::Reserved), format!("{:?}", t.read(resv as u64, 8)));
    r.check("no-access -> NoAccess", t.read(noacc as u64, 8) == Err(ReadError::NoAccess), format!("{:?}", t.read(noacc as u64, 8)));
    r.check("guard page -> NoAccess (never touched)", t.read(guard as u64, 8) == Err(ReadError::NoAccess), String::new());
    let pr = t.read(part as u64, 0x2000);
    r.check("half committed -> PartialRead(4096)", pr == Err(ReadError::PartialRead(0x1000)), format!("{pr:?}"));
    // SAFETY: release our own reservations.
    unsafe {
        for p in [resv, noacc, guard, part] {
            VirtualFree(p, 0, MEM_RELEASE);
        }
    }
    let fr = t.read(resv as u64, 8);
    r.check("released -> Free", fr == Err(ReadError::Free) || fr == Err(ReadError::Reserved), format!("{fr:?}"));

    // Value scan + next through the real commands.
    let range = format!("--range 0x{rb:X} 0x{:X}", rb + REGION as u64);
    let run = |ctx: &mut Ctx, line: &str| cmd::run(ctx, &words(line)).map_err(|e| format!("{e:?}"));
    let out = run(&mut ctx, &format!("scan value u32 {MARK} {range} --save selftest_u32"));
    let saved = store::load_scan(&dir, "selftest_u32").map(|s| s.hits.len());
    r.check("scan value finds 3", out.is_ok() && saved == Ok(3), format!("{out:?} {saved:?}"));
    // SAFETY: inside our region.
    unsafe { put(region, 0x1010, MARK + 1) };
    let out = run(&mut ctx, "next selftest_u32 changed");
    let after = store::load_scan(&dir, "selftest_u32").map(|s| s.hits.iter().map(|h| h.a).collect::<Vec<_>>());
    r.check("next changed keeps the one that moved", out.is_ok() && after == Ok(vec![rb + 0x1010]), format!("{out:?} {after:?}"));
    let out = run(&mut ctx, &format!("next selftest_u32 eq {}", MARK + 1));
    let after = store::load_scan(&dir, "selftest_u32").map(|s| (s.hits.len(), s.history.len()));
    r.check("next eq keeps it, history grows", out.is_ok() && after == Ok((1, 3)), format!("{out:?} {after:?}"));

    // Engine directly: aligned vs unaligned, reuse-style reread, narrow.
    let mut st = Stats::default();
    let spans = vec![[rb, rb + REGION as u64]];
    let needle = Needle::Value { t: VType::U32, target: types::parse_value(VType::U32, &MARK.to_string()).unwrap_or(types::Target { raw: 0, tol: 0.0 }) };
    let o = Opts { limit: 0, resident_only: false, aligned: true };
    let hits = scan::find(&t, &spans, &needle, &o, &mut st);
    r.check("engine scan: 2 left unchanged", hits.len() == 2, format!("{hits:?}"));
    let now: Vec<Option<u64>> = scan::reread(&t, &[rb + 0x1000, rb + 0x2004], 4).into_iter().map(|b| b.map(|b| types::raw_of(&b))).collect();
    let kept = scan::narrow(&hits, &now, VType::U32, NextMode::Unchanged);
    r.check("engine narrow unchanged", kept.len() == 2, format!("{kept:?}"));
    let o1 = Opts { limit: 1, ..o };
    let mut st1 = Stats::default();
    let one = scan::find(&t, &spans, &needle, &o1, &mut st1);
    r.check("--limit stops early", one.len() == 1 && st1.truncated, format!("{one:?}"));

    // AOB in our own image.
    let p = Pattern::parse("5E 1F 7E 57 ?? DE 01 02 03 04 A5 5A").unwrap_or_else(|_| unreachable!());
    let mut st2 = Stats::default();
    let ispans = scan::spans(&t, &Area::Module("exe".into()), &mut st2).unwrap_or_default();
    let ah = scan::find(&t, &ispans, &Needle::Aob(p), &o, &mut st2);
    r.check("aob finds our signature", ah.iter().any(|h| h.a == SIG.as_ptr() as u64), format!("{} hits", ah.len()));

    // Snapshots and diff.
    let a = run(&mut ctx, &format!("snap selftest_a 0x{ip:X} 0x28"));
    inner.hp = 99.0;
    std::hint::black_box(&inner);
    let b = run(&mut ctx, &format!("snap selftest_b 0x{ip:X} 0x28"));
    let ch = match (store::load_snap(&dir, "selftest_a"), store::load_snap(&dir, "selftest_b")) {
        (Ok(x), Ok(y)) => crate::diff::diff(&x.data(), &y.data(), 4),
        _ => Vec::new(),
    };
    r.check("snap + diff: one field changed at +0x8", a.is_ok() && b.is_ok() && ch.len() == 1 && ch[0].off == 8, format!("{ch:?}"));
    let d = run(&mut ctx, "diff selftest_a selftest_b --as f32");
    r.check("diff command", d.is_ok(), format!("{d:?}"));

    // References and the containing object.
    let rf = run(&mut ctx, &format!("refs 0x{ip:X} {range}"));
    let last = store::load_scan(&dir, "_last").map(|s| s.hits.iter().map(|h| h.a).collect::<Vec<_>>());
    r.check("refs finds all 3 references", rf.is_ok() && last == Ok(vec![rb + 0x108, rb + 0x3000, rb + 0x5018]), format!("{rf:?} {last:?}"));
    let co = cmd::containing_object(&t, rb + 0x5018, 0x200);
    r.check("refs: field +0x18 of an object with our fake vtable", co == Some((rb + 0x5000, vt)), format!("{co:?}"));
    r.check("refs: plain field has no object", cmd::containing_object(&t, rb + 0x3000, 0x200).is_none(), String::new());
    let vs = run(&mut ctx, &format!("scan vtable 0x{vt:X} {range} --save selftest_vt"));
    let vh = store::load_scan(&dir, "selftest_vt").map(|s| s.hits.len());
    r.check("scan vtable", vs.is_ok() && vh == Ok(1), format!("{vs:?} {vh:?}"));

    // The printing commands run end to end.
    for line in [
        "info".to_string(),
        format!("read 0x{:X} f64 3", ip + 0x10),
        format!("view 0x{:X} 0x20", rb + 0x100),
        format!("inspect 0x{:X} 0x40", rb + 0x5000),
        format!("chain [[0x{:X}]+8]+0x10", rb + 0x200),
        format!("dump 0x{:X} 0x20", SIG.as_ptr() as u64),
        format!("regions --range 0x{rb:X} 0x{:X}", rb + REGION as u64),
        "modules".to_string(),
        "db find body".to_string(),
    ] {
        let res = run(&mut ctx, &line);
        r.check(&format!("command: {line}"), res.is_ok(), format!("{res:?}"));
    }
    // Watch sees a change made by another thread.
    let hp_addr = ip + 8;
    let changer = std::thread::spawn(move || {
        for i in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            // SAFETY: inner outlives this thread (joined below); a racy f32 store is fine for a test.
            unsafe { std::ptr::write_volatile(hp_addr as *mut f32, 100.0 + i as f32) };
        }
    });
    let w = run(&mut ctx, &format!("watch 0x{hp_addr:X} 4 --ms 10 --secs 0.4 --as f32"));
    let _ = changer.join();
    r.check("watch", w.is_ok(), format!("{w:?}"));
    let x = run(&mut ctx, "export");
    r.check("export", x.is_ok(), format!("{x:?}"));

    std::hint::black_box(&inner);
    drop(inner);
    // SAFETY: our own region, released once.
    unsafe { VirtualFree(region as *mut c_void, 0, MEM_RELEASE) };
    println!("selftest: {} passed, {} failed", r.pass, r.fail);
    if r.fail == 0 {
        println!("selftest ok");
        0
    } else {
        1
    }
}
