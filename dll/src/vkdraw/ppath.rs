//! P path: bones from each character's own current pose.
//!
//! The game's animation owner of a body holds its final pose object P at +0x200. P's list is the
//! full rig, the same kind of list the pose hook catches. So every frame P is read straight from
//! the owner: no waiting for the hook, no matching by position. The old route (`skeleton`,
//! `rig_skeleton` and the pose hook) stays as the fallback.
//!
//! Live facts:
//! - owner = [[body+0x90] + u16[[body+0xA8]+8] * 8], and [owner+0x10] is the body (15/15).
//! - P = [owner+0x200] (15/15). P+0 holds the body's position (3 f64).
//! - Each owner goes round the same 3 pose objects about 4 times a second, so P is read again every
//!   frame. The 3 share one skeleton S = [P+0x240]; each has its own list L = [P+0x248] (0x20 per
//!   entry, x y z f32 at +0, body space).
//! - Level = (u16 [owner+0xB8] >> 3) & 7: 1-5 is the detail level (5 nearest). 0 was seen on 4
//!   bodies whose lists never changed (not animated).
//!
//! Reads only, through `safe_read`. What it does is counted on the heartbeat line.
//!
//! Bones by name. S has a bone table: the name of every bone in the list, as a CRC32. So the 18
//! bones are looked up by name once per skeleton (`BONES_BY_NAME`), not guessed by lengths. Live,
//! read only (8 bodies in the hub, levels 0 to 5, 8 reads each): the game moves the named bones at
//! every level, but face, finger and head-top bones only on near bodies. At level 3 they were 7 to
//! 29 cm off on 3 of 3 bodies, and `rig_layout`'s head point is one of them: the head line and ring
//! sat beside the head. By name, the head point is the top of the head worked out from the Head
//! bone, which the game moves at every level.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{
    BONE_COUNT, BONES_BY_NAME, CHEST, HEAD, HEAD_RING_FROM_HEAD, LIMB_LINES, P_PATH, POSE_LIST, POSE_NEAR, RIG_BAD_MAX, RIG_READ, RIG_SIZE,
    RIG_STRETCH, bonetrace, is_ptr, line_lens, place_with, rig_layout, rig_local_with,
};
use crate::entities::safe_read;

/// In the body: the owner table, and a slot whose u16 at +8 is the body's place in it.
const BODY_TABLE: u64 = 0x90;
const BODY_SLOT: u64 = 0xA8;
const SLOT_INDEX: u64 = 8;
/// In the owner: the body, the level word, the final pose P.
pub(super) const OWNER_BODY: u64 = 0x10;
pub(super) const OWNER_LEVEL: u64 = 0xB8;
pub(super) const OWNER_POSE: u64 = 0x200;
/// In P: the skeleton S. (P+0 is its position, `POSE_LIST` its list.)
const POSE_SKEL: u64 = 0x240;
/// In S: the bone count, a u16.
const SKEL_COUNT: u64 = 0x62;
/// In S: its bone table, one entry per list entry, in the same order: +0 the parent's entry (u16,
/// 0xFFFF for the root), +0xC the CRC32 of the bone's name.
const SKEL_TABLE: u64 = 0x70;
const TABLE_ENTRY: usize = 16;
const ENTRY_HASH: usize = 0xC;
/// A bone count over this: the table is not read (the most seen was 596).
const TABLE_MAX: usize = 2048;
/// How far up its parents a bone's named parent may be. It was the direct parent on all 14.
const UP_STEPS: usize = 4;

/// The 18 bones by name, in `BONE_LINES` order (pelvis, ankles, hands, knees, elbows, spine, chest,
/// toes, shoulders, head, hips): the bones `rig_layout` picks. The toes are the toe bones (it takes
/// the bone after them; toes are not drawn). The head is the Head bone: the head point is worked
/// out from it (`HEAD_TOP`).
const SLOT_NAMES: [&str; BONE_COUNT] = [
    "Hips", "LeftFoot", "RightFoot", "LeftHand", "RightHand", "LeftLeg", "RightLeg", "LeftForeArm", "RightForeArm", "Spine1", "LeftShoulder",
    "LeftToeBase", "RightToeBase", "LeftArm", "RightArm", "Head", "LeftUpLeg", "RightUpLeg",
];
/// The bone above each, as on all 14 skeletons. A table where one is not above it is not trusted.
const ABOVE_NAMES: [&str; BONE_COUNT] = [
    "Reference", "LeftLeg", "RightLeg", "LeftForeArm", "RightForeArm", "LeftUpLeg", "RightUpLeg", "LeftArm", "RightArm", "Spine", "Spine2",
    "LeftFoot", "RightFoot", "LeftShoulder", "RightShoulder", "Neck", "Hips", "Hips",
];
const SLOT_HASHES: [u32; BONE_COUNT] = hashes(SLOT_NAMES);
const ABOVE_HASHES: [u32; BONE_COUNT] = hashes(ABOVE_NAMES);

/// The Head bone's own frame: x up the head, y the way the face looks, z to the left, in metres
/// from the Head joint. The top of the head is 0.14 straight up: a bone sits there (0.1402 up on
/// all 11 rigs that have it, 3 had none), but the game left it behind on two players at level 3 (12
/// to 25 cm off), so it is worked out instead.
const HEAD_TOP: [f64; 3] = [0.14, 0.0, 0.0];
/// The middle of the head, for the ring (`HEAD_RING_FROM_HEAD`), in the Head bone's frame: eye
/// height (the eyes were 0.072 to 0.078 up on all 14 skeletons), 2 cm forward, between the back of
/// the skull (0.099 back) and the forehead (0.107 forward).
const HEAD_MIDDLE: [f64; 3] = [0.07, 0.02, 0.0];
/// The middle of the head is never farther than this from the chest (0.22 to 0.24 m on 12 bodies);
/// farther, the ring is placed as before.
const RING_NEAR_CHEST: f64 = 0.4;

/// Zlib's CRC32 of a name: the game's bone name hash (checked against 23 names it uses).
const fn crc32(s: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    let mut i = 0;
    while i < s.len() {
        c ^= s[i] as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            k += 1;
        }
        i += 1;
    }
    !c
}

const fn hashes(names: [&str; BONE_COUNT]) -> [u32; BONE_COUNT] {
    let mut out = [0; BONE_COUNT];
    let mut i = 0;
    while i < BONE_COUNT {
        out[i] = crc32(names[i].as_bytes());
        i += 1;
    }
    out
}

/// Why a skeleton's bones were not found by name (then `rig_layout` finds them by lengths).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameMiss {
    /// The name is not in the table.
    Missing(&'static str),
    /// The name is in it twice.
    Twice(&'static str),
    /// The bone that must be above it is not among its parents.
    Parent(&'static str),
}

/// The one entry of bone table `table` named `name` (hash `hash`).
fn entry_named(table: &[u8], hash: u32, name: &'static str) -> Result<usize, NameMiss> {
    let mut found = None;
    for (i, e) in table.as_chunks::<TABLE_ENTRY>().0.iter().enumerate() {
        if u32::from_le_bytes(e[ENTRY_HASH..ENTRY_HASH + 4].try_into().unwrap()) == hash {
            if found.is_some() {
                return Err(NameMiss::Twice(name));
            }
            found = Some(i);
        }
    }
    found.ok_or(NameMiss::Missing(name))
}

/// Bone `k` of the 18 in `table`, checked against the bone that must be above it.
fn slot_named(table: &[u8], k: usize) -> Result<usize, NameMiss> {
    let i = entry_named(table, SLOT_HASHES[k], SLOT_NAMES[k])?;
    let above = entry_named(table, ABOVE_HASHES[k], ABOVE_NAMES[k])?;
    let mut x = i;
    for _ in 0..UP_STEPS {
        // The root's parent (0xFFFF) is past the table: the walk ends there.
        let at = x * TABLE_ENTRY;
        x = match table.get(at..at + 2) {
            Some(p) => usize::from(u16::from_le_bytes([p[0], p[1]])),
            None => break,
        };
        if x == above {
            return Ok(i);
        }
    }
    Err(NameMiss::Parent(SLOT_NAMES[k]))
}

/// The 18 bones of a skeleton by their names, from its bone table (`BONE_LINES` order; the head
/// slot is the Head bone).
fn bones_by_name(table: &[u8]) -> Result<[usize; BONE_COUNT], NameMiss> {
    let mut idx = [0; BONE_COUNT];
    for (k, i) in idx.iter_mut().enumerate() {
        *i = slot_named(table, k)?;
    }
    Ok(idx)
}

/// Skeleton `skel`'s bone table, `count` entries, read into `buf`. None when the count looks wrong
/// or the table can't be read.
fn read_table<'a>(read: &dyn Fn(u64, &mut [u8]) -> usize, skel: u64, count: u16, buf: &'a mut Vec<u8>) -> Option<&'a [u8]> {
    let n = usize::from(count);
    if !(1..=TABLE_MAX).contains(&n) {
        return None;
    }
    let table = u64_at(read, skel.checked_add(SKEL_TABLE)?).filter(|v| is_ptr(*v))?;
    buf.resize(n * TABLE_ENTRY, 0);
    (read(table, buf) == buf.len()).then_some(&buf[..])
}

/// A bone's place and turn in body space: its position and its rotation (a quaternion, x y z w), as
/// the list holds them (+0 and +0x10 of its entry).
#[derive(Clone, Copy, Debug, PartialEq)]
struct BonePose {
    at: [f64; 3],
    q: [f64; 4],
}

impl BonePose {
    /// A point given in the bone's own frame, in body space.
    fn point(&self, local: [f64; 3]) -> [f64; 3] {
        let r = rotate(self.q, local);
        [0, 1, 2].map(|k| self.at[k] + r[k])
    }
}

/// Entry `i` of list `list` with its turn. None when it can't be read, is not finite, or the turn's
/// length is not 1 (it was 1.0000 on all 14 skeletons).
fn bone_pose(read: &dyn Fn(u64, &mut [u8]) -> usize, list: u64, i: usize) -> Option<BonePose> {
    let mut b = [0u8; RIG_SIZE];
    if read(list.checked_add((i * RIG_SIZE) as u64)?, &mut b) != RIG_SIZE {
        return None;
    }
    let f = |o: usize| f64::from(f32::from_le_bytes(b[o..o + 4].try_into().unwrap()));
    let (at, q) = ([f(0), f(4), f(8)], [f(0x10), f(0x14), f(0x18), f(0x1C)]);
    let len2: f64 = q.iter().map(|v| v * v).sum();
    (at.iter().chain(&q).all(|v| v.is_finite()) && (0.98..1.02).contains(&len2)).then_some(BonePose { at, q })
}

/// The ring's centre from the Head bone: HEAD_MIDDLE in its frame, in body space. None when that is
/// not finite or farther than RING_NEAR_CHEST from the chest `chest`.
fn ring_centre(head: &BonePose, chest: [f64; 3]) -> Option<[f64; 3]> {
    let c = head.point(HEAD_MIDDLE);
    let d = (0..3).map(|k| (c[k] - chest[k]).powi(2)).sum::<f64>().sqrt();
    // Written this way so a NaN fails too.
    (d <= RING_NEAR_CHEST).then_some(c)
}

/// `v` turned by the unit quaternion `q` (x y z w): v + 2w (u x v) + 2 u x (u x v).
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let u = [q[0], q[1], q[2]];
    let c = cross(u, v);
    let c2 = cross(u, c);
    [0, 1, 2].map(|k| v[k] + 2.0 * (q[3] * c[k] + c2[k]))
}

/// Level 0 this many frames in a row: the P path is skipped for that body (the old route draws,
/// usually the box) until its level is back.
const LEVEL0_FRAMES: u32 = 10;
/// A failed layout is tried again no sooner than this, from a fresh frame (a frame read while the
/// game writes it can fail).
const LAYOUT_RETRY: Duration = Duration::from_secs(1);
/// More skeletons than this: the cache starts again.
const LAYOUTS_MAX: usize = 512;
/// Bodies not asked for in this long are forgotten (looked at every 2 s).
const FORGET: Duration = Duration::from_secs(5);
const PRUNE_EVERY: Duration = Duration::from_secs(2);

fn u64_at(read: &dyn Fn(u64, &mut [u8]) -> usize, at: u64) -> Option<u64> {
    let mut b = [0u8; 8];
    (read(at, &mut b) == 8).then(|| u64::from_le_bytes(b))
}

/// The game's animation owner of a body: [[body+0x90] + u16[[body+0xA8]+8] * 8]. None when a step
/// is missing. `read` is the game's memory. The bone trace uses this too.
pub(super) fn anim_owner(read: &dyn Fn(u64, &mut [u8]) -> usize, body: u64) -> Option<u64> {
    let table = u64_at(read, body.checked_add(BODY_TABLE)?).filter(|v| is_ptr(*v))?;
    let slot = u64_at(read, body.checked_add(BODY_SLOT)?).filter(|v| is_ptr(*v))?;
    let mut i = [0u8; 2];
    if read(slot + SLOT_INDEX, &mut i) != 2 {
        return None;
    }
    let idx = u16::from_le_bytes(i);
    if idx == 0xFFFF {
        return None;
    }
    u64_at(read, table + u64::from(idx) * 8).filter(|v| is_ptr(*v))
}

/// A worked-out skeleton: which entry is each of the 18 bones, and the limb lengths in the frame it
/// came from (each body's first lengths for the torn-frame check).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    idx: [usize; BONE_COUNT],
    lens: [f64; LIMB_LINES.len()],
    /// Found by name: the head slot is the Head bone, and the head point is the top of the head
    /// worked out from it. False: found by lengths (`rig_layout`), as before.
    by_name: bool,
    /// The Head bone's entry, found by name (also when laid out by lengths), for the head point and
    /// the ring's centre. None: not found (the ring is placed as before).
    head: Option<usize>,
}

/// One frame's bones in body space, and the ring's centre from the Head bone (None: placed as
/// before).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    pts: [[f64; 3]; BONE_COUNT],
    ring: Option<[f64; 3]>,
}

/// One skeleton's layout (None: the last try failed), its bone count word then, and when.
struct Cached {
    layout: Option<Layout>,
    count: u16,
    at: Instant,
}

/// Layouts by skeleton pointer S: a body's 3 pose objects share one S.
#[derive(Default)]
struct Layouts(HashMap<u64, Cached>);

impl Layouts {
    /// The layout of skeleton `skel`, whose count word reads `count`: the cached one, else `work`
    /// works it out from a fresh frame. A failed try, or a new count (another skeleton at the same
    /// address), is tried again no sooner than LAYOUT_RETRY after the last try.
    fn get(&mut self, skel: u64, count: u16, now: Instant, work: impl FnOnce() -> Option<Layout>) -> Option<Layout> {
        if let Some(c) = self.0.get(&skel) {
            if c.layout.is_some() && c.count == count {
                return c.layout;
            }
            if now.saturating_duration_since(c.at) < LAYOUT_RETRY {
                return None;
            }
        }
        // Full: start again. A skeleton already in it is only tried again, so it stays.
        if self.0.len() >= LAYOUTS_MAX && !self.0.contains_key(&skel) {
            self.0.clear();
        }
        let layout = work();
        self.0.insert(skel, Cached { layout, count, at: now });
        layout
    }

    /// Start a skeleton again: its frames kept failing the stretch check, so a bad frame may have
    /// made its layout. The next ask works it out at once.
    fn forget(&mut self, skel: u64) {
        self.0.remove(&skel);
    }
}

/// Work out a layout for skeleton `skel` (`count` bones) with list `list` as it is now. By name
/// first (`BONES_BY_NAME`): its bone table, read into `tab`. Else, or when a name is missing or the
/// table looks wrong, by lengths as before: the list's first `count` entries when that looks right
/// (1 to 640), else 640 like the hook route, never past the count. The Head bone is looked up by
/// name either way (`HEAD_RING_FROM_HEAD`). `raw`, `pts` and `tab` are kept between calls, so no
/// new memory each time.
fn work(
    read: &dyn Fn(u64, &mut [u8]) -> usize,
    skel: u64,
    list: u64,
    count: u16,
    raw: &mut Vec<u8>,
    pts: &mut Vec<[f32; 3]>,
    tab: &mut Vec<u8>,
) -> Option<Layout> {
    let table = if BONES_BY_NAME || HEAD_RING_FROM_HEAD { read_table(read, skel, count, tab) } else { None };
    let head = table.and_then(|t| slot_named(t, HEAD).ok());
    if BONES_BY_NAME
        && let Some(idx) = table.and_then(|t| bones_by_name(t).ok())
    {
        let lay = Layout { idx, lens: [0.0; LIMB_LINES.len()], by_name: true, head };
        // The limb lengths from this frame. Not readable (the Head's turn too): by lengths.
        if let Some(f) = read_frame(read, list, &lay) {
            MORE[0].fetch_add(1, Ordering::Relaxed);
            return Some(Layout { lens: line_lens(&f.pts), ..lay });
        }
    }
    let n = if (1..=RIG_READ).contains(&usize::from(count)) { usize::from(count) } else { RIG_READ };
    raw.resize(n * RIG_SIZE, 0);
    let got = read(list, raw) / RIG_SIZE;
    pts.clear();
    pts.extend(raw[..got * RIG_SIZE].chunks_exact(RIG_SIZE).map(|e| [0, 4, 8].map(|k| f32::from_le_bytes(e[k..k + 4].try_into().unwrap()))));
    let idx = rig_layout(pts).ok()?;
    // The limb lengths from this same frame.
    let mut local = [[0.0; 3]; BONE_COUNT];
    for (l, &i) in local.iter_mut().zip(&idx) {
        *l = pts.get(i)?.map(f64::from);
    }
    MORE[1].fetch_add(1, Ordering::Relaxed);
    Some(Layout { idx, lens: line_lens(&local), by_name: false, head })
}

/// The 18 bones of list `list` as read now, in body space, and the ring's centre. Laid out by name,
/// the head point is the top of the head from the Head bone's place and turn (None when that can't
/// be read). The ring's centre is the middle of the head from the same.
fn read_frame(read: &dyn Fn(u64, &mut [u8]) -> usize, list: u64, lay: &Layout) -> Option<Frame> {
    let mut pts = rig_local_with(read, list, &lay.idx)?;
    let head = lay.head.and_then(|h| bone_pose(read, list, h));
    if lay.by_name {
        pts[HEAD] = head?.point(HEAD_TOP);
    }
    let ring = if HEAD_RING_FROM_HEAD { head.and_then(|h| ring_centre(&h, pts[CHEST])) } else { None };
    Some(Frame { pts, ring })
}

/// Since the last heartbeat: skeletons laid out by name, by lengths, and frames drawn with the
/// ring's centre from the Head bone.
static MORE: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

/// What the P path keeps per body.
struct Body {
    /// The owner and skeleton this belongs to: another owner starts it afresh.
    owner: u64,
    skel: u64,
    /// Frames in a row its level read 0.
    level0: u32,
    /// The limb lengths its frames must keep: its layout's at first. After a torn spell they are
    /// taken from its own frames (`retake`): two bodies can share a skeleton and still be built
    /// differently. A frame's lengths are only a candidate (`cand`) until the next frame has the
    /// same: a torn frame always matches itself.
    lens: Option<[f64; LIMB_LINES.len()]>,
    retake: bool,
    cand: Option<[f64; LIMB_LINES.len()]>,
    /// The last frame that passed (body space), drawn instead of a torn one, and torn frames in a
    /// row.
    good: Option<Frame>,
    bad: u32,
    /// The last frame read, and when the pose last changed (for the bone trace).
    last: [[f64; 3]; BONE_COUNT],
    moved: Instant,
    /// When this body was last asked for.
    seen: Instant,
}

impl Body {
    fn new(owner: u64, now: Instant) -> Self {
        Body { owner, skel: 0, level0: 0, lens: None, retake: false, cand: None, good: None, bad: 0, last: [[0.0; 3]; BONE_COUNT], moved: now, seen: now }
    }
}

#[derive(Default)]
struct State {
    layouts: Layouts,
    bodies: HashMap<u64, Body>,
    /// Kept for working out layouts.
    raw: Vec<u8>,
    pts: Vec<[f32; 3]>,
    table: Vec<u8>,
    pruned: Option<Instant>,
}

impl State {
    /// Forget bodies not asked for lately, and skeletons no body has now (every PRUNE_EVERY at
    /// most).
    fn prune(&mut self, now: Instant) {
        if self.pruned.is_none_or(|t| now.saturating_duration_since(t) >= PRUNE_EVERY) {
            self.bodies.retain(|_, b| now.saturating_duration_since(b.seen) < FORGET);
            let bodies = &self.bodies;
            self.layouts.0.retain(|s, _| bodies.values().any(|b| b.skel == *s));
            self.pruned = Some(now);
        }
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Why the P path gave nothing for a body this frame (in the heartbeat's order).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Miss {
    /// No owner found at the last scan.
    NoOwner,
    /// [owner+0x10] is not the body, or the owner can't be read.
    NotBody,
    /// No P, P can't be read, or P is more than POSE_NEAR from the body.
    PFar,
    /// No skeleton or list, or no layout (yet).
    NoLayout,
    /// The bones stretched, and no good frame from the last RIG_BAD_MAX frames; or new lengths not
    /// yet seen in two frames in a row.
    Torn,
    /// Level 0 for LEVEL0_FRAMES frames in a row.
    Level0,
    /// `place` said no (facing, a bone too far, pelvis height).
    BadPose,
}

/// One level read for a body: true when the P path is skipped for it now.
fn level0_skip(zeros: &mut u32, level: u16) -> bool {
    *zeros = if level == 0 { zeros.saturating_add(1) } else { 0 };
    *zeros >= LEVEL0_FRAMES
}

/// Limb lengths within RIG_STRETCH of each other: the same bones (a NaN never matches).
fn same_lens(a: &[f64; LIMB_LINES.len()], b: &[f64; LIMB_LINES.len()]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < RIG_STRETCH)
}

/// A frame's bones in body space, the ring's centre from the Head bone (None: placed as before),
/// the pose object they came from, and when it last changed.
#[derive(Debug)]
struct Got {
    local: [[f64; 3]; BONE_COUNT],
    ring: Option<[f64; 3]>,
    obj: u64,
    moved: Instant,
}

/// One body's bones this frame from its own current pose. `owner` is its animation owner from the
/// scan (0: none), `p` its position read this frame, `read` the game's memory.
fn body_frame(st: &mut State, read: &dyn Fn(u64, &mut [u8]) -> usize, owner: u64, body: u64, p: [f64; 3], now: Instant) -> Result<Got, Miss> {
    if !is_ptr(owner) {
        return Err(Miss::NoOwner);
    }
    // The owner's body, level word and P, in one read (owner+0x10 to +0x208).
    let mut o = [0u8; (OWNER_POSE + 8 - OWNER_BODY) as usize];
    if read(owner + OWNER_BODY, &mut o) != o.len() {
        return Err(Miss::NotBody);
    }
    let field = |off: u64, n: usize| {
        let i = (off - OWNER_BODY) as usize;
        &o[i..i + n]
    };
    if u64::from_le_bytes(field(OWNER_BODY, 8).try_into().unwrap()) != body {
        return Err(Miss::NotBody);
    }
    let level = (u16::from_le_bytes(field(OWNER_LEVEL, 2).try_into().unwrap()) >> 3) & 7;
    let pose = u64::from_le_bytes(field(OWNER_POSE, 8).try_into().unwrap());
    let b = st.bodies.entry(body).or_insert_with(|| Body::new(owner, now));
    if b.owner != owner {
        *b = Body::new(owner, now);
    }
    b.seen = now;
    if level0_skip(&mut b.level0, level) {
        return Err(Miss::Level0);
    }
    if !is_ptr(pose) {
        return Err(Miss::PFar);
    }
    // P's position, skeleton and list, in one read (P+0 to +0x250).
    let mut q = [0u8; (POSE_LIST + 8) as usize];
    if read(pose, &mut q) != q.len() {
        return Err(Miss::PFar);
    }
    let word = |at: u64| u64::from_le_bytes(q[at as usize..at as usize + 8].try_into().unwrap());
    let off = (0..3).map(|k| (f64::from_bits(word(8 * k as u64)) - p[k]).powi(2)).sum::<f64>().sqrt();
    // Written this way so a NaN fails too.
    if !(off <= POSE_NEAR) {
        return Err(Miss::PFar);
    }
    let (skel, list) = (word(POSE_SKEL), word(POSE_LIST));
    let mut c = [0u8; 2];
    if !is_ptr(skel) || !is_ptr(list) || read(skel + SKEL_COUNT, &mut c) != 2 {
        return Err(Miss::NoLayout);
    }
    let count = u16::from_le_bytes(c);
    if b.skel != skel {
        // Another skeleton: the old frames and lengths are not its own.
        b.skel = skel;
        b.lens = None;
        b.retake = false;
        b.cand = None;
        b.good = None;
        b.bad = 0;
    }
    let (raw, pts, table) = (&mut st.raw, &mut st.pts, &mut st.table);
    let lay = st.layouts.get(skel, count, now, || work(read, skel, list, count, raw, pts, table)).ok_or(Miss::NoLayout)?;
    let now_frame = read_frame(read, list, &lay);
    if b.retake {
        // New lengths only when two frames in a row agree. Not drawn until then: taken at once, a
        // torn frame would pass against itself.
        let got = now_frame.as_ref().map(|f| line_lens(&f.pts));
        match (got, b.cand) {
            (Some(g), Some(c)) if same_lens(&g, &c) => {
                b.lens = Some(c);
                b.retake = false;
                b.cand = None;
            }
            _ => {
                b.cand = got;
                return Err(Miss::Torn);
            }
        }
    }
    let lens = *b.lens.get_or_insert(lay.lens);
    // The same check as the hook route: bones never stretch, so a frame whose limb lengths differ
    // was read while the game wrote it.
    let fresh = now_frame.filter(|f| same_lens(&line_lens(&f.pts), &lens));
    let frame = match fresh {
        Some(f) => {
            if f.pts != b.last {
                b.last = f.pts;
                b.moved = now;
            }
            b.good = Some(f);
            b.bad = 0;
            f
        }
        None => {
            b.bad += 1;
            match b.good {
                Some(g) if b.bad <= RIG_BAD_MAX => g,
                _ => {
                    if b.bad > RIG_BAD_MAX {
                        // Torn too long: work the layout out again from a fresh frame, and take
                        // this body's lengths again from its own frames.
                        st.layouts.forget(skel);
                        b.retake = true;
                        b.good = None;
                        b.bad = 0;
                    }
                    return Err(Miss::Torn);
                }
            }
        }
    };
    Ok(Got { local: frame.pts, ring: frame.ring, obj: pose, moved: b.moved })
}

/// Bodies drawn from their own pose, then each Miss in order, since the last heartbeat.
static COUNTS: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

/// The P path for one body this frame: its bones in the world, or None (the reason is counted) and
/// the old route is next. `pick` gets the pose object drawn, for the bone trace; `mid` the ring's
/// centre in the world from the Head bone (None: the ring is placed as before).
pub(super) fn skeleton(
    owner: u64,
    body: u64,
    p: [f64; 3],
    pick: &mut Option<bonetrace::Pick>,
    mid: &mut Option<[f64; 3]>,
) -> Option<[[f64; 3]; BONE_COUNT]> {
    if !P_PATH {
        return None;
    }
    let now = Instant::now();
    let got = {
        let mut g = STATE.lock().ok()?;
        let st = g.get_or_insert_with(State::default);
        st.prune(now);
        body_frame(st, &safe_read, owner, body, p, now)
    };
    // The same last step as the old route: turn, move, pelvis height check. The ring's centre is
    // turned and moved with the bones.
    let placed = got.and_then(|g| place_with(body, p, &g.local, g.ring).map(|w| (w, g)).map_err(|_| Miss::BadPose));
    COUNTS[placed.as_ref().map_or_else(|m| *m as usize + 1, |_| 0)].fetch_add(1, Ordering::Relaxed);
    let ((w, ring), g) = placed.ok()?;
    *pick = Some(bonetrace::Pick { obj: g.obj, moved: g.moved });
    if ring.is_some() {
        MORE[2].fetch_add(1, Ordering::Relaxed);
    }
    *mid = ring;
    Some(w)
}

/// The P path's bones first; today's route only when the P path gave none.
pub(super) fn or_old_route<T, E>(p_path: Option<T>, old: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    match p_path {
        Some(v) => Ok(v),
        None => old(),
    }
}

/// Forget every body and skeleton (a jump: the old place's characters are gone).
pub(super) fn forget() {
    if let Ok(mut s) = STATE.lock() {
        *s = None;
    }
}

/// The heartbeat's P path part (empty with the P path off).
pub(super) fn heartbeat_text() -> String {
    if !P_PATH {
        return String::new();
    }
    counts_text(COUNTS.each_ref().map(|c| c.swap(0, Ordering::Relaxed)), MORE.each_ref().map(|c| c.swap(0, Ordering::Relaxed)))
}

fn counts_text(c: [u64; 8], more: [u64; 3]) -> String {
    format!(
        ", p path drawn {}, no owner {}, not body {}, P far {}, no layout {}, torn {}, level0 {}, bad pose {}, laid out by name {}, by lengths {}, ring from Head {}",
        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7], more[0], more[1], more[2]
    )
}

#[cfg(test)]
mod tests {
    use super::super::bonetrace::tests::rig;
    use super::*;

    /// Made-up game memory: bytes from BASE on, read all or nothing like `safe_read`.
    const BASE: u64 = 0x10_0000;
    const SIZE: usize = 0x16000;
    struct Mem(Vec<u8>);

    impl Mem {
        fn put(&mut self, at: u64, b: &[u8]) {
            let i = (at - BASE) as usize;
            self.0[i..i + b.len()].copy_from_slice(b);
        }

        /// List entry `i`'s place and turn.
        fn put_pose(&mut self, list: u64, i: usize, at: [f32; 3], q: [f32; 4]) {
            for (k, v) in at.iter().chain(&q).enumerate() {
                let off = if k < 3 { 4 * k } else { 0x10 + 4 * (k - 3) };
                self.put(list + (i * RIG_SIZE + off) as u64, &v.to_le_bytes());
            }
        }

        fn put_u64(&mut self, at: u64, v: u64) {
            self.put(at, &v.to_le_bytes());
        }

        fn put_pos(&mut self, at: u64, p: [f64; 3]) {
            for (k, v) in p.iter().enumerate() {
                self.put(at + 8 * k as u64, &v.to_le_bytes());
            }
        }

        fn put_list(&mut self, at: u64, pts: &[[f32; 3]]) {
            for (i, q) in pts.iter().enumerate() {
                for (k, v) in q.iter().enumerate() {
                    self.put(at + (i * RIG_SIZE + 4 * k) as u64, &v.to_le_bytes());
                }
            }
        }

        fn read(&self, at: u64, buf: &mut [u8]) -> usize {
            let Some(i) = at.checked_sub(BASE).map(|i| i as usize) else { return 0 };
            match i.checked_add(buf.len()).and_then(|end| self.0.get(i..end)) {
                Some(src) => {
                    buf.copy_from_slice(src);
                    buf.len()
                }
                None => 0,
            }
        }
    }

    const BODY: u64 = BASE + 0x100;
    const BODY2: u64 = BASE + 0x200;
    const TABLE: u64 = BASE + 0x400;
    const SLOT: u64 = BASE + 0x600;
    const OWNER: u64 = BASE + 0x1000;
    const OWNER2: u64 = BASE + 0x1800;
    const P1: u64 = BASE + 0x2000;
    const P2: u64 = BASE + 0x2800;
    const P3: u64 = BASE + 0x2C00;
    const SKEL: u64 = BASE + 0x3000;
    /// Room for 640 entries each.
    const L1: u64 = BASE + 0x4000;
    const L2: u64 = BASE + 0xA000;
    const L3: u64 = BASE + 0x10000;
    /// The bone table of `named_world` (200 rows).
    const BONES: u64 = BASE + 0x15000;
    const AT: [f64; 3] = [100.0, 200.0, 10.0];
    /// Bonetrace's made-up rig lays out to this (its rig_layout test).
    const IDX: [usize; BONE_COUNT] = [1, 4, 9, 18, 68, 3, 8, 17, 67, 13, 15, 6, 11, 16, 66, 21, 2, 7];
    /// The same rig by name: the same bones, the head slot the Head bone at 22.
    const IDX_NAMED: [usize; BONE_COUNT] = [1, 4, 9, 18, 68, 3, 8, 17, 67, 13, 15, 6, 11, 16, 66, 22, 2, 7];
    /// The made-up rig's named rows (entry, name, parent): IDX's bones, the Head at 22 under the
    /// Neck at 23, and the bones above them. The left toe is two steps under its foot.
    const NAMED: [(usize, &str, u16); 24] = [
        (0, "Reference", 0xFFFF),
        (1, "Hips", 0),
        (2, "LeftUpLeg", 1),
        (3, "LeftLeg", 2),
        (4, "LeftFoot", 3),
        (5, "", 4),
        (6, "LeftToeBase", 5),
        (7, "RightUpLeg", 1),
        (8, "RightLeg", 7),
        (9, "RightFoot", 8),
        (11, "RightToeBase", 9),
        (12, "Spine", 0),
        (13, "Spine1", 12),
        (14, "Spine2", 13),
        (15, "LeftShoulder", 14),
        (16, "LeftArm", 15),
        (17, "LeftForeArm", 16),
        (18, "LeftHand", 17),
        (23, "Neck", 14),
        (22, "Head", 23),
        (65, "RightShoulder", 14),
        (66, "RightArm", 65),
        (67, "RightForeArm", 66),
        (68, "RightHand", 67),
    ];
    /// The Head's turn in `named_world`: its x (up the head) up, y (the face) forward, z (left) to
    /// the body's left (a quarter turn about y).
    const HEAD_Q: [f32; 4] = [0.0, -std::f32::consts::FRAC_1_SQRT_2, 0.0, std::f32::consts::FRAC_1_SQRT_2];
    const HEAD_AT: [f32; 3] = [0.0, 0.0, 1.7];

    /// A bone table for the made-up rig: NAMED, the other rows unnamed under the root.
    fn table(named: &[(usize, &str, u16)]) -> Vec<u8> {
        let mut t = vec![0u8; 200 * TABLE_ENTRY];
        for i in 0..200 {
            t[i * TABLE_ENTRY + ENTRY_HASH..i * TABLE_ENTRY + ENTRY_HASH + 4].copy_from_slice(&(0x4000_0000 + i as u32).to_le_bytes());
        }
        for &(i, name, parent) in named {
            let hash = if name.is_empty() { 0x4000_0000 + i as u32 } else { crc32(name.as_bytes()) };
            t[i * TABLE_ENTRY..i * TABLE_ENTRY + 2].copy_from_slice(&parent.to_le_bytes());
            t[i * TABLE_ENTRY + ENTRY_HASH..i * TABLE_ENTRY + ENTRY_HASH + 4].copy_from_slice(&hash.to_le_bytes());
        }
        t
    }

    /// `world` with a bone table on its skeleton and the Head bone in both lists.
    fn named_world() -> Mem {
        let mut m = world();
        m.put(BONES, &table(&NAMED));
        m.put_u64(SKEL + SKEL_TABLE, BONES);
        for list in [L1, L2] {
            m.put_pose(list, 22, HEAD_AT, HEAD_Q);
        }
        m
    }

    /// The made-up rig's bones by name: as in the list, but the head point is the top of the head,
    /// 0.14 up the Head bone.
    fn named_bones(pts: &[[f32; 3]]) -> [[f64; 3]; BONE_COUNT] {
        let mut b = bones_of(pts);
        b[HEAD] = [0.0, 0.0, f64::from(HEAD_AT[2]) + 0.14];
        b
    }

    fn near(a: [f64; 3], b: [f64; 3], within: f64) -> bool {
        (0..3).map(|k| (a[k] - b[k]).powi(2)).sum::<f64>().sqrt() <= within
    }

    /// One body, its owner (level 2) and its two pose objects, both holding the made-up rig.
    fn world() -> Mem {
        let mut m = Mem(vec![0; SIZE]);
        m.put_pos(BODY + 0x20, AT);
        m.put_u64(BODY + BODY_TABLE, TABLE);
        m.put_u64(BODY + BODY_SLOT, SLOT);
        m.put(SLOT + SLOT_INDEX, &3u16.to_le_bytes());
        m.put_u64(TABLE + 3 * 8, OWNER);
        m.put_u64(OWNER + OWNER_BODY, BODY);
        m.put(OWNER + OWNER_LEVEL, &(2u16 << 3).to_le_bytes());
        m.put_u64(OWNER + OWNER_POSE, P1);
        for (pose, list) in [(P1, L1), (P2, L2)] {
            m.put_pos(pose, AT);
            m.put_u64(pose + POSE_SKEL, SKEL);
            m.put_u64(pose + POSE_LIST, list);
            m.put_list(list, &rig());
        }
        m.put(SKEL + SKEL_COUNT, &200u16.to_le_bytes());
        m
    }

    /// A second body with its own owner and pose object, on the same skeleton.
    fn add_second(m: &mut Mem, pts: &[[f32; 3]]) {
        m.put_u64(OWNER2 + OWNER_BODY, BODY2);
        m.put(OWNER2 + OWNER_LEVEL, &(2u16 << 3).to_le_bytes());
        m.put_u64(OWNER2 + OWNER_POSE, P3);
        m.put_pos(P3, AT);
        m.put_u64(P3 + POSE_SKEL, SKEL);
        m.put_u64(P3 + POSE_LIST, L3);
        m.put_list(L3, pts);
    }

    fn frame_of(st: &mut State, m: &Mem, owner: u64, body: u64, now: Instant) -> Result<Got, Miss> {
        body_frame(st, &|a, b| m.read(a, b), owner, body, AT, now)
    }

    fn frame(st: &mut State, m: &Mem, owner: u64, now: Instant) -> Result<Got, Miss> {
        frame_of(st, m, owner, BODY, now)
    }

    fn bones_of(pts: &[[f32; 3]]) -> [[f64; 3]; BONE_COUNT] {
        IDX.map(|i| pts[i].map(f64::from))
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn owner_from_the_body() {
        let mut m = world();
        assert_eq!(anim_owner(&|a, b| m.read(a, b), BODY), Some(OWNER));
        // No index in the slot (0xFFFF), or no table: no owner.
        m.put(SLOT + SLOT_INDEX, &0xFFFFu16.to_le_bytes());
        assert_eq!(anim_owner(&|a, b| m.read(a, b), BODY), None);
        let mut m = world();
        m.put_u64(BODY + BODY_TABLE, 0);
        assert_eq!(anim_owner(&|a, b| m.read(a, b), BODY), None);
    }

    #[test]
    fn bones_from_the_bodys_own_pose() {
        let m = world();
        let mut st = State::default();
        let g = frame(&mut st, &m, OWNER, Instant::now()).unwrap();
        assert_eq!((g.obj, g.local), (P1, bones_of(&rig())));
        // The made-up rig still lays out as in bonetrace's test, cached by its skeleton.
        assert_eq!(st.layouts.0.get(&SKEL).and_then(|c| c.layout).map(|l| l.idx), Some(IDX));
        // A bone count that looks wrong reads 640 entries, like the hook route.
        for count in [0u16, 1000] {
            let mut m = world();
            m.put(SKEL + SKEL_COUNT, &count.to_le_bytes());
            assert_eq!(frame(&mut State::default(), &m, OWNER, Instant::now()).unwrap().local, bones_of(&rig()));
        }
    }

    #[test]
    fn p_is_read_again_every_frame() {
        let mut m = world();
        let mut st = State::default();
        let t0 = Instant::now();
        frame(&mut st, &m, OWNER, t0).unwrap();
        // The owner moves on to its next pose object: the same skeleton, its own list, a new pose
        // (the whole rig 5 cm to the side).
        let next: Vec<[f32; 3]> = rig().iter().map(|q| [q[0] + 0.05, q[1], q[2]]).collect();
        m.put_list(L2, &next);
        m.put_u64(OWNER + OWNER_POSE, P2);
        let g = frame(&mut st, &m, OWNER, t0 + ms(16)).unwrap();
        assert_eq!((g.obj, g.local, g.moved), (P2, bones_of(&next), t0 + ms(16)));
        // Nothing changed: "moved" stays (the bone trace's frozen check).
        assert_eq!(frame(&mut st, &m, OWNER, t0 + ms(32)).unwrap().moved, t0 + ms(16));
        // One skeleton, one layout, worked out once.
        assert_eq!(st.layouts.0.len(), 1);
        assert_eq!(st.layouts.0[&SKEL].at, t0);
    }

    #[test]
    fn layout_cache() {
        let mut c = Layouts::default();
        let t0 = Instant::now();
        let a = Layout { idx: IDX, lens: [0.4; LIMB_LINES.len()], by_name: false, head: None };
        let mut b = a;
        b.idx[0] = 0;
        let mut runs = 0;
        // First sight: worked out. After that the same layout, not worked out again.
        assert_eq!(
            c.get(1, 200, t0, || {
                runs += 1;
                Some(a)
            }),
            Some(a)
        );
        assert_eq!(
            c.get(1, 200, t0 + ms(5000), || {
                runs += 1;
                Some(b)
            }),
            Some(a)
        );
        assert_eq!(runs, 1);
        // A new skeleton gets its own.
        assert_eq!(
            c.get(2, 200, t0, || {
                runs += 1;
                Some(b)
            }),
            Some(b)
        );
        assert_eq!(runs, 2);
        // A failed one waits a second, then is tried again.
        assert_eq!(
            c.get(3, 200, t0, || {
                runs += 1;
                None
            }),
            None
        );
        assert_eq!(
            c.get(3, 200, t0 + ms(999), || {
                runs += 1;
                Some(a)
            }),
            None
        );
        assert_eq!(runs, 3);
        assert_eq!(
            c.get(3, 200, t0 + ms(1000), || {
                runs += 1;
                Some(a)
            }),
            Some(a)
        );
        assert_eq!(runs, 4);
        // Another bone count at the same address is another skeleton.
        assert_eq!(
            c.get(1, 300, t0 + ms(5000), || {
                runs += 1;
                Some(b)
            }),
            Some(b)
        );
        assert_eq!(runs, 5);
        // Forgotten (torn too long): worked out again at once.
        c.forget(1);
        assert_eq!(
            c.get(1, 300, t0 + ms(5001), || {
                runs += 1;
                Some(a)
            }),
            Some(a)
        );
        assert_eq!(runs, 6);
        // Full: a new skeleton starts it again, a try again of one in it does not.
        let mut c = Layouts::default();
        for s in 0..LAYOUTS_MAX as u64 {
            c.get(s, 200, t0, || (s > 0).then_some(a));
        }
        assert_eq!(c.get(0, 200, t0 + ms(1000), || Some(a)), Some(a));
        assert_eq!(c.0.len(), LAYOUTS_MAX);
        c.get(LAYOUTS_MAX as u64, 200, t0, || Some(a));
        assert_eq!(c.0.len(), 1);
    }

    #[test]
    fn failed_layout_is_tried_again_from_a_fresh_frame() {
        let mut m = world();
        // Read while the game wrote it: the pelvis is off, so rig_layout fails.
        let mut half = rig();
        half[1][2] = 2.0;
        m.put_list(L1, &half);
        let mut st = State::default();
        let t0 = Instant::now();
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::NoLayout);
        m.put_list(L1, &rig());
        assert_eq!(frame(&mut st, &m, OWNER, t0 + ms(500)).unwrap_err(), Miss::NoLayout);
        assert_eq!(frame(&mut st, &m, OWNER, t0 + ms(1000)).unwrap().local, bones_of(&rig()));
    }

    #[test]
    fn misses_name_the_step() {
        let t0 = Instant::now();
        let miss = |change: &dyn Fn(&mut Mem), owner: u64| {
            let mut m = world();
            change(&mut m);
            frame(&mut State::default(), &m, owner, t0).unwrap_err()
        };
        assert_eq!(miss(&|_| {}, 0), Miss::NoOwner);
        assert_eq!(miss(&|m| m.put_u64(OWNER + OWNER_BODY, BODY + 0x40), OWNER), Miss::NotBody);
        // An owner that can't be read (freed) is not this body's either.
        assert_eq!(miss(&|_| {}, BASE + SIZE as u64), Miss::NotBody);
        assert_eq!(miss(&|m| m.put_u64(OWNER + OWNER_POSE, 0), OWNER), Miss::PFar);
        assert_eq!(miss(&|m| m.put_pos(P1, [AT[0] + 0.6, AT[1], AT[2]]), OWNER), Miss::PFar);
        assert_eq!(miss(&|m| m.put_pos(P1, [f64::NAN, AT[1], AT[2]]), OWNER), Miss::PFar);
        assert_eq!(miss(&|m| m.put_u64(P1 + POSE_SKEL, 0), OWNER), Miss::NoLayout);
        assert_eq!(miss(&|m| m.put_u64(P1 + POSE_LIST, 0), OWNER), Miss::NoLayout);
        // A count that looks right is never read past: 150 entries are too few for a layout.
        assert_eq!(miss(&|m| m.put(SKEL + SKEL_COUNT, &150u16.to_le_bytes()), OWNER), Miss::NoLayout);
    }

    #[test]
    fn torn_frames_draw_the_last_good_one_for_30_frames() {
        let mut m = world();
        let mut st = State::default();
        let t0 = Instant::now();
        let good = frame(&mut st, &m, OWNER, t0).unwrap().local;
        // Read mid-write: the left forearm 10 cm longer.
        let mut torn = rig();
        torn[18][2] -= 0.1;
        m.put_list(L1, &torn);
        for n in 1..=RIG_BAD_MAX {
            assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good, "torn frame {n}");
        }
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Torn);
        // Then its layout is worked out again, from the next frame. Its lengths are taken again
        // too: drawn once two frames in a row agree.
        assert!(!st.layouts.0.contains_key(&SKEL));
        m.put_list(L1, &rig());
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Torn);
        assert!(st.layouts.0.contains_key(&SKEL));
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
    }

    #[test]
    fn a_torn_frame_after_a_torn_spell_is_not_drawn() {
        let mut m = world();
        let mut st = State::default();
        let t0 = Instant::now();
        let good = frame(&mut st, &m, OWNER, t0).unwrap().local;
        // The left forearm `cm` longer: read while the game wrote it.
        let torn = |cm: f32| {
            let mut t = rig();
            t[18][2] -= cm / 100.0;
            t
        };
        m.put_list(L1, &torn(10.0));
        for _ in 0..RIG_BAD_MAX {
            assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
        }
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Torn);
        // The next frame is torn too (5 cm: still a layout). Not drawn, its lengths not kept.
        m.put_list(L1, &torn(5.0));
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Torn);
        m.put_list(L1, &rig());
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Torn);
        // Two good frames in a row: drawn. A torn frame after that draws the good one.
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
        m.put_list(L1, &torn(5.0));
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
    }

    #[test]
    fn two_bodies_on_one_skeleton_keep_their_own_lengths() {
        let mut m = world();
        // The second body is built 20% bigger: every limb over 4 cm longer.
        let big: Vec<[f32; 3]> = rig().iter().map(|q| q.map(|v| v * 1.2)).collect();
        add_second(&mut m, &big);
        let mut st = State::default();
        let t0 = Instant::now();
        assert!(frame(&mut st, &m, OWNER, t0).is_ok());
        // Against the first body's lengths its frames look torn: the old route draws it, and after
        // the torn spell (and one frame to check them) it takes its own lengths.
        for _ in 0..=RIG_BAD_MAX + 1 {
            assert_eq!(frame_of(&mut st, &m, OWNER2, BODY2, t0).unwrap_err(), Miss::Torn);
        }
        assert_eq!(frame_of(&mut st, &m, OWNER2, BODY2, t0).unwrap().local, bones_of(&big));
        // From then on neither pushes the other out.
        for _ in 0..3 {
            assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, bones_of(&rig()));
            assert_eq!(frame_of(&mut st, &m, OWNER2, BODY2, t0).unwrap().local, bones_of(&big));
        }
    }

    #[test]
    fn level0_rule() {
        let mut zeros = 0;
        assert!((1..LEVEL0_FRAMES).all(|_| !level0_skip(&mut zeros, 0)));
        assert!(level0_skip(&mut zeros, 0));
        assert!(level0_skip(&mut zeros, 0));
        // A level again: the P path at once, and 10 more zeros before the next skip.
        assert!(!level0_skip(&mut zeros, 3));
        assert!((1..LEVEL0_FRAMES).all(|_| !level0_skip(&mut zeros, 0)));
        assert!(level0_skip(&mut zeros, 0));
    }

    #[test]
    fn level0_for_10_frames_skips_the_p_path() {
        let mut m = world();
        // Bits 3-5 clear (level 0), the others set.
        m.put(OWNER + OWNER_LEVEL, &0x01C7u16.to_le_bytes());
        let mut st = State::default();
        let t0 = Instant::now();
        for n in 1..LEVEL0_FRAMES {
            assert!(frame(&mut st, &m, OWNER, t0).is_ok(), "frame {n}");
        }
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Level0);
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap_err(), Miss::Level0);
        m.put(OWNER + OWNER_LEVEL, &(1u16 << 3).to_le_bytes());
        assert!(frame(&mut st, &m, OWNER, t0).is_ok());
    }

    #[test]
    fn old_route_only_when_the_p_path_fails() {
        let mut old = 0;
        let r: Result<u32, String> = or_old_route(Some(1), || {
            old += 1;
            Ok(2)
        });
        assert_eq!((r, old), (Ok(1), 0));
        let r: Result<u32, String> = or_old_route(None, || {
            old += 1;
            Ok(2)
        });
        assert_eq!((r, old), (Ok(2), 1));
        // The P path fails (the owner no longer points at the body): the old route's answer counts,
        // its miss too (then the box is drawn).
        let mut m = world();
        m.put_u64(OWNER + OWNER_BODY, 0);
        let own = frame(&mut State::default(), &m, OWNER, Instant::now()).ok().map(|g| g.local);
        let r = or_old_route(own, || Err::<[[f64; 3]; BONE_COUNT], String>("no pose caught".into()));
        assert_eq!(r, Err("no pose caught".to_string()));
    }

    #[test]
    fn old_bodies_are_forgotten() {
        let m = world();
        let mut st = State::default();
        let t0 = Instant::now();
        frame(&mut st, &m, OWNER, t0).unwrap();
        st.prune(t0 + ms(2000));
        assert_eq!((st.bodies.len(), st.layouts.0.len()), (1, 1));
        // Its skeleton goes with it.
        st.prune(t0 + ms(6000));
        assert_eq!((st.bodies.len(), st.layouts.0.len()), (0, 0));
    }

    #[test]
    fn heartbeat_part() {
        assert_eq!(
            counts_text([500, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10]),
            ", p path drawn 500, no owner 1, not body 2, P far 3, no layout 4, torn 5, level0 6, bad pose 7, laid out by name 8, by lengths 9, ring from Head 10"
        );
        // Each miss counts in its own slot, in the text's order.
        let misses = [Miss::NoOwner, Miss::NotBody, Miss::PFar, Miss::NoLayout, Miss::Torn, Miss::Level0, Miss::BadPose];
        assert_eq!(misses.map(|m| m as usize + 1), [1, 2, 3, 4, 5, 6, 7]);
    }

    // Bones by name.

    #[test]
    fn crc32_is_the_games_name_hash() {
        // The usual check value, then 23 names as the game's bone tables hold them.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let seen = [
            ("Reference", 0x2c52_cbb0),
            ("Hips", 0xded1_0611),
            ("LeftUpLeg", 0x1761_83f0),
            ("LeftLeg", 0x060d_f401),
            ("LeftFoot", 0x5898_8870),
            ("LeftToeBase", 0xb950_94e1),
            ("RightUpLeg", 0x757f_1291),
            ("RightLeg", 0x863d_09fc),
            ("RightFoot", 0x9b14_362c),
            ("RightToeBase", 0x42be_0fcb),
            ("Spine", 0x530e_c1cb),
            ("Spine1", 0x8f39_fa4e),
            ("Spine2", 0x1630_abf4),
            ("Neck", 0x8023_796d),
            ("Head", 0x07c1_59a2),
            ("LeftShoulder", 0x2d46_60a8),
            ("LeftArm", 0xeb83_0ada),
            ("LeftForeArm", 0x89b9_3a80),
            ("LeftHand", 0xb675_f36c),
            ("RightShoulder", 0xf606_47e5),
            ("RightArm", 0x6bb3_f727),
            ("RightForeArm", 0x7257_a1aa),
            ("RightHand", 0x75f9_4d30),
        ];
        for (name, hash) in seen {
            assert_eq!(crc32(name.as_bytes()), hash, "{name}");
        }
        // Worked out at build time, in BONE_LINES order.
        assert_eq!((SLOT_HASHES[0], SLOT_HASHES[HEAD], ABOVE_HASHES[HEAD]), (0xded1_0611, 0x07c1_59a2, 0x8023_796d));
    }

    #[test]
    fn real_table_rows() {
        // The player's skeleton's rows 157 (Neck) and 158 (Head): parent u16 at +0, name hash at
        // +0xC. The Head is found under the Neck.
        let rows: Vec<u8> = "4b 00 dc 00 27 00 9d 00 00 01 1d ff 6d 79 23 80 9d 00 cc 00 28 00 9e 00 00 01 1d 72 a2 59 c1 07"
            .split_whitespace()
            .map(|h| u8::from_str_radix(h, 16).unwrap())
            .collect();
        let mut t = vec![0u8; 157 * TABLE_ENTRY];
        t.extend(&rows);
        assert_eq!(slot_named(&t, HEAD), Ok(158));
        // The rest of the 18 are not in these rows.
        assert_eq!(bones_by_name(&t), Err(NameMiss::Missing("Hips")));
    }

    #[test]
    fn bones_found_by_name() {
        /// NAMED's rows, for a test to change.
        type Rows = Vec<(usize, &'static str, u16)>;
        assert_eq!(bones_by_name(&table(&NAMED)), Ok(IDX_NAMED));
        let with = |change: &dyn Fn(&mut Rows)| {
            let mut n = NAMED.to_vec();
            change(&mut n);
            bones_by_name(&table(&n))
        };
        // A name not there, or there twice.
        assert_eq!(with(&|n| n[6].1 = "LeftToe"), Err(NameMiss::Missing("LeftToeBase")));
        assert_eq!(with(&|n| n.push((40, "Hips", 0))), Err(NameMiss::Twice("Hips")));
        // The bone that must be above it is not there, or not above it.
        assert_eq!(with(&|n| n[18].1 = ""), Err(NameMiss::Missing("Neck")));
        assert_eq!(with(&|n| n[17].2 = 16), Err(NameMiss::Parent("LeftHand")));
        // A parent past the table, or a bone that is its own parent: not above it.
        assert_eq!(with(&|n| n[17].2 = 999), Err(NameMiss::Parent("LeftHand")));
        assert_eq!(with(&|n| n[17].2 = 18), Err(NameMiss::Parent("LeftHand")));
        // Up to UP_STEPS bones between: still found. The right toe 4 steps under its foot, then 5.
        let chain = |steps: u16| {
            move |n: &mut Rows| {
                n[10].2 = 30;
                for s in 0..steps - 1 {
                    n.push((30 + usize::from(s), "", if s + 2 == steps { 9 } else { 31 + s }));
                }
            }
        };
        assert_eq!(with(&chain(4)), Ok(IDX_NAMED));
        assert_eq!(with(&chain(5)), Err(NameMiss::Parent("RightToeBase")));
    }

    #[test]
    fn head_frame_from_real_numbers() {
        // Head bones read (place, turn x y z w), and points the game put on the head: the top
        // (0.1402 up), the back of the skull (0.1095 up, 0.0987 back), the left eye (0.0748 up,
        // 0.0848 forward, 0.0315 left). The player's body, head bent forward:
        let pose = BonePose { at: [-0.0764219463, 0.0951317847, 1.449368], q: [0.170358837, 0.671708584, -0.399902135, -0.599886477] };
        assert!(near(pose.point(HEAD_TOP), [-0.107498631, 0.19447796, 1.54321921], 0.001));
        assert!(near(pose.point([0.1095, -0.0987, 0.0]), [-0.0760011002, 0.11128746, 1.59588683], 0.001));
        assert!(near(pose.point([0.0748, 0.0848, 0.0315]), [-0.144025773, 0.190397754, 1.4377985], 0.001));
        assert!(near(pose.point([0.0748, 0.0848, -0.0315]), [-0.0846773088, 0.211386919, 1.43529963], 0.001));
        // An NPC standing (its eyes 0.0735 up, 0.0794 forward).
        let npc = BonePose { at: [0.1060296, 0.18503727, 1.58528721], q: [0.143829688, 0.651657164, -0.0950816646, -0.738657773] };
        assert!(near(npc.point(HEAD_TOP), [0.124688089, 0.230990455, 1.71637368], 0.001));
        assert!(near(npc.point([0.1095, -0.0987, 0.0]), [0.115883365, 0.128089041, 1.72089231], 0.001));
        assert!(near(npc.point([0.0735, 0.0794, 0.0333]), [0.0865270495, 0.286786675, 1.6309073], 0.001));
        // A player at detail level 3 (its turn the other sign): its eyes fit, but its top bone was
        // not moved by the game, 11 cm from where the top is.
        let p = BonePose { at: [0.0477366447, 0.0739250481, 1.65859783], q: [0.240027726, -0.669048846, 0.37929666, 0.592363358] };
        assert!(near(p.point([0.0733, 0.0781, 0.0329]), [-0.0459745452, 0.103860445, 1.71229434], 0.001));
        assert!(!near(p.point(HEAD_TOP), [0.109079629, 0.132888407, 1.72097421], 0.1));
        // No turn leaves a point as it is; a turn keeps lengths.
        assert_eq!(rotate([0.0, 0.0, 0.0, 1.0], [0.1, 0.2, 0.3]), [0.1, 0.2, 0.3]);
        let r = rotate(pose.q, [0.3, -0.2, 0.1]);
        assert!((r.iter().map(|v| v * v).sum::<f64>() - 0.14).abs() < 1e-6);
    }

    #[test]
    fn bone_pose_reads_place_and_turn() {
        let m = named_world();
        let read = |a: u64, b: &mut [u8]| m.read(a, b);
        let got = bone_pose(&read, L1, 22).unwrap();
        assert_eq!((got.at, got.q), (HEAD_AT.map(f64::from), HEAD_Q.map(f64::from)));
        // The quarter turn: up the head is up, the face forward, its left the body's left.
        assert!(near(got.point([1.0, 0.0, 0.0]), [0.0, 0.0, 2.7], 1e-6));
        assert!(near(got.point([0.0, 1.0, 0.0]), [0.0, 1.0, 1.7], 1e-6));
        assert!(near(got.point([0.0, 0.0, 1.0]), [-1.0, 0.0, 1.7], 1e-6));
        // A turn that is not one (zeros, or not finite), or past the memory: none.
        let mut m = named_world();
        m.put_pose(L1, 22, HEAD_AT, [0.0; 4]);
        assert_eq!(bone_pose(&|a, b| m.read(a, b), L1, 22), None);
        m.put_pose(L1, 22, HEAD_AT, [f32::NAN, 0.0, 0.0, 1.0]);
        assert_eq!(bone_pose(&|a, b| m.read(a, b), L1, 22), None);
        assert_eq!(bone_pose(&|a, b| m.read(a, b), BASE + SIZE as u64, 0), None);
    }

    #[test]
    fn p_path_finds_bones_by_name() {
        let m = named_world();
        let mut st = State::default();
        let g = frame(&mut st, &m, OWNER, Instant::now()).unwrap();
        let lay = st.layouts.0[&SKEL].layout.unwrap();
        assert!(lay.by_name);
        assert_eq!(lay.idx, IDX_NAMED);
        // The list's bones, and the head point the top of the head: 0.14 up the Head bone.
        let want = named_bones(&rig());
        assert!((0..BONE_COUNT).all(|k| near(g.local[k], want[k], 1e-6)), "{:?}", g.local);
        assert_eq!(lay.lens, line_lens(&g.local));
    }

    #[test]
    fn no_names_means_lengths_as_before() {
        let laid_out = |m: &Mem| {
            let mut st = State::default();
            let g = frame(&mut st, m, OWNER, Instant::now()).unwrap();
            let lay = st.layouts.0[&SKEL].layout.unwrap();
            (lay.by_name, lay.idx, g.local)
        };
        let old = (false, IDX, bones_of(&rig()));
        // No table (world has none), a name missing, a parent wrong.
        assert_eq!(laid_out(&world()), old);
        let mut n = NAMED.to_vec();
        n[13].1 = "";
        let mut m = named_world();
        m.put(BONES, &table(&n));
        assert_eq!(laid_out(&m), old);
        let mut n = NAMED.to_vec();
        n[15].2 = 13;
        m.put(BONES, &table(&n));
        assert_eq!(laid_out(&m), old);
        // A count too big for the table: not read.
        let mut m = named_world();
        m.put(SKEL + SKEL_COUNT, &3000u16.to_le_bytes());
        assert_eq!(laid_out(&m), old);
        // The Head's turn can't be used: by lengths too.
        let mut m = named_world();
        m.put_pose(L1, 22, HEAD_AT, [0.0; 4]);
        assert_eq!(laid_out(&m), old);
    }

    // The ring's centre from the Head bone.

    #[test]
    fn ring_centre_from_real_numbers() {
        // The player's body, head bent forward: the ring sat on the back of the player's skull.
        let pose = BonePose { at: [-0.0764219463, 0.0951317847, 1.449368], q: [0.170358837, 0.671708584, -0.399902135, -0.599886477] };
        let chest = [-0.0351570062, -0.0383860767, 1.35675287];
        let c = ring_centre(&pose, chest).unwrap();
        assert!(near(c, [-0.096997, 0.157180, 1.481411], 0.0005), "{c:?}");
        // 6.5 cm behind the player's eyes' middle, 12.5 cm in front of the back of the player's
        // skull.
        let (eyes, back) = ([-0.114352, 0.200892, 1.436549], [-0.0760011002, 0.11128746, 1.59588683]);
        assert!(near(c, eyes, 0.07) && !near(c, eyes, 0.06));
        assert!(near(c, back, 0.13) && !near(c, back, 0.12));
        // The old centre (0.08 down from the back of the skull, the player's rig_layout head) was
        // 10.5 cm away, 17 cm from the player's eyes.
        let d = [0, 1, 2].map(|k| back[k] - chest[k]);
        let len = d.iter().map(|v| v * v).sum::<f64>().sqrt();
        let old = [0, 1, 2].map(|k| back[k] - 0.08 * d[k] / len);
        assert!(!near(old, c, 0.1) && !near(old, eyes, 0.16));
        // Farther than RING_NEAR_CHEST from the chest (0.47 m), or not a number: none.
        assert!(ring_centre(&pose, [chest[0], chest[1], chest[2] - 0.1]).is_some());
        assert_eq!(ring_centre(&pose, [chest[0], chest[1], chest[2] - 0.3]), None);
        assert_eq!(ring_centre(&pose, [f64::NAN, chest[1], chest[2]]), None);
        let mut bad = pose;
        bad.at[0] = f64::INFINITY;
        assert_eq!(ring_centre(&bad, chest), None);
    }

    #[test]
    fn p_path_gives_the_ring_centre() {
        let mut m = named_world();
        let mut st = State::default();
        let t0 = Instant::now();
        let g = frame(&mut st, &m, OWNER, t0).unwrap();
        // The quarter turn: 0.07 up the head is up, 0.02 forward is forward.
        let want = [0.0, 0.02, f64::from(HEAD_AT[2]) + 0.07];
        assert!(near(g.ring.unwrap(), want, 1e-6), "{:?}", g.ring);
        assert_eq!(st.layouts.0[&SKEL].layout.unwrap().head, Some(22));
        // A torn frame draws the last good frame and its ring.
        m.put_pose(L1, 22, HEAD_AT, [0.0; 4]);
        let torn = frame(&mut st, &m, OWNER, t0).unwrap();
        assert_eq!((torn.local, torn.ring), (g.local, g.ring));
        // The chest more than RING_NEAR_CHEST away: placed as before.
        let mut m = named_world();
        let mut pts = rig();
        pts[15] = [0.0, 0.0, 1.2];
        m.put_list(L1, &pts);
        m.put_pose(L1, 22, HEAD_AT, HEAD_Q);
        assert_eq!(frame(&mut State::default(), &m, OWNER, t0).unwrap().ring, None);
    }

    #[test]
    fn ring_centre_needs_only_the_head_by_name() {
        let t0 = Instant::now();
        // Laid out by lengths (Spine2's name missing), the Head still found by name: a centre.
        let mut n = NAMED.to_vec();
        n[13].1 = "";
        let mut m = named_world();
        m.put(BONES, &table(&n));
        let mut st = State::default();
        let g = frame(&mut st, &m, OWNER, t0).unwrap();
        let lay = st.layouts.0[&SKEL].layout.unwrap();
        assert_eq!((lay.by_name, lay.head), (false, Some(22)));
        assert_eq!(g.local, bones_of(&rig()));
        assert!(near(g.ring.unwrap(), [0.0, 0.02, f64::from(HEAD_AT[2]) + 0.07], 1e-6));
        // Its turn not a turn: no centre, the bones as before.
        m.put_pose(L1, 22, HEAD_AT, [0.0; 4]);
        let g = frame(&mut State::default(), &m, OWNER, t0).unwrap();
        assert_eq!((g.local, g.ring), (bones_of(&rig()), None));
        // No table, or no Head in it: no centre.
        assert_eq!(frame(&mut State::default(), &world(), OWNER, t0).unwrap().ring, None);
        let mut n = NAMED.to_vec();
        n[19].1 = "";
        let mut m = named_world();
        m.put(BONES, &table(&n));
        let mut st = State::default();
        let g = frame(&mut st, &m, OWNER, t0).unwrap();
        assert_eq!((st.layouts.0[&SKEL].layout.unwrap().head, g.ring), (None, None));
    }

    #[test]
    fn by_name_a_bad_head_turn_is_a_torn_frame() {
        let mut m = named_world();
        let mut st = State::default();
        let t0 = Instant::now();
        let good = frame(&mut st, &m, OWNER, t0).unwrap().local;
        // Read while the game wrote it: the last good frame is drawn, as for a stretched bone.
        m.put_pose(L1, 22, HEAD_AT, [0.0; 4]);
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
        m.put_pose(L1, 22, HEAD_AT, HEAD_Q);
        assert_eq!(frame(&mut st, &m, OWNER, t0).unwrap().local, good);
    }
}
