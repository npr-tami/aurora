//! Aurora.Launchpad: the small window beside Ghost Recon Breakpoint. It starts the game through
//! Steam or Ubisoft Connect, and loads the mod's DLL from `%APPDATA%\Aurora\Bin`. Its own files are
//! listed in `aurora_core::paths`.

#![windows_subsystem = "windows"]

mod folder;
mod install;
mod launchpad;
mod lock;
mod pipe;
mod platform;
mod process;
mod hello;
mod settings;
mod sign;
mod sign_key;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use aurora_core::keys::key_name;
use aurora_core::paths::{self, DLL_FILE};
use aurora_core::proto::{Request, Response};

use platform::Install;
use process::{Game, Proc};

/// The changelog shown in the window.
const CHANGELOG: &str = include_str!("../CHANGELOG.md");
/// "Aurora Launchpad 0.1.0": the window's title. The release exe keeps its own name
/// (tools\package.ps1).
const TITLE: &str = concat!("Aurora Launchpad ", env!("CARGO_PKG_VERSION"));

#[derive(PartialEq, Eq, Clone, Copy)]
enum Pane {
    None,
    Advanced,
    Changelog,
}

fn main() -> eframe::Result {
    // One Launchpad at a time, as Stand's: when one is open (it holds Bin\Launchpad.lock), show
    // that one and start nothing.
    if lock::other_open(&paths::launchpad_lock()) {
        lock::show_other(TITLE);
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_inner_size(launchpad::SMALL)
            .with_resizable(false)
            // Only the close button, as Stand's Launchpad.
            .with_maximize_button(false)
            .with_minimize_button(false),
        ..Default::default()
    };
    eframe::run_native("Aurora.Launchpad", options, Box::new(|cc| Ok(Box::new(App::new(cc.egui_ctx.clone())))))
}

/// The status line when the old `grbmod` folder could not move to `Aurora` (the full reason is in
/// the log).
const FOLDER_BUSY: &str = "Old grbmod folder in use: close the game, reopen.";
/// Bin's DLL stays held this long at most after a load that timed out.
const LATE_HOLD: Duration = Duration::from_secs(300);

/// After this many failed loads in a row, auto inject stops. Inject tries again.
const MAX_FAILED_LOADS: u32 = 3;

/// One more failed load. True when auto inject should stop; the count then starts
/// over, so Inject gets new tries.
fn load_failed(count: &mut u32) -> bool {
    *count += 1;
    if *count >= MAX_FAILED_LOADS {
        *count = 0;
        return true;
    }
    false
}

#[cfg(test)]
mod failed_loads {
    use super::*;

    /// The third failed load in a row stops auto inject.
    #[test]
    fn third_failed_load_stops() {
        let mut n = 0;
        assert_eq!([load_failed(&mut n), load_failed(&mut n), load_failed(&mut n)], [false, false, true]);
        assert_eq!(n, 0, "Inject starts with new tries");
        assert!(!load_failed(&mut n));
    }

    /// "x/y": what is really in, of what is ticked or already in.
    #[test]
    fn injected_line_says_in_of_wanted() {
        assert_eq!(injected_line(1, 1), "Injected 1/1 DLL.");
        assert_eq!(injected_line(1, 2), "Injected 1/2 DLLs.");
        assert_eq!(injected_line(2, 2), "Injected 2/2 DLLs.");
    }

    /// Only names really in the module list count, in any case.
    #[test]
    fn in_game_of_counts_only_whats_in() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let names = s(&["aurora.dll", "grbresearch_dll.dll", "gone.dll"]);
        let game = s(&["GRB_vulkan.exe", "AURORA.DLL", "grbresearch_dll.dll"]);
        assert_eq!(in_game_of(&names, &game), s(&["aurora.dll", "grbresearch_dll.dll"]));
        assert!(in_game_of(&names, &s(&["GRB_vulkan.exe"])).is_empty());
    }
}

/// The game's module names, from one snapshot.
fn module_names(p: &Proc) -> Vec<String> {
    p.modules().into_iter().map(|m| m.name).collect()
}

/// Of `names`, the ones in `in_game` (any case), in `names`' order.
fn in_game_of(names: &[String], in_game: &[String]) -> Vec<String> {
    names.iter().filter(|n| in_game.iter().any(|m| m.eq_ignore_ascii_case(n))).cloned().collect()
}

/// The status line while DLLs are in: how many really are in the game.
fn injected_line(n: usize, of: usize) -> String {
    format!("Injected {n}/{of} DLL{}.", if of == 1 { "" } else { "s" })
}

struct App {
    game: Option<Game>,
    last_poll: Instant,
    proc: Option<Arc<Proc>>,
    /// The mod is in the game and answered a ping. The Launchpad never unloads it; it only notices
    /// when the mod is gone.
    mod_in: bool,
    log: Vec<String>,
    /// The mod's menu key (read from its Hotkeys.txt, never written), for the status line.
    menu_key: u32,
    hotkeys_seen: Option<SystemTime>,
    /// The Launchpad's settings as shown, and as last written.
    cfg: settings::Saved,
    cfg_written: settings::Saved,
    /// The picked store's copy of the game, looked up at start and when the pick changes.
    install: Option<Install>,
    /// Inject by itself whenever the game is up and the mod is not in. Off once the mod has been
    /// in: unloaded from its menu, it stays unloaded.
    auto_inject: bool,
    last_inject_try: Instant,
    /// The game was started with Launch here: only then does auto inject act.
    launched: bool,
    launched_at: Instant,
    /// Inject as soon as the game is up (Inject button, or `--inject` for the test loop), instead
    /// of waiting for the title screen.
    inject_now: bool,
    pane: Pane,
    /// When the game was first seen this run, for the delay.
    game_seen: Option<Instant>,
    /// The game's main window size, to tell the title screen from the splash.
    game_size: Option<(i32, i32)>,
    /// Advanced list: the picked row (0 is the mod's own).
    dll_pick: Option<usize>,
    /// The first DLL row shown: the list shows 4 rows since Options moved under it, and the mouse
    /// wheel scrolls it (review: a 4th Add DLL was drawn nowhere).
    dll_top: usize,
    /// Where the drawing hooks at our last inject, and the mod's own line in Bin\hook.txt.
    hook_injected: Option<settings::Hook>,
    hook_line: Option<String>,
    files_check: Instant,
    /// The last launch problem, shown in the status line until the next try.
    launch_error: Option<String>,
    /// Why Bin's DLL was refused: the short line (as in the status line) and the whole message,
    /// which is the status line's tooltip while that line shows the short one.
    refusal: Option<(String, String)>,
    /// The refusal's own window is open. Closed by its button, or when the refusal goes.
    refusal_open: bool,
    /// The DLLs that really are in the game, by name: Bin's aurora.dll and any DLL in the Advanced
    /// list, ticked or not, found in the game's module list. For the status line, "Injected 2
    /// DLLs.". Counted after each inject and every poll.
    dlls_in: Vec<String>,
    /// The DLLs this Launchpad loaded into this game, by name, with "is Aurora". Cleared when the
    /// game's pid changes. A DLL taken off the Advanced list still counts while it is in.
    loaded: Vec<(String, bool)>,
    /// An Aurora is among `dlls_in` (the tooltip's menu-key line), set by `count_dlls`.
    aurora_counted: bool,
    /// The "of" in "Injected 1/2 DLLs.": the ticked DLLs plus any already in.
    dlls_of: usize,
    /// Bin's DLL, still held read only after a load that timed out: LoadLibraryW may open it later.
    /// Let go once the game has the mod, the game is gone, or after `LATE_HOLD`.
    late_hold: Option<(std::fs::File, Instant)>,
    /// Bin\Launchpad.lock, held while this Launchpad runs; Windows deletes the file when it closes.
    /// None when the folders could not be made.
    _lock: Option<std::fs::File>,
    /// Loads of the DLL that failed in a row.
    failed_loads: u32,
}

impl App {
    fn new(ctx: egui::Context) -> Self {
        launchpad::load_font(&ctx);
        // No slack round widgets: egui counts 5 px around each as on it, which let a button be
        // clicked beside it. `launchpad::hot` decides.
        ctx.all_styles_mut(|s| s.interaction.interact_radius = 0.0);
        let mut log = vec![format!("{TITLE} started")];
        // The folders are made when missing, here and again before every inject. First, before any
        // file is read: an old `grbmod` folder moves to `Aurora` with its files in it.
        let folder_error = match install::ensure_folders() {
            Ok(done) => {
                log.extend(done);
                None
            }
            Err(e) => {
                log.push(e);
                Some(FOLDER_BUSY.to_string())
            }
        };
        let folders_ok = folder_error.is_none();
        // The window's messages also go to Dumper\launchpad.log, from here on: only after the move,
        // as writing earlier would make an `Aurora` before the old folder moved over.
        rotate_log();
        // Bin\Launchpad.lock, once Bin is there. Another Launchpad that opened at the same moment
        // holds it: this one shows that one and closes without touching anything.
        let mut second = false;
        let lock = if folders_ok {
            match lock::take(&paths::launchpad_lock()) {
                Ok(f) => Some(f),
                Err(e) => {
                    second = lock::held_elsewhere(&e);
                    log.push(if second { "Another Launchpad is open: closing this one".to_string() } else { format!("Launchpad.lock not made: {e}") });
                    None
                }
            }
        } else {
            None
        };
        log_to_file(&log);
        if second {
            lock::show_other(TITLE);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let folders_ok = folders_ok && !second;
        // The old Aurora\launchpad.json becomes %LOCALAPPDATA%\Borealis\Aurora.Launchpad\
        // Settings.txt, and moves into Dumper\old. Only once the folders are there: Dumper\old is
        // inside Aurora.
        if folders_ok {
            let carried = settings::carry_over();
            log_to_file(&carried);
            log.extend(carried);
        }
        let cfg = settings::load();
        let inject = std::env::args().any(|a| a == "--inject");
        let mut app = App {
            game: None,
            last_poll: ago(Duration::from_secs(10)),
            proc: None,
            mod_in: false,
            log,
            menu_key: aurora_core::read_keys().0.menu,
            hotkeys_seen: modified(&aurora_core::hotkeys_path()),
            install: platform::find(cfg.platform, cfg.game_dir(cfg.platform)),
            cfg_written: cfg.clone(),
            cfg,
            auto_inject: inject,
            last_inject_try: ago(Duration::from_secs(60)),
            launched: false,
            launched_at: Instant::now(),
            inject_now: inject,
            pane: Pane::None,
            game_seen: None,
            game_size: None,
            dll_pick: None,
            dll_top: 0,
            hook_injected: None,
            hook_line: None,
            files_check: ago(Duration::from_secs(1)),
            launch_error: folder_error,
            refusal: None,
            late_hold: None,
            refusal_open: false,
            dlls_in: Vec::new(),
            loaded: Vec::new(),
            aurora_counted: false,
            dlls_of: 0,
            _lock: lock,
            failed_loads: 0,
        };
        // Only once the old folders have moved: either would make an `Aurora` beside them.
        if folders_ok {
            // Written now only when it was missing (first start, or read from an old file that
            // could not be carried over).
            if !paths::launchpad_settings().exists() {
                app.write_settings();
            }
            app.check_dll();
        }
        match &app.install {
            Some(i) => app.say(format!("{}: {}", app.cfg.platform.name(), i.dir.display())),
            None => app.say(format!("{}: game not found", app.cfg.platform.name())),
        }
        app
    }

    fn write_settings(&mut self) {
        match settings::save(&self.cfg) {
            Ok(()) => self.cfg_written = self.cfg.clone(),
            Err(e) => self.say(format!("settings not saved: {e}")),
        }
    }

    /// Save Settings.txt when a setting changed.
    fn keep_settings(&mut self) {
        if self.cfg != self.cfg_written {
            self.write_settings();
        }
    }

    /// Store picked or folder changed: look the game up again.
    fn find_install(&mut self) {
        self.install = platform::find(self.cfg.platform, self.cfg.game_dir(self.cfg.platform));
    }

    /// The mod's hook.txt line and its menu key (both only read), twice a second.
    fn follow_files(&mut self) {
        if self.files_check.elapsed() < Duration::from_millis(500) {
            return;
        }
        self.files_check = Instant::now();
        self.hook_line = std::fs::read_to_string(paths::hook_note()).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let t = modified(&aurora_core::hotkeys_path());
        if t != self.hotkeys_seen {
            self.hotkeys_seen = t;
            self.menu_key = aurora_core::read_keys().0.menu;
        }
    }

    fn say(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        log_to_file(std::slice::from_ref(&msg));
        self.log.push(msg);
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    /// Bin's DLL can't be injected: the whole message in its own window and in the log; auto inject
    /// stops. The status line shows nothing of it; `launch_error` still carries the short line so
    /// the other status texts stay away and `clear_refusal` knows it.
    fn refuse(&mut self, short: String, full: String) {
        self.say(full.clone());
        self.launch_error = Some(short.clone());
        self.refusal = Some((short, full));
        self.refusal_open = true;
        self.auto_inject = false;
    }

    /// The refusal's window is done with: closed; the status line stays empty.
    pub(crate) fn close_refusal(&mut self) {
        self.refusal_open = false;
    }

    /// The last refusal leaves the status line (a new try says why again if it still holds).
    fn clear_refusal(&mut self) {
        if let Some((short, _)) = self.refusal.take()
            && self.launch_error.as_deref() == Some(short.as_str())
        {
            self.launch_error = None;
        }
        self.refusal_open = false;
    }

    /// Say so when Bin has no DLL. Reads only: the Launchpad never writes Bin's DLL.
    fn check_dll(&mut self) {
        if install::check_dll() == install::Dll::Missing {
            // On the status line too, before any try.
            let (short, full) = (install::missing_short(), install::missing_text());
            self.say(full.clone());
            self.launch_error = Some(short.clone());
            self.refusal = Some((short, full));
            self.refusal_open = true;
        }
    }

    fn poll_game(&mut self) {
        if self.last_poll.elapsed() < Duration::from_secs(2) {
            return;
        }
        self.last_poll = Instant::now();
        let found = process::find_game(self.install.as_ref().map(|i| i.dir.as_path()));
        // Bin's DLL held after a load that timed out: let go once the game has the mod, the game is
        // gone, or after LATE_HOLD.
        if let Some((_, since)) = &self.late_hold
            && (since.elapsed() > LATE_HOLD || found.is_none() || self.proc.as_ref().is_some_and(|p| p.has_module(DLL_FILE)))
        {
            self.late_hold = None;
        }
        // The game restarts itself once while starting up (the store, then Ubisoft Connect): a game
        // that closes within 2 minutes of showing up is that restart, so keep waiting.
        let restart = self.game_seen.is_some_and(|t| t.elapsed() < Duration::from_secs(120));
        if self.launched && self.game.is_some() && found.is_none() && restart {
            self.launched_at = Instant::now();
            self.say("game restarted itself, waiting for it");
        }
        // Back to Launch when the launched game closes, or never showed up in 3 minutes.
        if self.launched && ((self.game.is_some() && found.is_none() && !restart) || (found.is_none() && self.launched_at.elapsed() > Duration::from_secs(180))) {
            self.launched = false;
            self.auto_inject = false;
        }
        if found != self.game {
            match &found {
                Some(g) => self.say(format!("game found: {}, {} (pid {})", g.exe, g.api(), g.pid)),
                None => self.say("game not running"),
            }
            if found.as_ref().map(|g| g.pid) != self.proc.as_ref().map(|p| p.pid) {
                self.proc = None;
                self.mod_in = false;
                self.loaded.clear();
                self.failed_loads = 0;
            }
            self.game = found;
        }
        // Unloaded from its own menu: back to Inject, without injecting again by itself.
        if self.mod_in && self.proc.as_ref().is_some_and(|p| !p.has_module(DLL_FILE)) {
            self.mod_in = false;
            self.auto_inject = false;
            self.say("the mod unloaded itself (from its menu)");
        }
        self.count_dlls();
        self.game_size = self.game.as_ref().and_then(|g| process::window_size(g.pid));
    }

    fn attach(&mut self) {
        let Some(g) = self.game.clone() else {
            self.say("nothing to attach to");
            return;
        };
        match Proc::open(g.pid) {
            Ok(p) => {
                self.say(format!("attached to pid {}", g.pid));
                self.proc = Some(p);
            }
            Err(e) => self.say(e),
        }
    }

    fn inject(&mut self) {
        let Some(p) = self.proc.clone() else {
            self.say("attach first");
            return;
        };
        // A refusal from before goes with this try; it says why again if it still holds.
        self.clear_refusal();
        match install::ensure_folders() {
            Ok(done) => done.into_iter().for_each(|l| self.say(l)),
            // The old folder could not move: injecting now would start the mod without the player's
            // files.
            Err(e) => {
                self.say(e);
                self.launch_error = Some(FOLDER_BUSY.into());
                return;
            }
        }
        // DirectX 11 is no longer refused: the mod draws on both builds. The mod's own row unticked
        // in Advanced: load only the others, once, and stop trying.
        if !self.cfg.aurora_on && !p.has_module(DLL_FILE) {
            self.say(format!("{DLL_FILE} is unticked in Advanced; loading only the others"));
            self.inject_extras();
            self.auto_inject = false;
            self.inject_now = false;
            return;
        }
        // Never two Auroras, under any name: another-named Aurora in the game means Bin's DLL stays
        // out; only the extras load.
        if let Some(other) = self.aurora_running(&p).filter(|n| !n.eq_ignore_ascii_case(DLL_FILE)) {
            self.say(format!("{other} is an Aurora and is in the game; {DLL_FILE} not loaded (never two), loading only the others"));
            self.inject_extras();
            self.count_dlls();
            self.auto_inject = false;
            self.inject_now = false;
            return;
        }
        if install::check_dll() == install::Dll::Missing {
            self.refuse(install::missing_short(), install::missing_text());
            return;
        }
        if !self.load_aurora(&p, &paths::dll(), true) {
            return;
        }
        self.connect();
        self.inject_extras();
        self.count_dlls();
    }

    /// Which DLLs are really in the game: Bin's aurora.dll and every DLL in the Advanced list,
    /// ticked or not, by their names in the game's module list (`Proc::has_module`). A tick says
    /// what to load, not what is in. Nothing while no game is attached. Not every frame: a module
    /// list is a snapshot of the game (`poll_game`, every 2 s, and after each inject).
    fn count_dlls(&mut self) {
        let Some(p) = self.proc.as_ref() else {
            self.dlls_in.clear();
            self.dlls_of = 0;
            self.aurora_counted = false;
            return;
        };
        // One module snapshot per poll (or inject), never per frame.
        let in_game = module_names(p);
        self.dlls_in = in_game_of(&self.dll_names(), &in_game);
        // "of": the ticked ones plus whatever is in already.
        let ticked = self.ticked_names();
        self.dlls_of = self.dlls_in.len() + ticked.iter().filter(|t| !self.dlls_in.iter().any(|n| n.eq_ignore_ascii_case(t))).count();
        // An Aurora among them, cheaply (no file reads every 2 s): Bin's name, a name that starts
        // with "aurora", or one this Launchpad loaded as Aurora.
        self.aurora_counted = self.dlls_in.iter().any(|n| n.get(..6).is_some_and(|s| s.eq_ignore_ascii_case("aurora")) || self.loaded.iter().any(|(l, a)| *a && l.eq_ignore_ascii_case(n)));
    }

    /// Every DLL the Launchpad knows, by name, once each: Bin's, the Advanced list's, and the ones
    /// it loaded into this game (also when taken off the list).
    fn dll_names(&self) -> Vec<String> {
        let list = self.cfg.extra_dlls.iter().filter_map(|d| Path::new(&d.path).file_name().map(|n| n.to_string_lossy().into_owned()));
        let mut names = vec![DLL_FILE.to_string()];
        for n in list.chain(self.loaded.iter().map(|(n, _)| n.clone())) {
            if !names.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
                names.push(n);
            }
        }
        names
    }

    /// The DLLs Inject would load, by name: Bin's when ticked, then the ticked Add ones.
    fn ticked_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.cfg.aurora_on {
            names.push(DLL_FILE.to_string());
        }
        names.extend(self.cfg.extra_dlls.iter().filter(|d| d.on).filter_map(|d| Path::new(&d.path).file_name().map(|n| n.to_string_lossy().into_owned())));
        names
    }

    /// One DLL loaded into this game by this Launchpad, for the count and the Aurora check.
    fn note_loaded(&mut self, name: &str, aurora: bool) {
        if !self.loaded.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
            self.loaded.push((name.to_string(), aurora));
        }
    }

    /// The name of an Aurora that is in the game, under any name. Never two. From the game's
    /// modules themselves (one snapshot), not from what this Launchpad remembers: Bin's aurora.dll
    /// by name, an Aurora this Launchpad loaded, then every module file outside the Windows folder
    /// that `sign::is_aurora` calls Aurora (name, pipe name, card or stamp), so a copy an earlier
    /// Launchpad loaded is seen too. Reads files: only on inject, never per frame.
    fn aurora_running(&self, p: &Proc) -> Option<String> {
        let files = p.module_files();
        let has = |n: &str| files.iter().any(|(m, _)| m.name.eq_ignore_ascii_case(n));
        if has(DLL_FILE) {
            return Some(DLL_FILE.to_string());
        }
        if let Some((n, _)) = self.loaded.iter().find(|(n, aurora)| *aurora && has(n)) {
            return Some(n.clone());
        }
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()).to_ascii_lowercase();
        files.iter().skip(1).find_map(|(m, path)| {
            let outside = !path.to_ascii_lowercase().starts_with(&windir);
            (outside && sign::is_aurora(Path::new(path))).then(|| m.name.clone())
        })
    }

    /// Bin's DLL, or an Add DLL that is Aurora (`sign::is_aurora`), into the game the gatekeeper's
    /// way: the file's stamp and card checked (held open until loaded), loaded, the module in the
    /// game matched against the file, the hello, the go. A copy already in the game gets the match
    /// and the hello without the go. True when it is in and accepted; else the refusal is said and
    /// shown and nothing more is loaded. `own`: Bin's DLL (the hook pick and the failed-load count
    /// are its).
    fn load_aurora(&mut self, p: &Proc, dll: &Path, own: bool) -> bool {
        let name = dll.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // The player's handshake: only with Aurora's stamp inside it and a build card that speaks
        // this protocol (Gatekeeper step 1). Held open, read only, until the game has loaded it. A
        // copy already in the game gets the same check on its file.
        let checked = match sign::check(dll) {
            Ok(c) => {
                self.say(format!("{name} checked: {} (stamp and build card)", c.card.describe()));
                c
            }
            Err(r) => {
                self.refuse(r.short, r.full);
                return false;
            }
        };
        let already_in = p.has_module(&name);
        if already_in {
            self.say(format!("{name} is already in the game"));
        } else {
            match p.inject(dll) {
                Ok(()) => {
                    if own {
                        self.hook_injected = Some(self.cfg.hook);
                        self.failed_loads = 0;
                    }
                    self.say(format!("{name} loaded"));
                    self.note_loaded(&name, true);
                }
                Err(e) => {
                    // Timed out: LoadLibraryW may still open the file, so it stays held. Any other
                    // failure: it has let go of the file.
                    if e == Proc::TIMED_OUT {
                        self.late_hold = Some((checked.file, Instant::now()));
                    }
                    self.say(format!("inject failed: {e}"));

                    // after 70; the status line then says "Not injected" and Inject gives 3 new
                    // tries.
                    if own && load_failed(&mut self.failed_loads) {
                        self.auto_inject = false;
                        self.inject_now = false;
                        self.say(format!("auto inject stopped after {MAX_FAILED_LOADS} failed loads; press Inject to try again"));
                    }
                    return false;
                }
            }
        }
        // Gatekeeper ideas 2 and 3, option B: the module in the game must be the checked file, and
        // must answer a fresh challenge with what the file says; else it is not accepted: no go (it
        // unloads itself), no pipe, no extras.
        drop(checked.file);
        if let Err(r) = self.hello(p, &name, &checked.card, &checked.signed, already_in) {
            self.refuse(r.short, r.full);
            return false;
        }
        true
    }

    /// Up to three tries at the hello (the DLL waits 15 s for the go), 5 s each; then the go,
    /// unless the mod was already in (it had its go). Err: why it is not accepted.
    fn hello(&mut self, p: &Proc, name: &str, card: &aurora_core::card::CardInfo, signed: &[u8], already_in: bool) -> Result<(), sign::Refused> {
        let t = Instant::now();
        // Idea 2's after-load half: the module in the game is the checked file (headers, size, the
        // card at its address). A copy with the same code loaded another way would answer the hello
        // as the file would.
        if let Err(e) = hello::matches_file(p, name, signed) {
            let what = if already_in { "no extras are loaded; unload it from its menu and inject again" } else { "it gets no go and unloads itself; inject again" };
            return Err(sign::Refused { short: format!("Not accepted: {name} in the game isn't the checked file."), full: format!("not accepted: {e}; {what}") });
        }
        let mut last = String::new();
        for try_no in 1..=3 {
            match hello::ask(p, name, card, signed) {
                Ok(answer) => {
                    self.say(format!("{name} accepted: {} answered the hello from pid {} at {:#x} (try {try_no}, {} ms)", answer.version, answer.pid, answer.module, t.elapsed().as_millis()));
                    if already_in {
                        return Ok(());
                    }
                    return hello::go(p, name, signed).map_err(|e| sign::Refused { short: "Not accepted: the go did not reach the mod.".into(), full: format!("not accepted: the hello checked out but the go failed ({e}); the mod unloads itself, inject again") });
                }
                Err(e) => {
                    self.say(format!("hello try {try_no}: {e}"));
                    last = e;
                    if t.elapsed() > Duration::from_secs(12) {
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        let what = if already_in { "no extras are loaded" } else { "it gets no go and unloads itself; inject again" };
        Err(sign::Refused { short: format!("Not accepted: {name} failed the hello."), full: format!("not accepted: the copy of {name} in the game did not answer the hello as the checked file says ({last}); {what}") })
    }

    /// Ask the mod "are you there?" once, then let go of the pipe: the mod keeps it free for its
    /// own Unload, and runs on whether the Launchpad stays open or not.
    fn connect(&mut self) {
        match pipe::Pipe::connect(Duration::from_secs(5)) {
            Ok(mut pipe) => match pipe.call(&Request::Ping) {
                Ok(Response::Pong { pid, proto, dll_version }) => {
                    if proto != aurora_core::PROTO_VERSION {
                        self.say(format!("the mod in the game is another version (protocol {proto}, this Launchpad {}); unload it from its menu and inject again", aurora_core::PROTO_VERSION));
                    }
                    self.say(format!("mod {dll_version} answered (pid {pid})"));
                    self.mod_in = true;
                    self.auto_inject = false;
                    // Injected, so the start-up restart is over: if the game goes now, it closed or
                    // crashed, and Launch should come back at once (not after 3 minutes).
                    self.game_seen = Some(ago(Duration::from_secs(3600)));
                }
                Ok(other) => self.say(format!("odd answer to ping: {other:?}")),
                Err(e) => self.say(format!("ping failed: {e}")),
            },
            Err(e) => self.say(format!("pipe: {e}")),
        }
    }

    /// `quit.request`: close the Launchpad. It never unloads the mod; the mod's own menu does
    /// (`unload.request` is the mod's now, see `aurora_core::paths`).
    fn requests(&mut self, ctx: &egui::Context) {
        if self.last_poll.elapsed() > Duration::from_millis(100) {
            return;
        }
        let quit = paths::quit_request();
        if quit.exists() {
            let _ = std::fs::remove_file(&quit);
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        }
    }

    /// The game's main window is up (title screen or later), not just its small splash, and the
    /// game has run a while (past its own restart).
    fn title_up(&self) -> bool {
        self.game_size.is_some_and(|(w, h)| w >= 800 && h >= 600) && self.game_seen.is_some_and(|t| t.elapsed() >= Duration::from_secs(10))
    }

    fn launch(&mut self) {
        match platform::launch(self.cfg.platform, self.install.as_ref()) {
            Ok(msg) => {
                self.launch_error = None;
                self.launched = true;
                self.launched_at = Instant::now();
                self.inject_now = false;
                self.auto_inject = self.cfg.auto_inject;
                self.say(msg);
            }
            Err(e) => {
                self.say(format!("launch: {e}"));
                self.launch_error = Some(e);
            }
        }
    }

    /// "Injected 2 DLLs.": only what is really in the game. `status_tip` names them, with the menu
    /// key.
    fn status(&self) -> String {
        injected_line(self.dlls_in.len(), self.dlls_of)
    }

    /// The status line's tooltip while DLLs are in: the menu key when an Aurora is in, and which
    /// DLLs are in.
    fn status_tip(&self) -> String {
        let names = self.dlls_in.join("\n");
        if self.aurora_counted {
            format!("{} opens the menu.\n{names}", key_name(self.menu_key))
        } else {
            names
        }
    }
}

/// Adds lines to `Dumper\launchpad.log`, stamped like the mod's dll.log (`[unix seconds]`). Skipped
/// while `Aurora` is not there yet (see `paths::dumper_for_writing`); a line that can't be written
/// is left out, never an error.
fn log_to_file(lines: &[String]) {
    use std::io::Write;
    if lines.is_empty() || paths::dumper_for_writing().is_none() {
        return;
    }
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(paths::launchpad_log()) else {
        return;
    };
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for line in lines {
        let _ = writeln!(f, "[{now}] {line}");
    }
}

/// At start: a log over 1 MB becomes `launchpad.old.log` (replacing the one before), so the file
/// never grows without end.
fn rotate_log() {
    let log = paths::launchpad_log();
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&log, log.with_file_name("launchpad.old.log"));
    }
}

/// When a file was last written.
fn modified(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// `d` ago, for times that must count as long past. Rust's clock on Windows starts
/// when Windows starts, so `Instant::now - d` panics (and the Launchpad closes) while Windows has
/// run less than `d`. Then this gives as far back as the clock goes.
pub(crate) fn ago(d: Duration) -> Instant {
    let now = Instant::now();
    if let Some(t) = now.checked_sub(d) {
        return t;
    }
    // The furthest step back that still works, to a millisecond.
    let (mut ok, mut too_far) = (Duration::ZERO, d);
    while too_far - ok > Duration::from_millis(1) {
        let mid = ok + (too_far - ok) / 2;
        if now.checked_sub(mid).is_some() {
            ok = mid;
        } else {
            too_far = mid;
        }
    }
    now.checked_sub(ok).unwrap_or(now)
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {

        // while the window is minimized (this then does not run).
        egui::CentralPanel::default().show(ui, |ui| self.launcher(ui));
    }

    /// The Launchpad's jobs (game check, quit.request, auto inject, files,
    /// settings). Minimized, they stopped before: quit.request waited until the window came back,
    /// and so did auto inject and the unload check.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_game();
        self.requests(ctx);
        match (&self.game, self.game_seen) {
            (Some(_), None) => self.game_seen = Some(Instant::now()),
            (None, Some(_)) => self.game_seen = None,
            _ => {}
        }
        let waited = self.game_seen.is_some_and(|t| t.elapsed() >= Duration::from_secs(self.cfg.inject_delay.into()));
        // Every 5 s at most, so a game still starting up is not hammered.
        if self.auto_inject && waited && !self.mod_in && self.last_inject_try.elapsed() > Duration::from_secs(5) {
            self.last_inject_try = Instant::now();
            if self.proc.is_none() {
                self.attach();
            }
            // Auto inject after Launch waits for the title screen: the game's own start and its
            // restart are left alone, and we are ready on load-in.
            if self.proc.is_some() && (self.inject_now || self.title_up()) {
                self.inject();
            }
        }
        self.follow_files();
        self.keep_settings();
        // The jobs above run 10 times a second; plenty and cheap.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod clock {
    use super::*;

    /// `ago` never panics, also for more time than the clock has.
    #[test]
    fn ago_never_panics() {
        let huge = Duration::from_secs(u64::MAX / 4);
        // The clock does not go back that far, so the old `Instant::now - huge` panics.
        assert!(Instant::now().checked_sub(huge).is_none());
        let t = ago(huge);
        assert!(t <= Instant::now());
        // As far back as the clock goes: one more second back is not there.
        assert!(t.checked_sub(Duration::from_secs(1)).is_none());
        // A short time works as before.
        assert!(ago(Duration::from_millis(5)).elapsed() >= Duration::from_millis(5));
        let hour = Duration::from_secs(3600);
        assert!(ago(hour).elapsed() >= hour || Instant::now().checked_sub(hour).is_none());
    }
}
