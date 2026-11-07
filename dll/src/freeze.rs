//! Freezes: values the DLL keeps writing back every few milliseconds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::{log, memory};

struct Freeze {
    addr: u64,
    bytes: Vec<u8>,
    /// Stop when these bytes at this address change (the object was freed or replaced).
    guard: Option<(u64, Vec<u8>)>,
}

static FREEZES: OnceLock<Mutex<HashMap<String, Freeze>>> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

fn map() -> &'static Mutex<HashMap<String, Freeze>> {
    FREEZES.get_or_init(|| {
        if let Ok(mut w) = WORKER.lock() {
            *w = Some(std::thread::spawn(worker));
        }
        Mutex::new(HashMap::new())
    })
}

/// Stop the worker thread and wait for it. Needed before the DLL is unloaded: a thread still
/// running inside the DLL crashes the game when the code goes away.
pub fn shutdown() {
    clear();
    STOP.store(true, Ordering::SeqCst);
    if let Ok(mut w) = WORKER.lock()
        && let Some(h) = w.take()
    {
        let _ = h.join();
        log("freeze worker stopped");
    }
}

/// Start, replace or (with empty bytes) stop a freeze.
pub fn set(id: &str, addr: u64, bytes: Vec<u8>) -> Result<(), String> {
    set_guarded(id, addr, bytes, None)
}

/// Same, with a guard: the freeze stops by itself once the guard bytes change.
pub fn set_guarded(id: &str, addr: u64, bytes: Vec<u8>, guard: Option<(u64, Vec<u8>)>) -> Result<(), String> {
    let mut m = map().lock().map_err(|_| "freeze lock poisoned")?;
    if bytes.is_empty() {
        if m.remove(id).is_some() {
            log(&format!("freeze {id} off"));
        }
        return Ok(());
    }
    memory::write(addr, &bytes)?;
    log(&format!("freeze {id} on: {} bytes at {addr:#x}", bytes.len()));
    m.insert(id.to_string(), Freeze { addr, bytes, guard });
    Ok(())
}

/// Ids of every running freeze, for the freeze guard's report.
pub fn ids() -> Vec<String> {
    map().lock().map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

/// Is this freeze still running (not stopped, not dropped by its guard)?
pub fn running(id: &str) -> bool {
    map().lock().map(|m| m.contains_key(id)).unwrap_or(false)
}

/// Is any freeze whose id starts with `prefix` running?
pub fn any_with_prefix(prefix: &str) -> bool {
    map().lock().map(|m| m.keys().any(|k| k.starts_with(prefix))).unwrap_or(false)
}

pub fn clear() {
    if let Ok(mut m) = map().lock()
        && !m.is_empty()
    {
        log(&format!("{} freezes cleared", m.len()));
        m.clear();
    }
}

fn worker() {
    loop {
        std::thread::sleep(Duration::from_millis(15));
        if STOP.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut m) = map().lock() else { continue };
        let mut dead = Vec::new();
        for (id, f) in m.iter() {
            if let Some((ga, gb)) = &f.guard
                && memory::read(*ga, gb.len()).ok().as_ref() != Some(gb)
            {
                dead.push(id.clone());
                continue;
            }
            if memory::write(f.addr, &f.bytes).is_err() {
                dead.push(id.clone());
            }
        }
        for id in dead {
            log(&format!("freeze {id} dropped: address went away"));
            m.remove(&id);
        }
    }
}
