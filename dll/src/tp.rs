//! Teleports, with one undo: every teleport remembers where the player stood, and undo goes back
//! there. Undo twice and the player is where the player started.

use std::sync::Mutex;
use std::time::Duration;

use crate::{fly, log, memory};

static LAST: Mutex<Option<[f64; 3]>> = Mutex::new(None);
/// Teleports so far, so the ESP knows a jump was a teleport (not fast flying).
static JUMPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn jumps() -> u64 {
    JUMPS.load(std::sync::atomic::Ordering::Relaxed)
}
/// Writes per try, 10 ms apart.
const HOLD_WRITES: u32 = 30;

fn here(addr: u64) -> Option<[f64; 3]> {
    let b = memory::read(addr, 24).ok()?;
    let v = |i: usize| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
    Some([v(0), v(1), v(2)])
}

/// Put the player at `pos`, remembering where the player was.
pub fn to(pos: [f64; 3]) -> Result<(), String> {
    if !pos.iter().all(|v| v.is_finite() && v.abs() < 100_000.0) {
        return Err(format!("bad position {pos:?}"));
    }
    let addr = fly::body_addr().ok_or("body not found")?;
    let before = here(addr);
    let mut b = Vec::with_capacity(24);
    for v in pos {
        b.extend_from_slice(&v.to_le_bytes());
    }
    // One write sometimes loses to the game writing the old place back. So hold it for a third of a
    // second, check it stuck, and try again up to 3 times. Flying holds the player at its own spot
    // every few ms: move that spot too, or fly undoes it.
    fly::jump(pos);
    let mut stuck = false;
    'tries: for attempt in 1..=3 {
        for _ in 0..HOLD_WRITES {
            // A far teleport can start a load that frees this body: never write into it then.
            if fly::body_addr() != Some(addr) {
                log("teleport: the player's body changed (a load started), stopped writing");
                stuck = true;
                break 'tries;
            }
            memory::write(addr, &b)?;
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(150));
        let off = here(addr).map(|p| ((p[0] - pos[0]).powi(2) + (p[1] - pos[1]).powi(2)).sqrt());
        if off.is_some_and(|d| d < 5.0) {
            stuck = true;
            if attempt > 1 {
                log(&format!("teleport stuck on try {attempt}"));
            }
            break;
        }
        log(&format!("teleport try {attempt} snapped back ({off:?} m off)"));
    }
    JUMPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let (Some(p), Ok(mut last)) = (before, LAST.lock()) {
        *last = Some(p);
    }
    if !stuck {
        return Err("teleport did not stick, try again".into());
    }
    log(&format!("teleported to {:.1} {:.1} {:.1}", pos[0], pos[1], pos[2]));
    Ok(())
}

/// Back to where the last teleport started.
pub fn undo() -> Result<(), String> {
    let back = LAST.lock().ok().and_then(|l| *l).ok_or("nothing to undo yet")?;
    to(back)
}
