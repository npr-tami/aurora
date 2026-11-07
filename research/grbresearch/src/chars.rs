//! `chars`: list NPC and player character components the way the mod's ESP finds them. A component
//! is a heap object whose first qword is the known vtable; +8 is its body, body+0x20 its position.
//! Each one is checked like the ESP does before it is shown.

use std::collections::BTreeMap;

use crate::cmd::{Args, Fail, Res, Session};
use crate::proc::{self, Target, USER_MAX, USER_MIN};
use crate::scan::{self, Opts, Stats};
use crate::types::raw_of;

/// GRB_vulkan.exe offsets (research.json vt.char_component, vt.player_component, off.body_root).
pub const CHAR_COMPONENT: u64 = 0x049B_FD70;
pub const PLAYER_COMPONENT: u64 = 0x04A4_C5F0;
pub const BODY_ROOT: u64 = 0x063E_BEB0;

#[derive(Debug, Clone)]
pub struct Char {
    pub marker: u64,
    pub player: bool,
    pub body: u64,
    pub pos: Option<[f64; 3]>,
    pub dead: Option<bool>,
    pub side: Option<i32>,
    /// Why it failed the ESP checks, if it did.
    pub bad: Option<&'static str>,
}

/// Position sanity like the mod: finite and |v| < 1e5.
pub fn sane_pos(p: &[f64; 3]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() < 1e5)
}

/// Check one candidate the way the ESP does: marker+0 == vtable, marker+8 == a body whose class tag
/// points into the exe and whose position is sane.
pub fn check(t: &Target, marker: u64, vt: u64, player: bool) -> Char {
    let mut c = Char { marker, player, body: 0, pos: None, dead: None, side: None, bad: None };
    let Ok(m) = t.read(marker, 0x10) else {
        c.bad = Some("marker unreadable");
        return c;
    };
    if raw_of(&m[..8]) != vt {
        c.bad = Some("vtable changed");
        return c;
    }
    c.body = raw_of(&m[8..16]);
    if !proc::is_ptr(c.body) {
        c.bad = Some("marker+8 is not a pointer");
        return c;
    }
    let Ok(b) = t.read(c.body, 0x10C) else {
        c.bad = Some("body unreadable");
        return c;
    };
    let tag = raw_of(&b[..8]);
    if !t.exe_module().is_some_and(|m| m.contains(tag)) {
        c.bad = Some("body class tag not in exe");
        return c;
    }
    let f = |o: usize| f64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]));
    let pos = [f(0x20), f(0x28), f(0x30)];
    if !sane_pos(&pos) {
        c.bad = Some("position not sane");
        return c;
    }
    c.pos = Some(pos);
    c.dead = Some(u32::from_le_bytes(b[0x108..0x10C].try_into().unwrap_or([0; 4])) & 0x4000 != 0);
    // Side: [[marker+0x60]+0x160]+0x104, valid 0..64.
    c.side = t
        .read_u64(marker + 0x60)
        .ok()
        .filter(|p| proc::is_ptr(*p))
        .and_then(|p| t.read_u64(p + 0x160).ok())
        .filter(|p| proc::is_ptr(*p))
        .and_then(|p| t.read(p + 0x104, 4).ok())
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .filter(|s| (0..64).contains(s));
    c
}

pub fn chars(s: &Session, a: &Args) -> Res {
    let t = s.t;
    let exe = t.exe_module().ok_or("no exe module")?;
    if !t.exe.eq_ignore_ascii_case("GRB_vulkan.exe") {
        println!("warning: these offsets are from GRB_vulkan.exe; on {} they are untested", t.exe);
    }
    let (npc_vt, player_vt) = (exe.base + CHAR_COMPONENT, exe.base + PLAYER_COMPONENT);
    let her_body = t.read_u64(exe.base + BODY_ROOT).ok().filter(|b| proc::is_ptr(*b));
    let her_pos = her_body.and_then(|b| t.read(b + 0x20, 24).ok()).map(|b| {
        let f = |o: usize| f64::from_le_bytes(b[o..o + 8].try_into().unwrap_or([0; 8]));
        [f(0), f(8), f(16)]
    });

    // Same memory the ESP walks: committed private writable, no guard pages, no GPU memory.
    scan::arm_cancel();
    let mut st = Stats::default();
    let spans: Vec<[u64; 2]> = t
        .regions(USER_MIN, USER_MAX)
        .into_iter()
        .filter(|r| r.committed() && r.kind_str() == "private" && r.writable() && r.readable() && !r.gpu())
        .map(|r| [r.base, r.end()])
        .collect();
    let opts = Opts { limit: 0, resident_only: !a.has("paged-out"), aligned: true };
    let mut found: Vec<(u64, bool)> = Vec::new();
    scan::walk(t, &spans, 0, &opts, &mut st, |addr, data, owned| {
        let mut off = 0;
        while off + 8 <= owned.min(data.len()) {
            let v = raw_of(&data[off..off + 8]);
            if v == npc_vt || v == player_vt {
                found.push((addr + off as u64, v == player_vt));
            }
            off += 8;
        }
        true
    });
    let mut list: Vec<Char> =
        found.iter().map(|&(m, p)| check(t, m, if p { player_vt } else { npc_vt }, p)).collect();
    if !t.alive() {
        return Err(Fail::Gone);
    }
    let dist = |c: &Char| match (c.pos, her_pos) {
        (Some(p), Some(h)) => ((p[0] - h[0]).powi(2) + (p[1] - h[1]).powi(2) + (p[2] - h[2]).powi(2)).sqrt(),
        _ => f64::MAX,
    };
    list.sort_by(|x, y| dist(x).total_cmp(&dist(y)));
    let mut rejected: BTreeMap<&str, usize> = BTreeMap::new();
    println!(
        "{} candidates ({}); the player's body {}",
        list.len(),
        st.summary(),
        her_body.map(|b| s.both(b)).unwrap_or_else(|| "not found".into())
    );
    println!("kind    marker            body              x          y          z       dist(m)  side      dead");
    for c in &list {
        if let Some(why) = c.bad {
            *rejected.entry(why).or_insert(0) += 1;
            if !a.has("all") {
                continue;
            }
            println!("{:<7} 0x{:<15X} 0x{:<15X} rejected: {why}", if c.player { "player" } else { "npc" }, c.marker, c.body);
            continue;
        }
        let kind = if her_body == Some(c.body) { "you" } else if c.player { "player" } else { "npc" };
        let p = c.pos.unwrap_or([0.0; 3]);
        let d = dist(c);
        let side = match c.side {
            Some(3) => "enemy".to_string(),
            Some(0) => "friendly".to_string(),
            Some(v) => format!("{v}"),
            None => "?".to_string(),
        };
        println!(
            "{kind:<7} 0x{:<15X} 0x{:<15X} {:>10.1} {:>10.1} {:>8.1} {:>8} {side:<9} {}",
            c.marker,
            c.body,
            p[0],
            p[1],
            p[2],
            if d == f64::MAX { "?".into() } else { format!("{:.0}", d * 1.5) },
            if c.dead == Some(true) { "dead" } else { "" }
        );
    }
    let ok = list.iter().filter(|c| c.bad.is_none()).count();
    let rej: Vec<String> = rejected.iter().map(|(k, v)| format!("{k} {v}")).collect();
    println!("{ok} valid{}", if rej.is_empty() { String::new() } else { format!(", rejected: {} (--all lists them)", rej.join(", ")) });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_sanity() {
        assert!(sane_pos(&[1.0, -2.0, 99_999.0]));
        assert!(!sane_pos(&[1.0, f64::NAN, 0.0]));
        assert!(!sane_pos(&[1e6, 0.0, 0.0]));
    }
}
