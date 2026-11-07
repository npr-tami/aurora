//! Extra log lines on why a character's bones turn into a box. Read-only toward the game: it only
//! reads (through `memory::read`), never writes, and adds no hooks. What is drawn never depends on
//! anything here.
//!
//! The draw thread only notes, per character, bones or box, and queues a line when that switches
//! (rate-limited). The scan thread does the game reads for those lines (`drain`), so a frame never
//! waits on them.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::ppath::{OWNER_BODY, OWNER_LEVEL, OWNER_POSE, anim_owner};
use super::{EXE, GAME_METRES, POS, POSES, is_ptr};
use crate::{log, memory};

// ------------------------------------------------------------------ reads (checked)

fn rd(addr: u64, off: u64, len: usize) -> Option<Vec<u8>> {
    memory::read(addr.checked_add(off)?, len).ok()
}

fn rd_u64(addr: u64, off: u64) -> Option<u64> {
    Some(u64::from_le_bytes(rd(addr, off, 8)?.try_into().ok()?))
}

fn rd_u32(addr: u64, off: u64) -> Option<u32> {
    Some(u32::from_le_bytes(rd(addr, off, 4)?.try_into().ok()?))
}

/// P path: `memory::read` in the shape the shared owner helper takes.
fn read_into(addr: u64, buf: &mut [u8]) -> usize {
    memory::read(addr, buf.len()).map_or(0, |v| {
        buf.copy_from_slice(&v);
        v.len()
    })
}

fn rd_f32(addr: u64, off: u64) -> Option<f32> {
    Some(f32::from_le_bytes(rd(addr, off, 4)?.try_into().ok()?))
}

fn floats(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

/// A position: three f64 (bodies at +0x20, pose objects at +0).
fn rd_pos(addr: u64) -> Option<[f64; 3]> {
    let b = rd(addr, 0, 24)?;
    let v = [0, 8, 16].map(|k| f64::from_le_bytes(b[k..k + 8].try_into().unwrap_or_default()));
    v.iter().all(|x| x.is_finite() && x.abs() < 1.0e5).then_some(v)
}

fn dist3(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt()
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

// ------------------------------------------------------------------ heartbeat text

/// The heartbeat's bone count.
pub(super) fn bones_text(ok: u64, miss: u64) -> String {
    format!("bones drawn/box/total {ok}/{miss}/{}", ok + miss)
}

/// The 'bones missed' band, in the game's own metres (GAME_METRES 1.5; the old labels "<30m" and
/// "60m+" were world units).
pub(super) fn band(dist: f64) -> &'static str {
    if dist < 30.0 {
        "<45 game m"
    } else if dist < 60.0 {
        "45-90 game m"
    } else {
        "90+ game m"
    }
}

/// Pointer path checks since the last heartbeat: checked, owner found, owner+0x10 is the body, its
/// final pose P was caught by the hook, P is one of our rigs, P is the rig drawn.
static PATH: [AtomicU64; 6] = [const { AtomicU64::new(0) }; 6];
/// Bones <-> box switches, and lines not logged because of the limit.
static SWITCHES: AtomicU64 = AtomicU64::new(0);
static OVER: AtomicU64 = AtomicU64::new(0);
/// Frames with frozen bones drawn, and new frozen spells.
static FROZEN_FRAMES: AtomicU64 = AtomicU64::new(0);
static FROZEN_NEW: AtomicU64 = AtomicU64::new(0);

/// The end of the heartbeat line (empty when there is nothing to say).
pub(super) fn heartbeat_tail() -> String {
    let path = PATH.each_ref().map(|a| a.swap(0, Ordering::Relaxed));
    tail_text(
        path,
        FROZEN_FRAMES.swap(0, Ordering::Relaxed),
        FROZEN_NEW.swap(0, Ordering::Relaxed),
        SWITCHES.swap(0, Ordering::Relaxed),
        OVER.swap(0, Ordering::Relaxed),
    )
}

pub(super) fn tail_text(path: [u64; 6], frozen: u64, frozen_new: u64, switches: u64, over: u64) -> String {
    if path[0] == 0 && frozen == 0 && switches == 0 && over == 0 {
        return String::new();
    }
    format!(
        ", path checks {} (owner {}, +0x10 is body {}, P caught {}, P a rig {}, P drawn {}), frozen bones {frozen} frames ({frozen_new} new), switches {switches} ({over} lines over the limit)",
        path[0], path[1], path[2], path[3], path[4], path[5]
    )
}

// ------------------------------------------------------------------ rig_layout steps

/// The rig_layout check that failed: why a caught pose object was not used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LayoutStep {
    /// Fewer than 200 entries read.
    Short,
    /// Entries 0 and 1 not the same point, or the pelvis not 0.2-1.4 m up.
    Pelvis,
    /// Thigh or shin not 0.3-0.55 m, or the left hip not 4-15 cm from the pelvis.
    LegLengths,
    /// No right leg matching the left in entries 7-79.
    RightLeg,
    /// No four-piece spine leading to the left arm.
    Spine,
    /// No twist twins for the left shoulder or elbow.
    Twist,
    /// No right arm the same distance on.
    RightArm,
    /// No head point.
    Head,
}

impl LayoutStep {
    const ALL: [LayoutStep; 8] = [
        LayoutStep::Short,
        LayoutStep::Pelvis,
        LayoutStep::LegLengths,
        LayoutStep::RightLeg,
        LayoutStep::Spine,
        LayoutStep::Twist,
        LayoutStep::RightArm,
        LayoutStep::Head,
    ];

    fn name(self) -> &'static str {
        match self {
            LayoutStep::Short => "short",
            LayoutStep::Pelvis => "pelvis",
            LayoutStep::LegLengths => "leg lengths",
            LayoutStep::RightLeg => "right leg",
            LayoutStep::Spine => "spine",
            LayoutStep::Twist => "twist",
            LayoutStep::RightArm => "right arm",
            LayoutStep::Head => "head",
        }
    }
}

/// What the last harvest made of one ring object.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tried {
    Rig,
    NoList,
    Unreadable,
    Fail(LayoutStep),
}

impl Tried {
    fn text(self) -> String {
        match self {
            Tried::Rig => "a rig".into(),
            Tried::NoList => "no list".into(),
            Tried::Unreadable => "points unreadable".into(),
            Tried::Fail(s) => format!("layout failed at {}", s.name()),
        }
    }
}

// ------------------------------------------------------------------ per harvest

/// What one harvest did, filled in by `harvest_poses`.
#[derive(Default)]
pub(super) struct Harvest {
    pub tried: HashMap<u64, Tried>,
    pub added: u32,
    pub torn: u32,
    pub moved: u32,
}

/// The last harvest's ring objects and what each was.
struct Ring {
    objs: HashSet<u64>,
    tried: HashMap<u64, Tried>,
}
static RING: Mutex<Option<Ring>> = Mutex::new(None);

/// Sums since the last harvest line.
struct HarvestLog {
    since: Instant,
    last_line: Option<Instant>,
    lines: u32,
    passes: u32,
    new: u64,
    added: u32,
    torn: u32,
    moved: u32,
    calls: Option<(u32, Instant)>,
    rate: Option<f64>,
}
static HLOG: Mutex<Option<HarvestLog>> = Mutex::new(None);

/// A harvest line every 10 s; sooner (but not within 5 s) when rigs came or went.
const HARVEST_EVERY: Duration = Duration::from_secs(10);
const HARVEST_SOONEST: Duration = Duration::from_secs(5);
const HARVEST_MAX_LINES: u32 = 1500;
/// Entries in the pose hook's ring.
const RING_LEN: f64 = crate::posehook::RING as f64;

/// After each harvest: keep its ring objects and results, and log a line now and then.
pub(super) fn harvested(objs: &[u64], calls: Option<u32>, h: Harvest, rigs: usize) {
    let now = Instant::now();
    let set: HashSet<u64> = objs.iter().copied().collect();
    let mut fails = [0u32; 8];
    let (mut no_list, mut unreadable) = (0u32, 0u32);
    for t in h.tried.values() {
        match t {
            Tried::Fail(s) => fails[*s as usize] += 1,
            Tried::NoList => no_list += 1,
            Tried::Unreadable => unreadable += 1,
            Tried::Rig => {}
        }
    }
    let distinct = set.len();
    let new = {
        let Ok(mut r) = RING.lock() else { return };
        let new = r.as_ref().map_or(distinct, |r| set.difference(&r.objs).count());
        *r = Some(Ring { objs: set, tried: h.tried });
        new
    };
    let Ok(mut g) = HLOG.lock() else { return };
    let s = g.get_or_insert_with(|| HarvestLog {
        since: now,
        last_line: None,
        lines: 0,
        passes: 0,
        new: 0,
        added: 0,
        torn: 0,
        moved: 0,
        calls: None,
        rate: None,
    });
    // Calls per second from the ring's write count (it only counts up; wraps at 2^32).
    if let (Some(c), Some((c0, t0))) = (calls, s.calls) {
        let dt = now.saturating_duration_since(t0).as_secs_f64();
        let d = c.wrapping_sub(c0);
        if dt > 0.2 && d < 1 << 30 {
            s.rate = Some(f64::from(d) / dt);
        }
    }
    // Hook off: forget the old rate, so the line says "?".
    if calls.is_none() {
        s.rate = None;
    }
    s.calls = calls.map(|c| (c, now));
    s.passes += 1;
    s.new += new as u64;
    s.added += h.added;
    s.torn += h.torn;
    s.moved += h.moved;
    let since_line = s.last_line.map(|t| now.saturating_duration_since(t));
    let changed = s.added + s.torn + s.moved > 0;
    let due = since_line.is_none_or(|d| d >= HARVEST_EVERY || (changed && d >= HARVEST_SOONEST));
    if !due || s.lines > HARVEST_MAX_LINES {
        return;
    }
    s.lines += 1;
    if s.lines > HARVEST_MAX_LINES {
        log("bonetrace: harvest line limit reached, no more harvest lines this session");
        return;
    }
    let rate = s.rate.map_or("? calls/s".to_string(), |r| format!("{r:.0} calls/s (ring covers {:.2} s)", RING_LEN / r.max(1.0)));
    let steps: Vec<String> = LayoutStep::ALL.iter().map(|st| format!("{} {}", st.name(), fails[*st as usize])).collect();
    log(&format!(
        "bonetrace: harvest over {:.0} s ({} passes): ring {rate}, {distinct} distinct, {} new; rigs +{} -{} (torn {}, list changed {}) = {rigs}; last pass: no list {no_list}, unreadable {unreadable}, layout failed at [{}]",
        now.saturating_duration_since(s.since).as_secs_f64(),
        s.passes,
        s.new,
        s.added,
        s.torn + s.moved,
        s.torn,
        s.moved,
        steps.join(", ")
    ));
    s.last_line = Some(now);
    s.since = now;
    (s.passes, s.new, s.added, s.torn, s.moved) = (0, 0, 0, 0, 0);
}

// ------------------------------------------------------------------ line limit

/// At most `per_min` lines a minute and `max` in all.
pub(super) struct Budget {
    per_min: u32,
    max: u32,
    window: Option<Instant>,
    used: u32,
    total: u32,
}

impl Budget {
    pub(super) const fn new(per_min: u32, max: u32) -> Self {
        Budget { per_min, max, window: None, used: 0, total: 0 }
    }

    pub(super) fn allow(&mut self, now: Instant) -> bool {
        if self.window.is_none_or(|w| now.saturating_duration_since(w) >= Duration::from_secs(60)) {
            self.window = Some(now);
            self.used = 0;
        }
        if self.used >= self.per_min || self.total >= self.max {
            return false;
        }
        self.used += 1;
        self.total += 1;
        true
    }
}

// ------------------------------------------------------------------ per character

/// The rig drawn for a body this frame, and when its pelvis, hand or head last changed.
pub(super) struct Pick {
    pub obj: u64,
    pub moved: Instant,
}

enum Kind {
    /// to_bones, first state held 3 s since first seen (not a real switch), why.
    Switch { to_bones: bool, from_new: bool, why: String },
    Frozen { age_ms: u64, moved: f64 },
}

struct Event {
    kind: Kind,
    body: u64,
    tag: u64,
    p: [f64; 3],
    dist: f64,
    speed: f64,
    pick: u64,
}

struct Track {
    bones: bool,
    since: Instant,
    /// Still in the state it had when first seen.
    first: bool,
    new_said: bool,
    seen: Instant,
    anchor: ([f64; 3], Instant),
    speed: f64,
    pick: u64,
    /// The drawn rig's last change, and the body position then.
    moved: Option<(Instant, [f64; 3])>,
    frozen: bool,
    checked: Option<Instant>,
    /// This body's last switch or frozen line.
    last_line: Option<Instant>,
}

impl Track {
    /// May this body have a switch or frozen line now? One per BODY_GAP, so one flickering body
    /// cannot use up the whole minute.
    fn line_ok(&self, now: Instant) -> bool {
        self.last_line.is_none_or(|l| now.saturating_duration_since(l) >= BODY_GAP)
    }
}

struct TraceState {
    tracks: HashMap<u64, Track>,
    events: Vec<Event>,
    budget: Budget,
    /// "Box since first seen" lines have their own small limit, and each body gets at most one per
    /// inject, even when it is forgotten and seen again.
    new_budget: Budget,
    new_told: HashSet<u64>,
}
static TRACE: Mutex<Option<TraceState>> = Mutex::new(None);

/// Switch and frozen lines: about 30 a minute, 3000 a session.
const LINES_PER_MIN: u32 = 30;
const LINES_MAX: u32 = 3000;
/// At most one switch or frozen line per body in this long.
const BODY_GAP: Duration = Duration::from_secs(15);
/// "Box since first seen" lines: 5 a minute, 300 a session.
const NEW_PER_MIN: u32 = 5;
const NEW_MAX: u32 = 300;
const QUEUE_MAX: usize = 64;
/// A body on the box since it was first seen gets one line after this long.
const NEW_BOX_AFTER: Duration = Duration::from_secs(3);
const FROZEN_AGE: Duration = Duration::from_millis(500);
const FROZEN_MOVE: f64 = 0.5;
const FORGET: Duration = Duration::from_secs(5);
const CHECK_EVERY: Duration = Duration::from_millis(500);

/// One character's bones-or-box result this frame (draw thread: no game reads, never waits).
pub(super) fn frame(body: u64, tag: u64, p: [f64; 3], dist: f64, why: Option<&str>, pick: Option<&Pick>) {
    let Ok(mut g) = TRACE.try_lock() else { return };
    let st = g.get_or_insert_with(|| TraceState {
        tracks: HashMap::new(),
        events: Vec::new(),
        budget: Budget::new(LINES_PER_MIN, LINES_MAX),
        new_budget: Budget::new(NEW_PER_MIN, NEW_MAX),
        new_told: HashSet::new(),
    });
    let now = Instant::now();
    let bones = why.is_none();
    let t = st.tracks.entry(body).or_insert_with(|| Track {
        bones,
        since: now,
        first: true,
        new_said: false,
        seen: now,
        anchor: (p, now),
        speed: 0.0,
        pick: 0,
        moved: None,
        frozen: false,
        checked: None,
        last_line: None,
    });
    t.seen = now;
    t.pick = pick.map_or(0, |k| k.obj);
    let dt = now.saturating_duration_since(t.anchor.1).as_secs_f64();
    if dt >= 0.2 {
        t.speed = dist3(p, t.anchor.0) / dt;
        t.anchor = (p, now);
    }
    let mut kinds: [Option<Kind>; 2] = [None, None];
    let mut first_seen: Option<Kind> = None;
    if t.bones != bones {
        SWITCHES.fetch_add(1, Ordering::Relaxed);
        t.bones = bones;
        t.since = now;
        t.first = false;
        let why = why.map_or_else(|| if pick.is_some() { "full rig" } else { "44-bone list" }.to_string(), str::to_string);
        kinds[0] = Some(Kind::Switch { to_bones: bones, from_new: false, why });
    } else if t.first && !bones && !t.new_said && now.saturating_duration_since(t.since) >= NEW_BOX_AFTER {
        t.new_said = true;
        // Already told this inject: say nothing, it is not a new body.
        if !st.new_told.contains(&body) {
            if st.events.len() < QUEUE_MAX && st.new_budget.allow(now) {
                st.new_told.insert(body);
                first_seen = Some(Kind::Switch { to_bones: false, from_new: true, why: why.unwrap_or_default().to_string() });
            } else {
                OVER.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    match pick {
        Some(k) if bones => {
            if t.moved.is_none_or(|(m, _)| m != k.moved) {
                t.moved = Some((k.moved, p));
            }
            let (m, at) = t.moved.unwrap_or((k.moved, p));
            let age = now.saturating_duration_since(m);
            let moved = dist3(p, at);
            if age > FROZEN_AGE && moved > FROZEN_MOVE {
                FROZEN_FRAMES.fetch_add(1, Ordering::Relaxed);
                if !t.frozen {
                    t.frozen = true;
                    FROZEN_NEW.fetch_add(1, Ordering::Relaxed);
                    kinds[1] = Some(Kind::Frozen { age_ms: age.as_millis() as u64, moved });
                }
            } else {
                t.frozen = false;
            }
        }
        _ => {
            t.frozen = false;
            t.moved = None;
        }
    }
    let (speed, drawn) = (t.speed, t.pick);
    for kind in kinds.into_iter().flatten() {
        if t.line_ok(now) && st.events.len() < QUEUE_MAX && st.budget.allow(now) {
            t.last_line = Some(now);
            st.events.push(Event { kind, body, tag, p, dist, speed, pick: drawn });
        } else {
            OVER.fetch_add(1, Ordering::Relaxed);
        }
    }
    if let Some(kind) = first_seen {
        st.events.push(Event { kind, body, tag, p, dist, speed, pick: drawn });
    }
}

/// The game's animation owner of an entity: owner = [[ent+0x90] + u16[[ent+0xA8]+8] * 8];
/// owner+0x10 should be the entity, owner+0x200 its final pose object (the r8 the pose hook notes).
/// Here ent is our body; that it is the same thing is what this checks.
struct AnimPath {
    owner: u64,
    back: bool,
    pose: u64,
}

fn anim_path(ent: u64) -> Option<AnimPath> {
    let owner = anim_owner(&read_into, ent)?;
    Some(AnimPath { owner, back: rd_u64(owner, OWNER_BODY) == Some(ent), pose: rd_u64(owner, OWNER_POSE).filter(|v| is_ptr(*v)).unwrap_or(0) })
}

/// Scan thread, every pass (about 10 a second): pointer path checks for the bodies drawn lately
/// (each every 0.5 s), and the queued switch and frozen lines.
pub(super) fn drain() {
    let now = Instant::now();
    let (events, checks) = {
        let Ok(mut g) = TRACE.lock() else { return };
        let Some(st) = g.as_mut() else { return };
        st.tracks.retain(|_, t| now.saturating_duration_since(t.seen) < FORGET);
        let checks: Vec<(u64, u64)> = st
            .tracks
            .iter_mut()
            .filter(|(_, t)| now.saturating_duration_since(t.seen) < CHECK_EVERY && t.checked.is_none_or(|c| now.saturating_duration_since(c) >= CHECK_EVERY))
            .map(|(b, t)| {
                t.checked = Some(now);
                (*b, t.pick)
            })
            .collect();
        (std::mem::take(&mut st.events), checks)
    };
    if events.is_empty() && checks.is_empty() {
        return;
    }
    let rigs: HashSet<u64> = POSES.lock().map(|p| p.iter().map(|r| r.obj).collect()).unwrap_or_default();
    // The ring is only changed by the harvest, on this same thread.
    let guard = RING.lock().ok();
    let ring = guard.as_ref().and_then(|g| g.as_ref());
    let in_ring = |o: u64| ring.is_some_and(|r| r.objs.contains(&o));
    for (body, pick) in checks {
        PATH[0].fetch_add(1, Ordering::Relaxed);
        let Some(a) = anim_path(body) else { continue };
        let counts = [true, a.back, a.pose != 0 && in_ring(a.pose), a.pose != 0 && rigs.contains(&a.pose), a.pose != 0 && a.pose == pick];
        for (n, c) in counts.into_iter().enumerate() {
            if c {
                PATH[n + 1].fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if events.is_empty() {
        return;
    }
    // Positions of the rigs and the other ring objects, read once for all lines of this pass.
    let pos_of = |o: &u64| Some((*o, rd_pos(*o)?));
    let rig_pos: Vec<(u64, [f64; 3])> = rigs.iter().filter_map(pos_of).collect();
    let other_pos: Vec<(u64, [f64; 3])> = ring.map(|r| r.objs.iter().filter(|o| !rigs.contains(o)).filter_map(pos_of).collect()).unwrap_or_default();
    for e in &events {
        log(&describe(e, &rigs, ring, &rig_pos, &other_pos));
    }
}

/// The nearest of `objs` to `at`, and how far.
fn nearest(objs: &[(u64, [f64; 3])], at: [f64; 3]) -> Option<(u64, f64)> {
    objs.iter().map(|&(o, q)| (o, dist3(q, at))).min_by(|a, b| a.1.total_cmp(&b.1))
}

/// One switch or frozen line, with what the game has for that body right now.
fn describe(e: &Event, rigs: &HashSet<u64>, ring: Option<&Ring>, rig_pos: &[(u64, [f64; 3])], other_pos: &[(u64, [f64; 3])]) -> String {
    let exe = EXE.load(Ordering::Relaxed);
    let in_ring = |o: u64| ring.is_some_and(|r| r.objs.contains(&o));
    let tried = |o: u64| ring.and_then(|r| r.tried.get(&o)).map_or("not tried".to_string(), |t| t.text());
    let head = match &e.kind {
        Kind::Switch { to_bones: true, why, .. } => format!("box -> bones ({why})"),
        Kind::Switch { from_new: true, why, .. } => format!("box since first seen, 3 s ({why})"),
        Kind::Switch { why, .. } => format!("bones -> box ({why})"),
        Kind::Frozen { age_ms, moved } => format!("bones frozen: rig unchanged {age_ms} ms while the body moved {moved:.2} u"),
    };
    let at = e.body.checked_add(POS).and_then(rd_pos).unwrap_or(e.p);
    let near_rig = nearest(rig_pos, at);
    let rig_txt = near_rig.map_or("none".to_string(), |(o, d)| format!("{d:.2} u (0x{o:x}, in ring {})", yn(in_ring(o))));
    // The nearest ring object that is not a rig, within 2 u: what stopped it.
    let other = nearest(other_pos, at).filter(|x| x.1 <= 2.0);
    let other_txt = other.map_or("none within 2 u".to_string(), |(o, d)| format!("{d:.2} u (0x{o:x}, {})", tried(o)));
    let path = anim_path(e.body);
    let path_txt = match &path {
        None => "no owner".to_string(),
        Some(a) if a.pose == 0 => format!("owner 0x{:x} (+0x10 is body: {}), no P", a.owner, yn(a.back)),
        Some(a) => {
            let d = rd_pos(a.pose).map_or("?".to_string(), |q| format!("{:.2} u", dist3(q, at)));
            format!(
                "owner 0x{:x} (+0x10 is body: {}), P 0x{:x} {d} from body, caught {}, a rig {}, {}",
                a.owner,
                yn(a.back),
                a.pose,
                yn(in_ring(a.pose)),
                yn(rigs.contains(&a.pose)),
                tried(a.pose)
            )
        }
    };
    let lvl = rd_u64(e.body, 0xB0).map_or("?".to_string(), |v| format!("{} (bit 4: {})", (v >> 20) & 7, (v >> 4) & 1));
    let b8 = path.as_ref().and_then(|a| rd_u32(a.owner, OWNER_LEVEL)).map_or("?".to_string(), |v| {
        format!("{:#05x} (last {}, now {}, pose lod {})", v & 0x1ff, v & 7, (v >> 3) & 7, (v >> 6) & 7)
    });
    let d118 = rd_f32(e.body, 0x118).map_or("?".to_string(), |v| format!("{v}"));
    let drawn = if e.pick != 0 { format!(", drawn rig 0x{:x} in ring {}", e.pick, yn(in_ring(e.pick))) } else { String::new() };
    format!(
        "bonetrace: 0x{:x} class exe+{:#x} at {:.0} game m: {head}; speed {:.1} u/s{drawn}; nearest rig {rig_txt}; nearest other ring object {other_txt}; path: {path_txt}; level b0 {lvl}, b8 {b8}, [+0x118] {d118}",
        e.body,
        e.tag.wrapping_sub(exe),
        e.dist * GAME_METRES,
        e.speed
    )
}

// ------------------------------------------------------------------ LOD table

/// The scene init that fills the animation level table: it reads the config count, then writes the
/// 6 cut-offs (A, 4 + 1 + 1 floats) and their squares (B, the same). Vulkan: count exe+0x62f6cb8, A
/// exe+0x5353a80, B exe+0x5355418.
pub(super) const LOD_TABLE: &str = "44 8B 05 ?? ?? ?? ?? F3 0F 11 0D ?? ?? ?? ?? F3 0F 10 0D ?? ?? ?? ?? F3 0F 11 0D ?? ?? ?? ?? 0F 11 05 ?? ?? ?? ?? F3 0F 10 05 ?? ?? ?? ?? F3 0F 11 05 ?? ?? ?? ?? 0F 28 05 ?? ?? ?? ?? 0F 11 05 ?? ?? ?? ?? F3 0F 10 05 ?? ?? ?? ?? F3 0F 11 05 ?? ?? ?? ?? 45 85 C0";
const LOD_CODE_LEN: usize = 87;

/// Where the pattern's code points: the config count, table A and table B.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct LodAddrs {
    pub count: u64,
    pub a: u64,
    pub b: u64,
}

/// Read the addresses out of the matched code (at = its address). The other stores must land at
/// A+0x10, A+0x14, B+0x10 and B+0x14, or it is not this code.
pub(super) fn lod_addrs(at: u64, code: &[u8]) -> Option<LodAddrs> {
    if code.len() < LOD_CODE_LEN {
        return None;
    }
    // Each of these instructions ends right after its 4-byte rip offset.
    let rel = |disp: usize| -> Option<u64> {
        let d = i32::from_le_bytes(code[disp..disp + 4].try_into().ok()?);
        Some(at.wrapping_add(disp as u64 + 4).wrapping_add(d as i64 as u64))
    };
    let (count, a, b) = (rel(3)?, rel(34)?, rel(64)?);
    (rel(11)? == a + 0x14 && rel(27)? == b + 0x14 && rel(50)? == a + 0x10 && rel(80)? == b + 0x10).then_some(LodAddrs { count, a, b })
}

static LOD: OnceLock<Option<LodAddrs>> = OnceLock::new();
static LOD_SAID: Mutex<LodLog> = Mutex::new(LodLog::new());

/// At most 20 "lod table" lines per inject, and a changed table at most once in 30 s.
const LOD_MAX_LINES: u32 = 20;
const LOD_GAP: Duration = Duration::from_secs(30);

/// What the LOD logger last said.
pub(super) struct LodLog {
    key: Option<Vec<u8>>,
    last: Option<Instant>,
    lines: u32,
}

#[derive(PartialEq, Eq, Debug)]
pub(super) enum LodSay {
    Nothing,
    /// Log the table; true when it is a change.
    Table(bool),
    /// Log once that the limit is reached.
    Limit,
}

impl LodLog {
    pub(super) const fn new() -> Self {
        LodLog { key: None, last: None, lines: 0 }
    }

    /// Should this table (as bytes) be logged now? A change seen too soon is kept for later: it is
    /// logged once the 30 s are up, if it is still there.
    pub(super) fn say(&mut self, key: &[u8], now: Instant) -> LodSay {
        if self.key.as_deref() == Some(key) || self.lines > LOD_MAX_LINES {
            return LodSay::Nothing;
        }
        if self.last.is_some_and(|l| now.saturating_duration_since(l) < LOD_GAP) {
            return LodSay::Nothing;
        }
        self.lines += 1;
        if self.lines > LOD_MAX_LINES {
            return LodSay::Limit;
        }
        let again = self.key.is_some();
        self.key = Some(key.to_vec());
        self.last = Some(now);
        LodSay::Table(again)
    }
}

fn fmt_f(v: f32) -> String {
    if v.abs() >= 1.0e5 { format!("{v:e}") } else { format!("{v}") }
}

fn fmt_list(v: &[f32], scale: f32) -> String {
    v.iter().map(|x| fmt_f(x * scale)).collect::<Vec<_>>().join(", ")
}

/// Scan thread, each scan pass: log the animation level table once, and again when it changes (a
/// world load can rewrite it), within the LodLog limits. Four small reads.
pub(super) fn lod_table() {
    let Some(exe) = memory::find_module("") else { return };
    let found = *LOD.get_or_init(|| {
        let at = match memory::find_code(&exe, "lod table", LOD_TABLE) {
            Ok(at) => at,
            Err(e) => {
                log(&format!("lod table: not found on this build ({e})"));
                return None;
            }
        };
        let got = memory::read(at, LOD_CODE_LEN).ok().and_then(|c| lod_addrs(at, &c));
        if got.is_none() {
            log("lod table: not found on this build (the pattern's code does not point at one table)");
        }
        got
    });
    let Some(l) = found else { return };
    let a = rd(l.a, 0, 24);
    let b = rd(l.b, 0, 24);
    let n = rd_u32(l.count, 0);
    let cfg = rd(l.count, 4, 24);
    let key: Vec<u8> = match (&a, &b, n, &cfg) {
        (Some(a), Some(b), Some(n), Some(c)) => [a.as_slice(), b.as_slice(), &n.to_le_bytes(), c.as_slice()].concat(),
        _ => Vec::new(),
    };
    let Ok(mut said) = LOD_SAID.lock() else { return };
    let again = match said.say(&key, Instant::now()) {
        LodSay::Nothing => return,
        LodSay::Limit => {
            log("lod table: line limit reached, no more lod table lines this inject");
            return;
        }
        LodSay::Table(again) => if again { " (changed)" } else { "" },
    };
    let (Some(a), Some(b), Some(n), Some(c)) = (a, b, n, cfg) else {
        log(&format!("lod table{again}: exe+{:#x} / exe+{:#x} / exe+{:#x} unreadable", l.a.wrapping_sub(exe.base), l.b.wrapping_sub(exe.base), l.count.wrapping_sub(exe.base)));
        return;
    };
    let (a, b, c) = (floats(&a), floats(&b), floats(&c));
    // The game copies config value i to level 5-i, so the values are for level 5, 4, ... We read 6;
    // a count over 6 is said, not shown.
    let shown = (n as usize).min(c.len());
    let cut = if n as usize > c.len() { format!(" (over {}: only the first {} shown)", c.len(), c.len()) } else { String::new() };
    log(&format!(
        "lod table{again}: level 0-5 cut-offs (exe+{:#x}) [{}] u = [{}] game m; squared (exe+{:#x}) [{}]; config count (exe+{:#x}) {n}{cut}, values for level 5 down [{}]",
        l.a.wrapping_sub(exe.base),
        fmt_list(&a, 1.0),
        fmt_list(&a, GAME_METRES as f32),
        l.b.wrapping_sub(exe.base),
        fmt_list(&b, 1.0),
        l.count.wrapping_sub(exe.base),
        fmt_list(&c[..shown], 1.0)
    ));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn heartbeat_bones_text() {
        // The log line that showed the bug: 'bones 1548/0'.
        assert_eq!(bones_text(1548, 0), "bones drawn/box/total 1548/0/1548");
        assert_eq!(bones_text(18591, 56), "bones drawn/box/total 18591/56/18647");
        assert_eq!(band(29.9), "<45 game m");
        assert_eq!(band(30.0), "45-90 game m");
        assert_eq!(band(59.9), "45-90 game m");
        assert_eq!(band(60.0), "90+ game m");
        assert_eq!(tail_text([0; 6], 0, 0, 0, 0), "");
        assert_eq!(
            tail_text([40, 38, 38, 30, 25, 22], 120, 2, 5, 1),
            ", path checks 40 (owner 38, +0x10 is body 38, P caught 30, P a rig 25, P drawn 22), frozen bones 120 frames (2 new), switches 5 (1 lines over the limit)"
        );
    }

    #[test]
    fn budget_limits_lines() {
        let t0 = Instant::now();
        let mut b = Budget::new(30, 50);
        assert!((0..30).all(|_| b.allow(t0)));
        assert!(!b.allow(t0));
        assert!(!b.allow(t0 + Duration::from_secs(59)));
        // A new minute: 20 more, then the session cap.
        let t1 = t0 + Duration::from_secs(60);
        assert!((0..20).all(|_| b.allow(t1)));
        assert!(!b.allow(t1));
        assert!(!b.allow(t1 + Duration::from_secs(120)));
    }

    #[test]
    fn lod_lines_are_limited() {
        let t0 = Instant::now();
        let s = |n: u32| Duration::from_secs(u64::from(n));
        let mut l = LodLog::new();
        assert_eq!(l.say(b"a", t0), LodSay::Table(false));
        assert_eq!(l.say(b"a", t0 + s(100)), LodSay::Nothing);
        // A change within 30 s waits, then is logged.
        assert_eq!(l.say(b"b", t0 + s(10)), LodSay::Nothing);
        assert_eq!(l.say(b"b", t0 + s(30)), LodSay::Table(true));
        // 20 lines in all, then one limit line, then nothing.
        let mut t = t0 + s(30);
        for i in 3..=LOD_MAX_LINES {
            t += s(30);
            assert_eq!(l.say(&i.to_le_bytes(), t), LodSay::Table(true));
        }
        assert_eq!(l.say(b"x", t + s(30)), LodSay::Limit);
        assert_eq!(l.say(b"y", t + s(90)), LodSay::Nothing);
    }

    /// The Vulkan exe's bytes at rva 0x11752a3.
    const VULKAN_CODE: &str = "44 8B 05 0E 1A 18 05 F3 0F 11 0D E2 E7 1D 04 F3 0F 10 0D 7A 4A 55 03 F3 0F 11 0D 6A 01 1E 04 0F 11 05 B7 E7 1D 04 F3 0F 10 05 D7 6F 41 03 F3 0F 11 05 B7 E7 1D 04 0F 28 05 E0 4A 55 03 0F 11 05 31 01 1E 04 F3 0F 10 05 A5 29 4F 03 F3 0F 11 05 31 01 1E 04 45 85 C0";

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|h| u8::from_str_radix(h, 16).unwrap()).collect()
    }

    #[test]
    fn lod_addrs_from_code() {
        let code = hex(VULKAN_CODE);
        assert_eq!(code.len(), LOD_CODE_LEN);
        assert_eq!(LOD_TABLE.split_whitespace().count(), LOD_CODE_LEN);
        assert_eq!(lod_addrs(0x11752a3, &code), Some(LodAddrs { count: 0x62f6cb8, a: 0x5353a80, b: 0x5355418 }));
        // The same code anywhere else points at the same places relative to it.
        let base = 0x7ff6_0000_0000u64;
        assert_eq!(lod_addrs(base + 0x11752a3, &code), Some(LodAddrs { count: base + 0x62f6cb8, a: base + 0x5353a80, b: base + 0x5355418 }));
        // A store that misses the table: not this code.
        let mut bad = code.clone();
        bad[11] ^= 0x10;
        assert_eq!(lod_addrs(0x11752a3, &bad), None);
        assert_eq!(lod_addrs(0x11752a3, &code[..80]), None);
    }

    /// A made-up rig that passes every rig_layout step; the rest are far apart fillers. The P
    /// path's tests use it too.
    pub(crate) fn rig() -> Vec<[f32; 3]> {
        let mut t: Vec<[f32; 3]> = (0..200).map(|i| [100.0 + 3.0 * i as f32, 50.0, 50.0]).collect();
        t[0] = [0.0, 0.0, 1.0];
        t[1] = [0.0, 0.0, 1.0];
        // Left leg 2-4, right leg 7-9.
        t[2] = [0.1, 0.0, 1.0];
        t[3] = [0.1, 0.0, 0.55];
        t[4] = [0.1, 0.0, 0.1];
        t[7] = [-0.1, 0.0, 1.0];
        t[8] = [-0.1, 0.0, 0.55];
        t[9] = [-0.1, 0.0, 0.1];
        // Spine 12-15, left arm 16-18, its twins 19 20, head 21.
        t[12] = [0.0, 0.0, 1.1];
        t[13] = [0.0, 0.0, 1.25];
        t[14] = [0.0, 0.0, 1.4];
        t[15] = [0.0, 0.0, 1.55];
        t[16] = [0.15, 0.0, 1.55];
        t[17] = [0.15, 0.0, 1.25];
        t[18] = [0.15, 0.0, 0.98];
        t[19] = t[16];
        t[20] = t[17];
        t[21] = [0.0, 0.0, 1.85];
        // Right arm 50 on: 66-68, twins 69 70.
        t[66] = [-0.15, 0.0, 1.55];
        t[67] = [-0.15, 0.0, 1.25];
        t[68] = [-0.15, 0.0, 0.98];
        t[69] = t[66];
        t[70] = t[67];
        t
    }

    #[test]
    fn rig_layout_names_the_failed_step() {
        use super::super::rig_layout;
        assert_eq!(rig_layout(&rig()), Ok([1, 4, 9, 18, 68, 3, 8, 17, 67, 13, 15, 6, 11, 16, 66, 21, 2, 7]));
        let broken = |f: &dyn Fn(&mut Vec<[f32; 3]>)| {
            let mut t = rig();
            f(&mut t);
            rig_layout(&t)
        };
        assert_eq!(broken(&|t| t.truncate(150)), Err(LayoutStep::Short));
        assert_eq!(broken(&|t| t[1][2] = 2.0), Err(LayoutStep::Pelvis));
        assert_eq!(broken(&|t| t[4] = [0.1, 0.0, -0.5]), Err(LayoutStep::LegLengths));
        assert_eq!(broken(&|t| t[9] = [-0.1, 0.0, -0.5]), Err(LayoutStep::RightLeg));
        assert_eq!(broken(&|t| t[14] = [0.0, 0.0, 1.3]), Err(LayoutStep::Spine));
        assert_eq!(broken(&|t| t[19] = [400.0, 0.0, 0.0]), Err(LayoutStep::Twist));
        assert_eq!(broken(&|t| t[69] = [500.0, 1.0, 1.0]), Err(LayoutStep::RightArm));
        assert_eq!(broken(&|t| t[21] = [0.0, 0.0, 1.6]), Err(LayoutStep::Head));
    }
}
