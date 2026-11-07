//! Watch on request: a `watch.request` file holding `addr millis [w|a|x key]` watches that address
//! with a hardware watchpoint and writes the hits to `watch.txt` as JSON.
//! - `w` (the default): who writes the 4 bytes at addr (up to 40 writers).
//! - `a`: who reads or writes them (up to 40).
//! - `x`: who runs the code at addr, one hit per value of register number `key` (0 rax 1 rbx 2 rcx
//!   3 rdx 4 rsi 5 rdi 6 rbp 7 rsp 8..15 r8..r15; default 1), up to 256 (all the catch list holds).
//!
//! `addr` is hex, or `exe+offset`. `millis` is 100 to 30000 (default 5000). A watch ends early once
//! 256 hits are kept; `watch.txt` then says `"ended_early": true`, and `total` counts only the hits
//! until then.

use std::sync::Mutex;

use crate::{folder, hwbp, log, parse_addr, take_request};

static HANDLE: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

/// Called by the worker twice a second.
pub fn check_request() {
    let Some(text) = take_request("watch.request") else { return };
    let mut parts = text.split_whitespace();
    let addr = parts.next().and_then(parse_addr);
    let millis = parts.next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(5000).clamp(100, 30_000);
    let mode = parts.next();
    let access = mode == Some("a");
    let exec = mode == Some("x");
    let key = parts.next().and_then(|k| k.parse::<usize>().ok()).unwrap_or(1).min(15);
    let Some(addr) = addr else {
        log("watch request: bad address");
        return;
    };
    let Ok(mut slot) = HANDLE.lock() else { return };
    if slot.as_ref().is_some_and(|h| !h.is_finished()) {
        log("watch request: already running");
        return;
    }
    *slot = Some(std::thread::spawn(move || {
        log(&format!("watch request: {addr:#x} for {millis} ms ({})", if exec { "runs" } else if access { "access" } else { "writers" }));
        let r = if exec {
            hwbp::watch(addr, 0, millis, hwbp::MAX_RAW as u32, Some(key), 1)
        } else {
            hwbp::watch(addr, 4, millis, 40, None, if access { 3 } else { 1 })
        };
        let out = match r {
            Ok(w) => serde_json::json!({ "addr": addr, "total": w.total, "ended_early": w.ended_early, "hits": w.hits }).to_string(),
            Err(e) => serde_json::json!({ "addr": addr, "error": e }).to_string(),
        };
        let _ = std::fs::write(folder().join("watch.txt"), out);
        log("watch request: done");
    }));
}

/// Unload: end a running watch (its watchpoint comes off every thread) and wait for it.
pub fn shutdown() {
    hwbp::shutdown();
    if let Some(h) = HANDLE.lock().ok().and_then(|mut h| h.take()) {
        let _ = h.join();
    }
}
