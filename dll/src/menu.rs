//! The in-game menu: Stand's look and keys, drawn into the game's own frames by vkdraw, so it works
//! with the Aurora window closed.
//!
//! Keys, as in Stand: the menu key (F4 by default) opens and closes, Right Ctrl and Right Shift go
//! to the next and previous section (round and round), Up and Down move in the list, Enter does it.
//! Also: Right opens and raises a value, Left lowers a value or goes back, Backspace goes back; the
//! number pad works like the arrows. Enter on a number opens the command box with its command and
//! value, as Stand's, to type a new one; Left and Right still step it. Rows that are a list of
//! names (colours, Style) go to the next one on Enter. Delete on a saved spot removes it. Menu >
//! Hotkeys: Enter on one waits for the new key, Escape cancels that, Delete removes the key. While
//! the menu is open those keys are kept from the game (dinput.rs and the window messages here).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use aurora_core::keys::key_name;
use aurora_core::menuview::{Kind, NEW_BOX, Painter, Row, Spot, View};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Input::{GetRawInputData, HRAWINPUT, RAWINPUT, RAWINPUTHEADER, RID_INPUT, RIM_TYPEKEYBOARD};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GWLP_WNDPROC, GetWindowLongPtrW, SetWindowLongPtrW, WM_CHAR, WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_SYSCHAR,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::{credits, entities, fly, gearscore, godmode, log, memory, tp, vkdraw};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    SelfTab,
    /// Self > Movement, Fly inside.
    Movement,
    Fly,
    /// Game > Progress, and Progress > Editor with gear score and credits (skill points later).
    Progress,
    Editor,
    /// Editor > Gear Score: new drops' score, what the player has on and owns; Credits: the credits
    /// and Pick Other Points.
    GearScore,
    Credits,
    /// Self > Weapons.
    Weapon,
    /// Aurora > Experiments > Silent Aim: all its rows in one list. It was in Self > Weapons until
    /// the player moved it into Experiments.
    SilentAim,
    Teleport,
    World,
    Esp,
    /// World > ESP > Targets: who gets drawn; Drawing: how.
    EspTargets,
    EspDrawing,
    EspColours,
    /// World > Target: pick, yeet and bring.
    Target,
    /// Game-feel switches.
    Game,
    /// Game > Disables: switches that turn a game restriction off.
    Disables,
    /// Game > Rendering: what the player sees, after Disables.
    Rendering,
    /// The menu's own things, the bottom tab, named "Aurora" as Stand's "Stand" tab.
    Menu,
    /// Menu > Settings: Appearance (Position inside), Notifications, Warnings, then Mouse Support.
    MenuSettings,
    Appearance,
    Position,
    /// Appearance > Notifications: how they look.
    NoteLook,
    /// Appearance's lists.
    MenuColours,
    Tabs,
    AddressBar,
    /// Aurora > Settings > Notifications, as Stand's: only Suppress Generic Responses.
    Notifications,
    CustomPosition,
    Timing,
    Suppress,
    /// Menu > Settings > Warnings, as Stand's.
    Warnings,
    AutoProceed,
    /// What `findc` found: every row with the word in its name.
    Find,
    /// Menu > Experiments: untested switches. Unhidden behind a warning page.
    Experiments,
    ExpWarn,
    /// Menu > Profiles, as Stand's: a page per profile (its number in PROFILE_NAMES), and the
    /// question before deleting one.
    Profiles,
    Profile(u32),
    ProfileDelete(u32),
    /// The "O" menu of a row (Stand's), its hotkeys, one of them.
    Ctx,
    RowHotkeys,
    CtxKey(u32),
    /// Menu > Saved Commands.
    Saved,
    /// Menu > Commands With Non-Default State, as Stand's.
    NonDefault,
    /// Menu > Hotkey List: every hotkey in one place.
    HotkeyList,
    /// "Are you absolutely sure?" before unloading, as Stand's.
    Unload,
    /// What a risky row can do, read once before it works (as Stand's).
    Risk,
    /// Which of the points the shop read are the credits.
    CreditsPicker,
    OtherPick,
}

// Things with both a switch and a value get their own page, like ESP; Teleport sits at the top of
// World. Game holds game-feel switches, Menu the menu's own, its looks under Menu > Settings >
// Appearance. Weapon is no tab any more: it is Self > Weapons. The last tab is "Aurora", as Stand's
// "Stand".
const SECTIONS: [(Page, &str); 4] = [(Page::SelfTab, "Self"), (Page::World, "World"), (Page::Game, "Game"), (Page::Menu, "Aurora")];

fn page_name(p: Page) -> String {
    match p {
        Page::Movement => "Movement".into(),
        Page::Fly => "Fly".into(),
        Page::Weapon => "Weapons".into(),
        Page::SilentAim => "Silent Aim".into(),
        Page::Progress => "Progress".into(),
        Page::Editor => "Editor".into(),
        Page::GearScore => "Gear Score".into(),
        Page::Credits => "Credits".into(),
        Page::Teleport => "Teleport".into(),
        Page::Esp => "ESP".into(),
        Page::EspTargets => "Targets".into(),
        Page::EspDrawing => "Drawing".into(),
        Page::EspColours => "Colours".into(),
        Page::Target => "Target".into(),
        Page::MenuSettings => "Settings".into(),
        Page::Appearance => "Appearance".into(),
        Page::Position => "Position".into(),
        Page::NoteLook => "Notifications".into(),
        Page::ExpWarn => "Experiments".into(),
        Page::MenuColours => "Colours".into(),
        Page::Tabs => "Tabs".into(),
        Page::AddressBar => "Address Bar".into(),
        Page::Notifications => "Notifications".into(),
        Page::CustomPosition => "Custom Position".into(),
        Page::Timing => "Timing".into(),
        Page::Suppress => "Suppress Generic Responses".into(),
        Page::Warnings => "Warnings".into(),
        Page::AutoProceed => "Automatically Proceed On Warnings".into(),
        Page::Find => format!("Find Command: {}", find_text()),
        Page::Disables => "Disables".into(),
        Page::Rendering => "Rendering".into(),
        Page::Experiments => "Experiments".into(),
        Page::Profiles => "Profiles".into(),
        Page::Profile(n) => profile_name(n),
        Page::ProfileDelete(_) => "Delete".into(),
        Page::Ctx => ctx().0.rsplit(" > ").next().unwrap_or("").to_string(),
        Page::RowHotkeys => "Hotkeys".into(),
        Page::CtxKey(vk) => key_name(vk),
        Page::Saved => "Saved Commands".into(),
        Page::NonDefault => "Commands With Non-Default State".into(),
        Page::HotkeyList => "Hotkeys".into(),
        Page::Unload => "Unload Aurora".into(),
        Page::Risk => "Warning".into(),
        Page::CreditsPicker => "Credits".into(),
        Page::OtherPick => "Pick Other Points".into(),
        _ => SECTIONS.iter().find(|x| x.0 == p).map(|x| x.1).unwrap_or("").into(),
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Id {
    God,
    Fly,
    FlySpeed,
    FlySprint,
    SafeFly,
    /// Fly > Hop Up and Hop Height. Stand While Flying's row went: it is always on
    /// while Fly is on (`fly::follow_fly`).
    HopUp,
    HopHeight,
    /// Self > Undetected: enemy NPCs' eyesight and alert meter only (`unseen.rs`). The menu label
    /// is "Undetected"; it is not a bypass and has nothing to do with anti-cheat.
    Unseen,
    /// Self > Ignore only me: enemy NPCs drop what they perceive of the player, at their perception
    /// inbox. Off by default.
    UnseenInbox,
    /// Aurora > Experiments > Fly Water Fixes: Wait For Calm and Snap Guard (`fly.rs`), Let Game
    /// Fall (`airwalk.rs`).
    FlyCalm,
    FlyGuard,
    FlyLetFall,
    /// Game > Disables > NVG In Erewhon: in hubs the player's goggles group runs in the hub's
    /// goggles leaf's place, so the player's NVG key puts them on. Off by default.
    NvgZone,
    NoRecoil,
    NoShake,
    /// Game > Rendering > No NVG Overlay: goggles down, no green screen.
    NoNvgOverlay,
    NoSway,
    Stamina,
    Damage,
    /// Weapon > Fire Rate: the player's guns only.
    FireRate,
    Accuracy,
    Ammo,
    /// Weapon > Infinite Reserve.
    Reserve,
    /// Weapon > Silent Aim, its FOV (degrees) and Range (HUD metres). AI soldiers and drones only,
    /// never players.
    SilentAim,
    SilentFov,
    SilentRange,
    /// Silent Aim > Show FOV Circle and Priority.
    SilentCircle,
    SilentPriority,
    /// Silent Aim > Magic Bullets, No Range Loss, Instant Hit.
    SilentMagic,
    SilentNoRange,
    SilentInstant,
    /// Self > Weapons > No Bullet Drop (it rides Silent Aim's hook, so it lives in `silentaim.rs`).
    SilentNoDrop,
    GearValue,
    /// Gear Score: the player's gear level, the item in a slot (`gearscore::SLOTS`), all the player
    /// owns.
    GearTotal,
    GearSlot(usize),
    GearOwned,
    WhereAmI,
    /// Teleport > Coordinates: X, Y or Z (0, 1, 2), and going there.
    Coord(usize),
    GoCoords,
    Credits,
    CreditsPick(u64),
    PickOther,
    /// Pick Other Points > Re-scan: watch again.
    Rescan,
    OtherPick(u64),
    Spot(usize),
    SaveSpot,
    Undo,
    Open(Page),
    EspOn,
    EspEnemies,
    EspFriendlies,
    EspPlayers,
    /// Drones as diamonds.
    EspDrones,
    EspHideDead,
    EspDistance,
    EspEdge,
    EspBones,
    /// A box colour: 0 hostile, 1 friendly, 3 players (the order of EspStyle::colors), 5 drones.
    EspColour(usize),
    /// Its hex code, typed.
    EspHex(usize),
    EspRange,
    EspLines,
    Aim,
    Yeet,
    Bring,
    MenuColour,
    /// Appearance > Position: X and Y in pixels, Move With Mouse.
    PosX,
    PosY,
    MoveMouse,
    /// Menu > Settings > Mouse Support: the menu uses the mouse only when on; off by default.
    MouseSupport,
    /// Aurora > Settings > Enable Experimental Features: Aurora > Experiments shows only while it
    /// is on, and off turns every experiment off.
    Experimental,
    /// One of `LAYOUT_ROWS`.
    Layout(usize),
    /// Notifications: Position, Border Colour (was Toast Position and Toast Colour), Background
    /// Colour, Custom Position's X and Y, Flash Notification. Invert Flow, Padding and Flash Colour
    /// went.
    ToastPosition,
    ToastColour,
    BackColour,
    NotifyX,
    NotifyY,
    FlashNote,
    /// Menu > Unload Aurora, and its Yes.
    Unload,
    UnloadYes,
    /// The Yes under the Experiments warning.
    ExpYes,
    /// A line of a warning page's text: only shown, never picked, highlighted or hovered.
    WarnText,
    /// Warnings: Automatically Proceed On Warnings from the command box or a hotkey, Force Me To
    /// Read Warnings, Skip Already-Accepted Warnings.
    AutoBox,
    AutoKey,
    ForceRead,
    SkipAccepted,
    /// Suppress Generic Responses after the command box and a hotkey. Its Menu switch went: the
    /// menu by hand never shows one.
    QuietBox,
    QuietKey,
    /// Menu > Profiles: Create Profile, Open Folder, and each profile's rows (by its number).
    NewProfile,
    OpenProfiles,
    ProfileSave(u32),
    ProfileLoad(u32),
    ProfileOnInject(u32),
    ProfileAuto(u32),
    ProfileDelete(u32),
    ProfileDeleteYes(u32),
    /// Aurora > Apply Default State, also the command `default`.
    ApplyDefaults,
    /// A row with hotkeys in the Hotkey List, by its place in Hotkeys.txt.
    KeyRow(usize),
    Info,
    /// A hotkey row.
    Bind(Bind),
    CtxDefaults,
    /// Set To Min Value / Set To Max Value of a value row, as Stand's.
    CtxMin,
    CtxMax,
    /// Save State / Load State: this row (or every row in this list) into or from the active
    /// profile, no other row touched.
    CtxSaveState,
    CtxLoadState,
    CtxSave,
    CtxCopy,
    CtxAddKey,
    CtxRemoveKey(u32),
    /// Rename a teleport spot, from its "O" menu.
    CtxRename,
    /// A saved command, by its place in Saved Commands.txt.
    Run(usize),
    Header,
}

/// What a hotkey row sets: an action, or a saved place's own key (both in Hotkeys.txt now;
/// keys.json and spots.json were the old files).
#[derive(Clone, Copy, PartialEq, Debug)]
enum Bind {
    Action(&'static str),
    /// A new hotkey for the row the "O" menu is about.
    RowAdd,
}

/// The menu's own keys, the only ones that are not a row's: on top of Hotkeys.txt, changeable.
/// Fly's are built in; God, Fly, Target and the rest take hotkeys from their "O" menu as every row.
const BIND_ROWS: [(&str, &str); 2] = [("Menu", "menu"), ("Command Box", "cmdbox")];

#[derive(Clone)]
struct Item {
    id: Id,
    row: Row,
    help: String,
}

/// Box colours to pick from; the first three are the old defaults' look.
const BOX_COLOURS: [(&str, [u8; 3]); 9] = [
    ("Red", [235, 80, 70]),
    ("Orange", [255, 140, 0]),
    ("Yellow", [240, 210, 60]),
    ("Green", [110, 210, 130]),
    ("Teal", [0, 190, 180]),
    ("Blue", [110, 170, 255]),
    ("Purple", [160, 90, 255]),
    ("Pink", [255, 0, 255]),
    ("White", [255, 255, 255]),
];

/// `EspColour(i)`'s number for the drones' colour.
const DRONE_COLOUR: usize = 5;

/// The ESP colour behind `EspColour(i)`: `EspStyle::colors[i]`, or the drones' own.
fn esp_colour(s: &aurora_core::proto::EspStyle, i: usize) -> [u8; 3] {
    if i == DRONE_COLOUR { s.drone } else { s.colors[i] }
}

fn esp_colour_mut(s: &mut aurora_core::proto::EspStyle, i: usize) -> &mut [u8; 3] {
    if i == DRONE_COLOUR { &mut s.drone } else { &mut s.colors[i] }
}

fn box_colour_name(c: [u8; 3]) -> String {
    BOX_COLOURS.iter().find(|b| b.1 == c).map_or("Custom".into(), |b| b.0.into())
}

/// The hex code of a colour that is not one of the list's; empty for the list's own.
fn box_colour_hex(c: [u8; 3]) -> String {
    if BOX_COLOURS.iter().any(|b| b.1 == c) { String::new() } else { format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]) }
}

/// "FF8800" or "#ff8800" to a colour.
fn parse_hex(t: &str) -> Option<[u8; 3]> {
    let t = t.trim_start_matches('#');
    if t.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn item(id: Id, text: impl Into<String>, kind: Kind, help: &str) -> Item {
    Item { id, row: Row { text: text.into(), kind, dim: false }, help: help.into() }
}

const ACCENTS: [(&str, [u8; 3]); 7] = [
    ("Pink", [255, 0, 255]),
    ("Purple", [140, 60, 255]),
    ("Blue", [0, 110, 255]),
    ("Teal", [0, 170, 160]),
    ("Green", [40, 170, 70]),
    ("Orange", [255, 120, 0]),
    ("Red", [220, 30, 45]),
];

/// The player's menu choices. Like every setting they live in profiles, as Stand's: each inject
/// starts from the defaults and the Load On Inject profile puts the saved settings back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Prefs {
    accent: usize,
    /// Where the menu sits, in screen pixels, as Stand's: X the menu's right edge, the wall, Y the
    /// address bar's top. None: where it always sat (`default_pos`).
    x: Option<i32>,
    y: Option<i32>,
    /// Move With Mouse: drag the menu by its header (the mouse part wires the drag). While it is on
    /// the menu has the mouse, Mouse Support or not.
    move_mouse: bool,
    /// Mouse Support: off, the menu never takes the mouse from the game, unless Move With Mouse is
    /// on.
    mouse_support: bool,
    /// Enable Experimental Features: off, Aurora > Experiments is not shown and no experiment runs.
    experimental: bool,
    /// Warnings, as Stand's: Automatically Proceed On Warnings from the command box and from a
    /// hotkey, Force Me To Read Warnings, Skip Already-Accepted Warnings.
    auto_box: bool,
    auto_key: bool,
    force_read: bool,
    skip_accepted: bool,
    /// Suppress Generic Responses after the command box and after a hotkey. The menu itself never
    /// shows them.
    quiet_box: bool,
    quiet_key: bool,
    /// Tabs width, tabs height, address bar height and width, notification size, time, width and
    /// text size, List Width, List Height, Max Visible Commands, Spacer Size (see `LAYOUT_ROWS`).
    layout: [u32; LAYOUT_N],
    /// Notifications: where (`NOTIFY_POSITIONS`), the border colour (0: the menu's, else `ACCENTS[n
    /// - 1]`), the background (`BACKGROUNDS`), and Custom Position in screen pixels.
    toast_corner: usize,
    toast_colour: usize,
    back_colour: usize,
    notify_x: i32,
    notify_y: i32,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            accent: 0,
            x: None,
            y: None,
            move_mouse: false,
            mouse_support: false,
            experimental: false,
            auto_box: false,
            auto_key: false,
            force_read: true,
            skip_accepted: true,
            quiet_box: false,
            quiet_key: false,
            layout: LAYOUT_ROWS.map(|r| r.2),
            toast_corner: 0,
            toast_colour: 0,
            back_colour: 0,
            notify_x: NOTIFY_XY_DEFAULT.0,
            notify_y: NOTIFY_XY_DEFAULT.1,
        }
    }
}

/// Number rows of Appearance and Notifications: name, the old menu.json's key (read once, to carry
/// the player's values over; none for the new ones), usual, least, most. Sizes are pixels at 1080p.
/// Then Stand's size rows, straight in Appearance: List Width and List Height (one row's height) in
/// units where 450 / 32 is the menu as it looked at Menu Size 85% (`menuview::LIST`), Max Visible
/// Commands (rows before the list scrolls, its own thing), Spacer Size (the see-through gap between
/// the menu's parts; 3 is the gap it always had).
const LAYOUT_ROWS: [(&str, &str, u32, u32, u32); LAYOUT_N] = [
    ("Tabs Width", "tabswidth", 118, 60, 400),
    ("Tabs Height", "tabsheight", 34, 20, 80),
    ("Address Bar Height", "addressbarheight", 26, 16, 80),
    ("Address Bar Width", "addressbarwidth", 602, 200, 1600),
    // Percent of the menu's size: the box's padding, bar and Width.
    ("Size", "toastsize", 100, 50, 200),
    ("Time", "toastseconds", 3, 1, 15),
    // The widest a notification grows before its text wraps.
    ("Width", "", 400, 100, 1600),
    // The text's size, percent of the size it always had.
    ("Text Size", "", 100, 50, 300),
    ("List Width", "", 450, 0, 32767),
    ("List Height", "", 32, 0, 32767),
    ("Max Visible Commands", "", 10, 3, 255),
    ("Spacer Size", "", 3, 0, 50),
];
const LAYOUT_N: usize = 12;
/// Their commands: width and height as w and h; the notification ones as Stand's notify..
/// commands; the size rows as Stand's.
const LAYOUT_COMMANDS: [&str; LAYOUT_N] = [
    "tabsw", "tabsh", "addressbarh", "addressbarw", "notifysize", "notifytime", "notifywidth", "notifytextsize", "listwidth", "listheight", "menuheight", "spacersize",
];
/// What follows each number on its row.
const LAYOUT_UNITS: [&str; LAYOUT_N] = ["", "", "", "", "%", " s", "", "%", "", "", "", ""];
const TABS_WIDTH: usize = 0;
const TOAST_SIZE: usize = 4;
const TOAST_TIME: usize = 5;
const NOTIFY_WIDTH: usize = 6;
const NOTE_TEXT: usize = 7;
const LIST_W: usize = 8;
const LIST_H: usize = 9;
const VISIBLE_ROWS: usize = 10;
const SPACER_SIZE: usize = 11;
/// The old Menu Size the default List Width / Height look like.
const LOOK_SIZE: u32 = 85;
/// The gap between stacked notifications, pixels at 1080p.
const NOTE_GAP: f32 = 4.0;

/// Where notifications show, the default first; Custom uses Custom Position's X and Y.
const NOTIFY_POSITIONS: [&str; 5] = ["Bottom Left", "Bottom Right", "Top Left", "Top Right", "Custom"];
const CUSTOM: usize = 4;
/// Custom Position's usual X and Y, in screen pixels.
const NOTIFY_XY_DEFAULT: (i32, i32) = (40, 900);
/// Notification backgrounds; the first is the usual.
const BACKGROUNDS: [(&str, [u8; 3]); 3] = [("Black", [0, 0, 0]), ("Dark Grey", [32, 32, 32]), ("Grey", [70, 70, 70])];
/// What Flash Notification shows.
const FLASH_TEXT: &str = "A test notification, so you can see how they look.";

/// Menu X and Y and Custom Position: Stand's range, in pixels.
const PIXEL_MIN: f64 = -32768.0;
const PIXEL_MAX: f64 = 32767.0;

fn toast_colour_name(n: usize) -> &'static str {
    if n == 0 { "Menu Colour" } else { ACCENTS[(n - 1) % ACCENTS.len()].0 }
}

/// The game's screen size, or 1920 x 1080 before it is known (and in tests).
fn screen_size() -> (i32, i32) {
    vkdraw::screen().unwrap_or((1920, 1080))
}

/// Percent of a screen side in pixels, as the menu was placed before.
fn pct_px(pct: f64, len: i32) -> i32 {
    (len as f32 * pct.clamp(0.0, 100.0) as f32 / 100.0) as i32
}

/// Where the menu always sat: its picture at 64% and 52% of this screen. X is the wall since so X
/// is where the list's right edge was with the default X at Menu Size 85% (the look List Width /
/// Height default to), kept on the screen as it was drawn: flush with the right edge on 16:9
/// screens.
fn default_pos(axis: usize) -> i32 {
    let (w, h) = screen_size();
    if axis == 0 { old_x_to_wall(pct_px(64.0, w) + old_x_shift(100, LAYOUT_ROWS[TABS_WIDTH].2, h), LOOK_SIZE, (w, h)) } else { pct_px(52.0, h) }
}

/// The menu's X (0) or Y (1) in pixels: X the menu's right edge on screen, the wall, Y the address
/// bar's top edge. The mouse drag writes the same numbers.
fn menu_pos(p: &Prefs, axis: usize) -> i32 {
    [p.x, p.y][axis].unwrap_or_else(|| default_pos(axis))
}

/// Evening X was the address bar's left edge: the wall (the list's right edge) sat this far right
/// of it at that Menu Size on a screen `screen_h` high.
fn old_bar_to_wall(size: u32, screen_h: i32) -> i32 {
    let scale = screen_h.max(1) as f32 / 1080.0 * size.clamp(60, 200) as f32 / 100.0;
    aurora_core::menuview::old_bar_to_wall(scale).round() as i32
}

/// An old X (the address bar's left edge) at that Menu Size as today's X (the wall), where the
/// list's right edge was drawn: never past the screen's right edge, where the old picture was kept
/// too.
fn old_x_to_wall(x: i32, size: u32, (w, h): (i32, i32)) -> i32 {
    (x.max(0) + old_bar_to_wall(size, h)).min(w.max(1))
}

/// The wall on screen: X, kept on the screen, and never closer to its left edge than the list is
/// wide, so the list itself always shows. What sticks out further left (tabs, bar, help boxes) is
/// cut at the screen's edge; the wall stays.
fn wall_at(x: i32, list_w: i32, sw: i32) -> i32 {
    x.min(sw).max(list_w.min(sw))
}

/// Where the menu's picture goes on screen: its right edge on the wall, its top (the address bar's)
/// at Y, kept on the screen.
fn menu_place(wall: i32, y: i32, frame: (u32, u32), sh: i32) -> (i32, i32) {
    (wall - frame.0 as i32, y.min(sh - frame.1 as i32).max(0))
}

/// The menu's tabs and bar sizes, List Width / Height and Spacer Size as the picture takes them.
fn layout_of(p: &Prefs) -> [f32; 4] {
    std::array::from_fn(|i| p.layout[i] as f32)
}

fn list_of(p: &Prefs) -> [f32; 2] {
    [p.layout[LIST_W] as f32, p.layout[LIST_H] as f32]
}

/// Max Visible Commands, but never more rows than fit on the screen (and in the drawers' slot)
/// under the address bar.
fn shown_rows(p: &Prefs) -> usize {
    let (_, sh) = screen_size();
    let (_, slot_h) = vkdraw::slot_size(vkdraw::MENU);
    let fit = aurora_core::menuview::rows_fit(layout_of(p), list_of(p), p.layout[SPACER_SIZE] as f32, sh.max(1) as f32 / 1080.0, (sh.max(1) as u32).min(slot_h) as f32);
    (p.layout[VISIBLE_ROWS] as usize).min(fit).max(1)
}

/// How far right of an old X the address bar sat: the help boxes' room at that Menu Size and Tabs
/// Width on a screen `screen_h` high. Old profiles, menu.json and the old percent commands add it,
/// so the player's menu stays put.
fn old_x_shift(size: u32, tabs_w: u32, screen_h: i32) -> i32 {
    let scale = screen_h.max(1) as f32 / 1080.0 * size.clamp(60, 200) as f32 / 100.0;
    aurora_core::menuview::left_reach(tabs_w as f32, scale).round() as i32
}

/// The choices a list row goes through, and the one it shows.
fn choice_names(id: Id) -> Vec<&'static str> {
    match id {
        Id::ToastPosition => NOTIFY_POSITIONS.to_vec(),
        Id::ToastColour => (0..=ACCENTS.len()).map(toast_colour_name).collect(),
        Id::BackColour => BACKGROUNDS.iter().map(|b| b.0).collect(),
        Id::MenuColour => ACCENTS.iter().map(|a| a.0).collect(),
        _ => Vec::new(),
    }
}

fn choice_at(p: &Prefs, id: Id) -> usize {
    match id {
        Id::ToastPosition => p.toast_corner,
        Id::ToastColour => p.toast_colour,
        Id::BackColour => p.back_colour,
        Id::MenuColour => p.accent,
        _ => 0,
    }
}

fn set_choice(p: &mut Prefs, id: Id, n: usize) {
    match id {
        Id::ToastPosition => p.toast_corner = n,
        Id::ToastColour => p.toast_colour = n,
        Id::BackColour => p.back_colour = n,
        Id::MenuColour => p.accent = n,
        _ => {}
    }
}

fn choice_row(id: Id, name: &str, prefs: Prefs) -> Item {
    let names = choice_names(id);
    item(id, name, Kind::Choice(names[choice_at(&prefs, id) % names.len()].into()), "")
}

fn layout_row(i: usize, prefs: Prefs) -> Item {
    item(Id::Layout(i), LAYOUT_ROWS[i].0, Kind::Choice(format!("{}{}", prefs.layout[i], LAYOUT_UNITS[i])), &range(Id::Layout(i), prefs))
}

struct State {
    open: bool,
    section: usize,
    /// The section's list and the pages opened inside it, each with its picked row and scroll.
    /// Never empty while the menu is open.
    stack: Vec<(Page, usize, usize)>,
    /// Each section's stack while another section is shown, so coming back lands where the player
    /// was.
    saved: [Vec<(Page, usize, usize)>; SECTIONS.len()],
    generation: u64,
    /// The number being typed into the picked row.
    edit: Option<String>,
    /// The command box's caret, in bytes into `edit`; None: at the end. Left / Right move it,
    /// typing and Backspace work there.
    caret: Option<usize>,
    /// Waiting for the new key of this hotkey row, with the keys that were already down.
    bind: Option<(Bind, [bool; 256])>,
    /// The key just picked, kept from everything until it is let go.
    bind_hold: Option<u32>,
    /// Menu keys still down after the menu closed (a row closed it, or Unload): kept from the game
    /// until let go, or the game sees them go down right then.
    nav_hold: bool,
    /// A mouse button still down as the menu let go of the mouse (a click closed it, or Yes under
    /// Unload Aurora): the mouse stays the menu's until let go, or the game sees a fresh press and
    /// fires.
    mouse_hold: bool,
    /// Unload picked: it starts once the keys that picked it are let go (3 s at most), so the game
    /// never gets that key.
    unload_at: Option<Instant>,
    /// Keys down when the command box opened, so they don't type.
    box_was: [bool; 256],
    /// The row's command when a menu click on a value row opened the box: Enter on that command
    /// counts as done by hand in the menu, so no "set to".
    box_row: Option<String>,
    /// Backspace, Left or Right held in the command box: which, since when, last repeat.
    box_held: Option<(u32, Instant, Instant)>,
    /// Commands run from the box, oldest first; Up/Down walk them.
    history: Vec<String>,
    hist_pos: Option<usize>,
    /// Tab cycling: what was typed, and which match is shown.
    tab: Option<(String, usize)>,
    last: Option<(View, (i32, i32))>,
    /// The menu picture on screen. The mouse clicks against this.
    drawn: Option<Drawn>,
    prefs: Prefs,
    /// The profile last loaded, saved or created (Automatically Save writes to it), and its text as
    /// last read or written.
    profile: Option<String>,
    profile_text: String,
    /// The Load On Inject profile, loaded within a second of inject.
    inject_load: Option<String>,
    /// The last menu keys acted on, oldest first, written to the log when Unload runs.
    keys_seen: std::collections::VecDeque<(Instant, String)>,
    /// When the menu last opened.
    opened_at: Instant,
    /// The picked row (section, page, row) and since when it is picked.
    picked: (usize, Option<(Page, usize)>, Instant),
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// Profile lines for switches that need the player in the world: kept until then, their rows
/// showing a waiting ring. Their state for profiles is the line's, so Save and Automatically Save
/// keep them.
static WAITING: Mutex<Vec<(Found, String)>> = Mutex::new(Vec::new());
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);
static PAINTER: OnceLock<Option<Painter>> = OnceLock::new();
/// The latest toasts and when each came, oldest first; shown together, stacked, as Stand's
/// notifications.
static TOASTS: Mutex<Vec<(String, Instant)>> = Mutex::new(Vec::new());
/// At most this many are kept.
const TOASTS_MAX: usize = 8;

/// A short message in a corner. Hotkeys use it too.
pub fn toast(msg: impl Into<String>) {
    if let Ok(mut t) = TOASTS.lock() {
        t.push((msg.into(), Instant::now()));
        let n = t.len();
        if n > TOASTS_MAX {
            t.drain(..n - TOASTS_MAX);
        }
    }
}

/// What `findc` looks for.
static FIND: Mutex<String> = Mutex::new(String::new());

fn find_text() -> String {
    FIND.lock().map(|f| f.clone()).unwrap_or_default()
}

/// An "are you sure" page: each is accepted on its own, for Skip Already-Accepted Warnings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Warn {
    Unload,
    Credits,
    /// Turning on Enable Experimental Features.
    Experiments,
}

/// Warnings answered Yes (or read, for the credits one) since this inject.
static ACCEPTED: Mutex<Vec<Warn>> = Mutex::new(Vec::new());

fn accepted(w: Warn) -> bool {
    ACCEPTED.lock().map(|a| a.contains(&w)).unwrap_or(false)
}

fn accept(w: Warn) {
    if let Ok(mut a) = ACCEPTED.lock()
        && !a.contains(&w)
    {
        a.push(w);
    }
}

/// Profiles whose delete was answered Yes since this inject, lower case. Each profile is asked on
/// its own: a Yes for one is not a Yes for another.
static ACCEPTED_DELETES: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn delete_accepted(name: &str) -> bool {
    let k = name.to_lowercase();
    ACCEPTED_DELETES.lock().map(|a| a.contains(&k)).unwrap_or(false)
}

fn accept_delete(name: &str) {
    let k = name.to_lowercase();
    if let Ok(mut a) = ACCEPTED_DELETES.lock()
        && !a.contains(&k)
    {
        a.push(k);
    }
}

/// No "are you sure" page for this action: Force Me To Read Warnings is off, Automatically Proceed
/// On Warnings is on for where it came from (command box, command.request or hotkey), or Skip
/// Already-Accepted Warnings is on and this one was accepted before. With Force Me To Read Warnings
/// on, the menu itself always asks the first time.
fn warn_skipped(p: &Prefs, from: u8, was_accepted: bool) -> bool {
    if !p.force_read {
        return true;
    }
    match from {
        FROM_BOX if p.auto_box => true,
        FROM_KEY if p.auto_key => true,
        _ => p.skip_accepted && was_accepted,
    }
}

/// How long a warning waits before its Yes shows (as Unload's always did).
const READ_WAIT: Duration = Duration::from_secs(2);

/// Force Me To Read Warnings: the Yes row shows only `READ_WAIT` after the question. Off, no
/// warning page shows at all now; this still says "at once" for it. `asked` is how long ago it was
/// asked (None: not asked).
fn yes_shows(force_read: bool, asked: Option<Duration>) -> bool {
    asked.is_some_and(|t| !force_read || t >= READ_WAIT)
}

/// How long ago a question was asked.
fn asked_ago(at: &Mutex<Option<Instant>>) -> Option<Duration> {
    at.lock().ok().and_then(|t| *t).map(|t| t.elapsed())
}

/// Show an "are you sure" page, menu opened if needed: from a hotkey or the command box too, the
/// question must be seen.
fn ask(st: &mut State, page: Page) {
    if !st.open {
        st.open = true;
        take_keys();
    }
    if st.stack.is_empty() {
        st.stack.push((SECTIONS[st.section].0, 0, 0));
    }
    st.stack.push((page, 0, 0));
}

fn painter() -> Option<&'static Painter> {
    PAINTER
        .get_or_init(|| {
            let p = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf").ok().and_then(Painter::new);
            if p.is_none() {
                log("menu: Segoe UI not found");
            }
            p
        })
        .as_ref()
}

/// `%APPDATA%\Aurora`, where the menu keeps Hotkeys.txt, Places, Profiles, Saved Commands, Meta
/// State.txt and command.request. None in unit tests: they read and write nothing there, as on a PC
/// without Aurora.
fn folder() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(aurora_core::paths::root())
}

/// The Suppress Generic Responses switches where `done` and the hotkeys look.
fn mirror_quiet(p: Prefs) {
    QUIET_BOX.store(p.quiet_box, Ordering::Relaxed);
    QUIET_KEY.store(p.quiet_key, Ordering::Relaxed);
}

/// A menu setting changed. No file is written: it is a profile row like any other, so Save and
/// Automatically Save keep it. The Suppress switches are mirrored for the hotkeys' thread.
fn prefs_changed(p: Prefs) {
    mirror_quiet(p);
}

/// A file's text: `Ok(None)` when it is not there yet, `Err` when it is there but could not be read
/// (locked, not UTF-8). Callers must not write over a file on `Err`: it would wipe what the player
/// has in it.
fn read_text(path: &std::path::Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            log(&format!("menu: {name} could not be read ({e}): left as it is, changes not saved"));
            Err(e.to_string())
        }
    }
}

/// Gear Score's Total (everything the player has on) or one slot.
fn set_worn(id: Id, g: u32) -> Result<(), String> {
    match id {
        Id::GearSlot(i) => gearscore::set_slot(i, g),
        _ => gearscore::set_equipped(g).map(|_| ()),
    }
}

/// Set All Owned Items' value: what the last one set, while busy that it is.
fn owned_text() -> String {
    if gearscore::owned_busy() {
        "setting".into()
    } else {
        gearscore::owned_last().map_or_else(|| "-".into(), |(v, _)| v.to_string())
    }
}

/// The player's drop gear score (0: the game's own again). Callers keep it to `gear_max`. A value
/// up to 1000 is also the one profiles keep (`GEAR_NORMAL`).
fn set_gear(g: u32) {
    let v = (g.clamp(1, gearscore::HIGH_MAX), g != 0);
    *GEAR.lock().unwrap_or_else(|e| e.into_inner()) = v;
    if v.0 <= gearscore::MAX {
        *GEAR_NORMAL.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }
}

/// Enable Experimental Features off: Dropped Item Gear Score back to the last value up to 1000 the
/// player had; `gear_tick` writes it in the world. If that is the game's own, a drop score over
/// 1000 that a hold left in the game goes to 1000 there. Items already set keep their score.
fn gear_back() {
    *GEAR.lock().unwrap_or_else(|e| e.into_inner()) = gear_normal();
    GEAR_CAP_CHECK.store(true, Ordering::SeqCst);
}

/// Dropped Item Gear Score as profiles keep it: a custom value up to 1000, or the game's own.
fn gear_normal() -> (u32, bool) {
    *GEAR_NORMAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// A drop gear score as its row's state: the number while held, else 0 (the game's own).
fn gear_state((v, held): (u32, bool)) -> String {
    if held { v.to_string() } else { "0".into() }
}

/// The highest gear score the player can set: 1000, or a million while Enable Experimental Features
/// is on. Off, no row says more is possible.
fn gear_max(prefs: Prefs) -> u32 {
    if prefs.experimental { gearscore::HIGH_MAX } else { gearscore::MAX }
}

/// A typed gear score as it is set: whole, 0 up to `gear_max`.
fn gear_typed(v: f64, prefs: Prefs) -> u32 {
    v.round().clamp(0.0, f64::from(gear_max(prefs))) as u32
}

/// The row's text: the player's, or the game's own as it is now.
fn gear_text() -> String {
    match gear() {
        (v, true) => v.to_string(),
        _ => game_gear().map_or_else(|| "Game's own".into(), |v| v.to_string()),
    }
}

/// Where the game keeps the gear score for new drops: `[globals::LEVEL_ROOT]+0x490`
/// (`[exe+0x63A0D20]` in build 25349072; its gear level is the i32 before it). The root is the one
/// found by pattern: until it is found, or when a game update moved it, this is None and the hold
/// does nothing.
fn gear_addr() -> Option<u64> {
    let exe = memory::find_module("")?;
    let rva = crate::globals::LEVEL_ROOT.rva()?;
    let root = memory::read(exe.base + rva, 8).ok().map(|b| u64::from_le_bytes(b.try_into().unwrap()))?;
    (root > 0x10000 && memory::readable(root + 0x490, 4)).then_some(root + 0x490)
}

fn game_gear() -> Option<u32> {
    let a = gear_addr()?;
    memory::read(a, 4).ok().map(|b| u32::from_le_bytes(b.try_into().unwrap())).filter(|&v| v <= gearscore::HIGH_MAX)
}

/// The gear score kept for new drops, and whether it is held. Off unless the player or a profile
/// turned it on, so every inject starts with the game's own; a profile keeps it.
static GEAR: Mutex<(u32, bool)> = Mutex::new((GEAR_DEFAULT, false));
/// The last `GEAR` up to 1000: what profiles keep and what the switch going off brings back.
static GEAR_NORMAL: Mutex<(u32, bool)> = Mutex::new((GEAR_DEFAULT, false));

/// Ten times a second, in the world: the player's drop gear score back whenever the game rewrites
/// it. It was the window's job, which stops while the window is minimized.
fn gear_tick(checked: &mut Instant) {
    if checked.elapsed() < Duration::from_millis(100) {
        return;
    }
    *checked = Instant::now();
    let (value, on) = gear();
    if fly::body_addr().is_none() {
        return;
    }
    if !on {
        gear_cap_check();
        return;
    }
    let Some(a) = gear_addr() else { return };
    let now = memory::read(a, 4).ok().map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    if now.is_some_and(|v| v != value) && memory::write(a, &value.to_le_bytes()).is_ok() {
        log(&format!("gear score was {}, set back to {value}", now.unwrap_or(0)));
    }
}

/// Set at every inject and each time Enable Experimental Features goes off: `gear_cap_check` is
/// due.
static GEAR_CAP_CHECK: AtomicBool = AtomicBool::new(true);

/// Once after every inject and each time Enable Experimental Features goes off, in the world with
/// the hold and the switch off: a drop score over 1000 left in the game (by a Load, the switch
/// going off outside the world, an unload) goes to 1000. The game's own are a few hundred; a number
/// over a million is no gear score and is left alone.
fn gear_cap_check() {
    if gearscore::cap() > gearscore::MAX || !GEAR_CAP_CHECK.load(Ordering::SeqCst) {
        return;
    }
    let Some(a) = gear_addr() else { return };
    GEAR_CAP_CHECK.store(false, Ordering::SeqCst);
    let now = memory::read(a, 4).ok().map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    if let Some(v) = now.filter(|v| (gearscore::MAX + 1..=gearscore::HIGH_MAX).contains(v))
        && memory::write(a, &gearscore::MAX.to_le_bytes()).is_ok()
    {
        log(&format!("gear score: drop score {v} left over 1000 with Enable Experimental Features off, set to 1000"));
    }
}

/// The gear score kept for new drops, and whether the player set one. Not set, the game's own is
/// shown and left alone.
fn gear() -> (u32, bool) {
    *GEAR.lock().unwrap_or_else(|e| e.into_inner())
}

/// Saved spots, one file each in `%APPDATA%\Aurora\Places\` as Stand's: `<name>.txt` with `X: ..`,
/// `Y: ..`, `Z: ..` lines. Only the menu reads them; a key for one is a row hotkey like any other
/// (O > Hotkeys, in Hotkeys.txt). In name order.
fn spots() -> Vec<(String, [f64; 3])> {
    let Some(dir) = places() else { return Vec::new() };
    let Ok(read) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut v: Vec<(String, [f64; 3])> = read
        .filter_map(|e| {
            let path = e.ok()?.path();
            if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt")) {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_string();
            Some((name, place_pos(&std::fs::read_to_string(&path).ok()?)?))
        })
        .collect();
    v.sort_by_key(|s| s.0.to_lowercase());
    v
}

/// `X: 1.5` `Y: -2` `Z: 3` lines into a position.
fn place_pos(text: &str) -> Option<[f64; 3]> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = [None; 3];
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let at = match k.trim().to_ascii_uppercase().as_str() {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            _ => continue,
        };
        p[at] = v.trim().parse::<f64>().ok().filter(|x| x.is_finite());
    }
    Some([p[0]?, p[1]?, p[2]?])
}

fn place_text(p: [f64; 3]) -> String {
    format!("X: {}\nY: {}\nZ: {}\n", p[0], p[1], p[2])
}

/// The Places folder, made if needed. The first time, the spots in teleports.json (or the older
/// spots.json) become files, and a key one had becomes a row hotkey; the old file is left where it
/// was.
fn places() -> Option<std::path::PathBuf> {
    let f = folder()?;
    let dir = f.join("Places");
    if dir.is_dir() {
        return Some(dir);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let old = ["teleports.json", "spots.json"].iter().find_map(|n| std::fs::read_to_string(f.join(n)).ok());
    let list: Vec<serde_json::Value> = old.and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut keys = Vec::new();
    for s in &list {
        let (Some(name), Some(p)) = (s.get("name").and_then(|n| n.as_str()), s.get("pos").and_then(|p| p.as_array())) else { continue };
        let Some(pos) = (|| Some([p.first()?.as_f64()?, p.get(1)?.as_f64()?, p.get(2)?.as_f64()?]))() else { continue };
        let name = file_name(name);
        if name.is_empty() || aurora_core::paths::write_file(&dir.join(format!("{name}.txt")), place_text(pos)).is_err() {
            continue;
        }
        if let Some(k) = s.get("key").and_then(|k| k.as_u64()).filter(|&k| k != 0) {
            keys.push((format!("World > Teleport > Go To {name}"), vec![k as u32]));
        }
    }
    log(&format!("menu: {} spots moved into the Places folder", list.len()));
    if !keys.is_empty() {
        let mut all = row_keys();
        all.extend(keys);
        set_row_keys(all);
    }
    Some(dir)
}

/// A spot name as a file name: no characters Windows keeps out of names.
fn file_name(name: &str) -> String {
    name.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect::<String>().trim().trim_end_matches('.').to_string()
}

fn add_spot(pos: [f64; 3]) -> Result<String, String> {
    let dir = places().ok_or("no Places folder")?;
    let taken: Vec<String> = spots().into_iter().map(|s| squash(&s.0)).collect();
    let n = (1..).find(|n| !taken.contains(&format!("spot{n}"))).unwrap_or(1);
    let name = format!("spot {n}");
    aurora_core::paths::write_file(&dir.join(format!("{name}.txt")), place_text(pos)).map_err(|e| e.to_string())?;
    Ok(name)
}

/// Take a saved spot out (the player's Delete on its row): its file goes.
fn remove_spot(i: usize) -> Result<String, String> {
    let dir = places().ok_or("no Places folder")?;
    let name = spots().get(i).map(|s| s.0.clone()).ok_or("That spot is gone")?;
    std::fs::remove_file(dir.join(format!("{name}.txt"))).map_err(|e| e.to_string())?;
    Ok(name)
}

/// A spot's name as its commands have it: `gotoerehwon`, `renamespot erehwon`.
fn squash(name: &str) -> String {
    name.to_lowercase().replace(' ', "")
}

/// Give a saved spot a new name: its file is renamed. Its row's hotkeys and its place in Saved
/// Commands follow it to the new address. Returns the old name.
fn rename_spot(i: usize, name: &str, prefs: Prefs) -> Result<String, String> {
    let name = file_name(name);
    if name.is_empty() {
        return Err("Type the new name after the spot's".into());
    }
    let list = spots();
    if list.iter().enumerate().any(|(j, s)| j != i && squash(&s.0) == squash(&name)) {
        return Err(format!("A spot called {name} is already there"));
    }
    let old = list.get(i).map(|s| s.0.clone()).ok_or("That spot is gone")?;
    let old_addr = all_rows(prefs).into_iter().find(|f| f.item.id == Id::Spot(i)).map(|f| f.addr);
    let dir = places().ok_or("no Places folder")?;
    std::fs::rename(dir.join(format!("{old}.txt")), dir.join(format!("{name}.txt"))).map_err(|e| e.to_string())?;
    if let Some(old_addr) = old_addr {
        let new_addr = match old_addr.rsplit_once(" > ") {
            Some((page, _)) => format!("{page} > Go To {name}"),
            None => format!("Go To {name}"),
        };
        let mut keys = row_keys();
        if let Some(e) = keys.iter_mut().find(|e| e.0 == old_addr) {
            e.0 = new_addr.clone();
            set_row_keys(keys);
        }
        let mut saved = saved_list();
        if let Some(a) = saved.iter_mut().find(|a| **a == old_addr) {
            *a = new_addr;
            set_saved(&saved);
        }
    }
    Ok(old)
}

/// Open a folder in the player's own folder app (the folder's default verb), on its own thread so
/// the game's frame never waits for the shell.
fn open_folder(dir: &std::path::Path) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    std::thread::spawn(move || {
        use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        // SAFETY: COM for this thread only; the path is NUL-terminated and lives to the end.
        unsafe {
            let com = CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
            let r = ShellExecuteW(std::ptr::null_mut(), std::ptr::null(), wide.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
            if (r as isize) <= 32 {
                crate::log(&format!("open folder: ShellExecute said {}", r as isize));
            }
            if com >= 0 {
                CoUninitialize();
            }
        }
    });
}

/// Rename and say so.
fn renamed(st: &mut State, i: usize, name: &str) {
    match rename_spot(i, name, st.prefs) {
        Ok(old) => done(st, format!("{old} renamed to {}", name.trim())),
        Err(e) => note(st, e),
    }
}

/// The row name of a hotkey, for toasts.
fn bind_name(b: Bind) -> String {
    match b {
        Bind::Action(a) => BIND_ROWS.iter().find(|r| r.1 == a).map(|r| r.0.to_string()).unwrap_or_else(|| a.to_string()),
        Bind::RowAdd => "This command".into(),
    }
}

/// Set (or with 0 remove) a hotkey, and say so; a key already used elsewhere is named.
fn set_bind(st: &mut State, b: Bind, vk: u32) {
    let saved = match b {
        Bind::Action(a) => {
            let mut k = crate::keys::get();
            k.set(a, vk);
            aurora_core::save_keys(&k).map(|()| crate::keys::set(k))
        }
        Bind::RowAdd => Ok(()),
    };
    if let Err(e) = saved {
        note(st, e);
        return;
    }
    let name = bind_name(b);
    if vk == 0 {
        confirm(st, format!("{name}: key removed"));
        return;
    }
    let k = crate::keys::get();
    let also: Vec<String> = aurora_core::keys::ACTIONS
        .iter()
        .filter(|(a, _)| Bind::Action(a) != b && k.get(a) == vk)
        .map(|(a, desc)| BIND_ROWS.iter().find(|r| r.1 == *a).map(|r| r.0.to_string()).unwrap_or_else(|| desc.to_string()))
        .collect();
    if also.is_empty() {
        confirm(st, format!("{name} set to {}", key_name(vk)));
    } else {
        note(st, format!("{name} set to {} (also {})", key_name(vk), also.join(", ")));
    }
}

/// `prefs` comes in: the caller holds STATE, and locking it again here would hang. Hidden pages:
/// put a page here and the row that opens it stays in its list but grayed out, the cursor skips it,
/// and nothing can open it: not Enter, not a hotkey, not a command or a profile. Rows inside it
/// (and inside its own submenus) can't be reached either. Apply Default State still resets them.
/// Take the page out to show it again. Empty. Experiments shows while Enable Experimental
/// Features is on, not through this list.
const HIDDEN: &[Page] = &[];

fn hidden(page: Page) -> bool {
    HIDDEN.contains(&page)
}

/// Gray out the rows that open a hidden page (`activate` and the cursor skip grayed rows).
fn hide_rows(v: &mut [Item], hidden: impl Fn(Page) -> bool) {
    for it in v {
        if let Id::Open(p) = it.id
            && hidden(p)
        {
            it.row.dim = true;
        }
    }
}

/// Move With Mouse: never greyed out. On, it gives the menu the mouse for as long as it is on,
/// Mouse Support or not.
fn move_with_mouse(prefs: Prefs) -> Item {
    item(Id::MoveMouse, "Move With Mouse", Kind::Toggle(prefs.move_mouse), "")
}

fn items(page: Page, prefs: Prefs) -> Vec<Item> {
    let mut v = items_of(page, prefs);
    // Lists first, above switches, values and actions, on every page, except where the player set
    // the order the player: the "O" pages keep their order, and Profiles has Create Profile and
    // Open Folder on top, as Stand's. Credits keeps its order too: the credits on top, Pick Other
    // Points under. ESP is the switch and the range, then Targets and Drawing. Find keeps menu
    // order. Self and Notifications have their lists on top now, so they are sorted like the rest.
    if !is_ctx(page) && !matches!(page, Page::Profiles | Page::Credits | Page::Esp | Page::Find) {
        v.sort_by_key(|i| !matches!(i.row.kind, Kind::Sub));
    }
    hide_rows(&mut v, hidden);
    v
}

fn items_of(page: Page, prefs: Prefs) -> Vec<Item> {
    match page {
        // Infinite Stamina straight in Self again, Weapons a list here, Progress gone to Game. The
        // player's order since Movement, Weapons, God Mode, Infinite Stamina, Undetected.
        Page::SelfTab => {
            let (entity, armed, _) = godmode::status();
            vec![
                item(Id::Open(Page::Movement), "Movement", Kind::Sub, ""),
                item(Id::Open(Page::Weapon), "Weapons", Kind::Sub, ""),
                item(Id::God, "God Mode", Kind::Toggle(entity != 0 || armed), ""),
                item(Id::Stamina, "Infinite Stamina", Kind::Toggle(crate::weapon::stamina_on()), ""),
                item(Id::Unseen, "Undetected", Kind::Toggle(crate::unseen::is_on()), ""),
            ]
        }
        // Only Fly.
        Page::Movement => vec![item(Id::Open(Page::Fly), "Fly", Kind::Sub, "")],
        // Live: a swap shows the new item's score at once.
        Page::GearScore => {
            let on = gearscore::equipped();
            let dash = |v: Option<u32>| v.map_or_else(|| "-".to_string(), |v| v.to_string());
            // Dropped Item Gear Score on top, the rest under. The player's note on each number
            // while more than 1000 can be set.
            let gear_help = if prefs.experimental { GEAR_HIGH_HELP } else { "" };
            let mut v = vec![item(Id::GearValue, "Dropped Item Gear Score", Kind::Choice(gear_text()), gear_help)];
            v.push(item(Id::GearTotal, "Total Gear Score", Kind::Choice(dash(gearscore::total())), gear_help));
            for (i, name) in gearscore::SLOTS.iter().enumerate() {
                match i {
                    0 => v.push(item(Id::Header, "Weapons", Kind::Header, "")),
                    3 => v.push(item(Id::Header, "Gear", Kind::Header, "")),
                    _ => {}
                }
                v.push(item(Id::GearSlot(i), *name, Kind::Choice(dash(on[i].and_then(gearscore::score))), gear_help));
            }
            v.push(item(Id::Header, "Everything You Own", Kind::Header, ""));
            v.push(item(Id::GearOwned, "Set All Owned Items", Kind::Value(owned_text()), gear_help));
            v
        }
        // Game > Progress holds the Editor; Gear Score and Credits are in it, skill points later.
        Page::Progress => vec![item(Id::Open(Page::Editor), "Editor", Kind::Sub, "")],
        // Two lists. Dropped Item Gear Score is one row, always on once the player sets a value; 0
        // or Default State give the game's own back.
        Page::Editor => vec![item(Id::Open(Page::GearScore), "Gear Score", Kind::Sub, ""), item(Id::Open(Page::Credits), "Credits", Kind::Sub, "")],
        Page::Credits => vec![
            item(Id::Credits, "Credits", Kind::Value(credits_text()), &range(Id::Credits, prefs)),
            // Other points than the credits, camo points and the like. A list, Re-scan inside. The
            // player's three lines, one box.
            item(Id::PickOther, "Pick Other Points", Kind::Sub, "Open a shop within 20 s.\nPick the amount you want.\nCredits then changes that one."),
        ],
        Page::Weapon => vec![
            item(Id::NoRecoil, "No Recoil", Kind::Toggle(crate::weapon::recoil_on()), ""),
            item(Id::Accuracy, "No Spread", Kind::Toggle(crate::weapon::accuracy_on()), ""),
            // No Bullet Drop under No Spread, No Scoped Sway over Infinite Mag, Infinite Reserve under it.
            item(Id::SilentNoDrop, "No Bullet Drop", Kind::Toggle(crate::silentaim::no_drop()), "Your shots fly straight, with no drop over distance.
All your shots, Silent Aim on or off."),
            item(Id::NoSway, "No Scoped Sway", Kind::Toggle(crate::weapon::sway_on()), ""),
            item(Id::Ammo, "Infinite Mag", Kind::Toggle(crate::weapon::ammo_on()), ""),
            item(Id::Reserve, "Infinite Reserve", Kind::Toggle(crate::weapon::reserve_on()), ""),
            item(Id::Damage, "Damage Multiplier", Kind::Choice(damage_text()), ""),
            item(Id::FireRate, "Fire Rate", Kind::Choice(fire_text()), ""),
        ],
        // All of Silent Aim in one list, opened from Experiments.
        Page::SilentAim => vec![
            item(Id::SilentAim, "Silent Aim", Kind::Toggle(crate::silentaim::is_on()), "Your shots go to an AI soldier or drone inside the circle.
Never players, co-op players included. Never animals."),
            item(Id::SilentFov, "Silent Aim FOV", Kind::Choice(format!("{:.0} deg", crate::silentaim::fov())), &range(Id::SilentFov, prefs)),
            item(Id::SilentRange, "Silent Aim Range", Kind::Choice(format!("{:.0} m", crate::silentaim::range_m())), &range(Id::SilentRange, prefs)),
            item(Id::SilentCircle, "Show FOV Circle", Kind::Toggle(crate::silentaim::circle()), "The FOV as a circle round the crosshair while Silent Aim is on.
What is inside it can be picked."),
            item(Id::SilentPriority, "Priority", Kind::Choice(crate::silentaim::priority().name().into()), "Which target inside the circle goes first:
Crosshair (nearest the crosshair), Nearest or Farthest from you."),
            item(Id::SilentMagic, "Magic Bullets", Kind::Toggle(crate::silentaim::magic()), "Steered shots go through walls to the target.
Not when a player is near the line. Needs Silent Aim on."),
            item(Id::SilentNoRange, "No Range Loss", Kind::Toggle(crate::silentaim::no_range()), "Your shots keep their damage at any distance.
Not while a player is near your crosshair. Needs Silent Aim on."),
            item(Id::SilentInstant, "Instant Hit", Kind::Toggle(crate::silentaim::instant()), "Your shots arrive at once.
Not while a player is near your crosshair. Needs Silent Aim on."),
        ],
        Page::Fly => {
            let k = crate::keys::get();
            // One line per key in one box, the keys as bound, L / R short.
            let fly_help = format!("W A S D move\n{} up\n{} down\n{} sprint", short_key(k.up), short_key(k.down), short_key(k.fast));
            // Multipliers with two decimals, no m/s, as Stand's Levitation.
            let sprint_help = format!("How fast you go while you hold [{}].", short_key(k.fast));
            vec![
                item(Id::Fly, "Fly", Kind::Toggle(fly::is_on()), &fly_help),
                // "Fly Speed".
                item(Id::FlySpeed, "Fly Speed", Kind::Choice(format!("{:.2}", fly::speed())), &range(Id::FlySpeed, prefs)),
                item(Id::FlySprint, "Sprint", Kind::Choice(format!("{:.2}", fly::sprint())), &sprint_help),
                // Hop Up, Hop Height under it.
                item(Id::HopUp, "Hop Up", Kind::Toggle(fly::hop_up()), &format!("Hops when you press [{}]\nwhile standing on the ground.", short_key(k.up))),
                item(Id::HopHeight, "Hop Height", Kind::Choice(format!("{:.2}", fly::hop_height())), "How high the hop lifts you, in metres."),
                // Text from fly.rs safe_start / safe_end, split at the comma.
                item(Id::SafeFly, "Safe Fly", Kind::Toggle(fly::safe()), "Turns on God Mode while you fly,\nuntil you land."),
            ]
        }
        // Where Am I on top, a line the cursor passes over.
        Page::Teleport => {
            // Two decimals, as Coordinates take them.
            let pos = entities::player_pos().map(|p| format!("{:.2}, {:.2}, {:.2}", p[0], p[1], p[2])).unwrap_or_else(|| "-".into());
            let mut v = vec![item(Id::WhereAmI, "Where Am I", Kind::Value(pos), "")];
            v.extend(spots().into_iter().enumerate().map(|(i, (name, _))| item(Id::Spot(i), format!("Go To {name}"), Kind::Action, "")));
            v.push(item(Id::SaveSpot, "Save This Spot", Kind::Action, ""));
            v.push(item(Id::Undo, "Undo Last Teleport", Kind::Action, ""));
            // Anywhere by coordinates: X, Y and Z keep what the player set.
            let c = coords();
            v.push(item(Id::Header, "Coordinates", Kind::Header, ""));
            for (i, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                v.push(item(Id::Coord(i), axis, Kind::Choice(number(c[i])), &range(Id::Coord(i), prefs)));
            }
            v.push(item(Id::GoCoords, "Go To Coordinates", Kind::Action, ""));
            v
        }
        // Target is back in World.
        Page::World => vec![
            item(Id::Open(Page::Teleport), "Teleport", Kind::Sub, ""),
            item(Id::Open(Page::Esp), "ESP", Kind::Sub, ""),
            item(Id::Open(Page::Target), "Target", Kind::Sub, ""),
        ],
        // The switch and the range, then Targets and Drawing.
        Page::Esp => {
            let c = vkdraw::config();
            vec![
                item(Id::EspOn, "ESP", Kind::Toggle(c.on), ""),
                item(Id::EspRange, "Range", Kind::Choice(format!("{:.0} m", c.style.range_m)), &range(Id::EspRange, prefs)),
                item(Id::Open(Page::EspTargets), "Targets", Kind::Sub, ""),
                item(Id::Open(Page::EspDrawing), "Drawing", Kind::Sub, ""),
            ]
        }
        Page::EspTargets => {
            let c = vkdraw::config();
            vec![
                item(Id::EspEnemies, "Hostile", Kind::Toggle(c.hostile), ""),
                item(Id::EspFriendlies, "Friendly", Kind::Toggle(c.friendly), ""),
                item(Id::EspPlayers, "Players", Kind::Toggle(c.players), ""),
                item(Id::EspDrones, "Drones", Kind::Toggle(c.drones), ""),
                item(Id::EspHideDead, "Hide Dead", Kind::Toggle(c.hide_dead), ""),
            ]
        }
        Page::EspDrawing => {
            let s = vkdraw::config().style;
            vec![
                // A choice, not a switch: it picks how, not whether.
                item(Id::EspBones, "Style", Kind::Choice(if s.bones { "Bones" } else { "Box" }.into()), ""),
                item(Id::EspLines, "Lines", Kind::Choice(if s.width == 0 { "auto".into() } else { format!("{} px", s.width) }), &range(Id::EspLines, prefs)),
                item(Id::EspEdge, "Black Edge", Kind::Toggle(s.edge), ""),
                item(Id::EspDistance, "Distance", Kind::Toggle(s.labels), ""),
                item(Id::Open(Page::EspColours), "Colours", Kind::Sub, ""),
            ]
        }
        Page::EspColours => {
            let s = vkdraw::config().style;
            [(0, "Hostile"), (1, "Friendly"), (3, "Players"), (DRONE_COLOUR, "Drones")]
                .into_iter()
                .flat_map(|(i, name)| {
                    [
                        item(Id::EspColour(i), name, Kind::Choice(box_colour_name(esp_colour(&s, i))), ""),
                        item(Id::EspHex(i), "Hex Code", Kind::Value(box_colour_hex(esp_colour(&s, i))), ""),
                    ]
                })
                .collect()
        }
        Page::Target => {
            let t = entities::target();
            // No help text on Pick Target.
            let mut v = vec![item(Id::Aim, "Pick Target", Kind::Value(t.map(|t| format!("{:.1} m", t.2)).unwrap_or_else(|| "none".into())), "")];
            for (id, name) in [(Id::Yeet, "Yeet Target"), (Id::Bring, "Bring Target")] {
                let mut it = item(id, name, Kind::Action, "");
                it.row.dim = t.is_none();
                v.push(it);
            }
            v
        }
        Page::Ctx => {
            let (addr, sub) = ctx();
            let rows = all_rows(prefs);
            let id = rows.iter().find(|f| f.addr == addr).map(|f| f.item.id);
            let mut v = Vec::new();
            if sub {
                v.push(item(Id::CtxDefaults, "Apply Default State To Children", Kind::Action, ""));
            } else if id.is_some_and(has_default) {
                // Every row with a default gets it, as Stand's.
                v.push(item(Id::CtxDefaults, "Apply Default State", Kind::Action, ""));
            }
            // Every value row, as Stand's.
            if !sub && id.and_then(|id| limits(id, prefs)).is_some() {
                v.push(item(Id::CtxMin, "Set To Min Value", Kind::Action, ""));
                v.push(item(Id::CtxMax, "Set To Max Value", Kind::Action, ""));
            }
            // Every row with a state, and lists with such rows inside.
            if !state_rows(&rows, &addr, sub, prefs).is_empty() {
                v.push(item(Id::CtxSaveState, "Save State", Kind::Action, "Saves only this to the active profile."));
                v.push(item(Id::CtxLoadState, "Load State", Kind::Action, "Loads only this from the active profile."));
            }
            if matches!(id, Some(Id::Spot(_))) {
                v.push(item(Id::CtxRename, "Rename", Kind::Action, "Type the new name in the command box."));
            }
            v.push(item(Id::Open(Page::RowHotkeys), "Hotkeys", Kind::Sub, ""));
            v.push(item(Id::CtxSave, "Save", Kind::Toggle(saved_list().contains(&addr)), "Saves this command so you can find it\nin Aurora > Saved Commands."));
            v.push(item(Id::CtxCopy, "Copy Address", Kind::Action, ""));
            v
        }
        Page::RowHotkeys => {
            let keys = keys_of(&ctx().0);
            let mut v = vec![item(Id::CtxAddKey, "Add Hotkey", Kind::Action, "")];
            if !keys.is_empty() {
                v.push(item(Id::Header, "Hotkeys", Kind::Header, ""));
                v.extend(keys.into_iter().map(|vk| item(Id::Open(Page::CtxKey(vk)), key_name(vk), Kind::Sub, "")));
            }
            v
        }
        Page::CtxKey(vk) => vec![item(Id::CtxRemoveKey(vk), "Remove Hotkey", Kind::Action, "")],
        // The saved rows themselves, working as where they live (as Stand's). The rows themselves,
        // working in place, as Saved Commands.
        Page::NonDefault => {
            let v: Vec<Item> = nondefault_rows(prefs).into_iter().map(|e| e.1).collect();
            if v.is_empty() { vec![item(Id::Info, "Everything is at its default.", Kind::Info, "")] } else { v }
        }
        Page::Saved => {
            let v: Vec<Item> = saved_rows(prefs).into_iter().map(|e| e.1).collect();
            if v.is_empty() { vec![item(Id::Info, "Nothing saved yet", Kind::Info, "")] } else { v }
        }
        Page::HotkeyList => key_list(),
        // Warning text is only shown: never picked, highlighted or hovered.
        Page::Risk => vec![
            item(Id::WarnText, "Credits live on Ubisoft's servers.", Kind::Info, ""),
            item(Id::WarnText, "A changed amount can go back at the next login,", Kind::Info, ""),
            item(Id::WarnText, "or get the account looked at.", Kind::Info, ""),
            item(Id::WarnText, if risk_read(prefs.force_read) { "Go back, then press it again." } else { "Read this first." }, Kind::Info, ""),
        ],
        Page::CreditsPicker => {
            let v: Vec<Item> = credits::found()
                .into_iter()
                .map(|(o, b)| item(Id::CreditsPick(o), b.to_string(), Kind::Action, "Pick the amount the shop\nshows as your credits."))
                .collect();
            if v.is_empty() { vec![item(Id::Info, "Open a shop first", Kind::Info, "")] } else { v }
        }
        Page::OtherPick => {
            let mut v = vec![item(Id::Rescan, "Re-scan", Kind::Action, "Watches again for 20 s.\nOpen a shop, then pick\nthe amount you want.")];
            let seen: Vec<Item> = credits::others()
                .into_iter()
                .map(|(o, b)| item(Id::OtherPick(o), b.to_string(), Kind::Action, "Credits then changes this amount."))
                .collect();
            if credits::watching() {
                v.push(item(Id::Info, "Watching: open a shop now", Kind::Info, ""));
            } else if seen.is_empty() {
                v.push(item(Id::Info, "Nothing seen; Re-scan, then open a shop", Kind::Info, ""));
            }
            v.extend(seen);
            v
        }
        Page::Unload => {
            let mut v = vec![item(Id::WarnText, "Are you absolutely sure you want to do this?", Kind::Info, "")];
            // Yes only after 2 s, so the question gets read (as Stand's); at once with Force Me To
            // Read Warnings off.
            if yes_shows(prefs.force_read, asked_ago(&UNLOAD_ASKED)) {
                v.push(item(Id::UnloadYes, "Yes", Kind::Action, ""));
            }
            v
        }
        // Disables, then Rendering, as Stand's Game tab; Progress last, from Self.
        Page::Game => vec![
            item(Id::Open(Page::Disables), "Disables", Kind::Sub, ""),
            item(Id::Open(Page::Rendering), "Rendering", Kind::Sub, ""),
            item(Id::Open(Page::Progress), "Progress", Kind::Sub, ""),
        ],
        // Restrictions turned off: the old "No..." rows, by what they disable. No help lines here.
        // Checked both are a game restriction turned off, so both stay.
        Page::Disables => vec![
            item(Id::NoShake, "Screen Shake", Kind::Toggle(crate::weapon::shake_on()), ""),
            // A hub restriction, so it stays here; NVG Overlay went to Rendering.
            item(Id::NvgZone, "NVG In Erewhon", Kind::Toggle(crate::nvgzone::is_on()), ""),
        ],
        // What the player sees: NVG Overlay came from Disables. "No NVG Overlay" since.
        Page::Rendering => vec![item(Id::NoNvgOverlay, "No NVG Overlay", Kind::Toggle(crate::nvg::is_on()), "")],
        // Unload stays last.
        Page::Menu => {
            let mut v = vec![
                item(Id::Open(Page::MenuSettings), "Settings", Kind::Sub, ""),
                item(Id::Open(Page::Profiles), "Profiles", Kind::Sub, ""),
                // Hotkeys under Profiles, over Experiments.
                item(Id::Open(Page::HotkeyList), "Hotkeys", Kind::Sub, ""),
                item(Id::Open(Page::Saved), "Saved Commands", Kind::Sub, ""),
                item(Id::Open(Page::NonDefault), "Commands With Non-Default State", Kind::Sub, "Every command not at its default."),
                item(Id::ApplyDefaults, "Apply Default State", Kind::Action, "Resets all commands back\nto their default state."),
                item(Id::Unload, "Unload Aurora", Kind::Action, ""),
            ];
            // Only while Enable Experimental Features is on; under it.
            if prefs.experimental {
                v.insert(EXPERIMENTS_ROW, item(Id::Open(Page::Experiments), "Experiments", Kind::Sub, "Untested.\nCan crash your game or\ncause unwanted behaviour."));
            }
            v
        }
        Page::MenuSettings => vec![
            item(Id::Open(Page::Appearance), "Appearance", Kind::Sub, ""),
            item(Id::Open(Page::Notifications), "Notifications", Kind::Sub, ""),
            item(Id::Open(Page::Warnings), "Warnings", Kind::Sub, ""),
            // Under the lists, off by default.
            item(Id::MouseSupport, "Mouse Support", Kind::Toggle(prefs.mouse_support), "Use the mouse in the menu."),
            // Under Mouse Support, off by default.
            item(Id::Experimental, "Enable Experimental Features", Kind::Toggle(prefs.experimental), "Shows Aurora > Experiments.\nOff turns all experiments off."),
        ],
        // Split like Stand's: Settings > Notifications holds only Suppress Generic Responses; how
        // they look is Appearance > Notifications.
        Page::Notifications => vec![item(Id::Open(Page::Suppress), "Suppress Generic Responses", Kind::Sub, "Hides plain replies like \"Fly on\".")],
        // As Stand's Notifications page; the rows of Appearance > Toasts came here. Invert Flow,
        // Padding and Flash Colour went, Text Size came under Size; the lists go on top (`items`).
        // In Appearance.
        Page::NoteLook => vec![
            choice_row(Id::ToastPosition, "Position", prefs),
            item(Id::Open(Page::CustomPosition), "Custom Position", Kind::Sub, ""),
            layout_row(NOTIFY_WIDTH, prefs),
            layout_row(TOAST_SIZE, prefs),
            layout_row(NOTE_TEXT, prefs),
            choice_row(Id::ToastColour, "Border Colour", prefs),
            choice_row(Id::BackColour, "Background Colour", prefs),
            item(Id::FlashNote, "Flash Notification", Kind::Action, ""),
            item(Id::Open(Page::Timing), "Timing", Kind::Sub, ""),
        ],
        // Used when Position is Custom: screen pixels, as the menu's own X and Y.
        Page::CustomPosition => vec![
            item(Id::NotifyX, "X", Kind::Choice(prefs.notify_x.to_string()), &range(Id::NotifyX, prefs)),
            item(Id::NotifyY, "Y", Kind::Choice(prefs.notify_y.to_string()), &range(Id::NotifyY, prefs)),
        ],
        Page::Timing => vec![layout_row(TOAST_TIME, prefs)],
        // Command Box and Hotkey only: the menu by hand never shows a plain reply. Errors and
        // warnings always show.
        Page::Suppress => vec![
            item(Id::QuietBox, "Command Box", Kind::Toggle(prefs.quiet_box), "Includes the command box."),
            item(Id::QuietKey, "Hotkey", Kind::Toggle(prefs.quiet_key), ""),
        ],
        // A list for each thing, then Stand's size rows straight in it.
        Page::Appearance => vec![
            item(Id::Open(Page::Position), "Position", Kind::Sub, ""),
            item(Id::Open(Page::MenuColours), "Colours", Kind::Sub, ""),
            item(Id::Open(Page::Tabs), "Tabs", Kind::Sub, ""),
            item(Id::Open(Page::AddressBar), "Address Bar", Kind::Sub, ""),
            item(Id::Open(Page::NoteLook), "Notifications", Kind::Sub, ""),
            layout_row(VISIBLE_ROWS, prefs),
            layout_row(LIST_W, prefs),
            layout_row(LIST_H, prefs),
            layout_row(SPACER_SIZE, prefs),
        ],
        // The menu's own colours; more go here later, ESP's stay in ESP.
        Page::MenuColours => vec![choice_row(Id::MenuColour, "Menu Colour", prefs)],
        Page::Tabs => (0..2).map(|i| layout_row(i, prefs)).collect(),
        Page::AddressBar => (2..4).map(|i| layout_row(i, prefs)).collect(),
        // Pixels, as Stand's. X is the wall, the menu's right edge.
        Page::Position => vec![
            item(Id::PosX, "X", Kind::Choice(menu_pos(&prefs, 0).to_string()), "The menu's right edge, in screen pixels."),
            item(Id::PosY, "Y", Kind::Choice(menu_pos(&prefs, 1).to_string()), "The address bar's top, in screen pixels."),
            move_with_mouse(prefs),
        ],
        // As Stand's. No commands here, so the help lines are as the player's approved picture has
        // them.
        Page::Warnings => vec![
            item(Id::Open(Page::AutoProceed), "Automatically Proceed On Warnings", Kind::Sub, ""),
            item(Id::ForceRead, "Force Me To Read Warnings", Kind::Toggle(prefs.force_read), ""),
            item(Id::SkipAccepted, "Skip Already-Accepted Warnings", Kind::Toggle(prefs.skip_accepted), ""),
        ],
        Page::AutoProceed => vec![
            item(Id::AutoBox, "Command Box", Kind::Toggle(prefs.auto_box), "Includes the command box."),
            item(Id::AutoKey, "Hotkey", Kind::Toggle(prefs.auto_key), ""),
        ],
        // Findc's rows, working as where they live (as Saved Commands).
        Page::Find => {
            let v: Vec<Item> = find_rows(prefs).into_iter().map(|e| e.1).collect();
            if v.is_empty() { vec![item(Id::Info, "No matching commands", Kind::Info, "")] } else { v }
        }
        // Untested switches live here until a test proves them. Fly Water Fixes: the water snap
        // after Fly off. Silent Aim's list came from Self > Weapons.
        Page::Experiments => vec![
            item(Id::Open(Page::SilentAim), "Silent Aim", Kind::Sub, ""),
            item(Id::UnseenInbox, "Ignore only me (untested)", Kind::Toggle(crate::inbox::is_on()), "Enemies drop what they sense of you only.\nNot tested: leave off."),
            item(Id::Header, "Fly Water Fixes", Kind::Header, ""),
            item(Id::FlyCalm, "Wait For Calm (untested)", Kind::Toggle(fly::calm()), "At Fly off, holds you until the\ngame stops pulling you (1.5 s max)."),
            item(Id::FlyGuard, "Snap Guard (untested)", Kind::Toggle(fly::guard()), "If the game throws you back after\nFly off, puts you back (3 tries)."),
            item(Id::FlyLetFall, "Let Game Fall (untested)", Kind::Toggle(crate::airwalk::let_fall()), "While you fly, the game may\nstart your fall. Aiming stays."),
        ],
        // Before Enable Experimental Features goes on, as the other warnings:
        // Yes after the read wait.
        Page::ExpWarn => {
            let mut v = vec![
                item(Id::WarnText, "Experiments are untested.", Kind::Info, ""),
                item(Id::WarnText, "They can crash your game or", Kind::Info, ""),
                item(Id::WarnText, "cause unwanted behaviour.", Kind::Info, ""),
            ];
            if yes_shows(prefs.force_read, asked_ago(&EXP_ASKED)) {
                v.push(item(Id::ExpYes, "Yes", Kind::Action, ""));
            }
            v
        }
        Page::Profiles => {
            let mut v = vec![
                // No help lines on these two.
                item(Id::NewProfile, "Create Profile", Kind::Action, ""),
                item(Id::OpenProfiles, "Open Folder", Kind::Action, ""),
                item(Id::Header, "Profiles", Kind::Header, ""),
            ];
            v.extend(profiles().into_iter().map(|n| item(Id::Open(Page::Profile(profile_id(&n))), n, Kind::Sub, "")));
            v
        }
        Page::Profile(n) => {
            let name = profile_name(n);
            vec![
                item(Id::ProfileSave(n), "Save", Kind::Action, "Writes how everything is set now\ninto this profile."),
                item(Id::ProfileLoad(n), "Load", Kind::Action, "Sets everything as this profile has it."),
                // One line.
                item(Id::ProfileOnInject(n), "Load On Inject", Kind::Toggle(profile_flag(&name, ON_INJECT)), "Loads this profile when Aurora injects."),
                item(Id::ProfileAuto(n), "Automatically Save", Kind::Toggle(profile_flag(&name, AUTO_SAVE)), "Writes every change to this profile\nwhile it is the last one loaded or saved."),
                item(Id::ProfileDelete(n), "Delete", Kind::Action, ""),
            ]
        }
        Page::ProfileDelete(n) => {
            let mut v = vec![item(Id::WarnText, format!("Are you absolutely sure you want to delete {}?", profile_name(n)), Kind::Info, "")];
            // Yes only after 2 s, as Unload's (Force Me To Read Warnings).
            if yes_shows(prefs.force_read, asked_ago(&DELETE_ASKED)) {
                v.push(item(Id::ProfileDeleteYes(n), "Yes", Kind::Action, ""));
            }
            v
        }
    }
}

/// Lowest and highest for a value row, shown when it is picked. Damage Multiplier: 0 is off, up to
/// a million with two decimals as Fly.
const DAMAGE_MAX: f32 = 1_000_000.0;
/// Fire Rate: 0 is off, up to a million times as fast. The weapon hook holds the wait between shots
/// to 2 ms at least (`weapon::FIRE_MIN_S`, 500 shots a second), so past a few hundred nothing
/// changes.
const FIRE_MAX: f32 = 1_000_000.0;
const GEAR_DEFAULT: u32 = 1000;
/// The player's note on the gear score rows while Enable Experimental Features is on.
const GEAR_HIGH_HELP: &str = "Recommended max is 600.\nIf you set an absurd number,\nuse at your own risk.";

/// Left / Right step for a value row: 5 from 1000 up, else 1 (998, 999, 1000, 1005 and back down
/// the same way; below 0 the same, mirrored).
fn step(v: f64, dir: i32) -> f64 {
    let away = (v >= 0.0) == (dir > 0);
    if v.abs() > 1000.0 || (v.abs() >= 1000.0 && away) { 5.0 } else { 1.0 }
}

/// Teleport > Coordinates as the player set them: profile rows like the others. 0, 0, 0 at every
/// inject.
static COORDS: Mutex<[f64; 3]> = Mutex::new([0.0; 3]);

fn coords() -> [f64; 3] {
    *COORDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Up to two decimals, as typed; Left / Right still step whole numbers.
fn set_coord(i: usize, v: f64) {
    COORDS.lock().unwrap_or_else(|e| e.into_inner())[i] = cents(v.clamp(-COORD_MAX, COORD_MAX));
}

/// A number kept to two decimals.
fn cents(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

const COORD_MAX: f64 = 100_000.0;

/// A typed or stepped pixel position, kept in Stand's range.
fn pixel(v: f64) -> i32 {
    v.round().clamp(PIXEL_MIN, PIXEL_MAX) as i32
}

/// "x2.50" with two decimals, as Fly's rows; 0 is "Off", the game's own.
fn times_text(x: f32) -> String {
    if x <= 0.0 { "Off".into() } else { format!("x{:.2}", x) }
}

fn damage_text() -> String {
    times_text(crate::weapon::damage())
}

fn fire_text() -> String {
    times_text(crate::weapon::fire_rate())
}

/// A Damage Multiplier or Fire Rate value: 0.00 to `max`, two decimals.
fn times_value(x: f64, max: f32) -> f32 {
    cents(x.clamp(0.0, f64::from(max))) as f32
}

fn set_fire(st: &mut State, x: f32) {
    let x = times_value(f64::from(x), FIRE_MAX);
    switched(st, crate::weapon::set_fire_rate(x), &if x <= 0.0 { "Fire rate off".to_string() } else { format!("Fire rate {}", times_text(x)) });
}

fn set_damage(st: &mut State, x: f32) {
    let x = times_value(f64::from(x), DAMAGE_MAX);
    switched(st, crate::weapon::set_damage(x), &if x <= 0.0 { "Damage multiplier off".to_string() } else { format!("Damage {}", times_text(x)) });
}

fn range(id: Id, prefs: Prefs) -> String {
    match id {
        Id::Damage | Id::FireRate => "0.00 to 1000000.00 (0 is off)",
        Id::FlySpeed | Id::FlySprint => "0.01 to 10000.00",
        Id::HopHeight => "0.50 to 15.00 m",
        Id::GearValue => return format!("0 - {} (0 is the game's own)", gear_max(prefs)),
        Id::GearTotal | Id::GearSlot(_) | Id::GearOwned => return format!("0 - {}", gear_max(prefs)),
        Id::Coord(_) => "-100000.00 to 100000.00",
        Id::Credits => "0 - 2147483647",
        Id::EspRange => "25 - 1500 m",
        Id::SilentFov => "1 - 90 deg",
        Id::SilentRange => "10 - 1000 m",
        Id::EspLines => "0 - 6 px (0 is auto)",
        // As Stand's menux and notifypadding.
        Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY => "-32768 to 32767",
        Id::Layout(i) => return format!("{} to {}", LAYOUT_ROWS[i].3, LAYOUT_ROWS[i].4),
        _ => "",
    }
    .into()
}

/// A value row's lowest and highest: Set To Min / Max Value use them, and Left / Right go round at
/// them. None for rows that are not a number.
fn limits(id: Id, prefs: Prefs) -> Option<(f64, f64)> {
    Some(match id {
        Id::FlySpeed | Id::FlySprint => (fly::MIN, fly::MAX),
        Id::HopHeight => (fly::HOP_MIN_M, fly::HOP_MAX_M),
        Id::Damage => (0.0, f64::from(DAMAGE_MAX)),
        Id::FireRate => (0.0, f64::from(FIRE_MAX)),
        Id::GearValue | Id::GearTotal | Id::GearSlot(_) | Id::GearOwned => (0.0, f64::from(gear_max(prefs))),
        Id::Credits => (0.0, f64::from(credits::MAX)),
        Id::Coord(_) => (-COORD_MAX, COORD_MAX),
        Id::EspRange => (25.0, 1500.0),
        Id::SilentFov => (crate::silentaim::FOV_MIN, crate::silentaim::FOV_MAX),
        Id::SilentRange => (crate::silentaim::RANGE_MIN, crate::silentaim::RANGE_MAX),
        Id::EspLines => (0.0, 6.0),
        Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY => (PIXEL_MIN, PIXEL_MAX),
        Id::Layout(i) => (f64::from(LAYOUT_ROWS[i].3), f64::from(LAYOUT_ROWS[i].4)),
        _ => return None,
    })
}

/// Fly's Left / Right step: 2.00 to 2.10 or 1.90, as Stand's.
const FLY_STEP: f64 = 0.10;
/// Damage Multiplier's and Fire Rate's Left / Right step: Off, x1.00, x2.00 ... up to the top, then
/// Off again.
const TIMES_STEP: f64 = 1.0;

/// One Left / Right step on a value row, as Stand's: `by` up or down, stopping at the ends; one
/// more step at an end goes round to the other end. Only a value sitting on the end goes round: one
/// read from the game past the end (a 5160 weapon on a gear slot) is brought back to the end, not
/// round.
fn stepped(now: f64, by: f64, dir: i32, lo: f64, hi: f64) -> f64 {
    const NEAR: f64 = 1e-6;
    if dir > 0 {
        if (now - hi).abs() <= NEAR { lo } else { (now + by).clamp(lo, hi) }
    } else if (now - lo).abs() <= NEAR {
        hi
    } else {
        (now - by).clamp(lo, hi)
    }
}

fn note(_st: &mut State, msg: impl Into<String>) {
    let msg = msg.into();
    let from = FROM.load(Ordering::Relaxed);
    // While a profile loads, what went wrong goes in the log too (the toast after is the
    // profile's).
    if from == FROM_PROFILE {
        log(&format!("profile: {msg}"));
    }
    // Suppress Generic Responses > Command Box on: nothing at all after the command box (or a
    // command.request), errors such as "No command called x" too. The line still goes to dll.log.
    if hushed_now() {
        log(&format!("command box (suppressed): {msg}"));
        return;
    }
    toast(msg);
}

/// After the command box with its Suppress switch on, every notification is hidden, errors and
/// warnings included. `in_box`: a command-box line is still running, also while it loads a profile.
/// Hotkeys keep showing their errors.
fn box_hushed(from: u8, in_box: bool, quiet_box: bool) -> bool {
    (from == FROM_BOX || in_box) && quiet_box
}

/// `box_hushed` for what is running now. Work that finishes later on its own thread (Set All Owned
/// Items) asks this when it starts and keeps the answer.
pub(crate) fn hushed_now() -> bool {
    box_hushed(FROM.load(Ordering::Relaxed), IN_BOX.load(Ordering::Relaxed), QUIET_BOX.load(Ordering::Relaxed))
}

/// Where the command being run came from, for Suppress Generic Responses: the menu itself, the
/// command box, a hotkey, or a profile loading.
const FROM_MENU: u8 = 0;
const FROM_BOX: u8 = 1;
const FROM_KEY: u8 = 2;
const FROM_PROFILE: u8 = 3;
static FROM: AtomicU8 = AtomicU8::new(FROM_MENU);
/// The player's Suppress Generic Responses switches (command box, hotkey), kept here too so the
/// hotkeys' own thread can see them.
static QUIET_BOX: AtomicBool = AtomicBool::new(false);
/// A command-box line (or command.request) is running: set around `run_line` there, so a profile it
/// loads still counts as the command box for Suppress.
static IN_BOX: AtomicBool = AtomicBool::new(false);
static QUIET_KEY: AtomicBool = AtomicBool::new(false);

/// After the command box or a hotkey: hidden when the player suppressed those; while a profile
/// loads: always hidden.
fn quiet_from(from: u8) -> bool {
    match from {
        FROM_BOX => QUIET_BOX.load(Ordering::Relaxed),
        FROM_KEY => QUIET_KEY.load(Ordering::Relaxed),
        _ => true,
    }
}

/// A plain "X on" / "X off" / "set to" toast: never after something done by hand in the menu, nor
/// while a profile loads; after the command box or a hotkey unless the player suppressed those.
/// Errors and warnings go through `note` and show, except after the command box with its Suppress
/// switch on.
fn done(st: &mut State, msg: impl Into<String>) {
    if !quiet_from(FROM.load(Ordering::Relaxed)) {
        note(st, msg);
    }
}

/// Any other "it worked" toast ("key removed", "Profile saved", "Teleported to..."): from the menu
/// it shows (it is no plain on / off / set to); after the command box or a hotkey it is hidden by
/// that switch, as `done`. Errors show, except after a suppressed command box (`box_hushed`).
fn confirm(st: &mut State, msg: impl Into<String>) {
    let from = FROM.load(Ordering::Relaxed);
    if from == FROM_MENU || !quiet_from(from) {
        note(st, msg);
    }
}

/// A switch's result: its plain "on" / "off" as `done`, a failure always shown.
fn switched(st: &mut State, r: Result<(), String>, ok: &str) {
    match r {
        Ok(()) => done(st, ok),
        Err(e) => note(st, e),
    }
}

/// An action's result: its "it worked" as `confirm`, a failure always shown.
fn result(st: &mut State, r: Result<(), String>, ok: &str) {
    match r {
        Ok(()) => confirm(st, ok),
        Err(e) => note(st, e),
    }
}

fn esp_change(st: &mut State, f: impl FnOnce(&mut vkdraw::Config)) {
    let mut c = vkdraw::config();
    f(&mut c);
    if let Err(e) = vkdraw::set(c) {
        note(st, e);
    }
}

fn activate(st: &mut State, it: &Item) {
    if it.row.dim {
        return;
    }
    // A switch waiting for the world counts as on: Enter or its hotkey turns it off.
    if matches!(it.row.kind, Kind::Toggle(_)) && unwait(it.id) {
        done(st, format!("{} stays off", it.row.text));
        return;
    }
    match it.id {
        Id::God => {
            fly::forget_god();
            let (entity, armed, _) = godmode::status();
            if entity != 0 || armed {
                switched(st, godmode::off(), "God mode off");
            } else if godmode::on_if_known() {
                done(st, "God mode on");
            } else {
                // What to do next, not a "done": always shown.
                match godmode::arm_background(60_000) {
                    Ok(()) => note(st, "Armed: take one hit and it turns on"),
                    Err(e) => note(st, e),
                }
            }
        }
        Id::Fly => {
            let on = !fly::is_on();
            switched(st, fly::set(on), if on { "Fly on" } else { "Fly off" });
        }
        Id::NoShake => {
            let on = !crate::weapon::shake_on();
            switched(st, crate::weapon::set_shake(on), if on { "No screen shake on" } else { "No screen shake off" });
        }
        Id::NoNvgOverlay => {
            let on = !crate::nvg::is_on();
            switched(st, crate::nvg::set(on), if on { "No NVG overlay on" } else { "No NVG overlay off" });
        }
        Id::NoSway => {
            let on = !crate::weapon::sway_on();
            switched(st, crate::weapon::set_sway(on), if on { "No scoped sway on" } else { "No scoped sway off" });
        }
        Id::Stamina => {
            let on = !crate::weapon::stamina_on();
            switched(st, crate::weapon::set_stamina(on), if on { "Infinite stamina on" } else { "Infinite stamina off" });
        }
        Id::NoRecoil => {
            let on = !crate::weapon::recoil_on();
            switched(st, crate::weapon::set_recoil(on), if on { "No recoil on" } else { "No recoil off" });
        }
        Id::Accuracy => {
            let on = !crate::weapon::accuracy_on();
            switched(st, crate::weapon::set_accuracy(on), if on { "No spread on" } else { "No spread off" });
        }
        Id::Ammo => {
            let on = !crate::weapon::ammo_on();
            switched(st, crate::weapon::set_ammo(on), if on { "Infinite mag on" } else { "Infinite mag off" });
        }
        Id::Reserve => {
            let on = !crate::weapon::reserve_on();
            switched(st, crate::weapon::set_reserve(on), if on { "Infinite reserve on" } else { "Infinite reserve off" });
        }
        Id::SilentAim => {
            let on = !crate::silentaim::is_on();
            switched(st, crate::silentaim::set(on), if on { "Silent aim on" } else { "Silent aim off" });
        }
        Id::SilentCircle => crate::silentaim::set_circle(!crate::silentaim::circle()),
        Id::SilentMagic => crate::silentaim::set_magic(!crate::silentaim::magic()),
        Id::SilentNoRange => crate::silentaim::set_no_range(!crate::silentaim::no_range()),
        Id::SilentInstant => crate::silentaim::set_instant(!crate::silentaim::instant()),
        Id::SilentNoDrop => {
            let on = !crate::silentaim::no_drop();
            switched(st, crate::silentaim::set_no_drop(on), if on { "No bullet drop on" } else { "No bullet drop off" });
        }
        Id::Unseen => {
            let on = !crate::unseen::is_on();
            switched(st, crate::unseen::set(on), if on { "Undetected on" } else { "Undetected off" });
        }
        Id::UnseenInbox => {
            let on = !crate::inbox::is_on();
            switched(st, crate::inbox::set(on), if on { "Ignore only me on" } else { "Ignore only me off" });
        }
        Id::NvgZone => {
            let on = !crate::nvgzone::is_on();
            switched(st, crate::nvgzone::set(on), if on { "NVG in Erewhon on" } else { "NVG in Erewhon off" });
        }
        Id::HopUp => {
            let on = !fly::hop_up();
            fly::set_hop_up(on);
            done(st, if on { "Hop up on" } else { "Hop up off" });
        }
        Id::FlyCalm => {
            let on = !fly::calm();
            fly::set_calm(on);
            done(st, if on { "Wait for calm on" } else { "Wait for calm off" });
        }
        Id::FlyGuard => {
            let on = !fly::guard();
            fly::set_guard(on);
            done(st, if on { "Snap guard on" } else { "Snap guard off" });
        }
        Id::FlyLetFall => {
            let on = !crate::airwalk::let_fall();
            crate::airwalk::set_let_fall(on);
            done(st, if on { "Let game fall on" } else { "Let game fall off" });
        }
        Id::SafeFly => fly::set_safe(!fly::safe()),
        Id::Spot(i) => match spots().get(i) {
            Some((name, p)) => {
                let name = name.clone();
                result(st, tp::to(*p), &format!("Teleported to {name}"));
            }
            None => note(st, "That spot is gone"),
        },
        Id::SaveSpot => match entities::player_pos() {
            Some(p) => match add_spot(p) {
                Ok(name) => confirm(st, format!("Saved as {name}")),
                Err(e) => note(st, e),
            },
            None => note(st, "Your position was not found"),
        },
        Id::Undo => result(st, tp::undo(), "Back where the last teleport started"),
        Id::GoCoords => {
            let c = coords();
            result(st, tp::to(c), &format!("Teleported to {}, {}, {}", number(c[0]), number(c[1]), number(c[2])));
        }
        Id::Open(p) => {
            st.stack.push((p, first_pick(p, st.prefs), 0));
        }
        // Yes on the warning: Enable Experimental Features on, back where it was asked.
        Id::ExpYes => {
            accept(Warn::Experiments);
            if st.stack.last().is_some_and(|t| t.0 == Page::ExpWarn) {
                st.stack.pop();
            }
            set_experimental(st, true);
        }
        // Enable Experimental Features: on after the Experiments warning, as Warnings lets it; a
        // profile with it on turns it on without asking. Off at once, and every experiment with it.
        Id::Experimental => {
            let from = FROM.load(Ordering::Relaxed);
            if st.prefs.experimental {
                set_experimental(st, false);
            } else if from == FROM_PROFILE || warn_skipped(&st.prefs, from, accepted(Warn::Experiments)) {
                set_experimental(st, true);
            } else {
                ask_experiments(st);
            }
        }
        // Not at the title screen. The unload.request file for scripts still works.
        Id::Unload | Id::UnloadYes if crate::fly::body_addr().is_none() => {
            note(st, "Unload works once you're in the world");
        }
        Id::Unload => {
            if warn_skipped(&st.prefs, FROM.load(Ordering::Relaxed), accepted(Warn::Unload)) {
                unload_now(st);
            } else {
                if let Ok(mut t) = UNLOAD_ASKED.lock() {
                    *t = Some(Instant::now());
                }
                ask(st, Page::Unload);
            }
        }
        Id::UnloadYes => {
            accept(Warn::Unload);
            unload_now(st);
        }
        // Warnings and Position's switches. Mouse Support too: with it and Move With Mouse both
        // off, the worker gives the mouse back on this tick.
        Id::AutoBox | Id::AutoKey | Id::ForceRead | Id::SkipAccepted | Id::MoveMouse | Id::MouseSupport => {
            let p = &mut st.prefs;
            let b = match it.id {
                Id::AutoBox => &mut p.auto_box,
                Id::AutoKey => &mut p.auto_key,
                Id::ForceRead => &mut p.force_read,
                Id::SkipAccepted => &mut p.skip_accepted,
                Id::MouseSupport => &mut p.mouse_support,
                _ => &mut p.move_mouse,
            };
            *b = !*b;
            prefs_changed(st.prefs);
        }
        // A test notification, shown whatever Suppress Generic Responses says.
        Id::FlashNote => toast(FLASH_TEXT),
        Id::QuietBox => {
            st.prefs.quiet_box = !st.prefs.quiet_box;
            prefs_changed(st.prefs);
        }
        Id::QuietKey => {
            st.prefs.quiet_key = !st.prefs.quiet_key;
            prefs_changed(st.prefs);
        }
        Id::KeyRow(i) => {
            if let Some((addr, _)) = row_keys().into_iter().nth(i) {
                let sub = resolve(&addr, st.prefs).is_some_and(|f| matches!(f.item.id, Id::Open(_)));
                if let Ok(mut c) = CTX.lock() {
                    *c = Some((addr, sub));
                }
                st.stack.push((Page::RowHotkeys, first_pick(Page::RowHotkeys, st.prefs), 0));
            }
        }
        Id::EspOn => esp_change(st, |c| c.on = !c.on),
        Id::EspEnemies => esp_change(st, |c| c.hostile = !c.hostile),
        Id::EspFriendlies => esp_change(st, |c| c.friendly = !c.friendly),
        Id::EspPlayers => esp_change(st, |c| c.players = !c.players),
        Id::EspDrones => esp_change(st, |c| c.drones = !c.drones),
        Id::EspHideDead => esp_change(st, |c| c.hide_dead = !c.hide_dead),
        Id::EspDistance => esp_change(st, |c| c.style.labels = !c.style.labels),
        Id::EspEdge => esp_change(st, |c| c.style.edge = !c.style.edge),
        Id::EspBones => esp_change(st, |c| c.style.bones = !c.style.bones),
        Id::Aim => match entities::aim() {
            Ok((_, _, dist, _)) => note(st, format!("Target: {dist:.1} m away")),
            Err(e) => note(st, e),
        },
        Id::Yeet => result(st, entities::yeet_target(), "Yeeted"),
        Id::Bring => result(st, entities::bring_target(), "Brought over"),
        Id::Credits | Id::PickOther if ask_risk(st) => {}
        Id::Credits if credits::balance().is_none() && !credits::catching() && !credits::found().is_empty() => {
            st.stack.push((Page::CreditsPicker, 0, 0));
        }
        Id::CreditsPick(o) => {
            credits::pick(o);
            back(st);
            confirm(st, "Credits picked");
        }
        // The first visit watches at once; after that Re-scan does.
        Id::PickOther => {
            if !credits::watching() && credits::others().is_empty() {
                watch_others(st);
            }
            st.stack.push((Page::OtherPick, 0, 0));
        }
        Id::Rescan => watch_others(st),
        Id::OtherPick(o) => {
            credits::pick_own(o);
            back(st);
            confirm(st, "Picked; Credits changes that amount now");
        }
        Id::Credits if credits::balance().is_none() => {
            if credits::start_catch() {
                note(st, "Open a shop now");
            } else {
                note(st, "Still waiting for a shop");
            }
        }
        // Typing a value goes through the command box, filled in, as Stand's.
        Id::EspHex(_) | Id::Credits | Id::Damage | Id::FireRate | Id::FlySpeed | Id::FlySprint | Id::HopHeight | Id::GearValue | Id::GearTotal | Id::GearSlot(_) | Id::GearOwned | Id::EspRange | Id::SilentFov | Id::SilentRange | Id::EspLines | Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY | Id::Layout(_) | Id::Coord(_) => {
            let name = command(it.id).unwrap_or_default();
            let line = format!("{name} {}", current(it.id, st.prefs));
            open_box(st, line);
            // A click in the menu: typing the value is still by hand.
            if FROM.load(Ordering::Relaxed) == FROM_MENU {
                st.box_row = Some(name);
            }
        }
        Id::MenuColour | Id::EspColour(_) | Id::ToastPosition | Id::ToastColour | Id::BackColour | Id::SilentPriority => adjust(st, it.id, 1),
        Id::Bind(b) => st.bind = Some((b, held_now())),
        Id::CtxDefaults => match resolve(&ctx().0, st.prefs) {
            Some(Found { item: Item { id: Id::Open(p), .. }, .. }) => {
                defaults(st, p, 0);
                confirm(st, "Default state applied");
            }
            Some(f) if has_default(f.item.id) => {
                default_of(st, f.item.id);
                confirm(st, "Default state applied");
            }
            _ => note(st, "Nothing to reset here"),
        },
        Id::CtxMin | Id::CtxMax => match resolve(&ctx().0, st.prefs).map(|f| f.item.id) {
            // Credits read their warning first, as Enter on them.
            Some(Id::Credits) if ask_risk(st) => {}
            Some(id) => match limits(id, st.prefs) {
                Some((lo, hi)) => set_typed(st, id, if it.id == Id::CtxMin { lo } else { hi }),
                None => note(st, "No value here"),
            },
            None => note(st, "That command is gone"),
        },
        Id::CtxSaveState => save_state(st),
        Id::CtxLoadState => load_state(st),
        Id::CtxSave => {
            let addr = ctx().0;
            let mut v = saved_list();
            if let Some(i) = v.iter().position(|a| *a == addr) {
                v.remove(i);
                confirm(st, "Taken out of Saved Commands");
            } else {
                v.push(addr);
                confirm(st, "Saved to Aurora > Saved Commands");
            }
            set_saved(&v);
        }
        Id::CtxCopy => {
            if copy_text(&ctx().0) {
                confirm(st, "Address copied");
            } else {
                note(st, "Could not copy");
            }
        }
        Id::CtxAddKey => st.bind = Some((Bind::RowAdd, held_now())),
        // Back to the spots, with the command box filled in for the new name.
        Id::CtxRename => match resolve(&ctx().0, st.prefs).map(|f| f.item.id) {
            Some(Id::Spot(i)) => match spots().get(i) {
                Some(s) => {
                    let line = format!("renamespot {} ", squash(&s.0));
                    back(st);
                    open_box(st, line);
                }
                None => note(st, "That spot is gone"),
            },
            _ => note(st, "That spot is gone"),
        },
        Id::CtxRemoveKey(vk) => {
            remove_row_key(st, vk);
            back(st);
        }
        Id::Run(i) => {
            let saved = saved_list();
            match saved.get(i).and_then(|a| resolve(a, st.prefs)) {
                Some(f) => run_found(st, f, None),
                // Behind Enable Experimental Features while it is off: not gone.
                None if saved.get(i).is_some_and(|a| hidden_experiment(a, st.prefs)) => note(st, EXP_OFF),
                None => note(st, "That command is gone"),
            }
        }
        Id::NewProfile => open_box(st, "createprofile ".into()),
        Id::OpenProfiles => match profiles_dir() {
            // The player's default folder app (Glint), as a double-click; Explorer where none is
            // set. `ShellExecuteW` with no verb runs the folder's default verb.
            Some(d) => open_folder(&d),
            None => note(st, "No profiles folder"),
        },
        Id::ProfileSave(n) => save_said(st, &profile_name(n)),
        Id::ProfileLoad(n) => load_said(st, &profile_name(n)),
        Id::ProfileOnInject(n) => {
            let name = profile_name(n);
            let on = !profile_flag(&name, ON_INJECT);
            set_profile_flag(&name, ON_INJECT, on);
            done(st, if on { "Load on inject on" } else { "Load on inject off" });
        }
        Id::ProfileAuto(n) => {
            let name = profile_name(n);
            let on = !profile_flag(&name, AUTO_SAVE);
            set_profile_flag(&name, AUTO_SAVE, on);
            done(st, if on { "Automatically save on" } else { "Automatically save off" });
        }
        Id::ProfileDelete(n) => {
            if warn_skipped(&st.prefs, FROM.load(Ordering::Relaxed), delete_accepted(&profile_name(n))) {
                delete_profile(st, n);
            } else {
                if let Ok(mut t) = DELETE_ASKED.lock() {
                    *t = Some(Instant::now());
                }
                ask(st, Page::ProfileDelete(n));
            }
        }
        Id::ProfileDeleteYes(n) => {
            accept_delete(&profile_name(n));
            delete_profile(st, n);
        }
        Id::ApplyDefaults => {
            default_all(st);
            confirm(st, "Everything back to default");
        }
        Id::WhereAmI | Id::Info | Id::WarnText | Id::Header => {}
    }
}

/// The warning before Enable Experimental Features goes on, menu opened if needed; its Yes shows
/// after the read wait.
fn ask_experiments(st: &mut State) {
    if let Ok(mut t) = EXP_ASKED.lock() {
        *t = Some(Instant::now());
    }
    ask(st, Page::ExpWarn);
}

/// When the question before Experiments was asked.
static EXP_ASKED: Mutex<Option<Instant>> = Mutex::new(None);

/// Aurora > Experiments, the list's own address.
const EXPERIMENTS_ADDR: &str = "Aurora > Experiments";
/// Enable Experimental Features' address (its profile line).
const EXPERIMENTAL_ADDR: &str = "Aurora > Settings > Enable Experimental Features";
/// A hotkey or command on an Experiments row while Enable Experimental Features is off.
const EXP_OFF: &str = "Turn on Enable Experimental Features first";
/// Save State on Experiments rows.
const EXP_UNSAVED: &str = "Experiments are never saved";
/// Where the Experiments row sits in the Aurora list while the switch is on.
const EXPERIMENTS_ROW: usize = 3;

/// An address in Aurora > Experiments, the list itself too.
fn in_experiments(addr: &str) -> bool {
    addr.strip_prefix(EXPERIMENTS_ADDR).is_some_and(|rest| rest.is_empty() || rest.starts_with(" > "))
}

/// Enable Experimental Features on or off. Off: every experiment that is on goes off first, then
/// Experiments and its lists close wherever they are open, also a list opened from Find or Saved
/// Commands. The Aurora list's picks below the Experiments row move with their rows, so the cursor
/// and Back stay on the same row, never on Apply Default State by accident.
fn set_experimental(st: &mut State, on: bool) {
    if st.prefs.experimental == on {
        return;
    }
    if !on {
        // The cap first, so Set All Owned Items writes no more than 1000 from here.
        gearscore::set_cap(gearscore::MAX);
        experiments_off(st);
        // No gear score over 1000 without it.
        gear_back();
        let pages = experiment_pages(st.prefs);
        for stack in std::iter::once(&mut st.stack).chain(st.saved.iter_mut()) {
            if let Some(i) = stack.iter().position(|x| pages.contains(&x.0)) {
                stack.truncate(i.max(1));
            }
        }
    }
    for stack in std::iter::once(&mut st.stack).chain(st.saved.iter_mut()) {
        for e in stack.iter_mut().filter(|e| e.0 == Page::Menu) {
            if on && e.1 >= EXPERIMENTS_ROW {
                e.1 += 1;
            } else if !on && e.1 > EXPERIMENTS_ROW {
                e.1 -= 1;
            }
        }
    }
    st.prefs.experimental = on;
    gearscore::set_cap(gear_max(st.prefs));
    prefs_changed(st.prefs);
    done(st, if on { "Experimental features on" } else { "Experimental features off" });
}

/// Experiments and the lists inside it (Silent Aim): what closes when the switch goes off.
fn experiment_pages(prefs: Prefs) -> Vec<Page> {
    let mut v = vec![Page::Experiments];
    for f in all_rows(Prefs { experimental: true, ..prefs }) {
        if let Id::Open(p) = f.item.id
            && in_experiments(&f.addr)
            && !v.contains(&p)
        {
            v.push(p);
        }
    }
    v
}

/// A saved address that lives in Experiments (also by an old address) while the switch is off:
/// hidden, not gone.
fn hidden_experiment(addr: &str, prefs: Prefs) -> bool {
    !prefs.experimental && (in_experiments(addr) || moved(addr).is_some_and(|m| in_experiments(&m)))
}

/// Every experiment that is on, off: the switches in Experiments (and its lists) that are off by
/// default, as Apply Default State does it. Values, and switches on by default (Show FOV Circle),
/// stay.
fn experiments_off(st: &mut State) {
    let prefs = Prefs { experimental: true, ..st.prefs };
    let off = on_off(false);
    for f in all_rows(prefs) {
        let id = f.item.id;
        if in_experiments(&f.addr)
            && matches!(f.item.row.kind, Kind::Toggle(_))
            && default_state(id).as_deref() == Some(off.as_str())
            && state_of(&f.item, prefs).is_some_and(|now| now != off)
        {
            default_of(st, id);
        }
    }
}

/// The rows inside Aurora > Experiments with a state in a row or list (`state_rows`), also while
/// the switch is off and hides them.
fn experiment_ids(prefs: Prefs, addr: &str, sub: bool) -> Vec<Id> {
    let on = Prefs { experimental: true, ..prefs };
    state_rows(&all_rows(on), addr, sub, on).into_iter().filter(|f| in_experiments(&f.addr)).map(|f| f.item.id).collect()
}

/// Rows inside Aurora > Experiments back to their default, also while the switch is off: Load and
/// Load On Inject all of them, Load State those in its row or list. How many.
fn experiments_default(st: &mut State, addr: &str, sub: bool) -> usize {
    let ids = experiment_ids(st.prefs, addr, sub);
    for &id in &ids {
        default_of(st, id);
    }
    ids.len()
}

/// A profile turns Enable Experimental Features on only with its line On: without the line it is
/// off, as any row a profile lacks (profiles keep the rows not at their default).
fn wants_experimental(wanted: &[(String, String)]) -> bool {
    wanted.iter().any(|(a, v)| a == EXPERIMENTAL_ADDR && v.eq_ignore_ascii_case("on"))
}

/// A typed number for a value row.
fn set_typed(st: &mut State, id: Id, v: f64) {
    if requeue(st, id, v) {
        return;
    }
    // The player's value now, not the profile's waiting one.
    unwait(id);
    match id {
        Id::Damage => set_damage(st, v as f32),
        Id::FireRate => set_fire(st, v as f32),
        Id::Coord(i) => {
            set_coord(i, v);
            done(st, format!("{} set to {}", ["X", "Y", "Z"][i], number(coords()[i])));
        }
        Id::FlySpeed => {
            fly::set_speed(v);
            done(st, format!("Fly speed set to {:.2}", fly::speed()));
        }
        Id::FlySprint => {
            fly::set_sprint(v);
            done(st, format!("Fly sprint set to {:.2}", fly::sprint()));
        }
        Id::HopHeight => {
            fly::set_hop_height(v);
            done(st, format!("Hop height set to {:.2} m", fly::hop_height()));
        }
        Id::Credits => match credits::set(v.round().clamp(0.0, f64::from(credits::MAX)) as u32) {
            Ok(c) => done(st, format!("Credits set to {c}")),
            Err(e) => {
                if credits::balance().is_none() && credits::start_catch() {
                    note(st, "Open a shop now, then set it again");
                } else {
                    note(st, e);
                }
            }
        },
        Id::GearValue => {
            let g = gear_typed(v, st.prefs);
            set_gear(g);
            done(st, if g == 0 { "Dropped item gear score is the game's own".to_string() } else { format!("Dropped item gear score set to {g}") });
        }
        Id::GearTotal | Id::GearSlot(_) => {
            let g = gear_typed(v, st.prefs);
            match set_worn(id, g) {
                Ok(()) => done(st, match id {
                    Id::GearSlot(i) => format!("{} set to {g}", gearscore::SLOTS[i]),
                    _ => format!("Everything you have on set to {g}"),
                }),
                Err(e) => note(st, e),
            }
        }
        Id::GearOwned => {
            let g = gear_typed(v, st.prefs);
            if gearscore::start_set_owned(g, hushed_now()) {
                note(st, format!("Setting all owned items to {g}..."));
            } else {
                note(st, "Still setting the last one");
            }
        }
        Id::EspRange => {
            let r = (v as f32).clamp(25.0, 1500.0);
            esp_change(st, |c| c.style.range_m = r);
            done(st, format!("ESP range set to {r:.0} m"));
        }
        Id::SilentFov => {
            crate::silentaim::set_fov(v);
            done(st, format!("Silent aim FOV set to {:.0} deg", crate::silentaim::fov()));
        }
        Id::SilentRange => {
            crate::silentaim::set_range_m(v);
            done(st, format!("Silent aim range set to {:.0} m", crate::silentaim::range_m()));
        }
        Id::EspLines => {
            let w = v.round().clamp(0.0, 6.0) as u8;
            esp_change(st, |c| c.style.width = w);
            done(st, if w == 0 { "Lines set to auto".to_string() } else { format!("Lines set to {w} px") });
        }
        Id::Layout(i) => {
            let (name, _, _, least, most) = LAYOUT_ROWS[i];
            st.prefs.layout[i] = v.round().clamp(f64::from(least), f64::from(most)) as u32;
            prefs_changed(st.prefs);
            done(st, format!("{name} set to {}", st.prefs.layout[i]));
        }
        Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY => {
            set_pixel(&mut st.prefs, id, pixel(v));
            prefs_changed(st.prefs);
        }
        _ => {}
    }
}

/// Menu X / Y or Custom Position X / Y, in pixels.
fn set_pixel(p: &mut Prefs, id: Id, v: i32) {
    match id {
        Id::PosX => p.x = Some(v),
        Id::PosY => p.y = Some(v),
        Id::NotifyX => p.notify_x = v,
        Id::NotifyY => p.notify_y = v,
        _ => {}
    }
}

fn pixel_of(p: &Prefs, id: Id) -> i32 {
    match id {
        Id::PosX => menu_pos(p, 0),
        Id::PosY => menu_pos(p, 1),
        Id::NotifyX => p.notify_x,
        _ => p.notify_y,
    }
}

/// Left and Right on a value. Number rows keep their own step and go round at their ends: one more
/// step past the highest gives the lowest, and back.
fn adjust(st: &mut State, id: Id, dir: i32) {
    unwait(id);
    let lim = limits(id, st.prefs);
    let go = |now: f64, by: f64| match lim {
        Some((lo, hi)) => stepped(now, by, dir, lo, hi),
        None => now,
    };
    match id {
        Id::FlySpeed => fly::set_speed(go(fly::speed(), FLY_STEP)),
        Id::FlySprint => fly::set_sprint(go(fly::sprint(), FLY_STEP)),
        Id::HopHeight => fly::set_hop_height(go(fly::hop_height(), FLY_STEP)),
        Id::GearValue => {
            // From the game's own while untouched; 0 is the game's own again.
            let now = match gear() {
                (v, true) => v,
                _ => game_gear().unwrap_or(GEAR_DEFAULT),
            };
            let now = f64::from(now);
            // Left / Right go round 1 to the top, 1000 or a million: 1 then Left gave 0, and the
            // next Left counted down from the game's own. 0 (the game's own) is Set To Min Value or
            // a typed 0.
            set_gear(stepped(now, step(now, dir), dir, 1.0, f64::from(gear_max(st.prefs))) as u32);
        }
        // Each step writes; no toast, as the other value rows.
        Id::GearTotal | Id::GearSlot(_) => {
            let now = match id {
                Id::GearSlot(i) => gearscore::slot_score(i),
                _ => gearscore::total(),
            };
            if let Some(now) = now {
                let now = f64::from(now);
                if let Err(e) = set_worn(id, go(now, step(now, dir)) as u32) {
                    note(st, e);
                }
            }
        }
        Id::EspRange => esp_change(st, |c| c.style.range_m = go(f64::from(c.style.range_m), 25.0) as f32),
        Id::SilentFov => crate::silentaim::set_fov(go(crate::silentaim::fov(), 1.0)),
        Id::SilentRange => crate::silentaim::set_range_m(go(crate::silentaim::range_m(), 10.0)),
        Id::EspLines => esp_change(st, |c| c.style.width = go(f64::from(c.style.width), 1.0) as u8),
        Id::EspBones => esp_change(st, |c| c.style.bones = !c.style.bones),
        Id::Coord(i) => {
            let x = coords()[i];
            set_coord(i, go(x, step(x, dir)));
        }
        // Steps of 1.00, round at the ends: Off, x1.00, x2.00 ... the top, Off. A typed x2.37 steps
        // to x3.37.
        Id::FireRate => {
            let x = waiting_value(id).and_then(|v| v.parse().ok()).unwrap_or_else(|| f64::from(crate::weapon::fire_rate()));
            let v = cents(go(x, TIMES_STEP));
            if !requeue(st, id, v) {
                set_fire(st, v as f32);
            }
        }
        Id::Damage => {
            // Still waiting for the world: step the waiting value.
            let x = waiting_value(id).and_then(|v| v.parse().ok()).unwrap_or_else(|| f64::from(crate::weapon::damage()));
            let v = cents(go(x, TIMES_STEP));
            if !requeue(st, id, v) {
                set_damage(st, v as f32);
            }
        }
        // Round and round the list; a colour set elsewhere starts from the first.
        Id::EspColour(i) => esp_change(st, |c| {
            let n = BOX_COLOURS.len() as i32;
            let at = BOX_COLOURS.iter().position(|b| b.1 == esp_colour(&c.style, i)).map_or(if dir > 0 { -1 } else { 0 }, |p| p as i32);
            *esp_colour_mut(&mut c.style, i) = BOX_COLOURS[(at + dir).rem_euclid(n) as usize].1;
        }),
        Id::SilentPriority => {
            use crate::silentaim::{Priority, priority, set_priority};
            let n = Priority::ALL.len() as i32;
            let at = Priority::ALL.iter().position(|p| *p == priority()).unwrap_or(0) as i32;
            set_priority(Priority::ALL[(at + dir).rem_euclid(n) as usize]);
        }
        Id::MenuColour | Id::ToastPosition | Id::ToastColour | Id::BackColour => {
            let n = choice_names(id).len() as i32;
            let at = (choice_at(&st.prefs, id) as i32 + dir).rem_euclid(n) as usize;
            set_choice(&mut st.prefs, id, at);
            prefs_changed(st.prefs);
        }
        Id::Layout(i) => {
            st.prefs.layout[i] = go(f64::from(st.prefs.layout[i]), 1.0) as u32;
            prefs_changed(st.prefs);
        }
        // One pixel a step, as Stand's.
        Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY => {
            let now = f64::from(pixel_of(&st.prefs, id));
            set_pixel(&mut st.prefs, id, pixel(go(now, 1.0)));
            prefs_changed(st.prefs);
        }
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Back,
    TabDown,
    TabUp,
    Delete,
    Context,
}

/// Arrows, Enter, Backspace, the number pad like Stand's (8 2 4 6, 5, 0), and Right Ctrl / Right
/// Shift for the sections.
const NAV: [(Key, [u32; 2]); 10] = [
    (Key::Up, [0x26, 0x68]),
    (Key::Down, [0x28, 0x62]),
    (Key::Left, [0x25, 0x64]),
    (Key::Right, [0x27, 0x66]),
    (Key::Enter, [0x0D, 0x65]),
    (Key::Back, [0x08, 0x60]),
    (Key::TabDown, [0xA3, 0]),
    (Key::TabUp, [0xA1, 0]),
    (Key::Delete, [0x2E, 0]),
    // O opens a row's own menu, as Stand's.
    (Key::Context, [0x4F, 0]),
];

/// Show tab `to`, back where the player was in it.
fn show_section(st: &mut State, to: usize) {
    if to >= SECTIONS.len() {
        return;
    }
    st.saved[st.section] = std::mem::take(&mut st.stack);
    st.section = to;
    st.stack = std::mem::take(&mut st.saved[st.section]);
    if st.stack.is_empty() {
        st.stack.push((SECTIONS[st.section].0, 0, 0));
    }
}

fn press(st: &mut State, key: Key) {
    let n = SECTIONS.len();
    if matches!(key, Key::TabDown | Key::TabUp) {
        let to = if key == Key::TabDown { (st.section + 1) % n } else { (st.section + n - 1) % n };
        show_section(st, to);
        return;
    }
    if st.stack.is_empty() {
        st.stack.push((SECTIONS[st.section].0, 0, 0));
    }
    if key == Key::Context {
        if !st.stack.iter().any(|x| is_ctx(x.0) || matches!(x.0, Page::HotkeyList | Page::Unload | Page::Risk | Page::ExpWarn | Page::CreditsPicker | Page::OtherPick | Page::ProfileDelete(_)))
            && let Some(a) = address_of(st)
        {
            if let Ok(mut c) = CTX.lock() {
                *c = Some(a);
            }
            st.stack.push((Page::Ctx, 0, 0));
        }
        return;
    }
    let (page, pick, _) = *st.stack.last().unwrap();
    let list = items(page, st.prefs);
    if list.is_empty() {
        if key == Key::Back || key == Key::Left {
            back(st);
        }
        return;
    }
    let pick = pick.min(list.len() - 1);
    let n = list.len();
    let shown = shown_rows(&st.prefs);
    // The first row the cursor can be on (the rows above it are only shown).
    let lead = list.iter().position(|i| !skipped(i)).unwrap_or(n);
    let set_pick = |st: &mut State, p: usize| {
        if let Some(top) = st.stack.last_mut() {
            top.1 = p;
            top.2 = scroll_to_show(p, top.2, shown, lead);
        }
    };
    match key {
        // Titles and grayed-out rows in a list are skipped.
        Key::Up | Key::Down => {
            let mut p = pick;
            for _ in 0..n {
                p = if key == Key::Up { (p + n - 1) % n } else { (p + 1) % n };
                if !skipped(&list[p]) {
                    break;
                }
            }
            set_pick(st, p);
        }
        // Not a greyed-out row.
        Key::Enter if !skipped(&list[pick]) => activate(st, &list[pick]),
        Key::Enter => {}
        Key::Right => match list[pick].row.kind {
            Kind::Sub => activate(st, &list[pick]),
            Kind::Value(_) | Kind::Choice(_) => adjust(st, list[pick].id, 1),
            _ => {}
        },
        Key::Left => match list[pick].row.kind {
            Kind::Value(_) | Kind::Choice(_) if !matches!(list[pick].id, Id::Info | Id::WhereAmI | Id::Aim | Id::Bind(_) | Id::EspHex(_) | Id::Credits | Id::GearOwned) => adjust(st, list[pick].id, -1),
            _ => back(st),
        },
        Key::Back => back(st),
        Key::Delete => match list[pick].id {
            Id::Spot(i) => match remove_spot(i) {
                Ok(name) => confirm(st, format!("Removed {name}")),
                Err(e) => note(st, e),
            },
            // Without a menu key the menu could never open again.
            Id::Bind(Bind::Action("menu")) => note(st, "The menu always needs a key"),
            Id::Bind(b) => set_bind(st, b, 0),
            _ => {}
        },
        Key::TabDown | Key::TabUp | Key::Context => {}
    }
}

/// Out of a submenu; on the section's own list nothing happens (Insert closes).
fn back(st: &mut State) {
    if st.stack.last().is_some_and(|t| t.0 == Page::Risk) && !risk_read(st.prefs.force_read) {
        return;
    }
    if st.stack.len() > 1 {
        st.stack.pop();
    }
}

fn view(st: &State) -> View {
    let section_page = SECTIONS[st.section].0;
    let (page, pick, scroll) = st.stack.last().copied().unwrap_or((section_page, 0, 0));
    let list = items(page, st.prefs);
    // Warning text is never highlighted.
    let pick = (!list.is_empty()).then(|| pick.min(list.len() - 1)).filter(|&k| list[k].id != Id::WarnText);
    let mut path = format!("Aurora {} > {}", crate::DLL_VERSION, SECTIONS[st.section].1);
    for (p, _, _) in st.stack.iter().skip(1) {
        path.push_str(" > ");
        path.push_str(&page_name(*p));
    }
    let mut help = match pick.and_then(|k| list.get(k)) {
        Some(i) if waiting_value(i.id).is_some() => wait_text().into(),
        Some(i) => i.help.clone(),
        None => String::new(),
    };
    // The command on top, what it does under it, how to give it a value at the bottom.
    let mut command_line = String::new();
    let mut enter_line = false;
    // The waiting ring's bright dot moves one step at each redraw (every 250 ms).
    let step = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() / 250) % 8) as u8;
    let waits: Vec<Id> = waiting().iter().map(|w| w.0.item.id).collect();
    // Stand's info boxes: its command, what it does, how to give it a value.
    if let Some(it) = pick.and_then(|k| list.get(k))
        && let Some(c) = command(it.id)
    {
        let r = range(it.id, st.prefs);
        if help == r {
            help.clear();
        }
        command_line = if r.is_empty() { format!("Command: {c}") } else { format!("Command: {c} [{r}]") };
        // Rows that need no context get no Enter line either.
        enter_line = (typed(it.id) || matches!(it.id, Id::EspHex(_))) && !no_context(page, it.id);
    }
    // Rows with hotkeys show them after the name, as Stand's: "Close Menu [ C ]".
    let labels = !is_ctx(page) && !matches!(page, Page::HotkeyList | Page::Unload | Page::Risk | Page::ExpWarn | Page::CreditsPicker | Page::OtherPick | Page::ProfileDelete(_));
    let saved: Vec<String> = listed_rows(page, st.prefs).map(|v| v.into_iter().map(|e| e.0).collect()).unwrap_or_default();
    let prefix = list_address(st);
    if let Some(a) = pick.and_then(|k| saved.get(k)) {
        // Saved Commands also say where the row lives, in a box of its own.
        help = [help, a.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(NEW_BOX);
    }
    help = help_lines(help, command_line, enter_line);
    // Findc's rows show only where they live.
    if page == Page::Find
        && let Some(a) = pick.and_then(|k| saved.get(k))
    {
        help = a.clone();
    }
    let mut rows: Vec<Row> = list
        .into_iter()
        .enumerate()
        .map(|(n, i)| {
            let id = i.id;
            let mut r = i.row;
            let addr = saved.get(n).cloned().unwrap_or_else(|| format!("{prefix} > {}", r.text));
            if waits.contains(&id) {
                r.kind = Kind::Waiting(step);
            }
            let keys = if labels && r.kind != Kind::Header { hotkeys_of(&addr) } else { Vec::new() };
            if !keys.is_empty() {
                r.text = format!("{} [ {} ]", r.text, keys.iter().map(|k| key_name(*k)).collect::<Vec<_>>().join(", "));
            }
            r
        })
        .collect();
    if let (Some(_), Some(k)) = (&st.bind, pick)
        && let Some(r) = rows.get_mut(k)
    {
        r.kind = Kind::Value("press a key".into());
        help = "Esc cancels.".into();
    }
    View {
        path,
        sections: SECTIONS.iter().map(|s| s.1.to_string()).collect(),
        section: st.section,
        rows,
        pick,
        scroll,
        help,
        accent: ACCENTS[st.prefs.accent].1,
        layout: layout_of(&st.prefs),
        list: list_of(&st.prefs),
        visible: shown_rows(&st.prefs),
        spacer: st.prefs.layout[SPACER_SIZE] as f32,
    }
}

/// The help boxes, top to bottom: the Command line is always the top box, what it does (the
/// context) under it, and "Click to input a value." always the very bottom box when the row takes a
/// typed value. One box can hold several lines: a line break inside a help text stays in its box,
/// `NEW_BOX` starts the next one.
fn help_lines(context: String, command: String, enter: bool) -> String {
    let hint = if enter { INPUT_HINT.to_string() } else { String::new() };
    [command, context, hint].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(NEW_BOX)
}

/// The bottom box on rows that take a typed value.
const INPUT_HINT: &str = "Click to input a value.";

/// An arrow key repeats only after its own fresh press, held 400 ms, then every 70 ms. A key down
/// when the menu opened or the game came back is not armed.
fn repeats(armed: bool, focused: bool, down_now: bool, held_for: Duration, since_repeat: Duration) -> bool {
    armed && focused && down_now && held_for > Duration::from_millis(400) && since_repeat > Duration::from_millis(70)
}

/// A menu key acts only when pressed while the menu is open and the game in front: one already down
/// when the menu opened, or when the game came back to the front, waits until it is let go.
fn fresh_press(was_down: bool, down_now: bool, focused: bool) -> bool {
    focused && down_now && !was_down
}

/// Keep the picked row's "since" current; call after anything that may move the pick.
fn note_pick(st: &mut State) {
    let now = (st.section, st.stack.last().map(|x| (x.0, x.1)));
    if (st.picked.0, st.picked.1) != now {
        st.picked = (now.0, now.1, Instant::now());
    }
}

/// A menu key about to act, kept for the log (the last 16).
fn note_key(st: &mut State, key: Key, repeat: bool) {
    note_seen(st, &format!("{key:?}{}", if repeat { " (held)" } else { "" }));
}

/// A key or click about to act, with where.
fn note_seen(st: &mut State, what: &str) {
    let at = address_of(st).map(|a| a.0).unwrap_or_default();
    let text = format!("{what} at {at}");
    st.keys_seen.push_back((Instant::now(), text));
    while st.keys_seen.len() > 16 {
        st.keys_seen.pop_front();
    }
}

fn down(vk: u32) -> bool {
    // SAFETY: plain Win32 query.
    vk != 0 && unsafe { GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0 }
}

/// The mouse buttons: left, right, middle and the two side ones.
const MOUSE_VKS: [u32; 5] = [0x01, 0x02, 0x04, 0x05, 0x06];

pub fn start() {
    if let Ok(mut w) = WORKER.lock()
        && w.is_none()
    {
        STOP.store(false, Ordering::SeqCst);
        drop_stale_requests();
        // Every inject starts from the defaults, as Stand's: nothing on, the menu's own look too.
        // The Load On Inject profile puts the saved settings back right after (what needs the world, once the
        // player is in it).
        let inject_load = profiles().into_iter().find(|n| profile_flag(n, ON_INJECT));
        // Skip Already-Accepted Warnings counts from this inject.
        if let Ok(mut a) = ACCEPTED.lock() {
            a.clear();
        }
        if let Ok(mut a) = ACCEPTED_DELETES.lock() {
            a.clear();
        }
        if let Some(n) = &inject_load {
            log(&format!("menu: profile {n} loads on inject"));
        }
        if let Ok(mut s) = STATE.lock() {
            *s = Some(new_state(inject_load));
            if let Some(st) = s.as_ref() {
                mirror_quiet(st.prefs);
            }
        }
        *w = Some(std::thread::spawn(worker));
    }
}

/// The menu as it is at inject: closed, on the first tab, every setting at its default.
fn new_state(inject_load: Option<String>) -> State {
    State {
        open: false,
        section: 0,
        stack: Vec::new(),
        saved: Default::default(),
        generation: 0,
        edit: None,
        caret: None,
        bind: None,
        bind_hold: None,
        nav_hold: false,
        mouse_hold: false,
        unload_at: None,
        box_was: [false; 256],
        box_row: None,
        box_held: None,
        history: Vec::new(),
        hist_pos: None,
        tab: None,
        last: None,
        drawn: None,
        prefs: Prefs::default(),
        profile: None,
        profile_text: String::new(),
        inject_load,
        keys_seen: std::collections::VecDeque::new(),
        opened_at: Instant::now(),
        picked: (0, None, Instant::now()),
    }
}

/// Request files that are already there at inject were written before this mod instance existed (a
/// crash, a test script that ran while the mod was out). Acting on them would unload it again, or
/// replay old commands, right after inject. So they are removed, with a log line, before the menu
/// worker first looks for them.
fn drop_stale_requests() {
    let mut stale = vec![aurora_core::paths::unload_request()];
    stale.extend(folder().map(|f| f.join("command.request")));
    for p in stale {
        if !p.exists() {
            continue;
        }
        match std::fs::remove_file(&p) {
            Ok(()) => log(&format!("menu: removed {} left from before this inject (not run)", p.display())),
            Err(e) => log(&format!("menu: could not remove old {}: {e}", p.display())),
        }
    }
}

pub fn shutdown() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = WORKER.lock().ok().and_then(|mut w| w.take()) {
        let _ = h.join();
    }
    // The mouse is the game's again first, then the window and DirectInput hooks come out.
    crate::dinput::set_mouse(false);
    OPEN.store(false, Ordering::SeqCst);
    unsubclass();
    crate::dinput::shutdown();
    vkdraw::set_pic(vkdraw::MENU, None);
    vkdraw::set_pic(vkdraw::TOAST, None);
    vkdraw::set_pic(vkdraw::INPUT, None);
    vkdraw::set_pic(vkdraw::CURSOR, None);
}

/// What the drawn notifications were made from: each one (None = the preview), and their looks.
type ToastShown = Option<(Vec<Option<Instant>>, [i64; 10])>;

/// A notification colour: 0 is the menu's, else `ACCENTS[n - 1]`.
fn note_colour(n: usize, accent: usize) -> [u8; 3] {
    match n {
        0 => ACCENTS[accent % ACCENTS.len()].1,
        n => ACCENTS[(n - 1) % ACCENTS.len()].1,
    }
}

/// Top to bottom, which of `n` notifications (0 the oldest) go where: at the bottom corners the
/// newest is lowest, at the top corners and Custom it is highest.
fn note_order(n: usize, corner: usize) -> Vec<usize> {
    if matches!(corner, 0 | 1) { (0..n).collect() } else { (0..n).rev().collect() }
}

/// Where the notifications' picture (`w` x `h`) goes on a `sw` x `sh` screen: 30 px from the side
/// and 90 from the bottom at 1080p, as it always sat, or 30 from the top. Custom Position's X / Y
/// is the picture's top-left, kept on screen as the menu.
fn note_place(corner: usize, (w, h): (u32, u32), (sw, sh): (i32, i32), custom: (i32, i32)) -> (i32, i32) {
    let s = sh as f32 / 1080.0;
    let (w, h) = (w as i32, h as i32);
    let left = (30.0 * s) as i32;
    let right = sw - w - left;
    let bottom = sh - h - (90.0 * s) as i32;
    let top = (30.0 * s) as i32;
    match corner {
        1 => (right, bottom),
        2 => (left, top),
        3 => (right, top),
        CUSTOM => (custom.0.min(sw - w).max(0), custom.1.min(sh - h).max(0)),
        _ => (left, bottom),
    }
}

/// Show the latest notifications, stacked as Stand's, each gone after Timing > Time. While
/// Notifications (or its Custom Position or Timing) is open a preview stays up, and they are drawn
/// again the moment a setting changes.
fn toast_tick(st: &mut State, shown: &mut ToastShown) {
    let p = st.prefs;
    let look = [
        p.toast_corner as i64,
        i64::from(p.layout[TOAST_SIZE]),
        p.toast_colour as i64,
        p.back_colour as i64,
        p.accent as i64,
        i64::from(p.layout[NOTIFY_WIDTH]),
        i64::from(p.layout[NOTE_TEXT]),
        i64::from(p.notify_x),
        i64::from(p.notify_y),
        i64::from(p.layout[TOAST_TIME]),
    ];
    let preview = st.open && st.stack.last().is_some_and(|t| matches!(t.0, Page::NoteLook | Page::CustomPosition | Page::Timing));
    let time = Duration::from_secs(u64::from(p.layout[TOAST_TIME]));
    let mut live: Vec<(String, Option<Instant>)> = match TOASTS.lock() {
        Ok(mut t) => {
            t.retain(|(_, at)| at.elapsed() <= time);
            t.iter().map(|(m, at)| (m.clone(), Some(*at))).collect()
        }
        Err(_) => Vec::new(),
    };
    if live.is_empty() {
        if !preview {
            if shown.take().is_some() {
                vkdraw::set_pic(vkdraw::TOAST, None);
            }
            return;
        }
        live.push(("Notification preview".to_string(), None));
    }
    let key: Vec<Option<Instant>> = live.iter().map(|(_, at)| *at).collect();
    if shown.as_ref().is_some_and(|s| s.0 == key && s.1 == look) {
        return;
    }
    if let Err(e) = vkdraw::ensure_hooks() {
        log(&format!("menu: toast not drawn: {e}"));
        *shown = Some((key, look));
        return;
    }
    let (Some((sw, sh)), Some(painter)) = (vkdraw::screen(), painter()) else { return };
    let s = sh as f32 / 1080.0;
    // Size scales the box, Text Size the text. Menu Size went: the player's old Menu Size is in
    // these two now (`upgrade_rows`).
    let scale = s * p.layout[TOAST_SIZE] as f32 / 100.0;
    let font = note_font(p, sh);
    let gap = (NOTE_GAP * scale).round() as u32;
    let back = BACKGROUNDS[p.back_colour % BACKGROUNDS.len()].1;
    let border = note_colour(p.toast_colour, p.accent);
    // The drawers skip a picture bigger than their slot, so the stack keeps inside it: the width is
    // capped and stacking stops at the slot's height too.
    let (max_w, max_h) = vkdraw::slot_size(vkdraw::TOAST);
    let width = (p.layout[NOTIFY_WIDTH] as f32).min(max_w as f32 / scale.max(0.01)).floor();
    let max_h = (sh.max(0) as u32).min(max_h);
    // Newest first, as many as fit on the screen.
    let mut frames: Vec<aurora_core::menuview::Frame> = Vec::new();
    let mut height = 0u32;
    for (msg, _) in live.iter().rev() {
        let note = aurora_core::menuview::NoteLook { width, border, back, font };
        let f = painter.render_note(msg, &note, scale);
        let more = f.h + if frames.is_empty() { 0 } else { gap };
        if !frames.is_empty() && height + more > max_h {
            break;
        }
        height += more;
        frames.push(f);
    }
    let n = frames.len();
    // `frames` is newest first: index n - 1 - k is the k-th oldest shown.
    let mut by_age: Vec<Option<aurora_core::menuview::Frame>> = frames.into_iter().rev().map(Some).collect();
    let ordered: Vec<aurora_core::menuview::Frame> = note_order(n, p.toast_corner).into_iter().filter_map(|k| by_age[k].take()).collect();
    // Bottom Right and Top Right line up on the right.
    let frame = aurora_core::menuview::stack(&ordered, gap, matches!(p.toast_corner, 1 | 3));
    st.generation += 1;
    let (x, y) = note_place(p.toast_corner, (frame.w, frame.h), (sw, sh), (p.notify_x, p.notify_y));
    vkdraw::set_pic(vkdraw::TOAST, Some(Arc::new(vkdraw::MenuPic { generation: st.generation, frame, x, y })));
    *shown = Some((key, look));
}

/// A notification's text size in pixels on a screen `sh` high: Text Size percent of the size it
/// always had.
fn note_font(p: Prefs, sh: i32) -> f32 {
    aurora_core::menuview::NOTE_FONT * sh as f32 / 1080.0 * p.layout[NOTE_TEXT] as f32 / 100.0
}

fn worker() {
    // Before the Load On Inject profile loads (its first look is a second from now), so the
    // player's old values are in it.
    carry_over_old_files(Prefs::default());
    let mut menu_was = false;
    let mut open_was = false;
    // Per key: down last time, when it went down, when it last repeated, and armed.
    let mut held = [(false, Instant::now(), Instant::now(), false); NAV.len()];
    let mut box_key_was = false;
    let mut file_checked = Instant::now();
    let mut profile_checked = Instant::now();
    let mut gear_checked = Instant::now();
    let mut hotkeys_checked = Instant::now() - Duration::from_secs(1);
    let mut row_was = std::collections::HashMap::new();
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut toast_shown: ToastShown = None;
    let mut input_shown: Option<String> = None;
    let mut pointer = Pointer::default();
    while !STOP.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(12));
        let focused = crate::keys::game_focused();
        // Down or not whatever is in front, so a key held while the game was behind is not a new
        // press when it comes back.
        let menu_key = down(crate::keys::get().menu);
        let Ok(mut guard) = STATE.lock() else { return };
        let Some(st) = guard.as_mut() else { return };
        let binding = st.bind.is_some() || st.bind_hold.is_some() || st.edit.is_some();
        let box_key = down(crate::keys::get().cmdbox);
        // While a hotkey is picked or a command typed, the menu key is just a key.
        if fresh_press(menu_was, menu_key, focused) && !binding {
            st.open = !st.open;
            if st.open {
                if st.stack.is_empty() {
                    st.stack.push((SECTIONS[st.section].0, 0, 0));
                }
                take_keys();
            }
        }
        menu_was = menu_key;
        if fresh_press(box_key_was, box_key, focused) && !binding {
            open_box(st, String::new());
        }
        box_key_was = box_key;
        if !st.open {
            st.bind = None;
        }
        // The picked key stays ours until let go, or it would fire its hotkey (or reach the game)
        // the moment the prompt ends.
        if st.bind_hold.is_some_and(|vk| !down(vk)) {
            st.bind_hold = None;
        }
        // Menu keys down while the menu is open stay its own until let go, also once it has closed
        // (see `nav_hold`).
        let nav_down = NAV.iter().any(|(_, vks)| vks.iter().any(|&v| down(v)));
        st.nav_hold = nav_down && (st.open || st.nav_hold);
        // The same for the mouse: a button down while the menu has the mouse stays the menu's until
        // let go (see `mouse_hold`).
        let button_down = MOUSE_VKS.iter().any(|&v| down(v));
        st.mouse_hold = focused && button_down && (st.mouse_hold || crate::dinput::menu_has_mouse());
        let box_open = st.edit.is_some();
        let ours = st.open || box_open || st.bind_hold.is_some() || st.nav_hold;
        crate::dinput::BINDING.store(box_open || st.bind_hold.is_some() || (st.open && st.bind.is_some()), Ordering::Relaxed);
        OPEN.store(ours, Ordering::Relaxed);
        EDITING.store(box_open, Ordering::Relaxed);
        crate::dinput::OPEN.store(ours, Ordering::Relaxed);
        crate::dinput::EDITING.store(box_open, Ordering::Relaxed);
        // The mouse is the menu's only while it is open with the game in front and Mouse Support or
        // Move With Mouse is on; said again every tick, so it lapses on its own if this loop ever
        // stops.
        crate::dinput::set_mouse(menu_takes_mouse(st, focused) || st.mouse_hold);
        toast_tick(st, &mut toast_shown);
        row_hotkeys(st, focused, &mut row_was);
        command_file(st, &mut file_checked);
        unload_tick(st);
        profile_tick(st, &mut profile_checked);
        gear_tick(&mut gear_checked);
        keep_hotkeys(&mut hotkeys_checked);
        let mut changed = false;
        if st.edit.is_some() {
            changed = box_step(st, focused);
        }
        input_tick(st, &mut input_shown);
        if !menu_takes_mouse(st, focused) {
            pointer.hide();
        }
        if !st.open {
            if st.last.take().is_some() {
                vkdraw::set_pic(vkdraw::MENU, None);
            }
            st.drawn = None;
            open_was = false;
            continue;
        }
        // Just opened (its key, a command or a hotkey): keys already down don't count, and arrows
        // held from before don't repeat.
        if !open_was {
            open_was = true;
            st.opened_at = Instant::now();
            for (n, (_, vks)) in NAV.iter().enumerate() {
                held[n] = (vks.iter().any(|&v| down(v)), Instant::now(), Instant::now(), false);
            }
        }
        note_pick(st);
        if st.bind.is_some() {
            changed |= binding_step(st, focused);
        }
        if changed || st.edit.is_some() || st.bind.is_some() || st.bind_hold.is_some() {
            // Keys used there must not also count as menu presses afterwards.
            for (n, (_, vks)) in NAV.iter().enumerate() {
                held[n].0 = vks.iter().any(|&v| down(v));
                held[n].3 = false;
            }
        }
        for (n, (key, vks)) in NAV.iter().enumerate() {
            if changed || st.edit.is_some() || st.bind.is_some() || st.bind_hold.is_some() {
                break;
            }
            // Down or not whatever is in front, so a key held while the game was behind counts as
            // held, not as a new press, when it comes back.
            let now = vks.iter().any(|&v| down(v));
            let (was, since, rep, armed) = held[n];
            let fire = if fresh_press(was, now, focused) {
                held[n] = (true, Instant::now(), Instant::now(), true);
                Some(false)
            } else if was && matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) && repeats(armed, focused, now, since.elapsed(), rep.elapsed()) {
                held[n].2 = Instant::now();
                Some(true)
            } else {
                held[n].0 = now;
                // Let go, or the game went behind: the next repeat needs a fresh press.
                if !now || !focused {
                    held[n].3 = false;
                }
                None
            };
            if let Some(repeat) = fire {
                note_key(st, *key, repeat);
                // Down now, so it stays ours if this press closes the menu.
                st.nav_hold = true;
                press(st, *key);
                note_pick(st);
                changed = true;
            }
        }
        // The mouse, as Stand's, only with Mouse Support or Move With Mouse on.
        if menu_takes_mouse(st, focused) && pointer.tick(st) {
            note_pick(st);
            changed = true;
        }
        // Mouse Support or Move With Mouse just turned off (a click, a key, a command) with the
        // other one off: the game gets the mouse back now, by the same path as closing the menu.
        if !menu_takes_mouse(st, focused) {
            if !st.mouse_hold {
                crate::dinput::set_mouse(false);
            }
            pointer.hide();
        }
        input_tick(st, &mut input_shown);
        if !st.open {
            vkdraw::set_pic(vkdraw::MENU, None);
            pointer.hide();
            st.last = None;
            st.drawn = None;
            continue;
        }
        // Values like god mode and the target change on their own: look again now and then.
        if !changed && last_draw.elapsed() < Duration::from_millis(250) && st.last.is_some() {
            continue;
        }
        last_draw = Instant::now();
        let Some((sw, sh)) = vkdraw::screen() else { continue };
        refit_nondefault(st);
        refit_warning(st);
        let v = view(st);
        // List Width / Height scale the menu, with the screen.
        let scale = sh as f32 / 1080.0;
        let list_w = aurora_core::menuview::list_width(&v, scale).round() as i32;
        let place = (wall_at(menu_pos(&st.prefs, 0), list_w, sw), menu_pos(&st.prefs, 1));

        // before, the menu kept its old size until something on it changed.
        if st.last.as_ref().is_some_and(|(lv, lp)| *lv == v && *lp == place) && st.drawn.as_ref().is_some_and(|d| d.scale == scale) {
            continue;
        }
        let Some(painter) = painter() else { continue };
        // X is the wall: the picture gets the room left of it the screen (and the drawers' slot)
        // has; what sticks out further left is cut, the wall stays.
        let (slot_w, _) = vkdraw::slot_size(vkdraw::MENU);
        let room = place.0.min(slot_w as i32).max(0) as f32;
        let frame = painter.render_in(&v, scale, room);
        st.generation += 1;
        let (x, y) = menu_place(place.0, place.1, (frame.w, frame.h), sh);
        st.drawn = Some(Drawn { x, y, scale, w: frame.w, h: frame.h, room, wall: place.0, list_w });
        let pic = vkdraw::MenuPic { generation: st.generation, frame, x, y };
        vkdraw::set_pic(vkdraw::MENU, Some(Arc::new(pic)));
        st.last = Some((v, place));
    }
}

/// On a warning page the pick goes to its first row that can be picked (its Yes, once it shows):
/// warning text never holds it.
fn refit_warning(st: &mut State) {
    let Some(top) = st.stack.last().copied().filter(|t| matches!(t.0, Page::Unload | Page::ExpWarn | Page::ProfileDelete(_) | Page::Risk)) else { return };
    let list = items(top.0, st.prefs);
    if list.get(top.1).is_some_and(|i| !skipped(i)) {
        return;
    }
    if let Some(p) = list.iter().position(|i| !skipped(i))
        && let Some(t) = st.stack.last_mut()
    {
        t.1 = p;
    }
}

// The mouse in the menu, as Stand's. While the menu is open with the game in front the game gets no
// mouse at all (dinput.rs and `eats`); the moves, buttons and wheel it would have read move this
// pointer instead. Clicks are tested against the picture on screen with the same sizes it was drawn
// with (`menuview::spot`).

/// The menu has the mouse: open, the game in front, and Mouse Support on or Move With Mouse on.
/// Both off, the game keeps its mouse: no pointer, clicks or wheel in the menu.
fn menu_takes_mouse(st: &State, focused: bool) -> bool {
    st.open && focused && (st.prefs.mouse_support || st.prefs.move_mouse)
}

/// Rows the wheel scrolls per notch (Windows' usual 3).
const WHEEL_ROWS: i64 = 3;
/// Mouse buttons, as `dinput::MouseRead::buttons` has them.
const LEFT: u32 = 1;
const RIGHT: u32 = 2;
/// Wheel turns seen in the game window's messages while the menu has the mouse: used only when the
/// game does not read its mouse through DirectInput.
static WHEEL_MSG: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// No game mouse read for this long: the menu follows Windows' own pointer.
const OS_POINTER_AFTER: Duration = Duration::from_millis(100);

/// The menu picture on screen: its top-left in screen pixels, the scale it was drawn at, its width
/// and height, the room it had left of the wall (`menuview::render_in`), the wall on screen and the
/// list's width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Drawn {
    x: i32,
    y: i32,
    scale: f32,
    w: u32,
    h: u32,
    room: f32,
    wall: i32,
    list_w: i32,
}

#[derive(Default)]
struct Pointer {
    /// Where it points, in screen pixels; None until the menu is first drawn.
    at: Option<(f32, f32)>,
    /// Had the mouse last tick.
    had: bool,
    /// Buttons down last tick.
    was: u32,
    /// Buttons already down when the menu took the mouse: they wait to be let go.
    stale: u32,
    /// Wheel turn not yet a whole notch.
    wheel: i32,
    /// Windows' own pointer last tick (when the game does not read its mouse).
    os: Option<(i32, i32)>,
    /// When the game last read its mouse through DirectInput.
    game_read: Option<Instant>,
    /// Dragging the bar (Move With Mouse): where the pointer and the menu were at the start.
    drag: Option<((f32, f32), (i32, i32))>,
    /// The pointer picture shown: its size step and where.
    shown: Option<(u32, (i32, i32))>,
}

impl Pointer {
    /// The menu closed, or the game went behind: no pointer, nothing held.
    fn hide(&mut self) {
        if self.shown.take().is_some() {
            vkdraw::set_pic(vkdraw::CURSOR, None);
        }
        self.had = false;
        self.drag = None;
        self.os = None;
    }

    fn show(&mut self, at: (f32, f32), sh: i32) {
        let scale = sh as f32 / 1080.0;
        let k = (scale.clamp(0.5, 4.0).round() as u32).max(1);
        let pos = (at.0 as i32, at.1 as i32);
        if self.shown == Some((k, pos)) {
            return;
        }
        self.shown = Some((k, pos));
        let frame = aurora_core::menuview::render_pointer(scale);
        // Same size step, same picture: the graphics card keeps it and only moves it.
        vkdraw::set_pic(vkdraw::CURSOR, Some(Arc::new(vkdraw::MenuPic { generation: u64::from(k), frame, x: pos.0, y: pos.1 })));
    }

    /// One tick with the menu open and the game in front. True: the menu changed.
    fn tick(&mut self, st: &mut State) -> bool {
        let Some((sw, sh)) = vkdraw::screen() else { return false };
        let read = crate::dinput::take_mouse();
        let msg_wheel = WHEEL_MSG.swap(0, Ordering::SeqCst);
        let mut jump = None;
        if read.fresh {
            self.game_read = Some(Instant::now());
        }
        // A tick between two of the game's reads (it reads once a frame, slower than this worker
        // below about 83 fps): nothing moved, buttons as the last read left them. Windows' pointer
        // only after no read for a while, so it never jumps to where the game puts Windows' cursor.
        let between = !read.fresh && self.game_read.is_some_and(|t| t.elapsed() < OS_POINTER_AFTER);
        let (buttons, wheel) = if read.fresh {
            self.os = None;
            (read.buttons, read.wheel)
        } else if between {
            (read.buttons, 0)
        } else {
            // The game did not read its mouse: Windows' own pointer, buttons and wheel.
            if let Some(p) = os_pointer(sw, sh) {
                if self.os.is_some_and(|o| o != p) {
                    jump = Some((p.0 as f32, p.1 as f32));
                }
                self.os = Some(p);
            }
            (u32::from(down(0x01)) | u32::from(down(0x02)) << 1 | u32::from(down(0x04)) << 2, msg_wheel)
        };
        if !self.had {
            self.had = true;
            self.stale = buttons;
            self.was = buttons;
            self.wheel = 0;
        }
        if self.at.is_none() {
            // First time: the middle of the menu.
            let Some(d) = st.drawn else { return false };
            self.at = Some((d.x as f32 + d.w as f32 / 2.0, d.y as f32 + d.h as f32 / 2.0));
        }
        let old = self.at.unwrap_or_default();
        let at = jump.unwrap_or((old.0 + read.dx as f32, old.1 + read.dy as f32));
        let at = (at.0.clamp(0.0, (sw - 1).max(0) as f32), at.1.clamp(0.0, (sh - 1).max(0) as f32));
        self.at = Some(at);
        let moved = at != old;
        self.stale &= buttons;
        let pressed = buttons & !self.was & !self.stale;
        self.was = buttons;
        self.show(at, sh);
        // While a number is typed or a key picked, only the pointer moves.
        if st.edit.is_some() || st.bind.is_some() || st.bind_hold.is_some() {
            self.drag = None;
            return false;
        }
        if let Some((from, menu)) = self.drag {
            if buttons & LEFT == 0 {
                self.drag = None;
                prefs_changed(st.prefs);
                return false;
            }
            return drag_menu(st, menu, (at.0 - from.0, at.1 - from.1), (sw, sh));
        }
        let mut spot = spot_at(st, at);
        if pressed & LEFT != 0 && spot == Some(Spot::Bar) && st.prefs.move_mouse {
            // From the wall as drawn and Y: the drag moves the wall.
            if let Some(d) = st.drawn {
                self.drag = Some((at, (d.wall, menu_pos(&st.prefs, 1))));
            }
            return false;
        }
        let mut changed = false;
        self.wheel += wheel;
        let notches = self.wheel / 120;
        self.wheel %= 120;
        let scrolled = if notches != 0 { wheel_scroll(st, notches) } else { 0 };
        if scrolled != 0 {
            changed = true;
            // The picture on screen is from before the scroll: the row under the pointer moved.
            if let Some(Spot::Row(i)) = spot {
                spot = usize::try_from(i as i64 + scrolled).ok().map(Spot::Row);
            }
        }
        // The pick follows the pointer onto a row, only when the pointer or the list moved, so the
        // arrow keys keep theirs while the mouse rests. Tabs only switch on a click.
        if (moved || scrolled != 0)
            && let Some(Spot::Row(i)) = spot
        {
            changed |= hover(st, i);
        }
        if pressed & LEFT != 0 {
            changed |= mouse_click(st, spot, false);
        } else if pressed & RIGHT != 0 {
            changed |= mouse_click(st, spot, true);
        }
        changed
    }
}

/// Windows' pointer inside the game window, in the game picture's pixels.
fn os_pointer(sw: i32, sh: i32) -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowInfo, WINDOWINFO};
    let h = SUB_HWND.load(Ordering::SeqCst) as HWND;
    if h.is_null() {
        return None;
    }
    // SAFETY: plain user32 queries into our own structs.
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        let mut wi: WINDOWINFO = std::mem::zeroed();
        wi.cbSize = std::mem::size_of::<WINDOWINFO>() as u32;
        if GetCursorPos(&mut p) == 0 || GetWindowInfo(h, &mut wi) == 0 {
            return None;
        }
        let c = wi.rcClient;
        let (cw, ch) = (c.right - c.left, c.bottom - c.top);
        if cw <= 0 || ch <= 0 {
            return None;
        }
        Some(((p.x - c.left) * sw / cw, (p.y - c.top) * sh / ch))
    }
}

/// What the pointer is on in the menu picture now on screen.
fn spot_at(st: &State, at: (f32, f32)) -> Option<Spot> {
    let (v, ..) = st.last.as_ref()?;
    let d = st.drawn?;
    aurora_core::menuview::spot_in(v, d.scale, d.room, at.0 - d.x as f32, at.1 - d.y as f32)
}

/// Move With Mouse: the menu goes where the drag takes it (from `menu`, the wall and Y when the
/// drag began), kept on screen, and X / Y follow. X is the wall: from the list's width (the list
/// always shows) to the screen's right edge.
fn drag_menu(st: &mut State, menu: (i32, i32), by: (f32, f32), (sw, sh): (i32, i32)) -> bool {
    let (list_w, h) = st.drawn.map_or((0, 0), |d| (d.list_w, d.h as i32));
    let x = (menu.0 + by.0.round() as i32).clamp(list_w.min(sw), sw.max(0));
    let y = (menu.1 + by.1.round() as i32).clamp(0, (sh - h).max(0));
    if st.prefs.x == Some(x) && st.prefs.y == Some(y) {
        return false;
    }
    st.prefs.x = Some(x);
    st.prefs.y = Some(y);
    true
}

/// The wheel scrolls the open list when it is longer than the menu shows; it never changes a value
/// or the pick. Up (more than 0) goes towards the top. Returns how many rows the list moved (up is
/// less than 0).
fn wheel_scroll(st: &mut State, notches: i32) -> i64 {
    let Some(&(page, _, scroll)) = st.stack.last() else { return 0 };
    let max = items(page, st.prefs).len().saturating_sub(shown_rows(&st.prefs)) as i64;
    let to = (scroll as i64 - i64::from(notches) * WHEEL_ROWS).clamp(0, max);
    if let Some(top) = st.stack.last_mut() {
        top.2 = to as usize;
    }
    to - scroll as i64
}

/// The pointer rests on row `i`: it becomes the picked row (titles and greyed rows are skipped, as
/// with the arrows). True when the pick moved.
fn hover(st: &mut State, i: usize) -> bool {
    let Some(&(page, pick, _)) = st.stack.last() else { return false };
    if pick == i || items(page, st.prefs).get(i).is_none_or(skipped) {
        return false;
    }
    if let Some(top) = st.stack.last_mut() {
        top.1 = i;
    }
    true
}

/// A click: left on a row is Enter on it (a switch, an action, a list, a value to type), left on a
/// tab shows that tab. Right click anywhere is only Back: it never opens a list, presses a row or
/// moves the pick. True when it did something.
fn mouse_click(st: &mut State, spot: Option<Spot>, right: bool) -> bool {
    if right {
        note_seen(st, "Right click");
        back(st);
        return true;
    }
    let what = "Click";
    match spot {
        Some(Spot::Tab(i)) => {
            if i == st.section || i >= SECTIONS.len() {
                return false;
            }
            show_section(st, i);
            note_seen(st, what);
            true
        }
        Some(Spot::Row(i)) => {
            let Some(&(page, ..)) = st.stack.last() else { return false };
            let list = items(page, st.prefs);
            let Some(it) = list.get(i) else { return false };
            if skipped(it) {
                return false;
            }
            if let Some(top) = st.stack.last_mut() {
                top.1 = i;
            }
            note_seen(st, what);
            press(st, Key::Enter);
            true
        }
        _ => false,
    }
}

/// Keys a hotkey can be: not the left and right mouse buttons (the player clicks in the game), not
/// Escape (cancels) or Delete (removes), not the IME keys.
fn bindable(vk: u32) -> bool {
    !matches!(vk, 0x00..=0x02 | 0x07 | 0x0A..=0x0C | 0x0E..=0x0F | 0x15..=0x1A | 0x1C..=0x1F | 0x2E | 0xFF)
}

/// Which keys are down right now.
fn held_now() -> [bool; 256] {
    let mut d = [false; 256];
    for (vk, slot) in d.iter_mut().enumerate() {
        *slot = down(vk as u32);
    }
    d
}

/// One step of waiting for a new hotkey. Returns true when the prompt ended.
fn binding_step(st: &mut State, focused: bool) -> bool {
    let Some((b, was)) = st.bind.as_mut() else { return false };
    let b = *b;
    let mut fresh = None;
    for vk in 1..256u32 {
        let now = focused && down(vk);
        if now && !was[vk as usize] && fresh.is_none() {
            fresh = Some(vk);
        }
        was[vk as usize] = now;
    }
    let Some(vk) = fresh else { return false };
    if b == Bind::RowAdd {
        match vk {
            0x1B | 0x2E => {}
            v if bindable(v) => add_row_key(st, v),
            _ => return false,
        }
        st.bind = None;
        st.bind_hold = Some(vk);
        return true;
    }
    match vk {
        0x1B => {}
        0x2E if b == Bind::Action("menu") => note(st, "The menu always needs a key"),
        0x2E => set_bind(st, b, 0),
        v if bindable(v) => set_bind(st, b, v),
        _ => return false,
    }
    st.bind = None;
    st.bind_hold = Some(vk);
    true
}

/// The keys that type a number: 0-9 on both rows and the dot.
fn digit(vk: u32) -> Option<char> {
    match vk {
        0x30..=0x39 => char::from_digit(vk - 0x30, 10),
        0x60..=0x69 => char::from_digit(vk - 0x60, 10),
        0xBE | 0x6E => Some('.'),
        _ => None,
    }
}

// Keeping the menu's keys from the game: the game window's messages pass through us while the menu
// is open, and arrow, Enter and Backspace presses stop here (and the digits while a number is being
// typed).
static OPEN: AtomicBool = AtomicBool::new(false);
static EDITING: AtomicBool = AtomicBool::new(false);
static SUB_HWND: AtomicIsize = AtomicIsize::new(0);
static ORIG_PROC: AtomicIsize = AtomicIsize::new(0);
static IN_PROC: AtomicU32 = AtomicU32::new(0);

fn is_nav(vk: u32) -> bool {
    NAV.iter().any(|(_, v)| v.contains(&vk)) || (EDITING.load(Ordering::Relaxed) && (digit(vk).is_some() || (0x41..=0x46).contains(&vk)))
}

/// Ctrl and Shift come as one key code for both sides; only the right ones are the menu's.
fn right_side(vk: u32, scan: u32, extended: bool) -> Option<u32> {
    match vk {
        0x11 if extended => Some(0xA3),
        0x10 if scan == 0x36 => Some(0xA1),
        0x11 | 0x10 => None,
        v => Some(v),
    }
}

/// A key that is a Aurora hotkey, menu open or not: the game never sees it.
fn hotkey_msg(msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    match msg {
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => crate::dinput::is_hotkey(wp as u32),
        WM_INPUT => {
            // SAFETY: as in `eats`.
            unsafe {
                let mut raw: RAWINPUT = std::mem::zeroed();
                let mut size = std::mem::size_of::<RAWINPUT>() as u32;
                let got = GetRawInputData(lp as HRAWINPUT, RID_INPUT, &mut raw as *mut RAWINPUT as *mut _, &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32);
                got != u32::MAX && raw.header.dwType == RIM_TYPEKEYBOARD && crate::dinput::is_hotkey(u32::from(raw.data.keyboard.VKey))
            }
        }
        _ => false,
    }
}

/// The game window must not get the mouse: the command box is open (`all` and typing), or the menu
/// has it.
fn mouse_ours(all: bool) -> bool {
    (all && EDITING.load(Ordering::Relaxed)) || crate::dinput::menu_has_mouse()
}

fn eats(msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    let all = crate::dinput::BINDING.load(Ordering::Relaxed);
    match msg {
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR | WM_SYSCHAR if all => true,
        WM_KEYDOWN | WM_KEYUP => right_side(wp as u32, ((lp >> 16) & 0xFF) as u32, (lp >> 24) & 1 == 1).is_some_and(is_nav),
        WM_CHAR => {
            wp == 0x0D
                || wp == 0x08
                || (EDITING.load(Ordering::Relaxed)
                    && (wp == '.' as usize || (0x30..=0x39).contains(&wp) || (0x41..=0x46).contains(&wp) || (0x61..=0x66).contains(&wp)))
        }
        // The mouse while the command box is open, or while the menu has it, as Stand's: moves,
        // presses and the wheel go; releases stay, so a button held when it opened is let go
        // normally.
        0x200 | 0x201 | 0x203 | 0x204 | 0x206 | 0x207 | 0x209 | 0x20A | 0x20B | 0x20D | 0x20E => mouse_ours(all),
        WM_INPUT => {
            // SAFETY: lp is the raw input handle of this message; the buffer is big enough
            // For a keyboard or mouse event, and a bigger event just fails the call.
            unsafe {
                let mut raw: RAWINPUT = std::mem::zeroed();
                let mut size = std::mem::size_of::<RAWINPUT>() as u32;
                let got = GetRawInputData(lp as HRAWINPUT, RID_INPUT, &mut raw as *mut RAWINPUT as *mut _, &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32);
                let k = raw.data.keyboard;
                // Mouse button-up flags: left, right, middle, 4, 5.
                let ups = 0x0002 | 0x0008 | 0x0020 | 0x0200 | 0x0800;
                got != u32::MAX
                    && match raw.header.dwType {
                        RIM_TYPEKEYBOARD => all || right_side(u32::from(k.VKey), u32::from(k.MakeCode), k.Flags & 2 != 0).is_some_and(is_nav),
                        0 => mouse_ours(all) && raw.data.mouse.Anonymous.ulButtons & ups == 0,
                        _ => false,
                    }
            }
        }
        _ => false,
    }
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    IN_PROC.fetch_add(1, Ordering::SeqCst);
    // The wheel as window messages, for the pointer when the game does not read its mouse.
    if msg == 0x20A && crate::dinput::menu_has_mouse() {
        WHEEL_MSG.fetch_add(i32::from((wp >> 16) as u16 as i16), Ordering::SeqCst);
    }
    let r = if (OPEN.load(Ordering::Relaxed) && eats(msg, wp, lp)) || hotkey_msg(msg, wp, lp) {
        // SAFETY: the default handler frees the raw input; other messages just stop.
        if msg == WM_INPUT { unsafe { DefWindowProcW(h, msg, wp, lp) } } else { 0 }
    } else {
        // SAFETY: the game's own window procedure, saved when we went in.
        unsafe { CallWindowProcW(std::mem::transmute::<isize, windows_sys::Win32::UI::WindowsAndMessaging::WNDPROC>(ORIG_PROC.load(Ordering::SeqCst)), h, msg, wp, lp) }
    };
    IN_PROC.fetch_sub(1, Ordering::SeqCst);
    r
}

fn subclass() {
    if SUB_HWND.load(Ordering::SeqCst) != 0 {
        return;
    }
    let Some(h) = vkdraw::find_window() else {
        log("menu: game window not found, keys reach the game too");
        return;
    };
    // The game's procedure is saved before ours goes in: a message can reach ours at once, and with
    // nothing saved yet the game would lose it.
    // SAFETY: plain window queries / our own procedure, kept until unsubclass puts the game's back.
    ORIG_PROC.store(unsafe { GetWindowLongPtrW(h, GWLP_WNDPROC) }, Ordering::SeqCst);
    let prev = unsafe { SetWindowLongPtrW(h, GWLP_WNDPROC, wndproc as *const () as isize) };
    if prev == 0 {
        log("menu: could not take the game window's messages, keys reach the game too");
        return;
    }
    ORIG_PROC.store(prev, Ordering::SeqCst);
    SUB_HWND.store(h as isize, Ordering::SeqCst);
    log("menu: menu keys are kept from the game while it is open");
}

fn unsubclass() {
    let h = SUB_HWND.swap(0, Ordering::SeqCst) as HWND;
    if h.is_null() {
        return;
    }
    // SAFETY: plain user32 calls on the game's window.
    unsafe {
        if GetWindowLongPtrW(h, GWLP_WNDPROC) != wndproc as *const () as isize {
            log("menu: someone wrapped the game window after us; putting the game's own back anyway");
        }
        SetWindowLongPtrW(h, GWLP_WNDPROC, ORIG_PROC.load(Ordering::SeqCst));
    }
    let t = Instant::now();
    while IN_PROC.load(Ordering::SeqCst) != 0 && t.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(50));
}

// ---- Commands: Stand's command box, the "O" menu of every row, hotkeys on any row, saved
// commands. ----

/// The "O" menu's row: its address and whether it opens a list.
static CTX: Mutex<Option<(String, bool)>> = Mutex::new(None);

fn ctx() -> (String, bool) {
    CTX.lock().ok().and_then(|c| c.clone()).unwrap_or_default()
}

fn is_ctx(p: Page) -> bool {
    matches!(p, Page::Ctx | Page::RowHotkeys | Page::CtxKey(_))
}

/// The name a row has in the command box: its whole name without spaces, with what it changes first
/// when the name alone says too little.
fn command(id: Id) -> Option<String> {
    let c = match id {
        Id::God => "godmode",
        Id::Fly => "fly",
        Id::FlySpeed => "flyspeed",
        Id::FlySprint => "flysprint",
        Id::SafeFly => "safefly",
        Id::HopUp => "flyhopup",
        Id::HopHeight => "flyhopheight",
        Id::Unseen => "undetected",
        Id::UnseenInbox => "ignoreonlyme",
        Id::FlyCalm => "flywaitforcalm",
        Id::FlyGuard => "flysnapguard",
        Id::FlyLetFall => "flyletgamefall",
        Id::NvgZone => "nvginerewhon",
        Id::NoRecoil => "norecoil",
        Id::NoShake => "noscreenshake",
        Id::NoNvgOverlay => "nonvgoverlay",
        Id::NoSway => "noscopedsway",
        Id::Damage => "damagemultiplier",
        Id::FireRate => "firerate",
        Id::Coord(0) => "coordinatesx",
        Id::Coord(1) => "coordinatesy",
        Id::Coord(_) => "coordinatesz",
        Id::GoCoords => "gotocoordinates",
        Id::Stamina => "infinitestamina",
        Id::Accuracy => "nospread",
        Id::Ammo => "infinitemag",
        Id::Reserve => "infinitereserve",
        Id::SilentAim => "silentaim",
        Id::SilentFov => "silentaimfov",
        Id::SilentRange => "silentaimrange",
        Id::SilentCircle => "silentaimcircle",
        Id::SilentPriority => "silentaimpriority",
        Id::SilentMagic => "magicbullets",
        Id::SilentNoRange => "norangeloss",
        Id::SilentInstant => "instanthit",
        Id::SilentNoDrop => "nobulletdrop",
        Id::GearValue => "droppeditemgearscore",
        Id::GearTotal => "totalgearscore",
        Id::GearSlot(i) => return Some(format!("{}gearscore", gearscore::SLOTS[i].to_lowercase())),
        Id::GearOwned => "setallowneditems",
        Id::Credits => "credits",
        Id::SaveSpot => "savethisspot",
        Id::Undo => "undolastteleport",
        Id::Spot(i) => return spots().get(i).map(|x| format!("goto{}", squash(&x.0))),
        Id::EspOn => "esp",
        Id::EspEnemies => "esphostile",
        Id::EspFriendlies => "espfriendly",
        Id::EspPlayers => "espplayers",
        Id::EspDrones => "espdrones",
        Id::EspHideDead => "esphidedead",
        Id::EspDistance => "espdistance",
        Id::EspEdge => "espblackedge",
        Id::EspBones => "espstyle",
        Id::EspColour(i) | Id::EspHex(i) => match i {
            0 => "esphostilecolour",
            1 => "espfriendlycolour",
            3 => "espplayerscolour",
            DRONE_COLOUR => "espdronescolour",
            _ => return None,
        },
        Id::EspRange => "esprange",
        Id::EspLines => "esplines",
        Id::Aim => "picktarget",
        Id::Yeet => "yeettarget",
        Id::Bring => "bringtarget",
        Id::MenuColour => "menucolour",
        // Notifications as Stand's notify... commands.
        Id::ToastPosition => "notifyposition",
        Id::ToastColour => "notifybordercolour",
        Id::BackColour => "notifybackgroundcolour",
        Id::NotifyX => "notifyx",
        Id::NotifyY => "notifyy",
        Id::FlashNote => "flashnotification",
        Id::NewProfile => "createprofile",
        // The player's names: "default" and "unload"; Commands With Non-Default State and Move With
        // Mouse have none. Their old names still work quietly (`old_row_command`).
        Id::ApplyDefaults => "default",
        Id::Unload => "unload",
        // Pixels as Stand's menux / menuy.
        Id::PosX => "menux",
        Id::PosY => "menuy",
        Id::MouseSupport => "mousesupport",
        Id::Experimental => "enableexperimentalfeatures",
        Id::Layout(i) => LAYOUT_COMMANDS[i],
        Id::QuietBox => "suppressgenericresponsescommandbox",
        Id::QuietKey => "suppressgenericresponseshotkey",
        _ => return None,
    };
    Some(c.into())
}

/// Rows with no "Click to input a value." box: all of Gear Score, Damage Multiplier and Fire Rate.
fn no_context(page: Page, id: Id) -> bool {
    page == Page::GearScore || matches!(id, Id::Damage | Id::FireRate)
}

/// Rows whose value is a typed number.
fn typed(id: Id) -> bool {
    matches!(id, Id::Damage | Id::FireRate | Id::Credits | Id::FlySpeed | Id::FlySprint | Id::HopHeight | Id::GearValue | Id::GearTotal | Id::GearSlot(_) | Id::GearOwned | Id::EspRange | Id::SilentFov | Id::SilentRange | Id::EspLines | Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY | Id::Layout(_) | Id::Coord(_))
}

/// A row's value as the command box fills it in.
fn current(id: Id, prefs: Prefs) -> String {
    let s = vkdraw::config().style;
    match id {
        Id::FlySpeed => format!("{:.2}", fly::speed()),
        Id::FlySprint => format!("{:.2}", fly::sprint()),
        Id::HopHeight => format!("{:.2}", fly::hop_height()),
        Id::GearValue => gear_state(gear()),
        Id::Credits => credits::balance().map(|c| c.to_string()).unwrap_or_default(),
        Id::GearTotal => gearscore::total().map(|t| t.to_string()).unwrap_or_default(),
        Id::GearSlot(i) => gearscore::slot_score(i).map(|t| t.to_string()).unwrap_or_default(),
        Id::GearOwned => gearscore::owned_last().map(|l| l.0).or_else(gearscore::total).map(|t| t.to_string()).unwrap_or_default(),
        Id::EspRange => format!("{:.0}", s.range_m),
        Id::SilentFov => format!("{:.0}", crate::silentaim::fov()),
        Id::SilentRange => format!("{:.0}", crate::silentaim::range_m()),
        Id::EspLines => s.width.to_string(),
        Id::Damage => waiting_value(id).unwrap_or_else(|| crate::weapon::damage().to_string()),
        Id::FireRate => waiting_value(id).unwrap_or_else(|| crate::weapon::fire_rate().to_string()),
        Id::Coord(i) => number(coords()[i]),
        Id::PosX | Id::PosY | Id::NotifyX | Id::NotifyY => pixel_of(&prefs, id).to_string(),
        Id::Layout(i) => prefs.layout[i].to_string(),
        Id::EspColour(i) | Id::EspHex(i) => box_colour_hex(esp_colour(&s, i)).trim_start_matches('#').to_string(),
        _ => String::new(),
    }
}

/// A row somewhere in the menu, with the way there.
#[derive(Clone)]
struct Found {
    section: usize,
    stack: Vec<(Page, usize, usize)>,
    addr: String,
    item: Item,
}

/// Every row of every section, with its address ("World > ESP > Range").
fn all_rows(prefs: Prefs) -> Vec<Found> {
    fn walk(prefs: Prefs, section: usize, page: Page, stack: &mut Vec<(Page, usize, usize)>, addr: &str, out: &mut Vec<Found>, shown: usize) {
        if stack.len() > 6 {
            return;
        }
        for (i, it) in items(page, prefs).into_iter().enumerate() {
            let a = format!("{addr} > {}", it.row.text);
            stack.push((page, i, i.saturating_sub(shown - 1)));
            if let Id::Open(sub) = it.id
                && !is_ctx(sub)
                && !hidden(sub)
                && !matches!(sub, Page::Saved | Page::NonDefault | Page::Find | Page::HotkeyList | Page::Unload | Page::Risk | Page::ExpWarn | Page::CreditsPicker | Page::OtherPick | Page::ProfileDelete(_))
            {
                walk(prefs, section, sub, stack, &a, out, shown);
            }
            out.push(Found { section, stack: stack.clone(), addr: a, item: it });
            stack.pop();
        }
    }
    let mut out = Vec::new();
    let shown = shown_rows(&prefs);
    for (n, (page, name)) in SECTIONS.iter().enumerate() {
        walk(prefs, n, *page, &mut Vec::new(), name, &mut out, shown);
    }
    out
}

fn resolve(addr: &str, prefs: Prefs) -> Option<Found> {
    all_rows(prefs).into_iter().find(|f| f.addr == addr)
}

/// The picked row's address, and whether it opens a list.
fn address_of(st: &State) -> Option<(String, bool)> {
    let mut addr = SECTIONS[st.section].1.to_string();
    let mut sub = false;
    for &(page, pick, _) in &st.stack {
        if let Some(list) = listed_rows(page, st.prefs) {
            let (a, it) = list.into_iter().nth(pick)?;
            addr = a;
            sub = matches!(it.id, Id::Open(_));
            continue;
        }
        let it = items(page, st.prefs).into_iter().nth(pick)?;
        addr = format!("{addr} > {}", it.row.text);
        sub = matches!(it.id, Id::Open(_));
    }
    Some((addr, sub))
}

/// Where the shown list lives, as `address_of` has it. A list opened from Saved
/// Commands, findc or Non-Default lives where its row says; before, its rows got a made-up address
/// there, so they showed no hotkeys.
fn list_address(st: &State) -> String {
    let mut addr = SECTIONS[st.section].1.to_string();
    for &(p, k, _) in st.stack.iter().rev().skip(1).rev() {
        if let Some((a, _)) = listed_rows(p, st.prefs).and_then(|v| v.into_iter().nth(k)) {
            addr = a;
        } else if let Some(it) = items(p, st.prefs).into_iter().nth(k) {
            addr = format!("{addr} > {}", it.row.text);
        }
    }
    addr
}

/// The menu's keys are kept from the game from now on (menu or command box shown).
fn take_keys() {
    if let Err(e) = vkdraw::ensure_hooks() {
        log(&format!("menu: drawing not possible: {e}"));
    }
    subclass();
    crate::dinput::ensure();
}

/// Open the menu at a row (inside it, if it opens a list).
fn navigate(st: &mut State, f: Found) {
    let into = match f.item.id {
        Id::Open(p) => Some(p),
        _ => None,
    };
    if into.is_some_and(hidden) {
        return;
    }
    st.saved[st.section] = std::mem::take(&mut st.stack);
    st.section = f.section;
    st.stack = f.stack;
    if let Some(p) = into {
        st.stack.push((p, 0, 0));
    }
    if !st.open {
        st.open = true;
        take_keys();
    }
}

fn open_box(st: &mut State, text: String) {
    st.edit = Some(text);
    st.caret = None;
    // A key still held when the last box closed must not repeat here.
    st.box_held = None;
    st.box_was = held_now();
    st.box_row = None;
    take_keys();
}

/// Do a row, from a command, a hotkey or Saved Commands. `arg` is what came after the command's
/// name.
fn run_found(st: &mut State, f: Found, arg: Option<&str>) {
    let id = f.item.id;
    let name = command(id).unwrap_or_default();
    let kind = f.item.row.kind.clone();
    match (kind, id) {
        (_, Id::SilentPriority) => match arg {
            Some(a) => match crate::silentaim::Priority::parse(a) {
                Some(p) => {
                    crate::silentaim::set_priority(p);
                    done(st, format!("Silent aim priority set to {}", p.name()));
                }
                None => note(st, "Use one of: Crosshair, Nearest, Farthest"),
            },
            None => adjust(st, id, 1),
        },
        (_, Id::EspColour(i) | Id::EspHex(i)) => match arg {
            Some(a) => match BOX_COLOURS.iter().find(|b| b.0.eq_ignore_ascii_case(a)).map(|b| b.1).or_else(|| parse_hex(a)) {
                Some(c) => {
                    esp_change(st, |cfg| *esp_colour_mut(&mut cfg.style, i) = c);
                    let shown = if box_colour_hex(c).is_empty() { box_colour_name(c) } else { box_colour_hex(c) };
                    done(st, format!("Colour set to {shown}"));
                }
                None => note(st, "Use a colour name or 6 characters, like FF8800"),
            },
            None => open_box(st, format!("{name} ")),
        },
        (_, Id::MenuColour) => match arg {
            Some(a) => match ACCENTS.iter().position(|x| x.0.eq_ignore_ascii_case(a)) {
                Some(n) => {
                    st.prefs.accent = n;
                    prefs_changed(st.prefs);
                }
                None => note(st, format!("Colours: {}", ACCENTS.iter().map(|x| x.0).collect::<Vec<_>>().join(", "))),
            },
            None => adjust(st, id, 1),
        },
        // Typed with or without spaces: "notifyposition topright", "notifybordercolour menu
        // colour", "notifybackgroundcolour dark grey".
        (_, Id::ToastPosition | Id::ToastColour | Id::BackColour) => match arg {
            Some(a) => {
                let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
                let want = squash(a);
                let names = choice_names(id);
                let menu = id == Id::ToastColour && want == "menu";
                match names.iter().position(|n| squash(n) == want || (menu && squash(n) == "menucolour")) {
                    Some(n) => {
                        set_choice(&mut st.prefs, id, n);
                        prefs_changed(st.prefs);
                        let what = match id {
                            Id::ToastPosition => "Notification position",
                            Id::ToastColour => "Border colour",
                            _ => "Background colour",
                        };
                        done(st, format!("{what} set to {}", names[n]));
                    }
                    None => note(st, format!("Use one of: {}", names.join(", "))),
                }
            }
            None => adjust(st, id, 1),
        },
        (_, Id::EspBones) => match arg.map(|a| a.to_ascii_lowercase()) {
            Some(a) if a == "box" || a == "bones" || a == "bone" => esp_change(st, |c| c.style.bones = a != "box"),
            Some(_) => note(st, "Use box or bones"),
            None => adjust(st, id, 1),
        },

        // the row and Set To Min / Max do.
        (_, Id::Credits) if typed_credits(arg, FROM.load(Ordering::Relaxed)) && ask_risk(st) => {}
        (_, id) if typed(id) => match arg.map(|a| a.parse::<f64>()) {
            Some(Ok(v)) if v.is_finite() => set_typed(st, id, v),
            Some(_) => note(st, format!("Not a number: {}", range(id, st.prefs))),
            None => open_box(st, format!("{name} {}", current(id, st.prefs))),
        },
        (Kind::Toggle(on), _) => match arg.map(|a| a.to_ascii_lowercase()) {
            None => activate(st, &f.item),
            Some(a) => match a.as_str() {
                // Waiting for the world: it will be on, so "off" takes it out of the wait.
                "on" | "true" | "1" if waiting_value(f.item.id).is_some() => note(st, format!("{}: {}", f.item.row.text, wait_text())),
                "off" | "false" | "0" if waiting_value(f.item.id).is_some() => activate(st, &f.item),
                "on" | "true" | "1" => {
                    if on {
                        note(st, format!("{} is already on", f.item.row.text));
                    } else {
                        activate(st, &f.item);
                    }
                }
                "off" | "false" | "0" => {
                    if on {
                        activate(st, &f.item);
                    } else {
                        note(st, format!("{} is already off", f.item.row.text));
                    }
                }
                _ => note(st, "Use on or off"),
            },
        },
        // "createprofile name" makes it at once; without a name the box waits for one.
        (_, Id::NewProfile) => match arg {
            Some(a) => new_profile(st, a),
            None => open_box(st, "createprofile ".into()),
        },
        (Kind::Sub, _) | (_, Id::Bind(_)) => navigate(st, f),
        _ => activate(st, &f.item),
    }
}

/// Rows whose command name went: the old name still runs them, quietly, hidden from lists and Tab.
fn old_row_command(name: &str) -> Option<Id> {
    match name {
        "commandswithnondefaultstate" => Some(Id::Open(Page::NonDefault)),
        "menumovewithmouse" => Some(Id::MoveMouse),
        _ => None,
    }
}

/// The player's old command names still work, hidden from lists and Tab. Menuleftright and
/// menuupdown took percent of the screen: a number becomes pixels, as the menu used to place
/// itself; X gets `shift` too: from the picture's left edge to the wall.
fn old_command(name: &str, arg: Option<&str>, screen: (i32, i32), shift: i32) -> Option<(&'static str, Option<String>)> {
    // Kept on the screen: "menuleftright 64" on a 2560 screen gave X 2561; now at most the screen's
    // width, as `old_x_to_wall`.
    let pct = |len: i32, add: i32| arg.map(|a| a.parse::<f64>().ok().filter(|v| v.is_finite()).map_or(a.to_string(), |v| (pct_px(v, len) + add).min(len.max(1)).to_string()));
    let same = arg.map(String::from);
    Some(match name {
        "toastposition" => ("notifyposition", same),
        "toastcolour" => ("notifybordercolour", same),
        "toastsize" => ("notifysize", same),
        "toasttime" => ("notifytime", same),
        "applydefaultstate" => ("default", same),
        "menuleftright" => ("menux", pct(screen.0, shift)),
        "menuupdown" => ("menuy", pct(screen.1, 0)),
        _ => return None,
    })
}

/// What the player typed in the command box: a name, then maybe a value.
fn run_line(st: &mut State, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let (name, arg) = match line.split_once(char::is_whitespace) {
        Some((n, a)) => (n, Some(a.trim()).filter(|a| !a.is_empty())),
        None => (line, None),
    };
    let name = name.to_ascii_lowercase();
    let screen = screen_size();
    let shift = old_x_shift(LOOK_SIZE, st.prefs.layout[TABS_WIDTH], screen.1) + old_bar_to_wall(LOOK_SIZE, screen.1);
    let (name, arg) = match old_command(&name, arg, screen, shift) {
        Some((new, a)) => (new.to_string(), a),
        None => (name, arg.map(String::from)),
    };
    let arg = arg.as_deref();
    // Commands of rows that went: one plain line, no error.
    if let Some((_, why)) = GONE_COMMANDS.iter().find(|c| c.0 == name) {
        log(&format!("menu: command {name} is gone ({why})"));
        return note(st, *why);
    }
    // "tp x y z": straight to a place, as in the old window's Go To.
    if name == "tp" {
        let v: Vec<f64> = arg.unwrap_or("").split_whitespace().filter_map(|x| x.parse().ok()).collect();
        match v[..] {
            [x, y, z] if [x, y, z].iter().all(|n| n.is_finite()) => result(st, tp::to([x, y, z]), &format!("Teleported to {x:.0}, {y:.0}, {z:.0}")),
            _ => note(st, "Use tp x y z"),
        }
        return;
    }
    // "findc esp": every row with esp in its name, on its own page.
    if name == "findc" {
        match arg {
            Some(a) => open_find(st, a),
            None => note(st, "Type findc and a word, like findc esp"),
        }
        return;
    }
    // "savemain" / "loadmain": save or load profile Main, any case. Another command's name wins
    // (savethisspot).
    let others: Vec<String> = row_commands(st.prefs).into_iter().map(|c| c.0).collect();
    match profile_command(&name, &profiles(), &others) {
        Some(ProfileCmd::Save(p)) => return save_said(st, &p),
        Some(ProfileCmd::Load(p)) => return load_said(st, &p),
        Some(ProfileCmd::Unknown(p)) => return note(st, format!("No profile called {p}")),
        None => {}
    }
    // "renamespot old new": the spot by its name without spaces, then the new name.
    if name == "renamespot" {
        let (who, new) = arg.unwrap_or("").split_once(char::is_whitespace).unwrap_or((arg.unwrap_or(""), ""));
        match spots().iter().position(|s| squash(&s.0) == who.to_lowercase()) {
            Some(i) => renamed(st, i, new),
            None if who.is_empty() => note(st, "Use renamespot name newname"),
            None => note(st, format!("No spot called {who}")),
        }
        return;
    }
    let rows = all_rows(st.prefs);
    let old = old_row_command(&name);
    let named = |n: &str| rows.iter().position(|f| command(f.item.id).as_deref() == Some(n) || (n == name && old == Some(f.item.id)));
    let found = named(&name).or_else(|| {
        let hits: Vec<String> = command_list(st.prefs).into_iter().map(|c| c.0).filter(|c| c.starts_with(&name)).collect();
        match &hits[..] {
            [one] => named(one),
            _ => None,
        }
    });
    match found {
        Some(i) => run_found(st, rows.into_iter().nth(i).unwrap(), arg),
        None if command_list(st.prefs).iter().any(|c| c.0.starts_with(&name)) => note(st, format!("Several commands start with {name}; Tab goes through them")),
        // An Experiments row while Enable Experimental Features is off.
        None if !st.prefs.experimental && all_rows(Prefs { experimental: true, ..st.prefs }).iter().any(|f| in_experiments(&f.addr) && command(f.item.id).as_deref() == Some(name.as_str())) => note(st, EXP_OFF),
        None => note(st, format!("No command called {name}")),
    }
}

/// The lines under the command box: part of a name lists the matches, 7 at most, then how many
/// more. A command typed in full shows only itself and its values (`box_syntax`): what a row does
/// is only in the help box beside the menu.
fn matches(prefs: Prefs, text: &str) -> Vec<String> {
    let word = text.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    if word.is_empty() {
        return Vec::new();
    }
    let list = command_list(prefs);
    if list.iter().any(|c| c.0 == word) {
        return vec![box_syntax(prefs, &word)];
    }
    let hits: Vec<&(String, String, String)> = list.iter().filter(|c| c.0.starts_with(&word)).collect();
    if hits.is_empty() {
        return vec!["No matching commands".into()];
    }
    let mut out: Vec<String> = hits.iter().take(7).map(|c| format!("{} - {}", c.0, c.1)).collect();
    if hits.len() > 7 {
        out.push(format!("... and {} more.", hits.len() - 7));
    }
    out
}

/// A full command under the box: its name, then its lowest and highest value, as the help box's
/// range without the note in brackets ("firerate 0.00 - 1000000.00"). Rows without a value: the
/// name only.
fn box_syntax(prefs: Prefs, word: &str) -> String {
    let r = all_rows(prefs).into_iter().find(|f| command(f.item.id).as_deref() == Some(word)).map(|f| range(f.item.id, prefs)).unwrap_or_default();
    let r = r.split(" (").next().unwrap_or_default().replace(" to ", " - ");
    if r.is_empty() { word.to_string() } else { format!("{word} {r}") }
}

/// Every command once, in menu order: name, row, what it does; then `save<name>` and `load<name>`
/// for each profile.
fn command_list(prefs: Prefs) -> Vec<(String, String, String)> {
    let mut v = row_commands(prefs);
    for p in profiles() {
        let s = squash(&p);
        for (c, row, help) in [
            (format!("save{s}"), "Save", "Saves the state of every command to this profile."),
            (format!("load{s}"), "Load", "Loads the state of every command from this profile."),
        ] {
            // Another command's name wins (a profile called "thisspot").
            if !v.iter().any(|x| x.0 == c) {
                v.push((c, row.into(), help.into()));
            }
        }
    }
    v
}

/// Every command but the profiles' own: the rows', tp, renamespot and findc.
fn row_commands(prefs: Prefs) -> Vec<(String, String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut v: Vec<(String, String, String)> = all_rows(prefs)
        .into_iter()
        .filter_map(|f| {
            let c = command(f.item.id)?;
            seen.insert(c.clone()).then_some((c, f.item.row.text, f.item.help))
        })
        .collect();
    v.push(("tp".into(), "Teleport To Coordinates".into(), "Type tp x y z.".into()));
    v.push(("renamespot".into(), "Rename Teleport Spot".into(), "Type renamespot, the spot's name without spaces, then the new name.".into()));
    v.push(("findc".into(), "Find Command".into(), "Type findc and a word: every row with it in its name.".into()));
    v
}

/// A typed `save<name>` / `load<name>`.
#[derive(Debug, PartialEq)]
enum ProfileCmd {
    Save(String),
    Load(String),
    /// Looks like one, but no profile has that name.
    Unknown(String),
}

/// `savemain` or `LoadMain` against the profiles' names, any case, spaces left out ("My Run" is
/// `savemyrun`). None when it is not one: no save / load at the start, nothing after it, or another
/// command's name or the start of one (`savethisspot`, `savethis`), so those keep working as
/// before.
fn profile_command(name: &str, profiles: &[String], others: &[String]) -> Option<ProfileCmd> {
    let name = name.to_ascii_lowercase();
    let (save, rest) = match (name.strip_prefix("save"), name.strip_prefix("load")) {
        (Some(r), _) => (true, r),
        (_, Some(r)) => (false, r),
        _ => return None,
    };
    if rest.is_empty() || others.contains(&name) {
        return None;
    }
    match profiles.iter().find(|p| squash(p) == rest) {
        Some(p) if save => Some(ProfileCmd::Save(p.clone())),
        Some(p) => Some(ProfileCmd::Load(p.clone())),
        None if others.iter().any(|c| c.starts_with(&name)) => None,
        None => Some(ProfileCmd::Unknown(rest.to_string())),
    }
}

/// Findc's page: the rows found, each with its address.
fn find_rows(prefs: Prefs) -> Vec<(String, Item)> {
    find_hits(all_rows(prefs), &find_text())
}

/// The rows whose shown name has `text` in it, any case. Titles and lines of text are no commands.
/// In menu order. A list that matches stands for its rows: they are not listed again under it.
fn find_hits(rows: Vec<Found>, text: &str) -> Vec<(String, Item)> {
    let want = text.trim().to_lowercase();
    if want.is_empty() {
        return Vec::new();
    }
    let mut hits: Vec<Found> = rows
        .into_iter()
        .filter(|f| !matches!(f.item.row.kind, Kind::Header | Kind::Info))
        .filter(|f| f.item.row.text.to_lowercase().contains(&want))
        .collect();
    hits.sort_by_key(|f| (f.section, f.stack.iter().map(|s| s.1).collect::<Vec<_>>()));
    let lists: Vec<String> = hits.iter().filter(|f| f.item.row.kind == Kind::Sub).map(|f| format!("{} > ", f.addr)).collect();
    hits.retain(|f| !lists.iter().any(|l| f.addr.starts_with(l.as_str())));
    hits.into_iter().map(|f| (f.addr, f.item)).collect()
}

/// Open findc's page in the Aurora tab: "Aurora > Aurora > Find Command: esp", as the player's
/// picture.
fn open_find(st: &mut State, text: &str) {
    if let Ok(mut f) = FIND.lock() {
        *f = text.trim().to_string();
    }
    let menu = SECTIONS.iter().position(|s| s.0 == Page::Menu).unwrap_or(0);
    st.saved[st.section] = std::mem::take(&mut st.stack);
    st.section = menu;
    st.stack = vec![(Page::Menu, 0, 0), (Page::Find, first_pick(Page::Find, st.prefs), 0)];
    if !st.open {
        st.open = true;
        take_keys();
    }
}

fn typed_char(vk: u32, shift: bool) -> Option<char> {
    match vk {
        0x41..=0x5A => Some(if shift { char::from(vk as u8) } else { char::from(vk as u8).to_ascii_lowercase() }),
        0x33 if shift => Some('#'),
        0x30..=0x39 if !shift => char::from_digit(vk - 0x30, 10),
        0x60..=0x69 => char::from_digit(vk - 0x60, 10),
        0x20 => Some(' '),
        0xBE | 0x6E => Some('.'),
        0xBD | 0x6D => Some('-'),
        _ => None,
    }
}

/// One step of the command box: typing, Backspace, Enter runs it, Escape closes it. Left and Right
/// move the caret, and typing and Backspace work at the caret.
fn box_step(st: &mut State, focused: bool) -> bool {
    let shift = down(0x10);
    let mut pressed = Vec::new();
    for vk in 0x08..0xDFu32 {
        let now = focused && down(vk);
        if now && !st.box_was[vk as usize] {
            pressed.push(vk);
        }
        st.box_was[vk as usize] = now;
    }
    let mut changed = false;
    // Held Backspace, Left and Right repeat, as in any text box.
    match st.box_held {
        Some((vk, since, last)) if focused && down(vk) => {
            if since.elapsed() > Duration::from_millis(400) && last.elapsed() > Duration::from_millis(35) {
                st.box_held = Some((vk, since, Instant::now()));
                changed |= box_key(st, vk);
            }
        }
        _ => st.box_held = None,
    }
    for vk in pressed {
        if st.edit.is_none() {
            break;
        }
        changed = true;
        match vk {
            0x1B => {
                st.edit = None;
                st.caret = None;
                st.bind_hold = Some(vk);
            }
            0x0D => {
                let line = st.edit.take().unwrap_or_default();
                st.caret = None;
                st.bind_hold = Some(vk);
                let l = line.trim().to_string();
                let word = l.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                if !l.is_empty() && st.history.last() != Some(&l) {
                    st.history.push(l);
                    if st.history.len() > 50 {
                        st.history.remove(0);
                    }
                }
                st.hist_pos = None;
                st.tab = None;
                // Still the clicked row's command: as from the menu, no "set to". Anything else
                // typed there counts as the command box.
                let by_hand = st.box_row.take().is_some_and(|r| !r.is_empty() && r == word);
                FROM.store(if by_hand { FROM_MENU } else { FROM_BOX }, Ordering::Relaxed);
                IN_BOX.store(!by_hand, Ordering::Relaxed);
                run_line(st, &line);
                IN_BOX.store(false, Ordering::Relaxed);
                FROM.store(FROM_MENU, Ordering::Relaxed);
            }
            // Up and Down bring back earlier commands, as Stand's.
            0x26 | 0x28 if !st.history.is_empty() => {
                let n = st.history.len();
                st.hist_pos = match (vk, st.hist_pos) {
                    (0x26, None) => Some(n - 1),
                    (0x26, Some(p)) => Some(p.saturating_sub(1)),
                    (_, Some(p)) if p + 1 < n => Some(p + 1),
                    _ => None,
                };
                st.edit = Some(st.hist_pos.map(|p| st.history[p].clone()).unwrap_or_default());
                st.caret = None;
                st.tab = None;
            }
            // Tab goes through every command starting with what was typed, as Stand's.
            0x09 => {
                let typed = st.tab.as_ref().map(|t| t.0.clone()).unwrap_or_else(|| st.edit.clone().unwrap_or_default().trim().to_ascii_lowercase());
                let found: Vec<String> = command_list(st.prefs).into_iter().map(|c| c.0).filter(|c| c.starts_with(&typed)).collect();
                if !found.is_empty() {
                    let next = st.tab.as_ref().map(|t| (t.1 + 1) % found.len()).unwrap_or(0);
                    st.edit = Some(found[next].clone());
                    st.caret = None;
                    st.tab = Some((typed, next));
                }
            }
            // Backspace, Left, Right: once now, and again while held.
            0x08 | 0x25 | 0x27 => {
                st.box_held = Some((vk, Instant::now(), Instant::now()));
                box_key(st, vk);
            }
            v => {
                if let Some(c) = typed_char(v, shift) {
                    type_at(st, c);
                }
            }
        }
    }
    changed
}

/// The command box, top middle, with the matching commands under it.
fn input_tick(st: &mut State, shown: &mut Option<String>) {
    let Some(text) = st.edit.clone() else {
        if shown.take().is_some() {
            vkdraw::set_pic(vkdraw::INPUT, None);
        }
        return;
    };
    let caret = caret_in(&text, st.caret);
    let list = list_of(&st.prefs);

    // (before, it waited for the next key the player typed).
    let screen = vkdraw::screen();
    let key = format!("{text}|{caret}|{}|{:?}|{screen:?}", st.prefs.accent, list);
    if shown.as_ref() == Some(&key) {
        return;
    }
    let (Some((sw, sh)), Some(p)) = (screen, painter()) else { return };
    let lines = matches(st.prefs, &text);
    let s = sh as f32 / 1080.0;
    // As big as the menu: List Width / Height.
    let frame = p.render_input("Aurora Command Box", &text, caret, &lines, ACCENTS[st.prefs.accent].1, s, list);
    st.generation += 1;
    let (x, y) = ((sw - frame.w as i32) / 2, (sh as f32 * 0.07) as i32);
    vkdraw::set_pic(vkdraw::INPUT, Some(Arc::new(vkdraw::MenuPic { generation: st.generation, frame, x, y })));
    *shown = Some(key);
}

/// Rows that moved: the start of an old address and the new one, so hotkeys and saved commands on
/// them keep working. The first that fits wins, so the longer ones come first.
const MOVED: &[(&str, &str)] = &[
    // Menu round 2: Infinite Stamina back in Self; Appearance > Toasts is Menu > Settings >
    // Notifications; Position is X and Y in pixels (the percent in old profiles is turned into
    // pixels by `upgrade_rows`); Disable Warnings is Automatically Proceed On Warnings (its On by
    // `upgrade_rows` too). Weapon and Self > Progress are at the end: every older move of their
    // rows comes first.
    ("Self > Movement > Infinite Stamina", "Self > Infinite Stamina"),
    // Fly's Speed is "Fly Speed".
    ("Self > Movement > Fly > Speed", FLY_SPEED_ADDR),
    ("Menu > Settings > Appearance > Toasts > Toast Position", "Menu > Settings > Notifications > Position"),
    ("Menu > Settings > Appearance > Toasts > Toast Size", "Menu > Settings > Notifications > Size"),
    ("Menu > Settings > Appearance > Toasts > Toast Time", "Menu > Settings > Notifications > Timing > Time"),
    ("Menu > Settings > Appearance > Toasts > Toast Colour", "Menu > Settings > Notifications > Border Colour"),
    // The list itself goes where the look rows are now.
    ("Menu > Settings > Appearance > Toasts", "Menu > Settings > Appearance > Notifications"),
    ("Menu > Settings > Appearance > Position > Menu Left / Right", POS_X_ADDR),
    ("Menu > Settings > Appearance > Position > Menu Up / Down", POS_Y_ADDR),
    ("Menu > Settings > Warnings > Disable Warnings", AUTO_PROCEED_ADDR),
    // NVG Overlay went from Disables to Rendering; before 4 Nov it was Game > No NVG Overlay.
    ("Game > Disables > NVG Overlay", "Game > Rendering > NVG Overlay"),
    ("Game > No NVG Overlay", "Game > Rendering > NVG Overlay"),
    // Screen Shake and NVG In Erewhon went into Disables; NVG In Erewhon was in Experiments under
    // its old name before that.
    ("Game > No Screen Shake", "Game > Disables > Screen Shake"),
    ("Menu > Experiments > NVG In Safe Zones (untested)", "Game > Disables > NVG In Erewhon"),
    // Silent Aim's rows went into its own list. Its switch keeps the address "Self > Weapons >
    // Silent Aim", which is the list now.
    ("Self > Weapons > Silent Aim FOV", "Self > Weapons > Silent Aim > Silent Aim FOV"),
    ("Self > Weapons > Silent Aim Range", "Self > Weapons > Silent Aim > Silent Aim Range"),
    // Then the list went into Experiments.
    ("Self > Weapons > Silent Aim", "Aurora > Experiments > Silent Aim"),
    // Infinite Stamina went into Movement and back to Self, so "Self > Infinite Stamina" is its
    // place again (see the top). ESP's Show and Look are Targets and Drawing, Colour is Colours.
    ("World > ESP > Show", "World > ESP > Targets"),
    ("World > ESP > Look > Colour", "World > ESP > Drawing > Colours"),
    ("World > ESP > Look", "World > ESP > Drawing"),
    // Appearance's rows went into lists.
    ("Menu > Settings > Appearance > Menu Colour", "Menu > Settings > Appearance > Colours > Menu Colour"),
    ("Menu > Settings > Appearance > Tabs Width", "Menu > Settings > Appearance > Tabs > Tabs Width"),
    ("Menu > Settings > Appearance > Tabs Height", "Menu > Settings > Appearance > Tabs > Tabs Height"),
    ("Menu > Settings > Appearance > Address Bar Height", "Menu > Settings > Appearance > Address Bar > Address Bar Height"),
    // Gear Score's page became one row; its switch went, so load_profile drops an old Score line
    // whose switch was not On. That row then went into the Gear Score list, Pick Other Points into
    // Credits'.
    ("Self > Progress > Gear Score > Score", "Self > Progress > Gear Score > Dropped Item Gear Score"),
    ("Self > Progress > Dropped Item Gear Score", "Self > Progress > Gear Score > Dropped Item Gear Score"),
    ("Self > Progress > Pick Other Points", "Self > Progress > Credits > Pick Other Points"),
    // Unlimited Ammo is Infinite Mag.
    ("Weapon > Unlimited Ammo", "Weapon > Infinite Mag"),
    ("Settings > Appearance > Menu Left / Right", "Menu > Settings > Appearance > Position > Menu Left / Right"),
    ("Settings > Appearance > Menu Up / Down", "Menu > Settings > Appearance > Position > Menu Up / Down"),
    ("Settings > Appearance", "Menu > Settings > Appearance"),
    ("Settings > Disable Warnings", "Menu > Settings > Warnings > Disable Warnings"),
    ("Settings > Saved Commands", "Menu > Saved Commands"),
    ("Settings > Hotkeys", "Menu > Hotkeys"),
    ("Settings > Unload grbmod", "Menu > Unload grbmod"),
    // Grbmod is now Aurora: the Unload row's name changed with it.
    ("Menu > Unload grbmod", "Menu > Unload Aurora"),
    ("Settings > No Zoomed Sway", "Weapon > No Scoped Sway"),
    ("Settings", "Game"),
    ("Weapon > No Zoomed Sway", "Weapon > No Scoped Sway"),
    ("Menu > Appearance > Menu Left / Right", "Menu > Settings > Appearance > Position > Menu Left / Right"),
    ("Menu > Appearance > Menu Up / Down", "Menu > Settings > Appearance > Position > Menu Up / Down"),
    ("Menu > Appearance", "Menu > Settings > Appearance"),
    ("Menu > Disable Warnings", "Menu > Settings > Warnings > Disable Warnings"),
    // Target was in Menu > Experiments from 2 Nov evening to; "World > Target" is its place again,
    // so a Hotkeys.txt with either address lands on the same row.
    ("Menu > Experiments > Target", "World > Target"),
    ("World > ESP > Hostile", "World > ESP > Show > Hostile"),
    ("World > ESP > Friendly", "World > ESP > Show > Friendly"),
    ("World > ESP > Players", "World > ESP > Show > Players"),
    ("World > ESP > Hide Dead", "World > ESP > Show > Hide Dead"),
    ("World > ESP > Style", "World > ESP > Look > Style"),
    ("World > ESP > Lines", "World > ESP > Look > Lines"),
    ("World > ESP > Black Edge", "World > ESP > Look > Black Edge"),
    ("World > ESP > Distance", "World > ESP > Look > Distance"),
    ("World > ESP > Colour", "World > ESP > Look > Colour"),
    // Last: the Weapon tab is Self > Weapons, and Self > Progress is Game > Progress > Editor. A
    // hotkey on the old Progress list opens the Editor, the list with the same rows.
    ("Self > Progress", "Game > Progress > Editor"),
    ("Weapon", "Self > Weapons"),
    // The very last: the Menu tab is "Aurora", so every "Menu > " address above, old or new, ends
    // up under Aurora.
    ("Menu", "Aurora"),
    // After that, as they come out of the move above: NVG Overlay is "No NVG Overlay"; how
    // notifications look went to Appearance > Notifications (Suppress Generic Responses stays in
    // Settings > Notifications).
    ("Game > Rendering > NVG Overlay", "Game > Rendering > No NVG Overlay"),
    ("Aurora > Settings > Notifications > Position", "Aurora > Settings > Appearance > Notifications > Position"),
    ("Aurora > Settings > Notifications > Custom Position", "Aurora > Settings > Appearance > Notifications > Custom Position"),
    ("Aurora > Settings > Notifications > Width", "Aurora > Settings > Appearance > Notifications > Width"),
    ("Aurora > Settings > Notifications > Size", "Aurora > Settings > Appearance > Notifications > Size"),
    ("Aurora > Settings > Notifications > Text Size", "Aurora > Settings > Appearance > Notifications > Text Size"),
    ("Aurora > Settings > Notifications > Border Colour", "Aurora > Settings > Appearance > Notifications > Border Colour"),
    ("Aurora > Settings > Notifications > Background Colour", "Aurora > Settings > Appearance > Notifications > Background Colour"),
    ("Aurora > Settings > Notifications > Flash Notification", "Aurora > Settings > Appearance > Notifications > Flash Notification"),
    ("Aurora > Settings > Notifications > Timing", "Aurora > Settings > Appearance > Notifications > Timing"),
];

/// Today's addresses of rows an older profile's lines are turned into. The tab is "Aurora" since.
const POS_X_ADDR: &str = "Aurora > Settings > Appearance > Position > X";
const POS_Y_ADDR: &str = "Aurora > Settings > Appearance > Position > Y";
const AUTO_PROCEED_ADDR: &str = "Aurora > Settings > Warnings > Automatically Proceed On Warnings";
const AUTO_BOX_ADDR: &str = "Aurora > Settings > Warnings > Automatically Proceed On Warnings > Command Box";
const AUTO_KEY_ADDR: &str = "Aurora > Settings > Warnings > Automatically Proceed On Warnings > Hotkey";
const SKIP_ACCEPTED_ADDR: &str = "Aurora > Settings > Warnings > Skip Already-Accepted Warnings";
/// Menu Size and Tabs Width, which `old_x_shift` needs for an old profile's X. Menu Size went: its
/// line becomes these List Width / Height lines and scales the notifications' Size and Text Size
/// (`upgrade_rows`).
const MENU_SIZE_ADDR: &str = "Aurora > Settings > Appearance > Menu Size";
const TABS_W_ADDR: &str = "Aurora > Settings > Appearance > Tabs > Tabs Width";
const LIST_W_ADDR: &str = "Aurora > Settings > Appearance > List Width";
const LIST_H_ADDR: &str = "Aurora > Settings > Appearance > List Height";
const NOTE_SIZE_ADDR: &str = "Aurora > Settings > Appearance > Notifications > Size";
const NOTE_TEXT_ADDR: &str = "Aurora > Settings > Appearance > Notifications > Text Size";

/// An old profile's Menu Size line, in percent (None: it has none, the old 100%).
fn profile_size(rows: &[(String, String)]) -> Option<u32> {
    rows.iter()
        .find(|(a, _)| moved(a).as_deref().unwrap_or(a) == MENU_SIZE_ADDR)
        .and_then(|(_, v)| v.trim_end_matches('%').trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.round().clamp(60.0, 200.0) as u32)
}

/// List Width and List Height that look as Menu Size `size` did (`menuview::list_at`).
fn list_for_size(size: u32) -> [u32; 2] {
    let l = aurora_core::menuview::list_at(size as f32);
    [l[0].round() as u32, l[1].round() as u32]
}

/// A notification Size or Text Size times the old Menu Size, kept in its row's range.
fn note_times_size(value: f64, size: u32, row: usize) -> u32 {
    (value * f64::from(size) / 100.0).round().clamp(f64::from(LAYOUT_ROWS[row].3), f64::from(LAYOUT_ROWS[row].4)) as u32
}

/// How far right of an old profile's X its address bar sat (`old_x_shift`), from its own Menu Size
/// and Tabs Width lines (the usual ones when it has none).
fn profile_x_shift(rows: &[(String, String)], screen_h: i32) -> i32 {
    let get = |addr: &str| {
        rows.iter().find(|(a, _)| moved(a).as_deref().unwrap_or(a) == addr).and_then(|(_, v)| v.trim_end_matches('%').trim().parse::<f64>().ok()).filter(|v| v.is_finite())
    };
    let size = get(MENU_SIZE_ADDR).map_or(100, |v| v.clamp(60.0, 200.0) as u32);
    let tabs = get(TABS_W_ADDR).map_or(LAYOUT_ROWS[TABS_WIDTH].2, |v| v.clamp(f64::from(LAYOUT_ROWS[TABS_WIDTH].3), f64::from(LAYOUT_ROWS[TABS_WIDTH].4)) as u32);
    old_x_shift(size, tabs, screen_h)
}

/// An older profile's lines in today's rows and units: Position's percent rows (Menu Left / Right,
/// Menu Up / Down, at any address they had) become X and Y in pixels on `screen`, as the menu sat;
/// Disable Warnings On becomes both Automatically Proceed switches and Skip Already-Accepted
/// Warnings (Off is today's defaults, so it goes). A line the profile already has at today's
/// address wins over a turned one. Every other line stays as it is.
fn upgrade_rows(rows: Vec<(String, String)>, screen: (i32, i32)) -> Vec<(String, String)> {
    // The percent X was the picture's left edge: plus the help boxes' room, plus the bar to the
    // wall.
    let shift = profile_x_shift(&rows, screen.1);
    let size = profile_size(&rows);
    let mut kept = Vec::new();
    let mut turned = Vec::new();
    for (addr, value) in rows {
        let name = addr.rsplit(" > ").next().unwrap_or("");
        let now = moved(&addr).unwrap_or_else(|| addr.clone());
        // Menu Size went: List Width / Height that look the same; the 85% look is their default, so
        // it needs no lines.
        if now == MENU_SIZE_ADDR {
            if let Some(s) = size {
                let l = list_for_size(s);
                if l != [LAYOUT_ROWS[LIST_W].2, LAYOUT_ROWS[LIST_H].2] {
                    turned.push((LIST_W_ADDR.to_string(), l[0].to_string()));
                    turned.push((LIST_H_ADDR.to_string(), l[1].to_string()));
                }
            }
            continue;
        }
        match name {
            "Menu Left / Right" | "Menu Up / Down" => {
                if let Ok(pct) = value.trim_end_matches('%').trim().parse::<f64>()
                    && pct.is_finite()
                {
                    let v = if name == "Menu Left / Right" { (POS_X_ADDR, old_x_to_wall(pct_px(pct, screen.0) + shift, size.unwrap_or(100), screen)) } else { (POS_Y_ADDR, pct_px(pct, screen.1)) };
                    turned.push((v.0.to_string(), v.1.to_string()));
                }
            }
            "Disable Warnings" => {
                if value.eq_ignore_ascii_case("on") {
                    for to in [AUTO_BOX_ADDR, AUTO_KEY_ADDR, SKIP_ACCEPTED_ADDR] {
                        turned.push((to.to_string(), "On".to_string()));
                    }
                }
            }
            _ => kept.push((addr, value)),
        }
    }
    // Notifications followed Menu Size before: their Size and Text Size take it in, so they look as
    // they did.
    if let Some(s) = size.filter(|&s| s != 100) {
        for (to, row) in [(NOTE_SIZE_ADDR, TOAST_SIZE), (NOTE_TEXT_ADDR, NOTE_TEXT)] {
            match kept.iter_mut().find(|(a, _)| moved(a).as_deref().unwrap_or(a) == to) {
                Some(line) => {
                    let v = line.1.trim_end_matches('%').trim().parse::<f64>().ok().filter(|v| v.is_finite()).unwrap_or(f64::from(LAYOUT_ROWS[row].2));
                    line.1 = note_times_size(v, s, row).to_string();
                }
                None => turned.push((to.to_string(), note_times_size(f64::from(LAYOUT_ROWS[row].2), s, row).to_string())),
            }
        }
    }
    let have: Vec<String> = kept.iter().map(|(a, _)| moved(a).unwrap_or_else(|| a.clone())).collect();
    for t in turned {
        if !have.contains(&t.0) && !kept.iter().any(|k| k.0 == t.0) {
            kept.push(t);
        }
    }
    kept
}

/// A profile's text as today's lines: Fly's units (`upgrade_profile`), then `upgrade_rows` on this
/// screen. Lines of rows that went on purpose (`GONE`) are left out quietly.
fn profile_rows(text: &str) -> Vec<(String, String)> {
    let screen = screen_size();
    let mut rows = upgrade_rows(read_tree(&upgrade_profile(text, screen)), screen);
    rows.retain(|(a, _)| !gone(a));
    rows
}

/// Rows taken out on purpose, with why: Stand While Flying is always on while Fly is on now;
/// Notifications lost Invert Flow, Padding and Flash Colour. Old profile lines, hotkeys and Saved
/// Commands on them are dropped without a toast; the log says so once.
const GONE: &[(&str, &str)] = &[
    ("Self > Movement > Fly > Stand While Flying", "always on with Fly"),
    // List Width / Height took Menu Size's place; the menu by hand never shows
    // plain replies now, so its Suppress switch went.
    (MENU_SIZE_ADDR, "List Width and List Height took its place"),
    ("Aurora > Settings > Notifications > Suppress Generic Responses > Menu", "the menu never shows plain replies now"),
    ("Aurora > Settings > Notifications > Invert Flow", "taken out of Notifications"),
    ("Aurora > Settings > Notifications > Padding", "taken out of Notifications"),
    ("Aurora > Settings > Notifications > Flash Colour", "taken out of Notifications"),
];
/// Their old commands, typed or in a command.request script, and the line they get.
const GONE_COMMANDS: &[(&str, &str)] = &[
    ("standwhileflying", "Stand While Flying is always on while Fly is on"),
    ("menusize", "Menu Size is now List Width and List Height"),
    ("suppressgenericresponsesmenu", "The menu never shows plain replies now"),
    ("notifyinvertflow", "Invert Flow is no longer in Notifications"),
    ("notifypadding", "Padding is no longer in Notifications"),
    ("notifyflashcolour", "Flash Colour is no longer in Notifications"),
];

/// A row taken out on purpose, at its address now or one it had before a move.
fn gone(addr: &str) -> bool {
    let now = moved(addr).unwrap_or_else(|| addr.to_string());
    let hit = GONE.iter().find(|g| g.0 == now);
    if let Some((_, why)) = hit {
        let mut said = GONE_SAID.lock().unwrap_or_else(|e| e.into_inner());
        if !said.contains(&now) {
            log(&format!("menu: {now} is no longer a row ({why}); its old lines, hotkeys and saved commands are ignored"));
            said.push(now);
        }
    }
    hit.is_some()
}

static GONE_SAID: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Rows that moved down into a list of their own name: only the address itself, since every row in
/// the new list starts with the old address.
const MOVED_EXACT: [(&str, &str); 1] = [("Self > Progress > Credits", "Self > Progress > Credits > Credits")];

/// Where a row that moved lives now; None when it did not move. A row that moved more than once
/// follows each move (Menu > Appearance > Menu Colour went to Menu > Settings > Appearance, then
/// into Colours).
fn moved(addr: &str) -> Option<String> {
    let mut now = moved_once(addr)?;
    for _ in 0..8 {
        match moved_once(&now) {
            Some(next) => now = next,
            None => break,
        }
    }
    Some(now)
}

fn moved_once(addr: &str) -> Option<String> {
    if let Some((_, new)) = MOVED_EXACT.iter().find(|(old, _)| *old == addr) {
        return Some((*new).to_string());
    }
    MOVED.iter().find_map(|(old, new)| match addr.strip_prefix(old) {
        Some("") => Some((*new).to_string()),
        Some(rest) if rest.starts_with(" > ") => Some(format!("{new}{rest}")),
        _ => None,
    })
}

/// Hotkeys on rows, from their "O" menu: `%APPDATA%\Aurora\Hotkeys.txt`, in Stand's tree as the
/// profiles: `Row: Key1, Key2` under its lists. Each row address with its keys.
type RowKeys = Vec<(String, Vec<u32>)>;
static ROW_KEYS: Mutex<Option<RowKeys>> = Mutex::new(None);

fn row_keys() -> RowKeys {
    let mut g = ROW_KEYS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(|| {
        let Some(f) = folder() else { return Vec::new() };
        // Hotkeys.txt once it is there; before that row_keys.json, read once and left on disk.
        let read = read_text(&f.join("Hotkeys.txt"));
        if read.is_err() {
            HOTKEYS_UNREADABLE.store(true, Ordering::Relaxed);
        }
        let (list, from_json): (RowKeys, bool) = match read {
            Err(_) => return Vec::new(),
            Ok(Some(text)) => {
                let list = read_tree(&text)
                    .into_iter()
                    // The menu's own two keys on top are not a row's.
                    .filter(|(addr, _)| addr != aurora_core::MENU_LINE && addr != aurora_core::BOX_LINE)
                    .map(|(addr, names)| {
                        let (keys, unknown) = parse_keys(&names);
                        for n in unknown {
                            log(&format!("menu: Hotkeys.txt: no key called {n} ({addr})"));
                        }
                        (addr, keys)
                    })
                    .collect();
                (list, false)
            }
            Ok(None) => {
                let v: Option<serde_json::Value> = std::fs::read_to_string(f.join("row_keys.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
                let Some(v) = v.and_then(|v| v.as_array().cloned()) else { return Vec::new() };
                let list = v
                    .iter()
                    .filter_map(|e| {
                        let addr = e.get("address")?.as_str()?.to_string();
                        let keys = e.get("keys")?.as_array()?.iter().filter_map(|k| k.as_u64()).map(|k| k as u32).collect();
                        Some((addr, keys))
                    })
                    .collect();
                log("menu: row hotkeys taken from row_keys.json into Hotkeys.txt");
                (list, true)
            }
        };
        // Rows that moved take their keys along (two old rows on one new one share them).
        let mut out: RowKeys = Vec::new();
        let mut changed = from_json;
        for (addr, keys) in list {
            let addr = match moved(&addr) {
                Some(new) => {
                    log(&format!("menu: hotkeys of {addr} moved to {new}"));
                    changed = true;
                    new
                }
                None => addr,
            };
            match out.iter_mut().find(|e| e.0 == addr) {
                Some(e) => {
                    for k in keys {
                        if !e.1.contains(&k) {
                            e.1.push(k);
                        }
                    }
                }
                None => out.push((addr, keys)),
            }
        }
        out.retain(|e| !e.1.is_empty());
        // Keys on a row that went are dropped, so they reach the game again and no "is gone" toast
        // comes on each press.
        let n = out.len();
        out.retain(|e| !gone(&e.0));
        changed |= out.len() != n;
        if changed {
            write_row_keys(&out);
        }
        out
    })
    .clone()
}

/// Hotkeys.txt was there but could not be read at load: never write over it this session.
static HOTKEYS_UNREADABLE: AtomicBool = AtomicBool::new(false);

fn write_row_keys(v: &RowKeys) {
    if HOTKEYS_UNREADABLE.load(Ordering::Relaxed) {
        log("menu: Hotkeys.txt could not be read at load, so it is not written (row hotkeys not saved)");
        return;
    }
    if let Some(f) = folder() {
        let rows: Vec<(String, String)> = v.iter().map(|(a, k)| (a.clone(), key_names(k))).collect();
        let _ = aurora_core::paths::write_file(&f.join("Hotkeys.txt"), aurora_core::with_menu_keys(&write_tree(&rows), &crate::keys::get()));
    }
}

/// Keys as Hotkeys.txt has them: the names the rows show, `F5, Numpad 2`.
fn key_names(keys: &[u32]) -> String {
    keys.iter().map(|k| key_name(*k)).collect::<Vec<_>>().join(", ")
}

/// `F5, Numpad 2` back to keys (any case), and the names that are no key.
fn parse_keys(text: &str) -> (Vec<u32>, Vec<String>) {
    let (mut keys, mut unknown) = (Vec::new(), Vec::new());
    for name in text.split(", ").map(str::trim).filter(|n| !n.is_empty()) {
        match (1..=255u32).find(|vk| key_name(*vk).eq_ignore_ascii_case(name)) {
            Some(vk) if !keys.contains(&vk) => keys.push(vk),
            Some(_) => {}
            None => unknown.push(name.to_string()),
        }
    }
    (keys, unknown)
}

fn set_row_keys(v: RowKeys) {
    let v: RowKeys = v.into_iter().filter(|e| !e.1.is_empty()).collect();
    write_row_keys(&v);
    *ROW_KEYS.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
}

fn keys_of(addr: &str) -> Vec<u32> {
    row_keys().into_iter().find(|e| e.0 == addr).map(|e| e.1).unwrap_or_default()
}

fn add_row_key(st: &mut State, vk: u32) {
    let addr = ctx().0;
    let mut all = row_keys();
    match all.iter_mut().find(|e| e.0 == addr) {
        Some(e) => {
            if !e.1.contains(&vk) {
                e.1.push(vk);
            }
        }
        None => all.push((addr, vec![vk])),
    }
    set_row_keys(all);
    confirm(st, format!("Hotkey {} added", key_name(vk)));
}

fn remove_row_key(st: &mut State, vk: u32) {
    let addr = ctx().0;
    let mut all = row_keys();
    if let Some(e) = all.iter_mut().find(|e| e.0 == addr) {
        e.1.retain(|k| *k != vk);
    }
    set_row_keys(all);
    confirm(st, format!("Hotkey {} removed", key_name(vk)));
}

/// Row hotkeys work with the menu closed too; not while typing or picking a key.
fn row_hotkeys(st: &mut State, focused: bool, was: &mut std::collections::HashMap<u32, bool>) {
    let busy = st.edit.is_some() || st.bind.is_some() || st.bind_hold.is_some();
    let all = row_keys();
    // Each key once, then every row it is on (one key may do several things).
    let mut vks: Vec<u32> = all.iter().flat_map(|e| e.1.iter().copied()).collect();
    vks.sort_unstable();
    vks.dedup();
    for vk in vks {
        // As the menu's keys: a key already down when first looked at, or when the game came back
        // to the front, waits until it is let go.
        let now = down(vk);
        let before = was.insert(vk, now).unwrap_or(now);
        if !fresh_press(before, now, focused) || busy {
            continue;
        }
        FROM.store(FROM_KEY, Ordering::Relaxed);
        for (addr, _) in all.iter().filter(|e| e.1.contains(&vk)) {
            match resolve(addr, st.prefs) {
                Some(f) => run_found(st, f, None),
                None if !st.prefs.experimental && in_experiments(addr) => note(st, EXP_OFF),
                None => note(st, format!("Hotkey: {addr} is gone")),
            }
        }
        FROM.store(FROM_MENU, Ordering::Relaxed);
    }
}

/// Menu > Saved Commands: `%APPDATA%\Aurora\Saved Commands.txt`, in Stand's tree: each saved row as
/// `Name: Saved` under its lists. Before that it was saved.json, a list of addresses, read once and
/// left on disk.
fn saved_list() -> Vec<String> {
    let Some(f) = folder() else { return Vec::new() };
    let read = read_text(&f.join("Saved Commands.txt"));
    // Read again on every look: one brief lock (antivirus, cloud sync) must not stop saving for the
    // whole session, only until the file reads again.
    SAVED_UNREADABLE.store(read.is_err(), Ordering::Relaxed);
    let list: Vec<String> = match read {
        Ok(Some(text)) => read_tree(&text).into_iter().map(|(addr, _)| addr).collect(),
        Err(_) => return Vec::new(),
        Ok(None) => {
            let old: Vec<String> = std::fs::read_to_string(f.join("saved.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            set_saved(&old);
            old
        }
    };
    // Rows that moved are saved at their new place, once; rows that went go.
    if !list.iter().any(|a| moved(a).is_some() || gone(a)) {
        return list;
    }
    let mut out: Vec<String> = Vec::new();
    for a in list.into_iter().filter(|a| !gone(a)) {
        let a = match moved(&a) {
            Some(new) => {
                log(&format!("menu: saved command {a} moved to {new}"));
                new
            }
            None => a,
        };
        if !out.contains(&a) {
            out.push(a);
        }
    }
    set_saved(&out);
    out
}

/// Saved Commands.txt was there but could not be read at the last look: don't write over it until
/// it reads again.
static SAVED_UNREADABLE: AtomicBool = AtomicBool::new(false);

fn set_saved(v: &[String]) {
    if SAVED_UNREADABLE.load(Ordering::Relaxed) {
        log("menu: Saved Commands.txt could not be read, so it is not written");
        return;
    }
    if let Some(f) = folder() {
        let rows: Vec<(String, String)> = v.iter().map(|a| (a.clone(), "Saved".to_string())).collect();
        let _ = aurora_core::paths::write_file(&f.join("Saved Commands.txt"), write_tree(&rows));
    }
}

/// Apply Default State To Children: every row inside `page`, and further in.
fn defaults(st: &mut State, page: Page, depth: usize) {
    if depth > 6 {
        return;
    }
    // Commands With Non-Default State: every row it lists, wherever it lives.
    if page == Page::NonDefault {
        for (_, it) in nondefault_rows(st.prefs) {
            default_of(st, it.id);
        }
        return;
    }
    for it in items(page, st.prefs) {
        match it.id {
            Id::Open(p) if !is_ctx(p) && !matches!(p, Page::Saved | Page::NonDefault | Page::HotkeyList | Page::Unload | Page::ProfileDelete(_)) => defaults(st, p, depth + 1),
            id => default_of(st, id),
        }
    }
}

/// Menu > Apply Default State and `default`: every row with a state, on every page, back to its
/// default. The menu's own settings too: colour, size, position, Suppress Generic Responses,
/// Warnings; they are states like any other. Hotkeys stay; they are not a state.
fn default_all(st: &mut State) {
    // Every row, also Experiments' while Enable Experimental Features is off: its values (FOV,
    // Range, Priority) stay when the switch goes off.
    let prefs = Prefs { experimental: true, ..st.prefs };
    for f in all_rows(prefs) {
        if state_of(&f.item, prefs).is_some() {
            default_of(st, f.item.id);
        }
    }
}

fn default_of(st: &mut State, id: Id) {
    unwait(id);
    let d = aurora_core::proto::EspStyle::DEFAULT;
    match id {
        Id::God => {
            let (entity, armed, _) = godmode::status();
            if entity != 0 || armed {
                let _ = godmode::off();
            }
        }
        Id::Fly => {
            if fly::is_on() {
                let _ = fly::set(false);
            }
        }
        Id::FlySpeed => fly::set_speed(fly::SPEED_DEFAULT),
        Id::FlySprint => fly::set_sprint(fly::SPRINT_DEFAULT),
        Id::Unseen => {
            let _ = crate::unseen::set(false);
        }
        Id::UnseenInbox => {
            let _ = crate::inbox::set(false);
        }
        Id::FlyCalm => fly::set_calm(false),
        Id::FlyGuard => fly::set_guard(false),
        Id::FlyLetFall => crate::airwalk::set_let_fall(false),
        Id::NvgZone => {
            let _ = crate::nvgzone::set(false);
        }
        Id::NoRecoil => {
            let _ = crate::weapon::set_recoil(false);
        }
        Id::NoShake => {
            let _ = crate::weapon::set_shake(false);
        }
        Id::NoNvgOverlay => {
            let _ = crate::nvg::set(false);
        }
        Id::NoSway => {
            let _ = crate::weapon::set_sway(false);
        }
        Id::Stamina => {
            let _ = crate::weapon::set_stamina(false);
        }
        Id::Damage => {
            let _ = crate::weapon::set_damage(0.0);
        }
        Id::FireRate => {
            let _ = crate::weapon::set_fire_rate(0.0);
        }
        Id::Accuracy => {
            let _ = crate::weapon::set_accuracy(false);
        }
        Id::Ammo => {
            let _ = crate::weapon::set_ammo(false);
        }
        Id::Reserve => {
            let _ = crate::weapon::set_reserve(false);
        }
        Id::SilentAim => {
            let _ = crate::silentaim::set(false);
        }
        Id::SilentFov => crate::silentaim::set_fov(crate::silentaim::FOV_DEFAULT),
        Id::SilentRange => crate::silentaim::set_range_m(crate::silentaim::RANGE_DEFAULT),
        Id::SilentCircle => crate::silentaim::set_circle(true),
        Id::SilentMagic => crate::silentaim::set_magic(false),
        Id::SilentNoRange => crate::silentaim::set_no_range(false),
        Id::SilentInstant => crate::silentaim::set_instant(false),
        Id::SilentNoDrop => {
            let _ = crate::silentaim::set_no_drop(false);
        }
        Id::SilentPriority => crate::silentaim::set_priority(crate::silentaim::Priority::Crosshair),
        Id::HopUp => fly::set_hop_up(true),
        Id::HopHeight => fly::set_hop_height(fly::HOP_DEFAULT_M),
        Id::SafeFly => fly::set_safe(true),
        Id::GearValue => set_gear(0),
        Id::Coord(i) => set_coord(i, 0.0),
        Id::EspOn => esp_change(st, |c| c.on = false),
        Id::EspEnemies => esp_change(st, |c| c.hostile = true),
        Id::EspFriendlies => esp_change(st, |c| c.friendly = true),
        Id::EspPlayers => esp_change(st, |c| c.players = true),
        Id::EspDrones => esp_change(st, |c| c.drones = true),
        Id::EspHideDead => esp_change(st, |c| c.hide_dead = true),
        Id::EspDistance => esp_change(st, |c| c.style.labels = d.labels),
        Id::EspEdge => esp_change(st, |c| c.style.edge = d.edge),
        Id::EspBones => esp_change(st, |c| c.style.bones = d.bones),
        Id::EspRange => esp_change(st, |c| c.style.range_m = d.range_m),
        Id::EspLines => esp_change(st, |c| c.style.width = d.width),
        Id::EspColour(i) => esp_change(st, |c| *esp_colour_mut(&mut c.style, i) = esp_colour(&d, i)),
        Id::MenuColour | Id::ToastPosition | Id::ToastColour | Id::BackColour => {
            set_choice(&mut st.prefs, id, 0);
            prefs_changed(st.prefs);
        }
        Id::NotifyX | Id::NotifyY => {
            set_pixel(&mut st.prefs, id, if id == Id::NotifyX { NOTIFY_XY_DEFAULT.0 } else { NOTIFY_XY_DEFAULT.1 });
            prefs_changed(st.prefs);
        }
        // Switches of the menu's own settings: as Prefs::default.
        Id::AutoBox | Id::AutoKey | Id::ForceRead | Id::SkipAccepted | Id::MoveMouse | Id::MouseSupport => {
            let d = Prefs::default();
            let p = &mut st.prefs;
            match id {
                Id::AutoBox => p.auto_box = d.auto_box,
                Id::AutoKey => p.auto_key = d.auto_key,
                Id::ForceRead => p.force_read = d.force_read,
                Id::SkipAccepted => p.skip_accepted = d.skip_accepted,
                Id::MouseSupport => p.mouse_support = d.mouse_support,
                _ => p.move_mouse = d.move_mouse,
            }
            prefs_changed(st.prefs);
        }
        // Off, every experiment with it.
        Id::Experimental => set_experimental(st, false),
        Id::Layout(i) => {
            st.prefs.layout[i] = LAYOUT_ROWS[i].2;
            prefs_changed(st.prefs);
        }
        // Where it always sat.
        Id::PosX => {
            st.prefs.x = None;
            prefs_changed(st.prefs);
        }
        Id::PosY => {
            st.prefs.y = None;
            prefs_changed(st.prefs);
        }
        Id::QuietBox | Id::QuietKey => {
            st.prefs.quiet_box &= id != Id::QuietBox;
            st.prefs.quiet_key &= id != Id::QuietKey;
            prefs_changed(st.prefs);
        }
        _ => {}
    }
}

/// Rows that `default_of` can put back.
fn has_default(id: Id) -> bool {
    matches!(
        id,
        Id::God | Id::Unseen | Id::UnseenInbox | Id::FlyCalm | Id::FlyGuard | Id::FlyLetFall | Id::NvgZone | Id::NoRecoil | Id::NoShake | Id::NoNvgOverlay | Id::NoSway | Id::Stamina | Id::Damage | Id::FireRate | Id::Accuracy | Id::Ammo | Id::Reserve | Id::SilentAim | Id::SilentFov | Id::SilentRange | Id::SilentCircle | Id::SilentPriority | Id::SilentMagic | Id::SilentNoRange | Id::SilentInstant | Id::SilentNoDrop | Id::Fly | Id::FlySpeed | Id::FlySprint | Id::HopUp | Id::HopHeight | Id::SafeFly | Id::GearValue | Id::Coord(_) | Id::EspOn | Id::EspEnemies | Id::EspFriendlies | Id::EspPlayers | Id::EspDrones | Id::EspHideDead | Id::EspDistance | Id::EspEdge | Id::EspBones | Id::EspRange | Id::EspLines | Id::EspColour(_) | Id::MenuColour | Id::PosX | Id::PosY | Id::MoveMouse | Id::MouseSupport | Id::Experimental | Id::Layout(_) | Id::ToastPosition | Id::ToastColour | Id::BackColour | Id::NotifyX | Id::NotifyY | Id::QuietBox | Id::QuietKey | Id::AutoBox | Id::AutoKey | Id::ForceRead | Id::SkipAccepted
    )
}

/// Copy Address puts the address on the clipboard.
fn copy_text(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    const CF_UNICODETEXT: u32 = 13;
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: plain clipboard calls; the memory is ours until the clipboard takes it.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let h = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
        let mut ok = false;
        if !h.is_null() {
            let p = GlobalLock(h) as *mut u16;
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
                GlobalUnlock(h);
                ok = !SetClipboardData(CF_UNICODETEXT, h).is_null();
            }
        }
        CloseClipboard();
        ok
    }
}

/// A row's hotkeys, from its "O" menu (Hotkeys.txt).
fn hotkeys_of(addr: &str) -> Vec<u32> {
    keys_of(addr)
}

/// Backspace, Left or Right in the command box, at its caret: Left / Right move it one character,
/// Backspace erases before it (with Ctrl, the word).
fn box_key(st: &mut State, vk: u32) -> bool {
    let Some(t) = st.edit.as_mut() else { return false };
    let at = caret_in(t, st.caret);
    st.caret = Some(match vk {
        0x25 => back_one(t, at),
        0x27 => ahead_one(t, at),
        _ => {
            st.tab = None;
            st.hist_pos = None;
            erase(t, at, down(0x11))
        }
    });
    true
}

/// A typed character goes in at the command box's caret, up to 60.
fn type_at(st: &mut State, c: char) {
    if let Some(t) = st.edit.as_mut()
        && t.len() < 60
    {
        let at = caret_in(t, st.caret);
        t.insert(at, c);
        st.caret = Some(at + c.len_utf8());
        st.tab = None;
        st.hist_pos = None;
    }
}

/// The command box's caret in `t`, in bytes: the end when None or past it, and always on a
/// character's start.
fn caret_in(t: &str, caret: Option<usize>) -> usize {
    let mut at = caret.map_or(t.len(), |c| c.min(t.len()));
    while !t.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The caret one character back from `at` (it stops at the start).
fn back_one(t: &str, at: usize) -> usize {
    t[..at].chars().next_back().map_or(at, |c| at - c.len_utf8())
}

/// The caret one character on from `at` (it stops at the end).
fn ahead_one(t: &str, at: usize) -> usize {
    t[at..].chars().next().map_or(at, |c| at + c.len_utf8())
}

/// Backspace in the command box: the character before the caret `at`; with Ctrl, the whole word
/// before it (and the spaces after that word). Gives the caret's new place.
fn erase(t: &mut String, at: usize, word: bool) -> usize {
    let from = if word {
        let mut from = at;
        while t[..from].ends_with(' ') {
            from -= 1;
        }
        while let Some(c) = t[..from].chars().next_back().filter(|&c| c != ' ') {
            from -= c.len_utf8();
        }
        from
    } else {
        back_one(t, at)
    };
    t.replace_range(from..at, "");
    from
}

/// Commands from `%APPDATA%\Aurora\command.request`, one per line, run as if typed in the command
/// box; the file goes once read. For scripts and tests.
fn command_file(st: &mut State, checked: &mut Instant) {
    if checked.elapsed() < Duration::from_millis(250) {
        return;
    }
    *checked = Instant::now();
    // `unload.request` (test scripts): the same as the menu's Unload > Yes.
    let unload = aurora_core::paths::unload_request();
    if unload.exists() {
        let _ = std::fs::remove_file(&unload);
        log("unload.request found");
        unload_now(st);
        return;
    }
    let Some(path) = folder().map(|f| f.join("command.request")) else { return };
    let Some(text) = take_request(&path) else { return };
    // As if typed in the command box, Suppress Generic Responses too.
    FROM.store(FROM_BOX, Ordering::Relaxed);
    IN_BOX.store(true, Ordering::Relaxed);
    for line in text.lines() {
        log(&format!("command file: {line}"));
        // A profile loaded by an earlier line puts FROM back to the menu when it ends.
        FROM.store(FROM_BOX, Ordering::Relaxed);
        run_line(st, line);
    }
    IN_BOX.store(false, Ordering::Relaxed);
    FROM.store(FROM_MENU, Ordering::Relaxed);
}

/// Set while command.request can't be removed, so the log says so once, not four
/// times a second.
static REQUEST_STUCK: AtomicBool = AtomicBool::new(false);

/// The text of a request file, given only once the file is removed. Before, a file
/// that could not be removed (still open in the script that wrote it) ran again every 250 ms. A
/// byte-order mark at the start (PowerShell's utf8) is dropped.
fn take_request(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        if !REQUEST_STUCK.swap(true, Ordering::Relaxed) {
            log(&format!("command file: can't remove it yet ({e}); it runs once it can be"));
        }
        return None;
    }
    REQUEST_STUCK.store(false, Ordering::Relaxed);
    Some(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
}

/// Every key that is an Aurora hotkey (default or custom, fly's moving keys aside) is kept from the
/// game, menu open or not; looked at again twice a second.
fn keep_hotkeys(checked: &mut Instant) {
    if checked.elapsed() < Duration::from_millis(500) {
        return;
    }
    *checked = Instant::now();
    let keys = crate::keys::get();
    let mut v: Vec<u32> = BIND_ROWS.iter().map(|r| keys.get(r.1)).collect();
    v.extend(row_keys().into_iter().flat_map(|e| e.1));
    v.retain(|&k| k != 0);
    crate::dinput::set_hotkeys(&v);
    // The keys are taken from the game window and DirectInput, so those must be ours.
    if SUB_HWND.load(Ordering::SeqCst) == 0 && vkdraw::find_window().is_some() {
        subclass();
    }
    crate::dinput::ensure();
}

/// Pages that list rows from elsewhere, each with its address: Saved Commands, Commands With
/// Non-Default State and findc's page.
fn listed_rows(page: Page, prefs: Prefs) -> Option<Vec<(String, Item)>> {
    match page {
        Page::Saved => Some(saved_rows(prefs)),
        Page::NonDefault => Some(nondefault_rows(prefs)),
        Page::Find => Some(find_rows(prefs)),
        _ => None,
    }
}

/// Commands With Non-Default State gets shorter as rows go back to default (a reset, a switch in
/// place, a hotkey): while it is shown, its pick and scroll stay inside what is left, so the list
/// never shows empty space.
fn refit_nondefault(st: &mut State) {
    let Some(top) = st.stack.last().copied().filter(|t| t.0 == Page::NonDefault) else { return };
    let n = items(Page::NonDefault, st.prefs).len().max(1);
    let pick = top.1.min(n - 1);
    let scroll = top.2.min(pick).min(n.saturating_sub(shown_rows(&st.prefs)));
    if let Some(t) = st.stack.last_mut() {
        (t.1, t.2) = (pick, scroll);
    }
}

/// Every row not at its default (what a profile saves), in menu order.
fn nondefault_rows(prefs: Prefs) -> Vec<(String, Item)> {
    all_rows(prefs)
        .into_iter()
        .filter(|f| {
            let (Some(v), Some(d)) = (state_of(&f.item, prefs), default_state(f.item.id)) else { return false };
            !same_state(f.item.id, &v, &d)
        })
        .map(|f| (f.addr, f.item))
        .collect()
}

/// Saved Commands: each saved address with its row as it is now; gone ones say so, and so do ones
/// hidden behind Enable Experimental Features. Lists first, as every page.
fn saved_rows(prefs: Prefs) -> Vec<(String, Item)> {
    let rows = all_rows(prefs);
    let mut v: Vec<(String, Item)> = saved_list()
        .into_iter()
        .enumerate()
        .map(|(i, a)| match rows.iter().find(|f| f.addr == a) {
            Some(f) => (a, f.item.clone()),
            None if hidden_experiment(&a, prefs) => (a.clone(), item(Id::Run(i), format!("{a} (hidden)"), Kind::Info, EXP_OFF)),
            None => (a.clone(), item(Id::Run(i), format!("{a} (gone)"), Kind::Info, "")),
        })
        .collect();
    v.sort_by_key(|e| !matches!(e.1.row.kind, Kind::Sub));
    v
}

/// Menu > Hotkey List: the menu's own actions first, then rows with hotkeys from their "O" menu.
/// Enter changes them.
fn key_list() -> Vec<Item> {
    let mut v = Vec::new();
    let keys = crate::keys::get();
    let help = "Click to change.";
    // Menu and Command Box always (their keys can be changed here); spots are rows below.
    let actions: Vec<Item> = BIND_ROWS
        .iter()
        .map(|r| item(Id::Bind(Bind::Action(r.1)), r.0, Kind::Value(key_name(keys.get(r.1))), help))
        .collect();
    if !actions.is_empty() {
        v.push(item(Id::Header, "Menu Actions", Kind::Header, ""));
        v.extend(actions);
    }
    let rows = row_keys();
    if !rows.is_empty() {
        v.push(item(Id::Header, "Commands", Kind::Header, ""));
        for (i, (addr, keys)) in rows.into_iter().enumerate() {
            let names = keys.iter().map(|k| key_name(*k)).collect::<Vec<_>>().join(", ");
            // Only the command's own name on the row; where it lives goes in the help line.
            let name = addr.rsplit(" > ").next().unwrap_or(&addr).to_string();
            let _ = addr;
            v.push(item(Id::KeyRow(i), name, Kind::Value(names), "Click to change."));
        }
    }
    if v.is_empty() {
        v.push(item(Id::Info, "No hotkeys yet", Kind::Info, ""));
    }
    v
}

/// A new page starts on its first row the cursor can be on.
fn first_pick(page: Page, prefs: Prefs) -> usize {
    items(page, prefs).iter().position(|i| !skipped(i)).unwrap_or(0)
}

/// Rows the cursor jumps over: titles, grayed-out rows (such as one opening a `HIDDEN` page), and
/// the Where Am I line.
fn skipped(i: &Item) -> bool {
    i.row.kind == Kind::Header || i.row.dim || matches!(i.id, Id::WhereAmI | Id::WarnText)
}

/// Where the list starts once the pick is on row `p` (`scroll`: where it started, `shown`: rows on
/// screen, `lead`: the first row the cursor can be on). The pick stays in view; on `lead` the list
/// goes back to its top, so the rows the cursor jumps over above it (Where Am I, a title) show
/// again: the cursor never goes up to them, and nothing else brought them back.
fn scroll_to_show(p: usize, scroll: usize, shown: usize, lead: usize) -> usize {
    if p == lead {
        return (p + 1).saturating_sub(shown);
    }
    if p < scroll {
        p
    } else if p >= scroll + shown {
        p + 1 - shown
    } else {
        scroll
    }
}

/// A key's name with Left / Right as L / R, for Fly's help: "L Shift".
fn short_key(vk: u32) -> String {
    let n = key_name(crate::fly::left_only(vk));
    if let Some(r) = n.strip_prefix("Left ") {
        format!("L {r}")
    } else if let Some(r) = n.strip_prefix("Right ") {
        format!("R {r}")
    } else {
        n
    }
}

/// When the unload question was asked.
static UNLOAD_ASKED: Mutex<Option<Instant>> = Mutex::new(None);
/// When the question before deleting a profile was asked.
static DELETE_ASKED: Mutex<Option<Instant>> = Mutex::new(None);

// ---- Profiles, as Stand's: `%APPDATA%\Aurora\Profiles`, one `<name>.txt` each in Stand's tree:
// the version line, then each list on its own line, what is inside one tab further in, rows as
// `Name: value`. Only rows not at their default are written; Load puts the missing ones back to it.
// ----

/// Profile names seen this session: a profile's page and rows keep their number while files come
/// and go.
static PROFILE_NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Profile lines that were not used, said once in the log.
static PROFILE_SKIPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn profile_id(name: &str) -> u32 {
    let mut v = PROFILE_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    match v.iter().position(|n| n == name) {
        Some(i) => i as u32,
        None => {
            v.push(name.to_string());
            (v.len() - 1) as u32
        }
    }
}

fn profile_name(n: u32) -> String {
    PROFILE_NAMES.lock().ok().and_then(|v| v.get(n as usize).cloned()).unwrap_or_default()
}

/// The profiles folder, made when missing.
fn profiles_dir() -> Option<PathBuf> {
    let d = folder()?.join("Profiles");
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

fn profile_path(name: &str) -> Option<PathBuf> {
    profiles_dir().map(|d| d.join(format!("{name}.txt")))
}

/// Every profile file's name, sorted. Only looks: a missing folder lists nothing and is not made.
fn profiles() -> Vec<String> {
    let Some(d) = folder().map(|f| f.join("Profiles")) else { return Vec::new() };
    let mut v: Vec<String> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if !p.is_file() || !p.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt")) {
                return None;
            }
            p.file_stem()?.to_str().map(String::from)
        })
        .collect();
    v.sort_by_key(|n| n.to_lowercase());
    v
}

/// Load On Inject and Automatically Save, as Stand keeps them: `%APPDATA%\Aurora\Meta State.txt`,
/// lines like `Load On Inject: Main` (one at most) and `Automatically Save: Main`.
const ON_INJECT: &str = "Load On Inject";
const AUTO_SAVE: &str = "Automatically Save";

fn meta_path() -> Option<PathBuf> {
    folder().map(|f| f.join("Meta State.txt"))
}

fn meta() -> Vec<(String, String)> {
    meta_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
        .trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|l| l.split_once(": ").map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect()
}

fn write_meta(v: &[(String, String)]) {
    use std::fmt::Write as _;
    let mut text = String::new();
    for (k, val) in v {
        let _ = writeln!(text, "{k}: {val}");
    }
    if let Some(p) = meta_path() {
        let _ = aurora_core::paths::write_file(&p, text);
    }
}

fn profile_flag(name: &str, key: &str) -> bool {
    meta().iter().any(|(k, v)| k == key && v == name)
}

fn set_profile_flag(name: &str, key: &str, on: bool) {
    let mut v = meta();
    // Only one profile loads on inject: this one takes it from the others.
    v.retain(|(k, n)| !(k == key && (n == name || (key == ON_INJECT && on))));
    if on {
        v.push((key.to_string(), name.to_string()));
    }
    write_meta(&v);
}

/// A row's state as a profile keeps it: On / Off, a choice as the row shows it, a number (two
/// decimals when it has a fraction, as Stand's). None for rows without one: actions, Credits and
/// Gear Score's rows (one-offs, the game keeps them), the Target tools, the profiles' own rows. The
/// menu's own settings and Teleport > Coordinates are states like any other since (the player's
/// plan, as Stand: every setting lives in profiles; before, the menu's were kept in menu.json and
/// X, Y, Z in settings.json, out of profiles).
fn state_of(it: &Item, prefs: Prefs) -> Option<String> {
    if let Some(v) = waiting_value(it.id) {
        return Some(v);
    }
    match (it.id, &it.row.kind) {
        (Id::Credits | Id::GearTotal | Id::GearSlot(_) | Id::GearOwned | Id::EspHex(_) | Id::ProfileOnInject(_) | Id::ProfileAuto(_), _) => None,
        (Id::MenuColour | Id::ToastPosition | Id::ToastColour | Id::BackColour | Id::SilentPriority, Kind::Choice(v)) => Some(v.clone()),
        (Id::EspColour(i), _) => Some(colour_state(esp_colour(&vkdraw::config().style, i))),
        (Id::EspBones, Kind::Choice(v)) => Some(v.clone()),
        (Id::FlySpeed, _) => Some(format!("{:.2}", fly::speed())),
        (Id::FlySprint, _) => Some(format!("{:.2}", fly::sprint())),
        (Id::HopHeight, _) => Some(format!("{:.2}", fly::hop_height())),
        (Id::Damage, _) => Some(number(f64::from(crate::weapon::damage()))),
        (Id::FireRate, _) => Some(number(f64::from(crate::weapon::fire_rate()))),
        (Id::EspRange, _) => Some(number(f64::from(vkdraw::config().style.range_m))),
        (id, _) if typed(id) => Some(current(id, prefs)),
        (_, Kind::Toggle(on)) => Some(on_off(*on)),
        _ => None,
    }
}

fn on_off(on: bool) -> String {
    if on { "On" } else { "Off" }.into()
}

fn number(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 { format!("{v:.0}") } else { format!("{v:.2}") }
}

/// A colour of the list by its name, any other by its code.
fn colour_state(c: [u8; 3]) -> String {
    let hex = box_colour_hex(c);
    if hex.is_empty() { box_colour_name(c) } else { hex }
}

/// What `default_of` puts a row back to, as `state_of` writes it.
fn default_state(id: Id) -> Option<String> {
    let d = aurora_core::proto::EspStyle::DEFAULT;
    Some(match id {
        Id::God | Id::Fly | Id::Unseen | Id::UnseenInbox | Id::FlyCalm | Id::FlyGuard | Id::FlyLetFall | Id::NvgZone | Id::NoRecoil | Id::NoShake | Id::NoNvgOverlay | Id::NoSway | Id::Stamina | Id::Accuracy | Id::Ammo | Id::Reserve | Id::SilentAim | Id::SilentMagic | Id::SilentNoRange | Id::SilentInstant | Id::SilentNoDrop | Id::EspOn | Id::QuietBox | Id::QuietKey | Id::AutoBox | Id::AutoKey | Id::MoveMouse | Id::MouseSupport | Id::Experimental => on_off(false),
        Id::SafeFly | Id::SilentCircle | Id::HopUp | Id::EspEnemies | Id::EspFriendlies | Id::EspPlayers | Id::EspDrones | Id::EspHideDead | Id::ForceRead | Id::SkipAccepted => on_off(true),
        Id::EspDistance => on_off(d.labels),
        Id::EspEdge => on_off(d.edge),
        Id::EspBones => if d.bones { "Bones" } else { "Box" }.into(),
        Id::EspColour(i) => colour_state(esp_colour(&d, i)),
        Id::MenuColour | Id::ToastPosition | Id::ToastColour | Id::BackColour => choice_names(id)[0].into(),
        Id::NotifyX => NOTIFY_XY_DEFAULT.0.to_string(),
        Id::NotifyY => NOTIFY_XY_DEFAULT.1.to_string(),
        Id::FlySpeed => format!("{:.2}", fly::SPEED_DEFAULT),
        Id::FlySprint => format!("{:.2}", fly::SPRINT_DEFAULT),
        Id::HopHeight => format!("{:.2}", fly::HOP_DEFAULT_M),
        Id::Damage | Id::FireRate => "0".into(),
        Id::GearValue | Id::Coord(_) => "0".into(),
        Id::EspRange => number(f64::from(d.range_m)),
        Id::SilentFov => number(crate::silentaim::FOV_DEFAULT),
        Id::SilentRange => number(crate::silentaim::RANGE_DEFAULT),
        Id::SilentPriority => crate::silentaim::Priority::Crosshair.name().into(),
        Id::EspLines => d.width.to_string(),
        // Where the menu always sat, in pixels on this screen.
        Id::PosX => default_pos(0).to_string(),
        Id::PosY => default_pos(1).to_string(),
        Id::Layout(i) => LAYOUT_ROWS[i].2.to_string(),
        _ => return None,
    })
}

/// Two states of a row the same? Numbers as numbers (8 and 8.00), the rest without case.
fn same_state(id: Id, a: &str, b: &str) -> bool {
    match (typed(id), a.parse::<f64>(), b.parse::<f64>()) {
        (true, Ok(x), Ok(y)) => (x - y).abs() < 0.005,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// The profile file: the rows not at their default, in Stand's tree, as `profile_state` keeps them.
fn profile_text(prefs: Prefs) -> String {
    let rows: Vec<(String, String)> = all_rows(prefs)
        .into_iter()
        .filter_map(|f| {
            let v = profile_state(&f, prefs)?;
            (!default_state(f.item.id).is_some_and(|d| same_state(f.item.id, &v, &d))).then_some((f.addr, v))
        })
        .collect();
    write_tree_v(&rows, PROFILE_VERSION)
}

/// A row's state as profiles keep it: nothing inside Aurora > Experiments (those rows always start
/// at their default; the switch itself is kept), and Dropped Item Gear Score only up to 1000
/// (`GEAR_NORMAL`). Save, Automatically Save and Save State all write this.
fn profile_state(f: &Found, prefs: Prefs) -> Option<String> {
    if in_experiments(&f.addr) {
        return None;
    }
    if f.item.id == Id::GearValue {
        return Some(gear_state(gear_normal()));
    }
    state_of(&f.item, prefs)
}

/// A profile's lines as Load takes them: none inside Aurora > Experiments (lines an older build
/// saved there are left out, not counted as skipped), and how many were left out.
fn loadable(lines: Vec<(String, String)>) -> (Vec<(String, String)>, usize) {
    let all = lines.len();
    let kept: Vec<(String, String)> = lines.into_iter().filter(|(a, _)| !in_experiments(a)).collect();
    let left = all - kept.len();
    (kept, left)
}

/// A profile line's value as it loads: Dropped Item Gear Score 1000 at most, whatever the switch
/// (profiles never keep more; this guards a hand-edited one).
fn profile_value(id: Id, value: String) -> String {
    match (id, value.trim().parse::<f64>()) {
        (Id::GearValue, Ok(v)) if v > f64::from(gearscore::MAX) => gearscore::MAX.to_string(),
        _ => value,
    }
}

/// Addresses and values in Stand's tree, for the profiles and Hotkeys.txt: the version line, each
/// list on its own line, what is inside one tab further in, rows as `Name: value`.
fn write_tree(rows: &[(String, String)]) -> String {
    write_tree_v(rows, 1)
}

/// `write_tree` with its version line; profiles are version 2 since (see `upgrade_profile`).
fn write_tree_v(rows: &[(String, String)], version: u32) -> String {
    use std::fmt::Write as _;
    let mut out = format!("Tree Compatibility Version: {version}\n");
    // The lists written so far down to the last row.
    let mut open: Vec<String> = Vec::new();
    // Rows in menu order (lists first) have each list's rows follow it; rows in another order write
    // their lists again.
    for (addr, v) in rows {
        let mut path: Vec<&str> = addr.split(" > ").collect();
        let Some(name) = path.pop() else { continue };
        let same = open.iter().zip(&path).take_while(|(a, b)| a == *b).count();
        open.truncate(same);
        for (depth, list) in path.iter().enumerate().skip(same) {
            let _ = writeln!(out, "{}{list}", "\t".repeat(depth));
            open.push((*list).to_string());
        }
        let _ = writeln!(out, "{}{name}: {v}", "\t".repeat(path.len()));
    }
    out
}

/// A profile file back into addresses and values ("World > ESP > Range", "250"). Old flat lines
/// (`World > ESP > Range: 250`) read the same.
fn read_tree(text: &str) -> Vec<(String, String)> {
    // Files pasted in by hand may start with a byte-order mark, or have spaces where tabs were: 4
    // spaces count as one tab.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    let mut path: Vec<String> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || (n == 0 && line.starts_with("Tree Compatibility Version")) {
            continue;
        }
        let indent: String = raw.chars().take_while(|c| *c == '\t' || *c == ' ').collect();
        let depth = indent.matches('\t').count() + indent.matches(' ').count() / 4;
        path.truncate(depth);
        match line.rsplit_once(": ") {
            Some((name, value)) => {
                let addr = path.iter().map(String::as_str).chain(std::iter::once(name.trim())).collect::<Vec<_>>().join(" > ");
                out.push((addr, value.trim().to_string()));
            }
            None => path.push(line.to_string()),
        }
    }
    out
}

fn save_profile(st: &mut State, name: &str) -> Result<(), String> {
    let path = profile_path(name).ok_or("No profiles folder")?;
    let text = profile_text(st.prefs);
    aurora_core::paths::write_file(&path, &text).map_err(|e| format!("Profile {name} not saved: {e}"))?;
    st.profile = Some(name.to_string());
    st.profile_text = text;
    // Own now: the Load On Inject one must not come over it later.
    st.inject_load = None;
    Ok(())
}

/// Save, from the profile's page or `save<name>`, and say so.
fn save_said(st: &mut State, name: &str) {
    match save_profile(st, name) {
        Ok(()) => confirm(st, format!("Profile {name} saved")),
        Err(e) => note(st, e),
    }
}

/// Load, from the profile's page or `load<name>`, and say so.
fn load_said(st: &mut State, name: &str) {
    match load_profile(st, name) {
        Ok(0) => confirm(st, format!("Profile {name} loaded")),
        Ok(k) => note(st, format!("Profile {name} loaded ({k} lines skipped, see dll.log)")),
        Err(e) => note(st, e),
    }
}

/// Create Profile: how everything is set now, under a new name.
fn new_profile(st: &mut State, name: &str) {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.ends_with('.') || name.chars().any(|c| c.is_control() || "<>:\"/\\|?*".contains(c)) {
        note(st, "Use a name of up to 40 characters, without < > : \" / \\ | ? *");
        return;
    }
    // Windows keeps these names for devices (NUL.txt is no file), also before a dot.
    let stem = name.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    let b = stem.as_bytes();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (b.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && (b'1'..=b'9').contains(&b[3]));
    if device {
        note(st, format!("Windows keeps the name {name} for itself; pick another"));
        return;
    }
    if profiles().iter().any(|p| p.eq_ignore_ascii_case(name)) {
        note(st, format!("A profile called {name} already exists"));
        return;
    }
    match save_profile(st, name) {
        Ok(()) => note(st, format!("Profile {name} created")),
        Err(e) => note(st, e),
    }
}

/// Load a profile: every row it has, the rest to their default, so the player gets exactly what was
/// saved; through the same steps as the player's own changes, so the hooks switch properly. Fly
/// comes last, so Safe Fly and god mode are set before it starts. Lines it does not know (or rows
/// that moved) are skipped and said once in the log. Gives how many were skipped.
fn load_profile(st: &mut State, name: &str) -> Result<usize, String> {
    // Lossy: a profile saved by another editor in another encoding still loads its rows (names are
    // plain ASCII).
    let text = profile_path(name)
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .ok_or_else(|| format!("Profile {name} could not be read"))?;
    // Rows that moved or were renamed still load from an older profile. Fly's old m/s lines become
    // the new multipliers (see `upgrade_profile`). Position in percent and Disable Warnings become
    // today's rows (`upgrade_rows`).
    let raw = profile_rows(&text);
    let old_gear_on = raw.iter().any(|(a, v)| a == "Self > Progress > Gear Score > Gear Score" && v.eq_ignore_ascii_case("on"));
    let wanted: Vec<(String, String)> = raw
        .into_iter()
        .filter(|(a, _)| old_gear_on || a != "Self > Progress > Gear Score > Score")
        .map(|(a, v)| (moved(&a).unwrap_or(a), v))
        .collect();
    let (wanted, left) = loadable(wanted);
    if left > 0 {
        log(&format!("profile {name}: {left} Experiments line(s) left out: Experiments always start at their default"));
    }
    // Back to where the command came from after, so rows on the same hotkey after Load still count
    // as a hotkey.
    let from = FROM.swap(FROM_PROFILE, Ordering::Relaxed);
    // Enable Experimental Features first: it decides whether the Experiments rows are there to
    // load.
    set_experimental(st, wants_experimental(&wanted));
    // Every row inside Experiments to its default, also with the switch off: it going off leaves
    // the values.
    experiments_default(st, EXPERIMENTS_ADDR, true);
    let rows: Vec<Found> = all_rows(st.prefs).into_iter().filter(|f| state_of(&f.item, st.prefs).is_some()).collect();
    let skip = |line: String| {
        let said = format!("{name}: {line}");
        let mut v = PROFILE_SKIPPED.lock().unwrap_or_else(|e| e.into_inner());
        if !v.contains(&said) {
            log(&format!("profile {name}: line not used: {line}"));
            v.push(said);
        }
    };
    // A new profile's lines take the place of any still waiting.
    waiting().clear();
    let mut skipped = 0;
    for (addr, value) in &wanted {
        if !rows.iter().any(|f| f.addr == *addr) {
            skipped += 1;
            skip(format!("{addr}: {value}"));
        }
    }
    let (fly, rest): (Vec<Found>, Vec<Found>) = rows.into_iter().partition(|f| f.item.id == Id::Fly);
    for f in rest.iter().chain(&fly) {
        let Some(value) = wanted.iter().find(|w| w.0 == f.addr).map(|w| profile_value(f.item.id, w.1.clone())).or_else(|| default_state(f.item.id)) else { continue };
        // Only what turns something on waits; turning off works anywhere.
        if needs_world(f.item.id) && !world_ready() && turns_on(f.item.id, &value) {
            if state_of(&f.item, st.prefs).is_some_and(|now| !same_state(f.item.id, &now, &value)) {
                waiting().push((f.clone(), value));
            }
            continue;
        }
        if !apply_state(st, f, &value) {
            skipped += 1;
            skip(format!("{}: {value}", f.addr));
        }
    }
    FROM.store(from, Ordering::Relaxed);
    st.profile = Some(name.to_string());
    st.profile_text = profile_text(st.prefs);
    st.inject_load = None;
    log(&format!("profile {name} loaded, {skipped} lines skipped, {} waiting for the world", waiting().len()));
    Ok(skipped)
}

// ---- Save State / Load State: one row, or every row in a list, into or from the active profile;
// no other line of the profile changes. ----

/// The rows a Save State / Load State is about: the row itself, or for a list every row inside it
/// (and further in) that has a state, as Apply Default State To Children.
fn state_rows(rows: &[Found], addr: &str, sub: bool, prefs: Prefs) -> Vec<Found> {
    let inside = format!("{addr} > ");
    rows.iter().filter(|f| (if sub { f.addr.starts_with(&inside) } else { f.addr == addr }) && state_of(&f.item, prefs).is_some()).cloned().collect()
}

/// The profile they use: the one last loaded, saved or created, else the Load On Inject one.
fn active_profile(st: &State) -> Option<String> {
    st.profile.clone().or_else(|| profiles().into_iter().find(|n| profile_flag(n, ON_INJECT)))
}

/// A profile's text with these lines set: replaced where the row is (also at an address it had
/// before a move), added where it is not. Always written, also at the default, so Load State finds
/// it again. Every other line keeps its value.
fn set_lines(text: &str, lines: &[(String, String)]) -> String {
    let mut rows = profile_rows(text);
    for (addr, value) in lines {
        let same = |r: &(String, String)| r.0 == *addr || moved(&r.0).as_deref() == Some(addr.as_str());
        match rows.iter().position(same) {
            Some(i) => {
                rows[i] = (addr.clone(), value.clone());
                // The same row at an old address too: dropped, so it cannot win on Load.
                let mut k = 0;
                rows.retain(|r| {
                    let keep = k <= i || !same(r);
                    k += 1;
                    keep
                });
            }
            None => rows.push((addr.clone(), value.clone())),
        }
    }
    write_tree_v(&rows, PROFILE_VERSION)
}

/// The value a profile has for a row (at its address now, or one it had before a move).
fn line_of(text: &str, addr: &str) -> Option<String> {
    profile_rows(text).into_iter().find(|(a, _)| a == addr || moved(a).as_deref() == Some(addr)).map(|r| r.1)
}

fn save_state(st: &mut State) {
    let (addr, sub) = ctx();
    let Some(name) = active_profile(st) else {
        note(st, "No active profile: create or load one in Aurora > Profiles");
        return;
    };
    let rows = state_rows(&all_rows(st.prefs), &addr, sub, st.prefs);
    // As Save: never Experiments rows, the gear score 1000 at most (`profile_state`).
    let lines: Vec<(String, String)> = rows.iter().filter_map(|f| Some((f.addr.clone(), profile_state(f, st.prefs)?))).collect();
    if lines.is_empty() {
        note(st, if rows.iter().any(|f| in_experiments(&f.addr)) { EXP_UNSAVED } else { "Nothing to save here" });
        return;
    }
    // A profile deleted outside the menu is not made again.
    let Some(path) = profile_path(&name).filter(|p| p.is_file()) else {
        note(st, format!("Profile {name} is gone"));
        return;
    };
    // Never written over when it could not be read: it would lose the player's other lines.
    let Ok(text) = read_text(&path) else {
        note(st, format!("Profile {name} could not be read: not saved"));
        return;
    };
    match aurora_core::paths::write_file(&path, set_lines(&text.unwrap_or_default(), &lines)) {
        Ok(()) => confirm(st, format!("State saved to {name}")),
        Err(e) => note(st, format!("Profile {name} not saved: {e}")),
    }
}

fn load_state(st: &mut State) {
    let (addr, sub) = ctx();
    let Some(name) = active_profile(st) else {
        note(st, "No active profile: create or load one in Aurora > Profiles");
        return;
    };
    let Some(text) = profile_path(&name).and_then(|p| std::fs::read(p).ok()).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        note(st, format!("Profile {name} could not be read"));
        return;
    };
    // A row with no line is at its default, as Load reads it: a normal Save leaves default rows
    // out.
    let mut found: Vec<(Found, String)> = state_rows(&all_rows(st.prefs), &addr, sub, st.prefs)
        .into_iter()
        .filter(|f| !in_experiments(&f.addr))
        .filter_map(|f| line_of(&text, &f.addr).map(|v| profile_value(f.item.id, v)).or_else(|| default_state(f.item.id)).map(|v| (f, v)))
        .collect();
    // Experiments rows are never in a profile: their default, as Load, the hidden ones too, as a
    // line here may turn the switch on.
    let exp = experiment_ids(st.prefs, &addr, sub);
    if found.is_empty() && exp.is_empty() {
        note(st, format!("Profile {name} has no state for this"));
        return;
    }
    // Fly last, as Load, so Safe Fly is set before it starts.
    found.sort_by_key(|(f, _)| f.item.id == Id::Fly);
    let from = FROM.swap(FROM_PROFILE, Ordering::Relaxed);
    let mut skipped = 0;
    for (f, value) in &found {
        // As Load: only what turns something on waits for the world.
        if needs_world(f.item.id) && !world_ready() && turns_on(f.item.id, value) {
            unwait(f.item.id);
            if state_of(&f.item, st.prefs).is_some_and(|now| !same_state(f.item.id, &now, value)) {
                waiting().push((f.clone(), value.clone()));
            }
            continue;
        }
        if !apply_state(st, f, value) {
            skipped += 1;
            log(&format!("profile {name}: load state, line not used: {}: {value}", f.addr));
        }
    }
    for id in exp {
        default_of(st, id);
    }
    FROM.store(from, Ordering::Relaxed);
    if skipped == 0 {
        confirm(st, format!("State loaded from {name}"));
    } else {
        note(st, format!("State loaded from {name} ({skipped} lines skipped, see dll.log)"));
    }
}

// ---- Fly's units in profiles. ----

/// Profiles are written as version 4: X is the wall, the menu's right edge. Version
/// 3: Sprint is its own speed and X the address bar's left edge (it was the picture's). Version 2
/// had Fly's multipliers; version 1 (or no version line) the old m/s values.
const PROFILE_VERSION: u32 = 4;
const FLY_SPEED_ADDR: &str = "Self > Movement > Fly > Fly Speed";
const FLY_SPRINT_ADDR: &str = "Self > Movement > Fly > Sprint";

/// The number on a tree file's version line; 1 when it has none.
fn tree_version(text: &str) -> u32 {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.lines().next().and_then(|l| l.trim().strip_prefix("Tree Compatibility Version:")).and_then(|v| v.trim().parse().ok()).unwrap_or(1)
}

/// Old Fly values as the new multipliers, so the player flies as fast as before: Speed s m/s is s /
/// 8; Sprint n was n x 4 m/s, so it is n x 4 / (new Speed x 8), with Speed 1.00 when there was no
/// Speed line. Rounded down to two decimals, so never faster than before. No Sprint line gives
/// None: the default 4.00 then, which is the 4x of every build before Sprint got its own row.
fn upgrade_fly(speed: Option<f64>, sprint: Option<f64>) -> (Option<f64>, Option<f64>) {
    // The tiny nudge keeps 1.60 from reading as 1.59 (float steps).
    let down = |x: f64| ((x * 100.0 + 1e-9).floor() / 100.0).clamp(fly::MIN, fly::MAX);
    let speed = speed.filter(|s| s.is_finite()).map(|s| down(s.clamp(0.5, 200.0) / fly::BASE));
    let base = speed.unwrap_or(fly::SPEED_DEFAULT) * fly::BASE;
    (speed, sprint.filter(|n| n.is_finite()).map(|n| down(n.clamp(1.0, 200.0) * 4.0 / base)))
}

/// An older profile's text as today's version, one step at a time (so a version 1 profile ends
/// where the same profile upgraded to 2 and later to 3 does). Every other line stays exactly as it
/// was. Today's version comes back as it is. `screen`: this screen's size, for X (`old_x_shift`,
/// `old_x_to_wall`).
fn upgrade_profile(text: &str, screen: (i32, i32)) -> String {
    let v = tree_version(text);
    if v >= PROFILE_VERSION {
        return text.to_string();
    }
    let two = if v < 2 { upgrade_v1(text) } else { text.to_string() };
    let three = if v < 3 { upgrade_v2(&two, screen.1) } else { two };
    upgrade_v3(&three, screen)
}

/// Version 3 as version 4: X was the address bar's left edge, it is the wall now, so it gets the
/// bar-to-wall width at the profile's own Menu Size (100 when it has none): the menu stays where it
/// was. Only lines named X at Position's address change.
fn upgrade_v3(text: &str, screen: (i32, i32)) -> String {
    let size = profile_size(&read_tree(text)).unwrap_or(100);
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = format!("Tree Compatibility Version: {PROFILE_VERSION}\n");
    let mut path: Vec<String> = Vec::new();
    for (n, raw) in body.split_inclusive('\n').enumerate() {
        let line = raw.trim();
        if n == 0 && line.starts_with("Tree Compatibility Version") {
            continue;
        }
        if !line.is_empty() {
            let indent: String = raw.chars().take_while(|c| *c == '\t' || *c == ' ').collect();
            let depth = indent.matches('\t').count() + indent.matches(' ').count() / 4;
            path.truncate(depth);
            match line.rsplit_once(": ") {
                Some((name, value)) => {
                    let name = name.trim();
                    let addr = path.iter().map(String::as_str).chain(std::iter::once(name)).collect::<Vec<_>>().join(" > ");
                    let addr = moved(&addr).unwrap_or(addr);
                    if addr == POS_X_ADDR
                        && name == "X"
                        && let Some(x) = value.trim().parse::<f64>().ok().filter(|x| x.is_finite())
                    {
                        let wall = old_x_to_wall(x.round().clamp(-32768.0, 32767.0) as i32, size, screen);
                        let eol = &raw[raw.trim_end_matches(['\r', '\n']).len()..];
                        out.push_str(&format!("{indent}{name}: {wall}{eol}"));
                        continue;
                    }
                }
                None => path.push(line.to_string()),
            }
        }
        out.push_str(raw);
    }
    out
}

/// A Sprint value as stored: two decimals, 0.01 to 10000.
fn fly_value(x: f64) -> f64 {
    ((x * 100.0).round() / 100.0).clamp(fly::MIN, fly::MAX)
}

/// Version 2 as version 3: Sprint is Speed x Sprint, so the player sprints as fast as before (no
/// Sprint line was 4.00: it gets one, Speed x 4, when Speed is not 1); X gets the help boxes' room
/// it had left of the bar (`profile_x_shift`), only on lines named X.
fn upgrade_v2(text: &str, screen_h: i32) -> String {
    let rows = read_tree(text);
    let get = |addr: &str| {
        rows.iter().find(|(a, _)| moved(a).as_deref().unwrap_or(a) == addr).and_then(|(_, v)| v.parse::<f64>().ok()).filter(|v| v.is_finite())
    };
    let speed = get(FLY_SPEED_ADDR);
    let sprint = get(FLY_SPRINT_ADDR).map(|n| fly_value(speed.unwrap_or(fly::SPEED_DEFAULT) * n));
    let shift = profile_x_shift(&rows, screen_h);
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    // Version 3 (`upgrade_v3` takes it on to today's).
    let mut out = "Tree Compatibility Version: 3\n".to_string();
    let mut path: Vec<String> = Vec::new();
    for (n, raw) in body.split_inclusive('\n').enumerate() {
        let line = raw.trim();
        if n == 0 && line.starts_with("Tree Compatibility Version") {
            continue;
        }
        if !line.is_empty() {
            let indent: String = raw.chars().take_while(|c| *c == '\t' || *c == ' ').collect();
            let depth = indent.matches('\t').count() + indent.matches(' ').count() / 4;
            path.truncate(depth);
            match line.rsplit_once(": ") {
                Some((name, value)) => {
                    let name = name.trim();
                    let addr = path.iter().map(String::as_str).chain(std::iter::once(name)).collect::<Vec<_>>().join(" > ");
                    let addr = moved(&addr).unwrap_or(addr);
                    let new = if addr == FLY_SPRINT_ADDR {
                        sprint.map(|s| format!("{s:.2}"))
                    } else if addr == POS_X_ADDR && name == "X" {
                        value.trim().parse::<f64>().ok().filter(|x| x.is_finite()).map(|x| (x.round() as i64 + i64::from(shift)).clamp(-32768, 32767).to_string())
                    } else {
                        None
                    };
                    if let Some(v) = new {
                        let eol = &raw[raw.trim_end_matches(['\r', '\n']).len()..];
                        out.push_str(&format!("{indent}{name}: {v}{eol}"));
                        continue;
                    }
                }
                None => path.push(line.to_string()),
            }
        }
        out.push_str(raw);
    }
    match speed {
        Some(s) if sprint.is_none() && fly_value(s * fly::SPRINT_DEFAULT) != fly::SPRINT_DEFAULT => {
            add_missing(&out, &[(FLY_SPRINT_ADDR.to_string(), format!("{:.2}", fly_value(s * fly::SPRINT_DEFAULT)))]).0
        }
        _ => out,
    }
}

/// Version 1 as version 2: the version line, and Fly's Speed and Sprint lines in the multipliers of
/// (`upgrade_fly`).
fn upgrade_v1(text: &str) -> String {
    let rows = read_tree(text);
    let get = |addr: &str| {
        rows.iter().find(|(a, _)| moved(a).as_deref().unwrap_or(a) == addr).and_then(|(_, v)| v.parse::<f64>().ok())
    };
    let (speed, sprint) = upgrade_fly(get(FLY_SPEED_ADDR), get(FLY_SPRINT_ADDR));
    let body = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = "Tree Compatibility Version: 2\n".to_string();
    // The same walk as `read_tree`.
    let mut path: Vec<String> = Vec::new();
    for (n, raw) in body.split_inclusive('\n').enumerate() {
        let line = raw.trim();
        if n == 0 && line.starts_with("Tree Compatibility Version") {
            continue;
        }
        if !line.is_empty() {
            let indent: String = raw.chars().take_while(|c| *c == '\t' || *c == ' ').collect();
            let depth = indent.matches('\t').count() + indent.matches(' ').count() / 4;
            path.truncate(depth);
            match line.rsplit_once(": ") {
                Some((name, _)) => {
                    let addr = path.iter().map(String::as_str).chain(std::iter::once(name.trim())).collect::<Vec<_>>().join(" > ");
                    let addr = moved(&addr).unwrap_or(addr);
                    let new = match addr.as_str() {
                        FLY_SPEED_ADDR => speed,
                        FLY_SPRINT_ADDR => sprint,
                        _ => None,
                    };
                    if let Some(v) = new {
                        let eol = &raw[raw.trim_end_matches(['\r', '\n']).len()..];
                        out.push_str(&format!("{indent}{}: {v:.2}{eol}", name.trim()));
                        continue;
                    }
                }
                None => path.push(line.to_string()),
            }
        }
        out.push_str(raw);
    }
    out
}

fn waiting() -> std::sync::MutexGuard<'static, Vec<(Found, String)>> {
    WAITING.lock().unwrap_or_else(|e| e.into_inner())
}

/// The profile line a row waits with, if it does.
fn waiting_value(id: Id) -> Option<String> {
    waiting().iter().find(|w| w.0.item.id == id).map(|w| w.1.clone())
}

/// A new value for a row that still waits for the world (Damage): it waits with that one; 0 takes
/// it out of the wait (off). False when the row is not waiting, or the world is ready.
fn requeue(st: &mut State, id: Id, v: f64) -> bool {
    if world_ready() || waiting_value(id).is_none() {
        return false;
    }
    let max = if id == Id::FireRate { FIRE_MAX } else { DAMAGE_MAX };
    let v = f64::from(times_value(v, max));
    if v == 0.0 {
        unwait(id);
        done(st, if id == Id::FireRate { "Fire rate stays off" } else { "Damage multiplier stays off" });
    } else if let Some(w) = waiting().iter_mut().find(|w| w.0.item.id == id) {
        w.1 = number(v);
    }
    true
}

/// No longer waiting; true when it was.
fn unwait(id: Id) -> bool {
    let mut w = waiting();
    let n = w.len();
    w.retain(|x| x.0.item.id != id);
    w.len() != n
}

/// Switches that work on the player's character or hook the game's code: not before the player is
/// in the world and the start-up code searches are done (the launcher's writes wait for the same).
fn needs_world(id: Id) -> bool {
    matches!(id, Id::God | Id::Fly | Id::Unseen | Id::UnseenInbox | Id::NvgZone | Id::NoRecoil | Id::NoShake | Id::NoNvgOverlay | Id::NoSway | Id::Stamina | Id::Accuracy | Id::Ammo | Id::Reserve | Id::SilentAim | Id::SilentNoDrop | Id::Damage | Id::FireRate)
}

fn world_ready() -> bool {
    fly::body_addr().is_some() && crate::WARMED.load(Ordering::Relaxed)
}

/// A profile line that turns its switch on (Damage: any value but 0).
fn turns_on(id: Id, value: &str) -> bool {
    match id {
        Id::Damage | Id::FireRate => value.parse::<f64>().is_ok_and(|v| v != 0.0),
        _ => value.eq_ignore_ascii_case("on"),
    }
}

/// What a waiting row waits for.
fn wait_text() -> &'static str {
    if fly::body_addr().is_none() { "Enables once your character loads." } else { "Enables once Aurora has started." }
}

/// Once the player is in the world: the waiting lines, in the profile's order (fly last).
fn apply_waiting(st: &mut State) {
    if waiting().is_empty() || !world_ready() {
        return;
    }
    let list = std::mem::take(&mut *waiting());
    let from = FROM.swap(FROM_PROFILE, Ordering::Relaxed);
    for (f, value) in &list {
        if !apply_state(st, f, value) {
            log(&format!("profile: line not used: {}: {value}", f.addr));
        }
    }
    FROM.store(from, Ordering::Relaxed);
    log(&format!("profile: {} waiting lines applied in the world", list.len()));
}

/// One profile line onto its row. False when the row has no state or the value is not one it takes.
fn apply_state(st: &mut State, f: &Found, value: &str) -> bool {
    // The row as it is now: an earlier line may have changed it (fly can turn god mode on).
    let Some(&(page, i, _)) = f.stack.last() else { return false };
    let Some(it) = items(page, st.prefs).into_iter().nth(i).filter(|it| it.id == f.item.id) else { return false };
    let Some(now) = state_of(&it, st.prefs) else { return false };
    if same_state(it.id, &now, value) {
        return true;
    }
    match it.row.kind {
        Kind::Toggle(_) if value.eq_ignore_ascii_case("on") || value.eq_ignore_ascii_case("off") => {
            activate(st, &it);
            true
        }
        Kind::Toggle(_) => false,
        // Choices and typed values as if typed after their command.
        _ => {
            run_found(st, Found { item: it, ..f.clone() }, Some(value));
            true
        }
    }
}

/// Delete (after its question, unless Warnings lets it go ahead): the file goes, and its pages
/// close.
fn delete_profile(st: &mut State, n: u32) {
    let name = profile_name(n);
    match profile_path(&name).map(std::fs::remove_file) {
        Some(Ok(())) => {
            let mut v = meta();
            let n = v.len();
            v.retain(|(_, p)| *p != name);
            if v.len() != n {
                write_meta(&v);
            }
            if st.profile.as_deref() == Some(name.as_str()) {
                st.profile = None;
            }
            confirm(st, format!("Profile {name} deleted"));
        }
        _ => note(st, format!("Profile {name} could not be deleted")),
    }
    let gone = |p: Page| matches!(p, Page::Profile(x) | Page::ProfileDelete(x) if x == n);
    for stack in std::iter::once(&mut st.stack).chain(st.saved.iter_mut()) {
        if let Some(i) = stack.iter().position(|x| gone(x.0)) {
            stack.truncate(i.max(1));
        }
    }
}

/// Once a second: the Load On Inject profile once the player is in the world, and Automatically
/// Save for the profile last loaded, saved or created.
fn profile_tick(st: &mut State, checked: &mut Instant) {
    if checked.elapsed() < Duration::from_secs(1) {
        return;
    }
    *checked = Instant::now();
    apply_waiting(st);
    // At inject; what needs the world waits for it (see `WAITING`).
    if let Some(name) = st.inject_load.clone() {
        // Not before the screen size is known: an old profile's X is moved by the help boxes' room
        // at this screen's height, and the 1080p stand-in would move it wrong. After 30 s it loads
        // anyway, so it is never stuck.
        static WAIT: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        if vkdraw::screen().is_none() && WAIT.get_or_init(Instant::now).elapsed() < Duration::from_secs(30) {
            return;
        }
        match load_profile(st, &name) {
            Ok(_) => note(st, format!("Profile {name} loaded")),
            Err(e) => {
                st.inject_load = None;
                note(st, e);
            }
        }
        return;
    }
    let Some(name) = st.profile.clone() else { return };
    if !profile_flag(&name, AUTO_SAVE) {
        return;
    }
    // A profile deleted outside the menu is not made again.
    let Some(path) = profile_path(&name).filter(|p| p.is_file()) else {
        st.profile = None;
        return;
    };
    let text = profile_text(st.prefs);
    if text != st.profile_text {
        if let Err(e) = aurora_core::paths::write_file(&path, &text) {
            log(&format!("profile {name}: not saved: {e}"));
        }
        st.profile_text = text;
    }
}

/// Take the mod out of the game. Only the mod unloads itself: it shuts every part down, then frees
/// its own DLL. Unload Aurora (menu, `unload.request`): the menu closes now, the unload starts in
/// `unload_tick` once the keys are let go.
fn unload_now(st: &mut State) {
    // Where it came from and the last menu keys, so an unload the player did not ask for can be
    // traced.
    let from = match FROM.load(Ordering::Relaxed) {
        FROM_BOX => "the command box or command.request",
        FROM_KEY => "a hotkey",
        FROM_PROFILE => "a profile",
        _ => "the menu or unload.request",
    };
    let keys: Vec<String> = st.keys_seen.iter().map(|(t, k)| format!("{k} {} ms ago", t.elapsed().as_millis())).collect();
    log(&format!(
        "unload asked from {from}; menu open {} ms; last menu keys: {}",
        st.opened_at.elapsed().as_millis(),
        if keys.is_empty() { "none".to_string() } else { keys.join(", ") }
    ));
    // DirectX 11: not while another program hooked on top of ours.
    if let Err(e) = vkdraw::can_unload() {
        log(&e);
        note(st, e);
        return;
    }
    // The unload starts in `unload_tick` once the keys that picked it are let go.
    if st.unload_at.is_none() {
        st.unload_at = Some(Instant::now());
        confirm(st, "Unloading");
    }
    st.open = false;
}

/// Starts a picked unload once no menu key (or picked key, or mouse button) is down any more, or
/// after 3 s.
fn unload_tick(st: &mut State) {
    let Some(at) = st.unload_at else { return };
    if (st.nav_hold || st.bind_hold.is_some() || st.mouse_hold) && at.elapsed() < Duration::from_secs(3) {
        return;
    }
    st.unload_at = None;
    // DirectX 11 again: up to 3 s went by since the check in `unload_now`.
    if let Err(e) = vkdraw::can_unload() {
        log(&e);
        note(st, e);
        return;
    }
    if let Err(e) = crate::server::unload_self() {
        note(st, e);
    }
}

/// When the credits warning was shown; it counts as read 2 s later (at once with Force Me To Read
/// Warnings off).
static RISK_SHOWN: Mutex<Option<Instant>> = Mutex::new(None);

fn risk_read(force_read: bool) -> bool {
    yes_shows(force_read, asked_ago(&RISK_SHOWN))
}

/// The credits warning before Credits, Pick Other Points or their Min / Max. True when its page
/// opened, so the action waits; false when it may go ahead. Read once, it counts as accepted; with
/// Skip Already-Accepted Warnings off it is asked again next time.
fn ask_risk(st: &mut State) -> bool {
    if warn_skipped(&st.prefs, FROM.load(Ordering::Relaxed), accepted(Warn::Credits)) {
        return false;
    }
    if risk_read(st.prefs.force_read) {
        accept(Warn::Credits);
        // Read once, it goes ahead; the old time is cleared every time, so turning Skip
        // Already-Accepted Warnings off asks again next time.
        if let Ok(mut t) = RISK_SHOWN.lock() {
            *t = None;
        }
        return false;
    }
    if let Ok(mut t) = RISK_SHOWN.lock() {
        *t = Some(Instant::now());
    }
    ask(st, Page::Risk);
    true
}

/// A Credits amount typed somewhere other than the box a click on the row opened
/// (that click asked already): the command box, command.request, or the box a hotkey or Saved
/// Commands opened.
fn typed_credits(arg: Option<&str>, from: u8) -> bool {
    from != FROM_MENU && arg.and_then(|a| a.parse::<f64>().ok()).is_some_and(f64::is_finite)
}

/// Pick Other Points' 20 s watch.
fn watch_others(st: &mut State) {
    if credits::start_pick_other() {
        note(st, "Open a shop now; the amounts show here after 20 s");
    } else {
        note(st, "Still watching, open a shop");
    }
}

fn credits_text() -> String {
    match credits::balance() {
        Some(c) => c.to_string(),
        None if credits::catching() => "open a shop".into(),
        None if !credits::found().is_empty() => "pick one".into(),
        None => "-".into(),
    }
}

// ---- The old settings.json and menu.json: like Stand, every setting lives in profiles now. At the
// first inject of this build the player's values in those two files become lines of the Load On
// Inject profile, then the files move into Dumper\old. Nothing is deleted. ----

/// The player's values in the old files, as rows and the values a profile keeps for them; only the
/// ones not at their default (a profile keeps no others). Not carried: ESP's switch and the gear
/// score hold (with its value). Old builds switched both off at every inject, so the file only said
/// how the last game ended; carrying "On" would switch them on at every inject, which never
/// happened before. The two ESP colours without a row (other NPCs, the target) were never settable:
/// fixed, not settings. The menu's x and y were percent of the screen; they are pixels on `screen`
/// now, and its no_warnings is both Automatically Proceed switches and Skip Already-Accepted.
fn old_values(settings: Option<&serde_json::Value>, menu: Option<&serde_json::Value>, screen: (i32, i32)) -> Vec<(Id, String)> {
    let mut v: Vec<(Id, String)> = Vec::new();
    if let Some(m) = menu {
        let u = |k: &str| m.get(k).and_then(|x| x.as_u64());
        if let Some(a) = u("accent") {
            v.push((Id::MenuColour, ACCENTS[a as usize % ACCENTS.len()].0.into()));
        }
        // Its size as List Width / Height that look the same.
        let size = u("size").map_or(100, |s| s.clamp(60, 200) as u32);
        if u("size").is_some() {
            let l = list_for_size(size);
            v.push((Id::Layout(LIST_W), l[0].to_string()));
            v.push((Id::Layout(LIST_H), l[1].to_string()));
        }
        // X was the picture's left edge: plus the help boxes' room and the bar to the wall.
        let tabs = u("tabswidth").map_or(LAYOUT_ROWS[TABS_WIDTH].2, |t| t.clamp(u64::from(LAYOUT_ROWS[TABS_WIDTH].3), u64::from(LAYOUT_ROWS[TABS_WIDTH].4)) as u32);
        let shift = old_x_shift(size, tabs, screen.1);
        if let Some(p) = m.get("x").and_then(|x| x.as_f64()).filter(|p| p.is_finite()) {
            v.push((Id::PosX, old_x_to_wall(pct_px(p, screen.0) + shift, size, screen).to_string()));
        }
        if let Some(p) = m.get("y").and_then(|x| x.as_f64()).filter(|p| p.is_finite()) {
            v.push((Id::PosY, pct_px(p, screen.1).to_string()));
        }
        if m.get("no_warnings").and_then(|x| x.as_bool()) == Some(true) {
            for id in [Id::AutoBox, Id::AutoKey, Id::SkipAccepted] {
                v.push((id, on_off(true)));
            }
        }
        for (id, key) in [(Id::QuietBox, "quiet_box"), (Id::QuietKey, "quiet_hotkey")] {
            if let Some(on) = m.get(key).and_then(|x| x.as_bool()) {
                v.push((id, on_off(on)));
            }
        }
        for (i, &(_, key, _, least, most)) in LAYOUT_ROWS.iter().enumerate() {
            if let Some(n) = u(key).filter(|_| !key.is_empty()) {
                // The notification Size followed the menu's size then.
                let n = if i == TOAST_SIZE { u64::from(note_times_size(n as f64, size, i)) } else { n };
                v.push((Id::Layout(i), n.clamp(u64::from(least), u64::from(most)).to_string()));
            }
        }
        if size != 100 && u("toastsize").is_none() {
            v.push((Id::Layout(TOAST_SIZE), note_times_size(f64::from(LAYOUT_ROWS[TOAST_SIZE].2), size, TOAST_SIZE).to_string()));
        }
        if size != 100 {
            v.push((Id::Layout(NOTE_TEXT), note_times_size(f64::from(LAYOUT_ROWS[NOTE_TEXT].2), size, NOTE_TEXT).to_string()));
        }
        // The four corners it had are the first four Positions.
        if let Some(n) = u("toast_corner") {
            v.push((Id::ToastPosition, NOTIFY_POSITIONS[n as usize % 4].into()));
        }
        if let Some(n) = u("toast_colour") {
            v.push((Id::ToastColour, toast_colour_name(n as usize % (ACCENTS.len() + 1)).into()));
        }
    }
    if let Some(s) = settings {
        for (id, key) in [(Id::EspEnemies, "esp_hostile"), (Id::EspFriendlies, "esp_friendly"), (Id::EspPlayers, "esp_players"), (Id::EspHideDead, "esp_hide_dead"), (Id::SafeFly, "fly_safe")] {
            if let Some(on) = s.get(key).and_then(|x| x.as_bool()) {
                v.push((id, on_off(on)));
            }
        }
        if let Some(style) = s.get("esp_style").and_then(|x| serde_json::from_value::<aurora_core::proto::EspStyle>(x.clone()).ok()) {
            v.push((Id::EspRange, number(f64::from(style.range_m.clamp(25.0, 1500.0)))));
            v.push((Id::EspBones, if style.bones { "Bones" } else { "Box" }.into()));
            v.push((Id::EspLines, style.width.min(6).to_string()));
            v.push((Id::EspEdge, on_off(style.edge)));
            v.push((Id::EspDistance, on_off(style.labels)));
            for i in [0, 1, 3] {
                v.push((Id::EspColour(i), colour_state(style.colors[i])));
            }
        }
        // settings.json kept m/s; Speed is a multiplier now. Sprint was 4 x Speed then; it is its
        // own speed now, so it comes too.
        if let (Some(speed), _) = upgrade_fly(s.get("fly_speed").and_then(|x| x.as_f64()), None) {
            v.push((Id::FlySpeed, format!("{speed:.2}")));
            v.push((Id::FlySprint, format!("{:.2}", fly_value(speed * fly::SPRINT_DEFAULT))));
        }
        if let Some(c) = s.get("goto_coords").and_then(|x| x.as_array()) {
            for (i, x) in c.iter().take(3).enumerate() {
                if let Some(x) = x.as_f64().filter(|x| x.is_finite()) {
                    v.push((Id::Coord(i), number(x.clamp(-COORD_MAX, COORD_MAX))));
                }
            }
        }
    }
    v.retain(|(id, val)| !default_state(*id).is_some_and(|d| same_state(*id, val, &d)));
    v
}

/// A profile's text with the lines it does not have yet added at its end, in Stand's tree. Lines it
/// has (at their address now, or one they had before a move) stay as they are: the player's profile
/// wins. Gives the text and how many lines were added. An empty profile gets the version line
/// first.
fn add_missing(text: &str, lines: &[(String, String)]) -> (String, usize) {
    // The player's old Position and Disable Warnings lines count as today's rows (`upgrade_rows`);
    // the player's lines as they are count too.
    let tree = read_tree(text);
    let have: Vec<String> = upgrade_rows(tree.clone(), screen_size()).into_iter().chain(tree).map(|(a, _)| moved(&a).unwrap_or(a)).collect();
    let missing: Vec<(String, String)> = lines.iter().filter(|(a, _)| !have.contains(a)).cloned().collect();
    if missing.is_empty() {
        return (text.to_string(), 0);
    }
    let tree = write_tree(&missing);
    if text.trim().is_empty() {
        return (tree, missing.len());
    }
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    // Without its version line: the file has one.
    out.push_str(tree.split_once('\n').map_or("", |(_, rest)| rest));
    (out, missing.len())
}

/// At inject, before the Load On Inject profile loads: the player's values in the old settings.json
/// and menu.json go into that profile (Main when none is marked, made if needed), only the lines it
/// does not have; then both files move into Dumper\old, and the old Offsets.json too (the cache in
/// Bin\Cache rebuilds by itself). A file that can't be read, or a profile that can't be written,
/// leaves the files where they are for the next inject; one that can't move stays. Each step goes
/// in dll.log.
fn carry_over_old_files(prefs: Prefs) {
    let olds = [aurora_core::paths::old_mod_settings(), aurora_core::paths::old_menu_settings()];
    let mut moving = Vec::new();
    if olds.iter().any(|p| p.is_file())
        && let (Ok(settings), Ok(menu)) = (read_old_json(&olds[0]), read_old_json(&olds[1]))
        && carry_values(prefs, settings.as_ref(), menu.as_ref())
    {
        moving.extend(olds.iter().cloned());
    }
    moving.push(aurora_core::paths::old_offsets());
    for p in moving.into_iter().filter(|p| p.is_file()) {
        match aurora_core::paths::move_aside(&p, &aurora_core::paths::old_files()) {
            Ok(to) => log(&format!("menu: moved {} to {}", p.display(), to.display())),
            Err(e) => log(&format!("menu: could not move {} ({e}): left where it is", p.display())),
        }
    }
}

/// An old JSON file: `Ok(None)` when it is not there, or not valid JSON (said in the log; it still
/// moves to Dumper\old, so nothing is lost); `Err` when it is there but can't be read.
fn read_old_json(p: &std::path::Path) -> Result<Option<serde_json::Value>, ()> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match std::fs::read_to_string(p) {
        Ok(t) => match serde_json::from_str(&t) {
            Ok(v) => Ok(Some(v)),
            Err(e) => {
                log(&format!("menu: old {name} is not valid JSON ({e}): its values are not carried over"));
                Ok(None)
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => {
            log(&format!("menu: old {name} could not be read ({e}): left where it is, tried again at the next inject"));
            Err(())
        }
    }
}

/// The old values into the Load On Inject profile (or Main). True when they are in it now, or there
/// was nothing to add.
fn carry_values(prefs: Prefs, settings: Option<&serde_json::Value>, menu: Option<&serde_json::Value>) -> bool {
    let rows = all_rows(prefs);
    let mut lines = Vec::new();
    for (id, value) in old_values(settings, menu, screen_size()) {
        match rows.iter().find(|f| f.item.id == id) {
            Some(f) => lines.push((f.addr.clone(), value)),
            None => log(&format!("menu: old setting {id:?} = {value} has no row: not carried over")),
        }
    }
    if lines.is_empty() {
        log("menu: the old settings.json / menu.json hold only defaults: nothing to add to a profile");
        return true;
    }
    let marked = profiles().into_iter().find(|n| profile_flag(n, ON_INJECT));
    let name = marked.clone().unwrap_or_else(|| "Main".to_string());
    let Some(path) = profile_path(&name) else {
        log("menu: no profiles folder: the old settings.json / menu.json are left where they are");
        return false;
    };
    let Ok(text) = read_text(&path) else { return false };
    // The lines added are in today's units, so an older profile is upgraded first.
    let (text, added) = add_missing(&upgrade_profile(&text.unwrap_or_default(), screen_size()), &lines);
    if added == 0 {
        log(&format!("menu: profile {name} already has all {} old settings", lines.len()));
        return true;
    }
    match aurora_core::paths::write_file(&path, text) {
        Ok(()) => {
            let note = if marked.is_some() { "" } else { " (no profile is marked Load On Inject, so it does not load by itself)" };
            log(&format!("menu: {added} of {} old settings added to profile {name}{note}", lines.len()));
            true
        }
        Err(e) => {
            log(&format!("menu: profile {name} not written ({e}): the old settings.json / menu.json are left where they are"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_presses_act() {
        // Down at open (or when the game came back) and still down: nothing.
        assert!(!fresh_press(true, true, true));
        // Pressed with the game behind: nothing.
        assert!(!fresh_press(false, true, false));
        // A real press with the menu open and the game in front.
        assert!(fresh_press(false, true, true));
        assert!(!fresh_press(false, false, true));
    }

    #[test]
    fn only_armed_arrows_repeat() {
        let ms = Duration::from_millis;
        // Held since the menu opened (not armed): never repeats.
        assert!(!repeats(false, true, true, ms(5000), ms(5000)));
        // Its own press, held long enough.
        assert!(repeats(true, true, true, ms(500), ms(100)));
        // Too soon, too fast, game behind, or let go.
        assert!(!repeats(true, true, true, ms(300), ms(100)));
        assert!(!repeats(true, true, true, ms(500), ms(50)));
        assert!(!repeats(true, false, true, ms(500), ms(100)));
        assert!(!repeats(true, true, false, ms(500), ms(100)));
    }

    #[test]
    fn key_names_read_back() {
        // Every key alone, and with the comma key on either side (the list's own ", ").
        for vk in 1..=255u32 {
            assert_eq!(parse_keys(&key_names(&[vk])), (vec![vk], Vec::new()), "{}", key_name(vk));
            if vk != 0xBC {
                assert_eq!(parse_keys(&key_names(&[vk, 0xBC])).0, vec![vk, 0xBC]);
                assert_eq!(parse_keys(&key_names(&[0xBC, vk])).0, vec![0xBC, vk]);
            }
        }
        assert_eq!(parse_keys("f5, numpad 2").0, vec![0x74, 0x62]);
        assert_eq!(parse_keys("F5, Nope").1, vec!["Nope".to_string()]);
    }

    #[test]
    fn target_rows_follow_their_move() {
        assert_eq!(moved("Menu > Experiments > Target"), Some("World > Target".to_string()));
        assert_eq!(moved("Menu > Experiments > Target > Yeet Target"), Some("World > Target > Yeet Target".to_string()));
        // Its place now: no move, so no loop back.
        assert_eq!(moved("World > Target > Bring Target"), None);
        // Infinite Stamina went into Movement on 3 Nov and back to Self.
        assert_eq!(moved("Self > Infinite Stamina"), None);
        assert_eq!(moved("Self > Movement > Infinite Stamina"), Some("Self > Infinite Stamina".to_string()));
        assert_eq!(moved("World > ESP > Hostile"), Some("World > ESP > Targets > Hostile".to_string()));
        assert_eq!(moved("World > ESP > Colour > Hex Code"), Some("World > ESP > Drawing > Colours > Hex Code".to_string()));
        assert_eq!(moved("World > ESP > Look > Lines"), Some("World > ESP > Drawing > Lines".to_string()));
        assert_eq!(moved("World > ESP > Drawing > Colours > Hostile"), None);
        assert_eq!(moved("World > ESP > Targets > Players"), None);
        // Renamed to Aurora: both old Unload addresses land on the new row.
        assert_eq!(moved("Menu > Unload grbmod"), Some("Aurora > Unload Aurora".to_string()));
        assert_eq!(moved("Settings > Unload grbmod"), Some("Aurora > Unload Aurora".to_string()));
        // The Menu tab is Aurora: every old Menu address lands there.
        assert_eq!(moved("Menu > Unload Aurora"), Some("Aurora > Unload Aurora".to_string()));
        assert_eq!(moved("Aurora > Unload Aurora"), None);
        assert_eq!(moved("Menu > Appearance > Menu Colour"), Some("Aurora > Settings > Appearance > Colours > Menu Colour".to_string()));
        assert_eq!(moved("Menu > Settings > Appearance > Colours > Menu Colour"), Some("Aurora > Settings > Appearance > Colours > Menu Colour".to_string()));
        assert_eq!(moved("Aurora > Settings > Appearance > Colours > Menu Colour"), None);
        assert_eq!(moved("Aurora > Settings > Appearance > Tabs > Tabs Width"), None);
        assert_eq!(moved("Menu > Hotkeys").as_deref(), Some("Aurora > Hotkeys"));
        assert_eq!(moved("Settings > Saved Commands").as_deref(), Some("Aurora > Saved Commands"));
        assert_eq!(moved("Menu > Profiles > Main > Load").as_deref(), Some("Aurora > Profiles > Main > Load"));
        // A row named "Menu..." is not the tab.
        assert_eq!(moved("Aurora > Settings > Notifications > Suppress Generic Responses > Menu"), None);
        // Fly's Speed is Fly Speed.
        assert_eq!(moved("Self > Movement > Fly > Speed").as_deref(), Some(FLY_SPEED_ADDR));
        assert_eq!(moved(FLY_SPEED_ADDR), None);
        assert_eq!(moved("Self > Movement > Fly > Sprint"), None);
        // Every old address lands on one that does not move again, never under "Menu".
        for (old, _) in MOVED {
            let now = moved(old).unwrap();
            assert!(!now.starts_with("Menu > ") && now != "Menu", "{old} -> {now}");
            assert_eq!(moved(&now), None, "{old} -> {now} moves again");
        }
        // Self > Progress is Game > Progress > Editor; older moves inside it first.
        assert_eq!(moved("Self > Progress > Credits"), Some("Game > Progress > Editor > Credits > Credits".to_string()));
        assert_eq!(moved("Self > Progress > Credits > Credits"), Some("Game > Progress > Editor > Credits > Credits".to_string()));
        assert_eq!(moved("Game > Progress > Editor > Credits > Credits"), None);
        assert_eq!(moved("Self > Progress > Credits > Pick Other Points"), Some("Game > Progress > Editor > Credits > Pick Other Points".to_string()));
        assert_eq!(moved("Self > Progress > Gear Score > Dropped Item Gear Score"), Some("Game > Progress > Editor > Gear Score > Dropped Item Gear Score".to_string()));
        assert_eq!(moved("Self > Progress > Dropped Item Gear Score"), Some("Game > Progress > Editor > Gear Score > Dropped Item Gear Score".to_string()));
        assert_eq!(moved("Self > Progress > Gear Score > Score"), Some("Game > Progress > Editor > Gear Score > Dropped Item Gear Score".to_string()));
        assert_eq!(moved("Self > Progress"), Some("Game > Progress > Editor".to_string()));
        // The probe rows that went have nowhere to go: their keys are simply never matched.
        assert_eq!(moved("Menu > Experiments > Target > Save As Hostile"), Some("World > Target > Save As Hostile".to_string()));
    }

    #[test]
    fn hotkeys_tree_reads_back() {
        let rows: Vec<(String, String)> = [
            ("World > ESP > Range", vec![0x76]),
            ("World > ESP", vec![0x75, 0xBC]),
            ("Self > God Mode", vec![0x74]),
            ("World > ESP > Look > Lines", vec![0x62, 0x41]),
            ("Menu > Profiles > Main > Load", vec![0xDE]),
        ]
        .into_iter()
        .map(|(a, k)| (a.to_string(), key_names(&k)))
        .collect();
        let text = write_tree(&rows);
        assert!(text.starts_with("Tree Compatibility Version: 1\nWorld\n\tESP\n\t\tRange: F7\n"));
        assert_eq!(read_tree(&text), rows);
    }

    fn test_prefs() -> Prefs {
        Prefs::default()
    }

    /// A row as the player's approved pictures draw it: name, and its kind with the value where the
    /// value is not an example or the game's.
    #[derive(Debug, PartialEq)]
    enum Pic {
        Sub,
        Tog,
        TogIs(bool),
        Val,
        ValIs(&'static str),
        Act,
    }

    fn pic_of(k: &Kind, want: &Pic) -> Pic {
        match (k, want) {
            (Kind::Sub, _) => Pic::Sub,
            (Kind::Toggle(on), Pic::TogIs(_)) => Pic::TogIs(*on),
            (Kind::Toggle(_), _) => Pic::Tog,
            (Kind::Choice(v), Pic::ValIs(w)) if v == w => Pic::ValIs(w),
            (Kind::Choice(v), Pic::ValIs(_)) => Pic::ValIs(Box::leak(v.clone().into_boxed_str())),
            (Kind::Choice(_), _) => Pic::Val,
            (Kind::Action, _) => Pic::Act,
            (k, _) => panic!("kind {k:?} is in no picture"),
        }
    }

    fn rows_are(page: Page, want: &[(&str, Pic)]) {
        let got = items(page, test_prefs());
        let names: Vec<&str> = got.iter().map(|i| i.row.text.as_str()).collect();
        assert_eq!(names, want.iter().map(|w| w.0).collect::<Vec<_>>(), "{page:?}");
        for (it, (name, w)) in got.iter().zip(want) {
            assert_eq!(&pic_of(&it.row.kind, w), w, "{page:?} > {name}");
        }
    }

    #[test]
    fn pages_match_her_approved_pictures() {
        use Pic::*;
        // 1 Self.png Lists on top.
        rows_are(Page::SelfTab, &[("Movement", Sub), ("Weapons", Sub), ("God Mode", Tog), ("Infinite Stamina", Tog), ("Undetected", Tog)]);
        // Silent Aim's list went to Experiments.
        rows_are(Page::Weapon, &[
            ("No Recoil", Tog),
            ("No Spread", Tog),
            ("No Bullet Drop", TogIs(false)),
            ("No Scoped Sway", Tog),
            ("Infinite Mag", Tog),
            ("Infinite Reserve", Tog),
            ("Damage Multiplier", ValIs("Off")),
            ("Fire Rate", ValIs("Off")),
        ]);
        rows_are(Page::SilentAim, &[
            ("Silent Aim", TogIs(false)),
            ("Silent Aim FOV", ValIs("20 deg")),
            ("Silent Aim Range", ValIs("300 m")),
            ("Show FOV Circle", TogIs(true)),
            ("Priority", ValIs("Crosshair")),
            ("Magic Bullets", TogIs(false)),
            ("No Range Loss", TogIs(false)),
            ("Instant Hit", TogIs(false)),
        ]);
        rows_are(Page::Movement, &[("Fly", Sub)]);
        // Hop Up and Hop Height after Sprint, no Stand While Flying.
        rows_are(Page::Fly, &[("Fly", Tog), ("Fly Speed", ValIs("1.00")), ("Sprint", ValIs("4.00")), ("Hop Up", Tog), ("Hop Height", ValIs("3.75")), ("Safe Fly", Tog)]);
        // 2 Game.png
        rows_are(Page::Game, &[("Disables", Sub), ("Rendering", Sub), ("Progress", Sub)]);
        rows_are(Page::Disables, &[("Screen Shake", Tog), ("NVG In Erewhon", Tog)]);
        rows_are(Page::Rendering, &[("No NVG Overlay", Tog)]);
        rows_are(Page::Progress, &[("Editor", Sub)]);
        rows_are(Page::Editor, &[("Gear Score", Sub), ("Credits", Sub)]);
        // 3 Settings.png Mouse Support under the lists, off; Enable Experimental Features under it,
        // off.
        rows_are(Page::MenuSettings, &[("Appearance", Sub), ("Notifications", Sub), ("Warnings", Sub), ("Mouse Support", TogIs(false)), ("Enable Experimental Features", TogIs(false))]);
        // Stand's size rows straight in Appearance, lists first; no Menu Size.
        rows_are(Page::Appearance, &[
            ("Position", Sub),
            ("Colours", Sub),
            ("Tabs", Sub),
            ("Address Bar", Sub),
            ("Notifications", Sub),
            ("Max Visible Commands", ValIs("10")),
            ("List Width", ValIs("450")),
            ("List Height", ValIs("32")),
            ("Spacer Size", ValIs("3")),
        ]);
        // X and Y are examples in the picture: pixels, as plain numbers.
        rows_are(Page::Position, &[("X", Val), ("Y", Val), ("Move With Mouse", TogIs(false))]);
        // Lists on top, no Invert Flow, Padding or Flash Colour, Text Size under Size. Settings >
        // Notifications holds only Suppress Generic Responses, the look rows are Appearance >
        // Notifications.
        rows_are(Page::Notifications, &[("Suppress Generic Responses", Sub)]);
        rows_are(Page::Suppress, &[("Command Box", TogIs(false)), ("Hotkey", TogIs(false))]);
        rows_are(Page::NoteLook, &[
            ("Custom Position", Sub),
            ("Timing", Sub),
            ("Position", ValIs("Bottom Left")),
            ("Width", ValIs("400")),
            ("Size", ValIs("100%")),
            ("Text Size", ValIs("100%")),
            ("Border Colour", ValIs("Menu Colour")),
            ("Background Colour", ValIs("Black")),
            ("Flash Notification", Act),
        ]);
        rows_are(Page::Timing, &[("Time", ValIs("3 s"))]);
        rows_are(Page::CustomPosition, &[("X", ValIs("40")), ("Y", ValIs("900"))]);
        rows_are(Page::Warnings, &[("Automatically Proceed On Warnings", Sub), ("Force Me To Read Warnings", TogIs(true)), ("Skip Already-Accepted Warnings", TogIs(true))]);
        rows_are(Page::AutoProceed, &[("Command Box", TogIs(false)), ("Hotkey", TogIs(false))]);
        // The tabs, and the pages' names in the address bar.
        assert_eq!(SECTIONS.map(|s| s.1), ["Self", "World", "Game", "Aurora"]);
        for (p, name) in [
            (Page::Weapon, "Weapons"),
            (Page::Editor, "Editor"),
            (Page::Notifications, "Notifications"),
            (Page::NoteLook, "Notifications"),
            (Page::ExpWarn, "Experiments"),
            (Page::CustomPosition, "Custom Position"),
            (Page::Timing, "Timing"),
            (Page::AutoProceed, "Automatically Proceed On Warnings"),
        ] {
            assert_eq!(page_name(p), name);
        }
        // Help lines as the pictures have them.
        let auto = items(Page::AutoProceed, test_prefs());
        assert_eq!(auto[0].help, "Includes the command box.");
        assert_eq!(command(Id::AutoBox), None, "no command line under it in the player's picture");
        assert_eq!(command(Id::PosX).as_deref(), Some("menux"));
        // The player's old names still reach today's commands; percent becomes pixels.
        assert_eq!(old_command("toastposition", Some("topright"), (2560, 1440), 0), Some(("notifyposition", Some("topright".into()))));
        assert_eq!(old_command("toasttime", None, (2560, 1440), 0), Some(("notifytime", None)));
        assert_eq!(old_command("menuleftright", Some("50"), (2560, 1440), 0), Some(("menux", Some("1280".into()))));
        // X is the bar since the old percent X gets the help boxes' room.
        assert_eq!(old_command("menuleftright", Some("50"), (2560, 1440), 283), Some(("menux", Some("1563".into()))));
        assert_eq!(old_command("menuupdown", Some("25"), (2560, 1440), 283), Some(("menuy", Some("360".into()))));
        // X stays on the screen.
        assert_eq!(old_command("menuleftright", Some("64"), (2560, 1440), 923), Some(("menux", Some("2560".into()))));
        assert_eq!(old_command("menux", Some("50"), (2560, 1440), 0), None);
        assert_eq!(range(Id::PosX, test_prefs()), "-32768 to 32767");
        assert_eq!(command(Id::Layout(NOTE_TEXT)).as_deref(), Some("notifytextsize"));
        assert_eq!(range(Id::Layout(NOTE_TEXT), test_prefs()), "50 to 300");
        // The player's rule: the Command line on top, the context under it, the hint last. Each in
        // its own box (`NEW_BOX`); a help text's own line breaks stay in its box.
        assert_eq!(help_lines(String::new(), "Command: flyspeed [0.01 to 10000.00]".into(), true), "Command: flyspeed [0.01 to 10000.00]\n\nClick to input a value.");
        assert_eq!(
            help_lines("How fast you go while you hold [Space].".into(), "Command: flysprint [0.01 to 10000.00]".into(), true),
            "Command: flysprint [0.01 to 10000.00]\n\nHow fast you go while you hold [Space].\n\nClick to input a value."
        );
        assert_eq!(help_lines("W A S D move\nL Shift up".into(), "Command: fly".into(), false), "Command: fly\n\nW A S D move\nL Shift up");
        // Every page's commands are its own: no two rows share one, and none but Save This Spot
        // starts with save or load (savemain / loadmain are the profiles').
        let mut seen = std::collections::HashSet::new();
        let pages = [
            Page::SelfTab, Page::Weapon, Page::SilentAim, Page::Movement, Page::Fly, Page::Game, Page::Disables, Page::Rendering, Page::Progress,
            Page::Editor, Page::World, Page::Esp, Page::EspTargets, Page::EspDrawing, Page::EspColours,
            Page::Menu, Page::MenuSettings, Page::Appearance, Page::Position, Page::MenuColours, Page::Tabs, Page::AddressBar,
            Page::Notifications, Page::NoteLook, Page::CustomPosition, Page::Timing, Page::Suppress, Page::Warnings, Page::AutoProceed,
            Page::Experiments,
        ];
        for p in pages {
            for it in items(p, test_prefs()) {
                let Some(c) = command(it.id) else { continue };
                // The ESP colour and its hex code share one command on purpose.
                if !matches!(it.id, Id::EspHex(_)) {
                    assert!(seen.insert(c.clone()), "{c} twice");
                }
                assert!(!(c.starts_with("save") || c.starts_with("load")) || c == "savethisspot", "{c}");
            }
        }
    }

    #[test]
    fn hidden_pages_are_skipped() {
        let prefs = Prefs { experimental: true, ..test_prefs() };
        // Experiments holds the untested switch.
        let mut rows = items_of(Page::Menu, prefs);
        let exp = rows.iter().position(|i| i.row.text == "Experiments").unwrap();
        assert!(!skipped(&rows[exp]));
        assert!(items(Page::Experiments, prefs).iter().any(|i| i.id == Id::UnseenInbox));
        assert!(!items(Page::SelfTab, prefs).iter().any(|i| i.id == Id::UnseenInbox));
        // Hiding a page grays its row: the cursor skips it and `activate` refuses it.
        hide_rows(&mut rows, |p| p == Page::Experiments);
        assert!(skipped(&rows[exp]) && rows[exp].row.dim);
        assert!(!skipped(&rows[exp - 1]), "other rows stay");
        // No page is hidden; Experiments shows, with, while Enable
        // Experimental Features is on.
        assert!(HIDDEN.is_empty());
        let row = items(Page::Menu, prefs).into_iter().find(|i| i.row.text == "Experiments").unwrap();
        assert!(!row.row.dim);
        assert_eq!(row.help, "Untested.\nCan crash your game or\ncause unwanted behaviour.");
    }

    /// Enable Experimental Features: off at first, and then Experiments is not shown and none of
    /// its rows can be reached. On only through the Experiments warning (the player's three lines,
    /// never picked or hovered; Yes after the read wait), back where it was asked; Experiments then
    /// opens straight in. Force Me To Read Warnings off: no warning page. Off turns the experiments
    /// that are on off and closes Experiments wherever it is open.
    #[test]
    fn experimental_features_toggle() {
        let mut st = on_notifications();
        st.prefs.force_read = true;
        st.prefs.skip_accepted = false;
        assert!(!Prefs::default().experimental && !st.prefs.experimental);
        let shown = |p: Prefs| items(Page::Menu, p).into_iter().any(|i| i.id == Id::Open(Page::Experiments));
        assert!(!shown(st.prefs), "not in Aurora while off");
        assert!(!all_rows(st.prefs).iter().any(|f| in_experiments(&f.addr)), "none of its rows reachable");
        // Its row in Aurora > Settings, its command and default.
        let f = resolve(EXPERIMENTAL_ADDR, st.prefs).expect("its address");
        assert_eq!(f.item.id, Id::Experimental);
        assert_eq!(command(Id::Experimental).as_deref(), Some("enableexperimentalfeatures"));
        assert_eq!(default_state(Id::Experimental), Some(on_off(false)));
        assert!(has_default(Id::Experimental));
        // On from the menu: the warning first, with.
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 4, 0)];
        FROM.store(FROM_MENU, Ordering::Relaxed);
        activate(&mut st, &f.item);
        assert!(!st.prefs.experimental, "not before Yes");
        assert_eq!(st.stack.last().unwrap().0, Page::ExpWarn);
        let rows = items(Page::ExpWarn, st.prefs);
        let text: Vec<&str> = rows.iter().filter(|i| i.id == Id::WarnText).map(|i| i.row.text.as_str()).collect();
        assert_eq!(text, ["Experiments are untested.", "They can crash your game or", "cause unwanted behaviour."]);
        // Warning text is never picked, highlighted, hovered or clicked.
        assert!(rows.iter().filter(|i| i.id == Id::WarnText).all(skipped));
        assert_eq!(view(&st).pick, None, "no Yes yet: nothing highlighted");
        assert!(!hover(&mut st, 0));
        assert!(!mouse_click(&mut st, Some(Spot::Row(1)), false));
        assert!(yes_shows(true, Some(READ_WAIT)) && !yes_shows(true, Some(Duration::from_millis(100))));
        // Yes: on, back on Settings; Experiments shows in Aurora and opens straight in.
        activate(&mut st, &item(Id::ExpYes, "Yes", Kind::Action, ""));
        assert!(st.prefs.experimental);
        assert_eq!(st.stack.iter().map(|t| t.0).collect::<Vec<_>>(), [Page::Menu, Page::MenuSettings]);
        assert!(shown(st.prefs));
        let open = items(Page::Menu, st.prefs).into_iter().find(|i| i.id == Id::Open(Page::Experiments)).unwrap();
        st.stack = vec![(Page::Menu, 0, 0)];
        activate(&mut st, &open);
        assert_eq!(st.stack.last().unwrap().0, Page::Experiments);
        // Silent Aim's list is in it, on top as every list.
        assert_eq!(items(Page::Experiments, st.prefs)[0].id, Id::Open(Page::SilentAim));
        // Off: an experiment that is on goes off, and Experiments closes, shown or behind another
        // tab.
        fly::set_calm(true);
        st.stack = vec![(Page::Menu, 3, 0), (Page::Experiments, 0, 0), (Page::SilentAim, 1, 0)];
        st.saved[3] = st.stack.clone();
        activate(&mut st, &f.item);
        assert!(!st.prefs.experimental);
        assert!(!fly::calm(), "the experiment that was on is off");
        assert_eq!(st.stack, [(Page::Menu, 3, 0)]);
        assert_eq!(st.saved[3], [(Page::Menu, 3, 0)]);
        assert!(!shown(st.prefs));
        // Force Me To Read Warnings off: on at once, no page.
        st.prefs.force_read = false;
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 4, 0)];
        activate(&mut st, &f.item);
        assert!(st.prefs.experimental);
        assert_eq!(st.stack.last().unwrap().0, Page::MenuSettings);
        // Apply Default State turns it off; a profile has it on only with its line On.
        default_of(&mut st, Id::Experimental);
        assert!(!st.prefs.experimental);
        assert!(wants_experimental(&[(EXPERIMENTAL_ADDR.into(), "On".into())]));
        assert!(!wants_experimental(&[(EXPERIMENTAL_ADDR.into(), "Off".into())]) && !wants_experimental(&[]));
        // Hotkeys and commands on its rows while it is off say how to get them back.
        assert!(in_experiments("Aurora > Experiments") && in_experiments("Aurora > Experiments > Silent Aim > Silent Aim"));
        assert!(!in_experiments("Aurora > ExperimentsX") && !in_experiments("Aurora > Settings"));
        // A saved command on its rows is hidden, not gone, also by an old address (review).
        let (off, on) = (Prefs { experimental: false, ..st.prefs }, Prefs { experimental: true, ..st.prefs });
        assert!(hidden_experiment("Aurora > Experiments > Silent Aim > Silent Aim", off));
        assert!(hidden_experiment("Self > Weapons > Silent Aim > Silent Aim", off), "an old address too");
        assert!(!hidden_experiment("Aurora > Experiments > Silent Aim > Silent Aim", on));
        assert!(!hidden_experiment("Aurora > Settings > Mouse Support", off));
        // Off closes Silent Aim's list also where Find opened it, and the Aurora list's picks move
        // with their rows.
        FROM.store(FROM_MENU, Ordering::Relaxed);
        activate(&mut st, &f.item);
        assert!(st.prefs.experimental);
        let row = |p: Prefs, i: usize| items(Page::Menu, p)[i].id;
        assert_eq!(row(st.prefs, 5), Id::Open(Page::NonDefault));
        st.stack = vec![(Page::Menu, 5, 0), (Page::NonDefault, 0, 0)];
        st.saved[3] = vec![(Page::Menu, 0, 0), (Page::Find, 2, 0), (Page::SilentAim, 1, 0)];
        activate(&mut st, &f.item);
        assert!(!st.prefs.experimental);
        assert_eq!(st.stack, [(Page::Menu, 4, 0), (Page::NonDefault, 0, 0)]);
        assert_eq!(row(st.prefs, 4), Id::Open(Page::NonDefault), "still on its row, not on Apply Default State");
        assert_eq!(st.saved[3], [(Page::Menu, 0, 0), (Page::Find, 2, 0)]);
        // On again: the picks below Experiments move down one.
        st.stack = vec![(Page::Menu, 3, 0)];
        activate(&mut st, &f.item);
        assert!(st.prefs.experimental);
        assert_eq!(st.stack, [(Page::Menu, 4, 0)]);
        assert_eq!(row(st.prefs, 4), Id::Open(Page::Saved));
        assert_eq!(experiment_pages(st.prefs), [Page::Experiments, Page::SilentAim]);
        default_of(&mut st, Id::Experimental);
        assert!(!st.prefs.experimental);
    }

    /// A picked unload waits while a menu key or a mouse button that picked it is still down.
    #[test]
    fn unload_waits_for_keys_and_mouse() {
        let mut st = on_notifications();
        st.unload_at = Some(Instant::now());
        st.mouse_hold = true;
        unload_tick(&mut st);
        assert!(st.unload_at.is_some(), "waits for the mouse button");
        st.mouse_hold = false;
        st.nav_hold = true;
        unload_tick(&mut st);
        assert!(st.unload_at.is_some(), "waits for the key");
    }

    /// The player's own profile as it was (version 3, Menu Size 85, NVG Overlay On, no X line): the
    /// menu looks and sits the same. 85% is the List Width / Height defaults (no lines), the
    /// notifications take the player 85% (they followed Menu Size), NVG Overlay is No NVG Overlay,
    /// and with no X line the wall is the default: the screen's right edge, where the player's
    /// menu's list ended.
    #[test]
    fn her_profile_looks_the_same() {
        let saved = "Tree Compatibility Version: 3\nGame\n\tRendering\n\t\tNVG Overlay: On\nAurora\n\tSettings\n\t\tAppearance\n\t\t\tMenu Size: 85\n\t\tWarnings\n\t\t\tForce Me To Read Warnings: Off\n";
        let rows = upgrade_rows(read_tree(&upgrade_profile(saved, (2560, 1440))), (2560, 1440));
        let rows: Vec<(String, String)> = rows.into_iter().filter(|(a, _)| !gone(a)).map(|(a, v)| (moved(&a).unwrap_or(a), v)).collect();
        let want: Vec<(String, String)> = [
            ("Game > Rendering > No NVG Overlay", "On"),
            ("Aurora > Settings > Warnings > Force Me To Read Warnings", "Off"),
            (NOTE_SIZE_ADDR, "85"),
            (NOTE_TEXT_ADDR, "85"),
        ]
        .into_iter()
        .map(|(a, v)| (a.to_string(), v.to_string()))
        .collect();
        assert_eq!(rows, want);
        // Menu Size 85 is the defaults, other sizes are not.
        assert_eq!(list_for_size(85), [450, 32]);
        assert_eq!(list_for_size(100), [529, 38]);
        assert_eq!(list_for_size(60), [318, 23]);
        assert_eq!(aurora_core::menuview::axes(1.0, aurora_core::menuview::LIST), (0.85, 0.85));
        // The player's default wall at 2560 x 1440, the same as the player's list's right edge
        // before.
        let d = Prefs::default();
        assert_eq!(old_x_to_wall(pct_px(64.0, 2560) + old_x_shift(100, 118, 1440), LOOK_SIZE, (2560, 1440)), 2560);
        assert_eq!(menu_pos(&d, 1), 561);
        // Menu Size went: its row, command and lines.
        assert!(!items(Page::Appearance, d).iter().any(|i| i.row.text == "Menu Size"));
        assert!(gone(MENU_SIZE_ADDR));
        assert!(GONE_COMMANDS.iter().any(|g| g.0 == "menusize"));
        assert_eq!([LAYOUT_COMMANDS[LIST_W], LAYOUT_COMMANDS[LIST_H], LAYOUT_COMMANDS[VISIBLE_ROWS], LAYOUT_COMMANDS[SPACER_SIZE]], ["listwidth", "listheight", "menuheight", "spacersize"]);
        assert_eq!([LAYOUT_ROWS[LIST_W].2, LAYOUT_ROWS[LIST_H].2, LAYOUT_ROWS[VISIBLE_ROWS].2], [450, 32, 10]);
        assert_eq!((LAYOUT_ROWS[VISIBLE_ROWS].3, LAYOUT_ROWS[VISIBLE_ROWS].4), (3, 255));
        assert_eq!((LAYOUT_ROWS[LIST_W].3, LAYOUT_ROWS[LIST_W].4), (0, 32767));
    }

    /// Doing something by hand in the menu never shows a plain "on" / "off" / "set to"; the command
    /// box and hotkeys do unless the player suppressed them; a profile load never does. Other "it
    /// worked" lines still show from the menu.
    #[test]
    fn the_menu_by_hand_is_quiet() {
        assert!(quiet_from(FROM_MENU));
        assert!(quiet_from(FROM_PROFILE));
        assert_eq!(quiet_from(FROM_BOX), QUIET_BOX.load(Ordering::Relaxed));
        assert_eq!(quiet_from(FROM_KEY), QUIET_KEY.load(Ordering::Relaxed));
        // Suppress > Command Box hides errors from the box too; nothing else does.
        assert!(box_hushed(FROM_BOX, false, true));
        assert!(box_hushed(FROM_BOX, true, true));
        assert!(!box_hushed(FROM_BOX, true, false));
        // A profile loaded from the box counts as the box; from the menu or a hotkey it doesn't.
        assert!(box_hushed(FROM_PROFILE, true, true));
        for from in [FROM_MENU, FROM_KEY, FROM_PROFILE] {
            assert!(!box_hushed(from, false, true));
        }
        // The look rows moved, Suppress stayed.
        let d = Prefs::default();
        assert_eq!(items(Page::Notifications, d).len(), 1);
        assert!(items(Page::Appearance, d).iter().any(|i| i.id == Id::Open(Page::NoteLook)));
        let sup = items(Page::Suppress, d);
        assert_eq!((sup[0].help.as_str(), sup[1].help.as_str()), ("Includes the command box.", ""));
    }

    /// The player's command names: "default" and "unload"; Commands With Non-Default State and Move
    /// With Mouse have none; their old names still run quietly.
    #[test]
    fn short_command_names() {
        assert_eq!(command(Id::ApplyDefaults).as_deref(), Some("default"));
        assert_eq!(command(Id::Unload).as_deref(), Some("unload"));
        assert_eq!(command(Id::Open(Page::NonDefault)), None);
        assert_eq!(command(Id::MoveMouse), None);
        assert_eq!(old_command("applydefaultstate", None, (1920, 1080), 0), Some(("default", None)));
        assert_eq!(old_row_command("commandswithnondefaultstate"), Some(Id::Open(Page::NonDefault)));
        assert_eq!(old_row_command("menumovewithmouse"), Some(Id::MoveMouse));
        assert_eq!(old_row_command("default"), None);
    }

    /// One help box holds several lines: Fly's keys one a line, as bound, Left / Right as L / R;
    /// Pick Other Points' three lines; the player's Load On Inject line; no help on Pick Target;
    /// "Click to change." on hotkeys.
    #[test]
    fn help_boxes_hold_lines() {
        let d = test_prefs();
        let fly = items(Page::Fly, d).into_iter().find(|i| i.id == Id::Fly).unwrap();
        let k = crate::keys::get();
        assert_eq!(fly.help, format!("W A S D move\n{} up\n{} down\n{} sprint", short_key(k.up), short_key(k.down), short_key(k.fast)));
        assert_eq!(fly.help.lines().count(), 4);
        assert_eq!(short_key(0xA0), "L Shift");
        assert_eq!([short_key(0xA2), short_key(0xA3)], ["L Ctrl", "R Ctrl"]);
        assert_eq!(short_key(0x20), "Space");
        let pick = items(Page::Credits, d).into_iter().find(|i| i.id == Id::PickOther).unwrap();
        assert_eq!(pick.help, "Open a shop within 20 s.\nPick the amount you want.\nCredits then changes that one.");
        let aim = items(Page::Target, d).into_iter().find(|i| i.id == Id::Aim).unwrap();
        assert_eq!(aim.help, "");
    }

    /// Under the command box: a full command shows itself and its values, never what it does; part
    /// of one still lists the matches.
    #[test]
    fn full_command_shows_its_values_under_the_box() {
        let d = test_prefs();
        assert_eq!(matches(d, "firerate 0"), ["firerate 0.00 - 1000000.00"]);
        assert_eq!(matches(d, "FireRate"), ["firerate 0.00 - 1000000.00"]);
        assert_eq!(matches(d, "flyspeed"), ["flyspeed 0.01 - 10000.00"]);
        assert_eq!(matches(d, "esprange 300"), ["esprange 25 - 1500 m"]);
        assert_eq!(matches(d, "fly"), ["fly"]);
        assert!(matches(d, "").is_empty());
        let part = matches(d, "firera");
        assert!(part.iter().any(|l| l.starts_with("firerate - ")), "{part:?}");
        assert_eq!(matches(d, "zzzz"), vec!["No matching commands".to_string()]);
    }

    /// Left / Right in the command box: the caret moves one character at a time and stops at both
    /// ends; typing and Backspace work at the caret, Ctrl+Backspace takes the word before it.
    #[test]
    fn box_caret_moves_and_edits_there() {
        let mut t = String::from("credits 2147483647S");
        // The caret starts at the end; Left once: before the S.
        let at = back_one(&t, caret_in(&t, None));
        assert_eq!(&t[at..], "S");
        // Backspace there takes the 7, not the S; typing puts it back there, caret after it.
        let at = erase(&mut t, at, false);
        assert_eq!((t.as_str(), &t[at..]), ("credits 214748364S", "S"));
        let mut st = new_state(None);
        (st.edit, st.caret) = (Some(t.clone()), Some(at));
        type_at(&mut st, '7');
        assert_eq!((st.edit.as_deref(), st.caret), (Some("credits 2147483647S"), Some(at + 1)));
        // At 60 characters nothing more goes in, wherever the caret is.
        (st.edit, st.caret) = (Some("x".repeat(60)), Some(3));
        type_at(&mut st, '7');
        assert_eq!((st.edit.as_deref().map(str::len), st.caret), (Some(60), Some(3)));
        // Right past the end and Left past the start stop there; Backspace at the start: nothing.
        t.insert(at, '7');
        assert_eq!(ahead_one(&t, ahead_one(&t, at + 1)), t.len());
        let start = (0..40).fold(t.len(), |a, _| back_one(&t, a));
        assert_eq!(start, 0);
        assert_eq!(erase(&mut t, 0, false), 0);
        assert_eq!(t, "credits 2147483647S");
        // Ctrl+Backspace: the word before the caret, what is after it stays.
        let mut f = String::from("fly speed 50");
        assert_eq!(erase(&mut f, 9, true), 4);
        assert_eq!(f, "fly  50");
        // At the end, as before the caret: the last word and the spaces after it.
        let mut g = String::from("fly speed  ");
        assert_eq!(erase(&mut g, 11, true), 4);
        assert_eq!(g, "fly ");
        // A caret past the end or inside a character lands on a character's start.
        assert_eq!(caret_in("abc", Some(10)), 3);
        assert_eq!(caret_in("a\u{e9}", Some(2)), 1);
        assert_eq!((back_one("a\u{e9}", 3), ahead_one("a\u{e9}", 1)), (1, 3));
        // The keys themselves, on the box's state.
        (st.edit, st.caret) = (Some("abc".into()), None);
        assert!(box_key(&mut st, 0x25));
        assert_eq!(st.caret, Some(2));
        box_key(&mut st, 0x25);
        box_key(&mut st, 0x27);
        assert_eq!(st.caret, Some(2));
        for _ in 0..5 {
            box_key(&mut st, 0x27);
        }
        assert_eq!(st.caret, Some(3));
        st.edit = None;
        assert!(!box_key(&mut st, 0x25), "no box, nothing to do");
    }

    /// Force Me To Read Warnings off: no warning page blocks anything, from anywhere; on, the menu
    /// asks as before.
    #[test]
    fn force_read_off_skips_every_warning() {
        let off = Prefs { force_read: false, skip_accepted: false, auto_box: false, auto_key: false, ..Prefs::default() };
        for from in [FROM_MENU, FROM_BOX, FROM_KEY, FROM_PROFILE] {
            assert!(warn_skipped(&off, from, false), "{from}");
        }
        let on = Prefs { force_read: true, ..off };
        assert!(!warn_skipped(&on, FROM_MENU, false));
        assert!(!warn_skipped(&on, FROM_BOX, false));
        // The other warning pages' text is warning text too.
        for page in [Page::Unload, Page::Risk, Page::ProfileDelete(0)] {
            let rows = items(page, on);
            assert!(rows.iter().any(|i| i.id == Id::WarnText), "{page:?}");
            assert!(!rows.iter().any(|i| i.id == Id::Info), "{page:?}");
        }
    }

    /// A Credits amount typed in the command box, in command.request or in a box a
    /// hotkey opened reads the credits warning first, as a click on the row does. A box opened by
    /// that click asked already.
    #[test]
    fn typed_credits_read_the_warning_first() {
        assert!(typed_credits(Some("5000"), FROM_BOX));
        assert!(typed_credits(Some("5000"), FROM_KEY));
        assert!(!typed_credits(Some("5000"), FROM_MENU), "the click on the row asked already");
        assert!(!typed_credits(None, FROM_BOX), "no amount: the box opens, its Enter asks");
        assert!(!typed_credits(Some("lots"), FROM_BOX));
        assert!(!typed_credits(Some("inf"), FROM_BOX));
        // The warning page opens (Automatically Proceed off, so where it came from does not matter
        // here).
        let mut st = on_notifications();
        st.prefs = Prefs { force_read: true, skip_accepted: false, auto_box: false, auto_key: false, ..st.prefs };
        if let Ok(mut t) = RISK_SHOWN.lock() {
            *t = None;
        }
        assert!(ask_risk(&mut st));
        assert_eq!(st.stack.last().map(|t| t.0), Some(Page::Risk));
    }

    #[test]
    fn game_pages() {
        // Disables, then Rendering, as Stand's Game tab; no Camera page; Progress last.
        let prefs = test_prefs();
        let game = items(Page::Game, prefs);
        assert_eq!(game.iter().map(|i| (i.id, i.row.text.as_str())).collect::<Vec<_>>(), [
            (Id::Open(Page::Disables), "Disables"),
            (Id::Open(Page::Rendering), "Rendering"),
            (Id::Open(Page::Progress), "Progress")
        ]);
        let rows: Vec<(Id, String)> = items(Page::Disables, prefs).into_iter().map(|i| (i.id, i.row.text)).collect();
        assert_eq!(rows, [(Id::NoShake, "Screen Shake".to_string()), (Id::NvgZone, "NVG In Erewhon".to_string())]);
        let rows: Vec<(Id, String)> = items(Page::Rendering, prefs).into_iter().map(|i| (i.id, i.row.text)).collect();
        // "No NVG Overlay" since.
        assert_eq!(rows, [(Id::NoNvgOverlay, "No NVG Overlay".to_string())]);
        assert_eq!(page_name(Page::Rendering), "Rendering");
        assert!(!items(Page::Experiments, prefs).iter().any(|i| i.id == Id::NvgZone));
        // The typed commands stay as they were.
        assert_eq!(command(Id::NoShake).as_deref(), Some("noscreenshake"));
        assert_eq!(command(Id::NvgZone).as_deref(), Some("nvginerewhon"));
        assert_eq!(command(Id::NoNvgOverlay).as_deref(), Some("nonvgoverlay"));
    }

    #[test]
    fn saved_commands_tree() {
        let rows = vec![("Self > God Mode".to_string(), "Saved".to_string()), ("World > ESP > Range".to_string(), "Saved".to_string())];
        let text = write_tree(&rows);
        assert_eq!(text, "Tree Compatibility Version: 1
Self
	God Mode: Saved
World
	ESP
		Range: Saved
");
        assert_eq!(read_tree(&text), rows);
    }

    #[test]
    fn imported_profile_reads_like_ours() {
        let ours = "Tree Compatibility Version: 1\nSelf\n\tGod Mode: On\nWorld\n\tESP\n\t\tRange: 300\n";
        let pasted = "\u{feff}Tree Compatibility Version: 1\r\nSelf\r\n    God Mode: On\r\nWorld\r\n    ESP\r\n        Range: 300\r\n";
        assert_eq!(read_tree(pasted), read_tree(ours));
        assert_eq!(read_tree(ours)[1], ("World > ESP > Range".to_string(), "300".to_string()));
    }

    #[test]
    fn spot_command_names() {
        assert_eq!([squash("Spot 1"), squash("erehwon"), squash("Erewhon Hub")], ["spot1", "erehwon", "erewhonhub"]);
    }

    /// A spot file saved with a byte-order mark (PowerShell's utf8, some editors)
    /// still gives its spot; before, its X line was not seen.
    #[test]
    fn spot_file_with_byte_order_mark() {
        assert_eq!(place_pos("\u{feff}X: 1.5\r\nY: -2\r\nZ: 3\r\n"), Some([1.5, -2.0, 3.0]));
    }

    #[test]
    fn value_steps_and_position() {
        assert_eq!([step(5.0, 1), step(999.0, 1), step(1000.0, 1), step(1005.0, -1), step(1000.0, -1)], [1.0, 1.0, 5.0, 5.0, 1.0]);
        assert_eq!([step(-1000.0, -1), step(-1000.0, 1), step(-1005.0, 1), step(-3.0, -1)], [5.0, 1.0, 5.0, 1.0]);
        assert_eq!(place_pos("X: -3857.6\nY: 3057.7\nZ: 457.9\n"), Some([-3857.6, 3057.7, 457.9]));
        assert_eq!(place_pos("X: 1\nY: 2\n"), None);
        assert_eq!(file_name("  a/b:c?  "), "abc");
        assert_eq!([pixel(1323.4), pixel(-40000.0), pixel(40000.0), pixel(-0.6)], [1323, -32768, 32767, -1]);
        let prefs = test_prefs();
        let rows = items(Page::Profiles, prefs);
        assert_eq!(rows[1].row.text, "Open Folder");
        assert!(rows[2].row.text == "Profiles" && rows[2].row.kind == Kind::Header);
    }

    #[test]
    fn weapon_rows_and_new_switches() {
        let prefs = test_prefs();
        let names: Vec<String> = items(Page::Weapon, prefs).into_iter().map(|i| i.row.text).collect();
        assert_eq!(names, ["No Recoil", "No Spread", "No Bullet Drop", "No Scoped Sway", "Infinite Mag", "Infinite Reserve", "Damage Multiplier", "Fire Rate"]);
        let names: Vec<String> = items(Page::SilentAim, prefs).into_iter().map(|i| i.row.text).collect();
        assert_eq!(names, ["Silent Aim", "Silent Aim FOV", "Silent Aim Range", "Show FOV Circle", "Priority", "Magic Bullets", "No Range Loss", "Instant Hit"]);
        assert_eq!(command(Id::SilentAim).as_deref(), Some("silentaim"));
        assert_eq!(command(Id::SilentFov).as_deref(), Some("silentaimfov"));
        assert_eq!(command(Id::SilentRange).as_deref(), Some("silentaimrange"));
        assert_eq!(command(Id::SilentCircle).as_deref(), Some("silentaimcircle"));
        assert_eq!(command(Id::SilentPriority).as_deref(), Some("silentaimpriority"));
        // Off at inject, 20 deg wide and 300 m, circle shown, crosshair first; it hooks the game,
        // so a profile waits for the world.
        assert_eq!(default_state(Id::SilentAim), Some(on_off(false)));
        assert_eq!(default_state(Id::SilentFov).as_deref(), Some("20"));
        assert_eq!(default_state(Id::SilentRange).as_deref(), Some("300"));
        assert_eq!(default_state(Id::SilentCircle), Some(on_off(true)));
        assert_eq!(default_state(Id::SilentPriority).as_deref(), Some("Crosshair"));
        assert!([Id::SilentAim, Id::SilentFov, Id::SilentRange, Id::SilentCircle, Id::SilentPriority].into_iter().all(has_default));
        assert!(!needs_world(Id::SilentCircle) && !needs_world(Id::SilentPriority));
        // The three shot switches and No Bullet Drop: off at inject, their own commands.
        for (id, c) in [(Id::SilentMagic, "magicbullets"), (Id::SilentNoRange, "norangeloss"), (Id::SilentInstant, "instanthit"), (Id::SilentNoDrop, "nobulletdrop")] {
            assert_eq!(command(id).as_deref(), Some(c));
            assert_eq!(default_state(id), Some(on_off(false)));
            assert!(has_default(id));
        }
        // Old addresses (hotkeys, profiles) follow the rows into the list, and the list into
        // Experiments.
        assert_eq!(moved("Self > Weapons > Silent Aim FOV").as_deref(), Some("Aurora > Experiments > Silent Aim > Silent Aim FOV"));
        assert_eq!(moved("Self > Weapons > Silent Aim Range").as_deref(), Some("Aurora > Experiments > Silent Aim > Silent Aim Range"));
        assert_eq!(moved("Self > Weapons > Silent Aim > Magic Bullets").as_deref(), Some("Aurora > Experiments > Silent Aim > Magic Bullets"));
        assert_eq!(moved("Self > Weapons > Silent Aim").as_deref(), Some("Aurora > Experiments > Silent Aim"));
        assert_eq!(moved("Self > Weapons > No Recoil"), None);
        let on = Prefs { experimental: true, ..prefs };
        for (addr, id) in [
            ("Aurora > Experiments > Silent Aim > Silent Aim", Id::SilentAim),
            ("Aurora > Experiments > Silent Aim > Silent Aim FOV", Id::SilentFov),
            ("Aurora > Experiments > Silent Aim > Instant Hit", Id::SilentInstant),
        ] {
            assert_eq!(resolve(addr, on).map(|f| f.item.id), Some(id), "{addr}");
            assert!(resolve(addr, prefs).is_none(), "{addr} while off");
        }
        assert!(needs_world(Id::SilentAim) && needs_world(Id::SilentNoDrop) && !needs_world(Id::SilentFov));
        // No Bullet Drop is a weapon row: there with the switch off, and kept in profiles.
        let nd = resolve("Self > Weapons > No Bullet Drop", prefs).expect("No Bullet Drop");
        assert_eq!(nd.item.id, Id::SilentNoDrop);
        assert_eq!(profile_state(&nd, prefs).as_deref(), Some("Off"));
        assert_eq!(command(Id::FireRate).as_deref(), Some("firerate"));
        assert_eq!(command(Id::Reserve).as_deref(), Some("infinitereserve"));
        assert_eq!(default_state(Id::Reserve), Some(on_off(false)));
        assert!(has_default(Id::Reserve));
        // Its Menu switch went: the old command says so, no error.
        let names: Vec<String> = items(Page::Suppress, prefs).into_iter().map(|i| i.row.text).collect();
        assert_eq!(names, ["Command Box", "Hotkey"]);
        assert!(GONE_COMMANDS.iter().any(|g| g.0 == "suppressgenericresponsesmenu"));
        assert!(gone("Aurora > Settings > Notifications > Suppress Generic Responses > Menu"));
    }

    #[test]
    fn menu_look_and_coordinates_are_profile_rows() {
        // A profile keeps the menu's look, as Stand's.
        let prefs = Prefs { accent: 2, toast_corner: 3, x: Some(30), ..Prefs::default() };
        let colour = items(Page::MenuColours, prefs).remove(0);
        assert_eq!(state_of(&colour, prefs).as_deref(), Some("Blue"));
        let toast = items(Page::NoteLook, prefs).into_iter().find(|i| i.id == Id::ToastPosition).unwrap();
        assert_eq!(state_of(&toast, prefs).as_deref(), Some("Top Right"));
        let left = items(Page::Position, prefs).remove(0);
        assert_eq!(state_of(&left, prefs).as_deref(), Some("30"));
        let quiet = items(Page::Suppress, prefs).remove(0);
        assert_eq!(state_of(&quiet, prefs), default_state(Id::QuietBox));
        // Defaults as Prefs::default has them, so a fresh inject lists nothing.
        let d = Prefs::default();
        let pages = [
            Page::MenuColours, Page::Position, Page::Tabs, Page::AddressBar, Page::Notifications, Page::NoteLook, Page::CustomPosition, Page::Timing,
            Page::Suppress, Page::Warnings, Page::AutoProceed, Page::MenuSettings,
        ];
        for page in pages {
            for it in items(page, d) {
                // Lists and Flash Notification have no state.
                let Some(now) = state_of(&it, d) else { continue };
                let def = default_state(it.id).unwrap();
                assert!(same_state(it.id, &now, &def), "{}: {now} vs {def}", it.row.text);
                assert!(has_default(it.id), "{}", it.row.text);
            }
        }
        // Teleport > Coordinates too: 0 by default, and Apply Default State puts it back.
        let x = item(Id::Coord(0), "X", Kind::Choice("0".into()), "");
        assert_eq!(state_of(&x, d).as_deref(), Some("0"));
        assert_eq!(default_state(Id::Coord(2)).as_deref(), Some("0"));
        assert!(has_default(Id::Coord(1)));
    }

    #[test]
    fn old_files_become_profile_values() {
        let menu = serde_json::json!({
            "accent": 2, "size": 120, "x": 30.04, "y": 52.0, "no_warnings": true, "quiet_box": false, "quiet_hotkey": true,
            "quiet_menu": false, "tabswidth": 150, "tabsheight": 34, "addressbarheight": 999, "addressbarwidth": 602,
            "toast_corner": 3, "toast_colour": 0
        });
        let settings = serde_json::json!({
            "esp_on": true, "gear_hold": true, "gear_value": 666, "esp_hostile": true, "esp_friendly": false, "esp_players": true,
            "esp_hide_dead": true, "fly_safe": true, "fly_speed": 20.0, "goto_coords": [1.5, -2.0, 0.0], "auto_inject": false,
            "esp_style": {"range_m": 500.0, "labels": true, "edge": false, "bones": true, "width": 3,
                "colors": [[255, 0, 255], [110, 210, 130], [9, 9, 9], [1, 2, 3], [255, 255, 255]]}
        });
        let want: Vec<(Id, String)> = [
            (Id::MenuColour, "Blue"),
            // Menu Size 120% as List Width / Height: 450 x 120 / 85 and 32 x 120 / 85.
            (Id::Layout(LIST_W), "635"),
            (Id::Layout(LIST_H), "45"),
            // 30.04% of 1920 pixels; y's 52% is where the menu always sat, so it is left out.
            // 30.04% of 1920, plus the help boxes' room at Menu Size 120% with Tabs Width 150 (X
            // was the bar), plus the bar to the wall: 830 + 723.
            (Id::PosX, "1553"),
            // Disable Warnings On: both Automatically Proceed switches (Skip Already-Accepted
            // Warnings is on by default, so it is left out).
            (Id::AutoBox, "On"),
            (Id::AutoKey, "On"),
            (Id::QuietKey, "On"),
            (Id::Layout(0), "150"),
            // 999 is past the most, as the old load clamped it.
            (Id::Layout(2), "80"),
            // The notifications followed Menu Size: 120% of their usual 100%.
            (Id::Layout(TOAST_SIZE), "120"),
            (Id::Layout(NOTE_TEXT), "120"),
            (Id::ToastPosition, "Top Right"),
            (Id::EspFriendlies, "Off"),
            (Id::EspRange, "500"),
            (Id::EspBones, "Bones"),
            (Id::EspLines, "3"),
            (Id::EspEdge, "Off"),
            (Id::EspColour(0), "Pink"),
            (Id::EspColour(3), "#010203"),
            // 20 m/s in settings.json is Speed 2.50 now (x 8 m/s).
            (Id::FlySpeed, "2.50"),
            // Sprint was 4 x Speed then: 10.00 on its own now.
            (Id::FlySprint, "10.00"),
            (Id::Coord(0), "1.50"),
            (Id::Coord(1), "-2"),
        ]
        .into_iter()
        .map(|(id, v)| (id, v.to_string()))
        .collect();
        // Defaults are left out; ESP's switch and the gear hold are not carried (see old_values).
        assert_eq!(old_values(Some(&settings), Some(&menu), (1920, 1080)), want);
        assert!(old_values(None, None, (1920, 1080)).is_empty());
        // Values of the wrong kind are skipped, not guessed.
        assert!(old_values(Some(&serde_json::json!({"fly_speed": "fast", "esp_style": 3})), Some(&serde_json::json!({"accent": "pink", "x": null})), (1920, 1080)).is_empty());
    }

    #[test]
    fn old_values_only_fill_what_a_profile_lacks() {
        // Written by hand: no newline at the end, Menu Colour at the address it had before a move.
        let profile = "Tree Compatibility Version: 1\nSelf\n\tGod Mode: On\nMenu\n\tAppearance\n\t\tMenu Colour: Teal\n\tSettings\n\t\tAppearance\n\t\t\tMenu Size: 150";
        let lines: Vec<(String, String)> = [
            (MENU_SIZE_ADDR, "120"),
            ("Aurora > Settings > Appearance > Colours > Menu Colour", "Blue"),
            ("World > Teleport > X", "1.50"),
            (AUTO_BOX_ADDR, "On"),
        ]
        .into_iter()
        .map(|(a, v)| (a.to_string(), v.to_string()))
        .collect();
        let (text, added) = add_missing(profile, &lines);
        assert_eq!(added, 2);
        assert!(text.starts_with(profile), "the player's lines stay as they were");
        let read = read_tree(&text);
        assert!(read.contains(&("Menu > Settings > Appearance > Menu Size".into(), "150".into())));
        assert!(!read.iter().any(|(_, v)| v == "120" || v == "Blue"), "the player's values win: {read:?}");
        assert!(read.contains(&lines[2]) && read.contains(&lines[3]));
        assert_eq!(read.len(), 5);
        // A second time adds nothing.
        assert_eq!(add_missing(&text, &lines), (text.clone(), 0));
        // A new profile gets the version line and every line.
        let (fresh, n) = add_missing("", &lines);
        assert_eq!(n, 4);
        assert!(fresh.starts_with("Tree Compatibility Version: 1\n"));
        assert_eq!(read_tree(&fresh), lines);
    }

    #[test]
    fn fly_rows_are_multipliers() {
        let prefs = test_prefs();
        let rows = items(Page::Fly, prefs);
        let speed = rows.iter().find(|i| i.id == Id::FlySpeed).unwrap();
        let sprint = rows.iter().find(|i| i.id == Id::FlySprint).unwrap();
        assert_eq!(speed.row.kind, Kind::Choice("1.00".into()));
        assert_eq!(sprint.row.kind, Kind::Choice("4.00".into()));
        assert_eq!(sprint.help, format!("How fast you go while you hold [{}].", key_name(fly::left_only(crate::keys::get().fast))));
        assert_eq!(range(Id::FlySpeed, test_prefs()), "0.01 to 10000.00");
        assert_eq!(range(Id::FlySprint, test_prefs()), "0.01 to 10000.00");
        for it in &rows {
            assert!(!it.help.contains("m/s") && !format!("{:?}", it.row.kind).contains("m/s"), "{}", it.row.text);
        }
        assert_eq!(default_state(Id::FlySpeed).as_deref(), Some("1.00"));
        assert_eq!(default_state(Id::FlySprint).as_deref(), Some("4.00"));
        assert_eq!(state_of(speed, prefs), default_state(Id::FlySpeed));
        assert!(typed(Id::FlySpeed) && typed(Id::FlySprint));
    }

    #[test]
    fn hop_rows() {
        let prefs = test_prefs();
        let rows = items(Page::Fly, prefs);
        let up = rows.iter().find(|i| i.id == Id::HopUp).unwrap();
        let height = rows.iter().find(|i| i.id == Id::HopHeight).unwrap();
        assert_eq!(up.row.kind, Kind::Toggle(true));
        // Today's 2.5 units in the HUD's metres.
        assert_eq!(height.row.kind, Kind::Choice("3.75".into()));
        assert_eq!(default_state(Id::HopUp).as_deref(), Some("On"));
        assert_eq!(default_state(Id::HopHeight).as_deref(), Some("3.75"));
        assert_eq!(state_of(up, prefs), default_state(Id::HopUp));
        assert_eq!(state_of(height, prefs), default_state(Id::HopHeight));
        assert!(typed(Id::HopHeight) && !typed(Id::HopUp));
        assert!(has_default(Id::HopUp) && has_default(Id::HopHeight));
        assert!(!needs_world(Id::HopUp) && !needs_world(Id::HopHeight));
        assert_eq!(command(Id::HopUp).as_deref(), Some("flyhopup"));
        assert_eq!(command(Id::HopHeight).as_deref(), Some("flyhopheight"));
        assert_eq!(range(Id::HopHeight, test_prefs()), "0.50 to 15.00 m");
        assert_eq!(limits(Id::HopHeight, test_prefs()), Some((0.5, 15.0)));
        // Left / Right go 0.10 and round at the ends, as the other value rows.
        let go = |now: f64, dir: i32| fly::clamp_hop(stepped(now, FLY_STEP, dir, fly::HOP_MIN_M, fly::HOP_MAX_M));
        assert_eq!([go(3.75, 1), go(3.75, -1)], [3.85, 3.65]);
        assert_eq!([go(0.5, -1), go(15.0, 1), go(0.55, -1), go(14.95, 1)], [15.0, 0.5, 0.5, 15.0]);
        // Safe Fly's help line.
        let safe = rows.iter().find(|i| i.id == Id::SafeFly).unwrap();
        assert_eq!(safe.help, "Turns on God Mode while you fly,\nuntil you land.");
        assert!(rows.iter().all(|i| i.row.text != "Stand While Flying"));
    }

    #[test]
    fn old_stand_while_flying_lines_are_ignored() {
        // An old profile's line goes quietly; the other lines stay.
        let text = "Tree Compatibility Version: 2\nSelf\n\tMovement\n\t\tFly\n\t\t\tStand While Flying: On\n\t\t\tSpeed: 2.00\n\tGod Mode: On\n";
        let rows = profile_rows(text);
        assert!(rows.iter().all(|r| !r.0.ends_with("Stand While Flying")), "{rows:?}");
        assert!(rows.iter().any(|r| r.0 == "Self > Movement > Fly > Speed" && r.1 == "2.00"), "{rows:?}");
        assert!(rows.iter().any(|r| r.0 == "Self > God Mode"), "{rows:?}");
        assert!(gone("Self > Movement > Fly > Stand While Flying"));
        assert!(!gone("Self > Movement > Fly > Safe Fly") && !gone("Self > Movement > Fly"));
        // No row has it, and its old command is not a command any more.
        let prefs = test_prefs();
        assert!(all_rows(prefs).iter().all(|f| !gone(&f.addr)));
        assert!(row_commands(prefs).iter().all(|c| !GONE_COMMANDS.iter().any(|g| g.0 == c.0)));
        // Load State's line lookup finds nothing for it either.
        assert_eq!(line_of(text, "Self > Movement > Fly > Stand While Flying"), None);
    }

    #[test]
    fn steps_go_round_at_the_ends() {
        let fly = |now: f64, dir: i32| fly::clamp(stepped(now, FLY_STEP, dir, fly::MIN, fly::MAX));
        // 2.00 goes to 2.10 or 1.90.
        assert_eq!([fly(2.0, 1), fly(2.0, -1)], [2.1, 1.9]);
        // Down to 0.01, then round to the top; the top goes round to 0.01.
        assert_eq!([fly(0.1, -1), fly(0.05, -1), fly(0.01, -1)], [0.01, 0.01, 10_000.0]);
        assert_eq!([fly(10_000.0, 1), fly(9_999.95, 1), fly(10_000.0, -1), fly(0.01, 1)], [0.01, 10_000.0, 9_999.9, 0.11]);
        // Many steps never drift off two decimals.
        let mut v = 1.0;
        for _ in 0..37 {
            v = fly(v, 1);
        }
        assert_eq!(format!("{v:.2}"), "4.70");
        assert_eq!(v, 4.7);
        // Every other value row keeps its own step and ends.
        assert_eq!([stepped(1490.0, 25.0, 1, 25.0, 1500.0), stepped(1500.0, 25.0, 1, 25.0, 1500.0), stepped(25.0, 25.0, -1, 25.0, 1500.0)], [1500.0, 25.0, 1500.0]);
        assert_eq!([stepped(0.0, step(0.0, -1), -1, 0.0, 100.0), stepped(100.0, step(100.0, 1), 1, 0.0, 100.0)], [100.0, 0.0]);
        assert_eq!([stepped(100_000.0, step(100_000.0, 1), 1, -COORD_MAX, COORD_MAX), stepped(-100_000.0, 5.0, -1, -COORD_MAX, COORD_MAX)], [-100_000.0, 100_000.0]);
        assert_eq!([stepped(999.0, step(999.0, 1), 1, 0.0, 1000.0), stepped(1000.0, step(1000.0, 1), 1, 0.0, 1000.0)], [1000.0, 0.0]);
        assert_eq!(stepped(60.0, 1.0, -1, 60.0, 200.0), 200.0);
        // A game value past the end comes back to the end, not round.
        assert_eq!(stepped(5160.0, 5.0, 1, 0.0, 1000.0), 1000.0);
        assert_eq!(stepped(5160.0, 5.0, -1, 0.0, 1000.0), 1000.0);
        assert_eq!(stepped(-3.0, 1.0, -1, 0.0, 1000.0), 0.0);
        // Dropped Item Gear Score goes round 1 to 1000.
        assert_eq!([stepped(1.0, step(1.0, -1), -1, 1.0, 1000.0), stepped(1000.0, step(1000.0, 1), 1, 1.0, 1000.0)], [1000.0, 1.0]);
    }

    #[test]
    fn gear_score_over_1000_only_with_experimental() {
        // The player's pick: over 1000 only while Enable Experimental Features is on; while off, no
        // row says more is possible.
        let off = test_prefs();
        let on = Prefs { experimental: true, ..off };
        for id in [Id::GearValue, Id::GearTotal, Id::GearSlot(0), Id::GearSlot(7), Id::GearOwned] {
            assert_eq!(limits(id, off), Some((0.0, 1000.0)), "{id:?}");
            assert_eq!(limits(id, on), Some((0.0, 1_000_000.0)), "{id:?}");
        }
        assert_eq!([range(Id::GearTotal, off), range(Id::GearOwned, on)], ["0 - 1000", "0 - 1000000"]);
        assert_eq!(range(Id::GearValue, off), "0 - 1000 (0 is the game's own)");
        assert_eq!(range(Id::GearValue, on), "0 - 1000000 (0 is the game's own)");
        // The player's note on each gear score number while on, nothing while off.
        let rows = |p: Prefs| items(Page::GearScore, p).into_iter().filter(|i| matches!(i.id, Id::GearValue | Id::GearTotal | Id::GearSlot(_) | Id::GearOwned)).collect::<Vec<_>>();
        assert_eq!(rows(on).len(), 11);
        assert!(rows(on).iter().all(|i| i.help == "Recommended max is 600.\nIf you set an absurd number,\nuse at your own risk."));
        assert!(rows(off).iter().all(|i| i.help.is_empty()));
        // Typed and stepped values.
        assert_eq!([gear_typed(5000.0, off), gear_typed(5000.0, on), gear_typed(2e6, on), gear_typed(-3.0, on), gear_typed(749.6, off)], [1000, 5000, 1_000_000, 0, 750]);
        assert_eq!(stepped(1.0, step(1.0, -1), -1, 1.0, f64::from(gear_max(on))), 1_000_000.0);
        // Profiles keep only the last value up to 1000, and the switch going off brings it back.
        // Only this test sets GEAR; another test turning the switch off brings back the same value.
        let before = (gear(), gear_normal());
        let mut st = on_notifications();
        set_gear(750);
        set_experimental(&mut st, true);
        set_gear(5000);
        let row = all_rows(st.prefs).into_iter().find(|f| f.item.id == Id::GearValue).expect("its row");
        assert_eq!(profile_state(&row, st.prefs).as_deref(), Some("750"));
        set_experimental(&mut st, false);
        assert_eq!(gear(), (750, true));
        *GEAR_NORMAL.lock().unwrap_or_else(|e| e.into_inner()) = before.1;
        *GEAR.lock().unwrap_or_else(|e| e.into_inner()) = before.0;
    }

    #[test]
    fn experiments_never_in_a_profile() {
        // The player's rule: the switch saves; nothing inside Experiments does, and those rows
        // always start at their default (Load and Load State put them there).
        let on = Prefs { experimental: true, ..test_prefs() };
        let rows = all_rows(on);
        let inside: Vec<&Found> = rows.iter().filter(|f| in_experiments(&f.addr) && state_of(&f.item, on).is_some()).collect();
        assert!(inside.len() >= 5, "Experiments rows with a state: {}", inside.len());
        for f in &inside {
            assert_eq!(profile_state(f, on), None, "{}", f.addr);
            assert!(default_state(f.item.id).is_some(), "{} has a default to start at", f.addr);
        }
        let switch = rows.iter().find(|f| f.addr == EXPERIMENTAL_ADDR).expect("the switch");
        assert_eq!(profile_state(switch, on).as_deref(), Some("On"));
        let speed = rows.iter().find(|f| f.item.id == Id::FlySpeed).expect("Fly Speed");
        assert_eq!(profile_state(speed, on), state_of(&speed.item, on));
        // Lines an older build saved inside Experiments are left out at Load, not counted as
        // skipped; the switch and the other rows load as before.
        let lines = vec![
            (EXPERIMENTAL_ADDR.to_string(), "On".to_string()),
            (inside[0].addr.clone(), "On".to_string()),
            (inside[inside.len() - 1].addr.clone(), "5".to_string()),
            (speed.addr.clone(), "2.00".to_string()),
        ];
        let (kept, left) = loadable(lines);
        assert_eq!(left, 2);
        assert_eq!(kept.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), [EXPERIMENTAL_ADDR, speed.addr.as_str()]);
        // A hand-edited gear line over 1000 loads as 1000.
        assert_eq!([profile_value(Id::GearValue, "5000".into()), profile_value(Id::GearValue, "750".into()), profile_value(Id::FlySpeed, "5000".into())], ["1000", "750", "5000"]);
    }

    /// Load and Load State put the rows inside Experiments at their default, also with the switch
    /// off. Review: the switch going off left Show FOV Circle and the values as they were. Which
    /// rows only: setting one here raced the tests that read them.
    #[test]
    fn load_puts_experiments_at_their_default() {
        let off = test_prefs();
        assert!(!off.experimental);
        let all = experiment_ids(off, EXPERIMENTS_ADDR, true);
        for id in [Id::SilentAim, Id::SilentFov, Id::SilentRange, Id::SilentCircle, Id::SilentPriority, Id::FlyCalm] {
            assert!(all.contains(&id), "{id:?} goes to its default at Load");
        }
        // Load State on one row: that row; on another list: none.
        let on = Prefs { experimental: true, ..off };
        let circle = all_rows(on).into_iter().find(|f| f.item.id == Id::SilentCircle).expect("Show FOV Circle");
        assert_eq!(experiment_ids(off, &circle.addr, false), [Id::SilentCircle]);
        assert!(experiment_ids(off, "Aurora > Settings", true).is_empty());
    }

    /// The player's rule: no test may overwrite anything of the player's. The menu's folder is not there in
    /// tests, so Hotkeys.txt, Places, Profiles, Saved Commands, Meta State.txt and command.request
    /// are neither read nor written.
    #[test]
    fn unit_tests_touch_no_aurora_files() {
        assert_eq!(folder(), None);
        assert_eq!((profiles_dir(), profile_path("Main"), meta_path(), places()), (None, None, None, None));
        assert!(spots().is_empty() && profiles().is_empty());
    }

    #[test]
    fn value_rows_min_and_max() {
        let want: Vec<(Id, (f64, f64))> = vec![
            (Id::FlySpeed, (0.01, 10_000.0)),
            (Id::FlySprint, (0.01, 10_000.0)),
            (Id::HopHeight, (0.5, 15.0)),
            (Id::Damage, (0.0, 1_000_000.0)),
            (Id::FireRate, (0.0, 1_000_000.0)),
            (Id::GearValue, (0.0, 1000.0)),
            (Id::GearTotal, (0.0, 1000.0)),
            (Id::GearSlot(2), (0.0, 1000.0)),
            (Id::GearOwned, (0.0, 1000.0)),
            (Id::Credits, (0.0, 2_147_483_647.0)),
            (Id::Coord(1), (-100_000.0, 100_000.0)),
            (Id::EspRange, (25.0, 1500.0)),
            (Id::SilentFov, (1.0, 90.0)),
            (Id::SilentRange, (10.0, 1000.0)),
            (Id::EspLines, (0.0, 6.0)),
            (Id::Layout(LIST_W), (0.0, 32767.0)),
            (Id::Layout(VISIBLE_ROWS), (3.0, 255.0)),
            (Id::PosX, (-32768.0, 32767.0)),
            (Id::PosY, (-32768.0, 32767.0)),
            (Id::NotifyX, (-32768.0, 32767.0)),
            (Id::Layout(NOTE_TEXT), (50.0, 300.0)),
            (Id::Layout(NOTIFY_WIDTH), (100.0, 1600.0)),
            (Id::Layout(0), (60.0, 400.0)),
            (Id::Layout(5), (1.0, 15.0)),
        ];
        for (id, lim) in want {
            assert_eq!(limits(id, test_prefs()), Some(lim), "{id:?}");
        }
        // Every typed number row has both ends; lists and switches have none.
        for id in [Id::Damage, Id::FireRate, Id::Credits, Id::FlySpeed, Id::FlySprint, Id::HopHeight, Id::GearValue, Id::GearTotal, Id::GearSlot(0), Id::GearOwned, Id::EspRange, Id::SilentFov, Id::SilentRange, Id::EspLines, Id::PosX, Id::PosY, Id::Layout(3), Id::Coord(2)] {
            assert!(typed(id) && limits(id, test_prefs()).is_some(), "{id:?}");
        }
        for id in [Id::MenuColour, Id::EspColour(0), Id::EspHex(0), Id::EspBones, Id::ToastPosition, Id::God, Id::Fly, Id::Spot(0)] {
            assert_eq!(limits(id, test_prefs()), None, "{id:?}");
        }
    }

    #[test]
    fn old_fly_values_upgrade() {
        // 20 m/s with Sprint 8 (8 x 4 = 32 m/s): Speed 2.50, Sprint 1.60 (version 2), so still 20
        // and 32 m/s; version 3's Sprint is 2.50 x 1.60 = 4.00 on its own.
        assert_eq!(upgrade_fly(Some(20.0), Some(8.0)), (Some(2.5), Some(1.6)));
        assert_eq!(fly::metres_per_second(2.5, fly_value(2.5 * 1.6), false), 20.0);
        assert!((fly::metres_per_second(2.5, fly_value(2.5 * 1.6), true) - 32.0).abs() < 1e-9);
        // The old default 8 m/s is 1.00; Sprint alone is against Speed 1.00.
        assert_eq!(upgrade_fly(Some(8.0), None), (Some(1.0), None));
        assert_eq!(upgrade_fly(None, Some(10.0)), (None, Some(5.0)));
        // Not numbers stay out.
        assert_eq!(upgrade_fly(Some(f64::NAN), None), (None, None));
        // Never faster than the old save, and never 8x anything.
        for s in 1..=200 {
            for n in [1, 2, 8, 15, 50, 200] {
                let (Some(sp), Some(sr)) = upgrade_fly(Some(f64::from(s)), Some(f64::from(n))) else { panic!() };
                assert!(fly::metres_per_second(sp, sr, false) <= f64::from(s) + 1e-9, "speed {s}");
                assert!(fly::metres_per_second(sp, sr, false) >= f64::from(s) * 0.95, "speed {s}");
                // Version 3: Sprint is Speed x Sprint, rounded to 0.01 (at most 0.04 m/s more).
                let old_sprint = f64::from(n) * 4.0;
                let new_sprint = fly::metres_per_second(sp, fly_value(sp * sr), true);
                assert!(new_sprint <= old_sprint + 0.041, "sprint {s} {n}: {new_sprint} > {old_sprint}");
            }
        }
    }

    #[test]
    fn old_profile_text_upgrades_once() {
        let old = "Tree Compatibility Version: 1\r\nSelf\r\n\tMovement\r\n\t\tFly\r\n\t\t\tSpeed: 20\r\n\t\t\tSprint: 8\r\n\t\t\tSafe Fly: Off\r\nWorld\r\n\tESP\r\n\t\tRange: 300\r\n";
        assert_eq!(tree_version(old), 1);
        let new = upgrade_profile(old, (1920, 1080));
        assert_eq!(tree_version(&new), 4);
        // Speed 20 m/s is 2.50; Sprint 8 (8 x 4 = 32 m/s) was 1.60 x Speed in version 2 and is 4.00
        // on its own in version 3.
        assert_eq!(
            new,
            "Tree Compatibility Version: 4\nSelf\r\n\tMovement\r\n\t\tFly\r\n\t\t\tSpeed: 2.50\r\n\t\t\tSprint: 4.00\r\n\t\t\tSafe Fly: Off\r\nWorld\r\n\tESP\r\n\t\tRange: 300\r\n"
        );
        // The same as upgrading to 2, then 3, then 4.
        assert_eq!(new, upgrade_v3(&upgrade_v2(&upgrade_v1(old), 1080), (1920, 1080)));
        // A second time changes nothing: version 4 is today's.
        assert_eq!(upgrade_profile(&new, (1920, 1080)), new);
        assert_eq!(read_tree(&new)[3], ("World > ESP > Range".to_string(), "300".to_string()));
        // The old Speed line is Fly Speed now.
        assert_eq!(moved(&read_tree(&new)[0].0).as_deref(), Some(FLY_SPEED_ADDR));
        // Old flat lines, no version line, no Sprint (4 x Speed then): Sprint gets Speed x 4.
        let flat = "Self > Movement > Fly > Speed: 4\nSelf > God Mode: On";
        assert_eq!(read_tree(&upgrade_profile(flat, (1920, 1080))), vec![
            ("Self > Movement > Fly > Speed".to_string(), "0.50".to_string()),
            ("Self > God Mode".to_string(), "On".to_string()),
            (FLY_SPRINT_ADDR.to_string(), "2.00".to_string()),
        ]);
        // Profiles are written as version 4.
        assert!(write_tree_v(&[], PROFILE_VERSION).starts_with("Tree Compatibility Version: 4\n"));
        assert_eq!(upgrade_profile("", (1920, 1080)), "Tree Compatibility Version: 4\n");
    }

    #[test]
    fn save_and_load_state_round_trip() {
        let profile = "Tree Compatibility Version: 2\nSelf\n\tGod Mode: On\nGame\n\tDisables\n\t\tNVG Overlay: On\nWorld\n\tESP\n\t\tRange: 300\n";
        let speed = FLY_SPEED_ADDR.to_string();
        let text = set_lines(profile, &[(speed.clone(), "3.50".into())]);
        assert_eq!(line_of(&text, &speed).as_deref(), Some("3.50"));
        // Every other line keeps its value.
        for (a, v) in read_tree(profile) {
            assert_eq!(line_of(&text, &moved(&a).unwrap_or(a.clone())), Some(v), "{a}");
        }
        // Saved again: replaced, not added twice; a default is written too, so it loads back.
        let text = set_lines(&text, &[(speed.clone(), "1.00".into())]);
        assert_eq!(read_tree(&text).iter().filter(|r| r.0 == speed).count(), 1);
        assert_eq!(line_of(&text, &speed).as_deref(), Some("1.00"));
        // A row that moved is found at its old address, and saved at its new one.
        let nvg = "Game > Rendering > No NVG Overlay";
        assert_eq!(line_of(profile, nvg).as_deref(), Some("On"));
        let text = set_lines(profile, &[(nvg.to_string(), "Off".into())]);
        assert_eq!(line_of(&text, nvg).as_deref(), Some("Off"));
        assert!(!read_tree(&text).iter().any(|r| r.0 == "Game > Disables > NVG Overlay"));
        // None: nothing for this row.
        assert_eq!(line_of(profile, "Self > Undetected"), None);
        // An old profile's Fly line is upgraded, not kept in m/s under the new version line.
        let old = "Tree Compatibility Version: 1\nSelf\n\tMovement\n\t\tFly\n\t\t\tSpeed: 16\n";
        let text = set_lines(old, &[("Self > God Mode".to_string(), "On".into())]);
        assert_eq!(tree_version(&text), 4);
        assert_eq!(line_of(&text, &speed).as_deref(), Some("2.00"));
        assert_eq!(line_of(old, &speed).as_deref(), Some("2.00"));
        // Old and new address both there: one line left, the new one.
        let both = "Tree Compatibility Version: 2
Game
	Disables
		NVG Overlay: On
	Rendering
		NVG Overlay: On
";
        let text = set_lines(both, &[(nvg.to_string(), "Off".into())]);
        assert_eq!(read_tree(&text), [(nvg.to_string(), "Off".to_string())]);
    }

    #[test]
    fn state_rows_of_a_row_or_a_list() {
        let prefs = test_prefs();
        let found = |addr: &str, kind: Kind| Found { section: 3, stack: Vec::new(), addr: addr.into(), item: item(Id::NoShake, "x", kind, "") };
        let rows = vec![
            found("Game > Disables", Kind::Sub),
            found("Game > Disables > Screen Shake", Kind::Toggle(false)),
            found("Game > Disables > NVG In Erewhon", Kind::Toggle(true)),
            found("Game > Rendering > NVG Overlay", Kind::Toggle(false)),
        ];
        let addrs = |v: Vec<Found>| v.into_iter().map(|f| f.addr).collect::<Vec<_>>();
        assert_eq!(addrs(state_rows(&rows, "Game > Disables", true, prefs)), ["Game > Disables > Screen Shake", "Game > Disables > NVG In Erewhon"]);
        assert_eq!(addrs(state_rows(&rows, "Game > Rendering > NVG Overlay", false, prefs)), ["Game > Rendering > NVG Overlay"]);
        assert!(state_rows(&rows, "Game > Disables", false, prefs).is_empty(), "a list itself has no state");
    }

    #[test]
    fn nvg_overlay_keeps_its_keys_after_the_move() {
        // "No NVG Overlay": every older address lands on it.
        let now = Some("Game > Rendering > No NVG Overlay");
        assert_eq!(moved("Game > Disables > NVG Overlay").as_deref(), now);
        assert_eq!(moved("Game > No NVG Overlay").as_deref(), now);
        assert_eq!(moved("Settings > No NVG Overlay").as_deref(), now);
        assert_eq!(moved("Game > Rendering > NVG Overlay").as_deref(), now);
        assert_eq!(moved("Game > Rendering > No NVG Overlay"), None);
        // The rows that stayed in Disables did not move.
        assert_eq!(moved("Game > Disables > Screen Shake"), None);
        assert_eq!(moved("Game > Disables > NVG In Erewhon"), None);
        // Their older places.
        assert_eq!(moved("Game > No Screen Shake").as_deref(), Some("Game > Disables > Screen Shake"));
        assert_eq!(moved("Settings > No Screen Shake").as_deref(), Some("Game > Disables > Screen Shake"));
        assert_eq!(moved("Menu > Experiments > NVG In Safe Zones (untested)").as_deref(), Some("Game > Disables > NVG In Erewhon"));
    }

    #[test]
    fn round_two_rows_keep_their_keys() {
        let m = |a: &str| moved(a);
        // The Weapon tab is Self > Weapons, older names too.
        assert_eq!(m("Weapon > No Recoil").as_deref(), Some("Self > Weapons > No Recoil"));
        assert_eq!(m("Weapon > Unlimited Ammo").as_deref(), Some("Self > Weapons > Infinite Mag"));
        assert_eq!(m("Weapon > No Zoomed Sway").as_deref(), Some("Self > Weapons > No Scoped Sway"));
        assert_eq!(m("Settings > No Zoomed Sway").as_deref(), Some("Self > Weapons > No Scoped Sway"));
        assert_eq!(m("Self > Weapons > No Recoil"), None);
        assert_eq!(m("Weapons"), None);
        // Appearance > Toasts rows went to Menu > Settings > Notifications.
        let t = "Menu > Settings > Appearance > Toasts";
        // And on to Appearance > Notifications.
        assert_eq!(m(&format!("{t} > Toast Position")).as_deref(), Some("Aurora > Settings > Appearance > Notifications > Position"));
        assert_eq!(m(&format!("{t} > Toast Size")).as_deref(), Some("Aurora > Settings > Appearance > Notifications > Size"));
        assert_eq!(m(&format!("{t} > Toast Time")).as_deref(), Some("Aurora > Settings > Appearance > Notifications > Timing > Time"));
        assert_eq!(m(&format!("{t} > Toast Colour")).as_deref(), Some("Aurora > Settings > Appearance > Notifications > Border Colour"));
        for name in ["Position", "Custom Position > X", "Width", "Size", "Text Size", "Border Colour", "Background Colour", "Flash Notification", "Timing", "Timing > Time"] {
            assert_eq!(m(&format!("Aurora > Settings > Notifications > {name}")), Some(format!("Aurora > Settings > Appearance > Notifications > {name}")), "{name}");
        }
        assert_eq!(m(t).as_deref(), Some("Aurora > Settings > Appearance > Notifications"));
        assert_eq!(m("Menu > Settings > Notifications > Suppress Generic Responses > Hotkey").as_deref(), Some("Aurora > Settings > Notifications > Suppress Generic Responses > Hotkey"));
        assert_eq!(m("Aurora > Settings > Notifications > Suppress Generic Responses > Hotkey"), None);
        // Position's rows, from every place they had.
        for old in ["Settings > Appearance > Menu Left / Right", "Menu > Appearance > Menu Left / Right", "Menu > Settings > Appearance > Position > Menu Left / Right"] {
            assert_eq!(m(old).as_deref(), Some(POS_X_ADDR), "{old}");
        }
        assert_eq!(m("Menu > Appearance > Menu Up / Down").as_deref(), Some(POS_Y_ADDR));
        // Disable Warnings, from every place it had: its list.
        for old in ["Settings > Disable Warnings", "Menu > Disable Warnings", "Menu > Settings > Warnings > Disable Warnings"] {
            assert_eq!(m(old).as_deref(), Some(AUTO_PROCEED_ADDR), "{old}");
        }
        // Every address they land on is a row today (no Teleport rows here: they read the player's
        // files).
        let today = [
            "Self > Infinite Stamina",
            "Self > Weapons > Infinite Mag",
            POS_X_ADDR,
            POS_Y_ADDR,
            AUTO_PROCEED_ADDR,
            AUTO_BOX_ADDR,
            AUTO_KEY_ADDR,
            SKIP_ACCEPTED_ADDR,
            "Aurora > Settings > Appearance > Notifications > Timing > Time",
            "Aurora > Settings > Appearance > Notifications > Border Colour",
            "Aurora > Settings > Notifications > Suppress Generic Responses > Hotkey",
            LIST_W_ADDR,
            LIST_H_ADDR,
            NOTE_SIZE_ADDR,
            NOTE_TEXT_ADDR,
            "Game > Rendering > No NVG Overlay",
            "Game > Progress > Editor > Gear Score",
            "Game > Progress > Editor > Credits > Credits",
        ];
        let prefs = test_prefs();
        for addr in today {
            let parts: Vec<&str> = addr.split(" > ").collect();
            let mut page = SECTIONS.iter().find(|s| s.1 == parts[0]).unwrap().0;
            for (k, name) in parts.iter().enumerate().skip(1) {
                let it = items(page, prefs).into_iter().find(|i| i.row.text == *name).unwrap_or_else(|| panic!("{addr}: no {name}"));
                if k + 1 < parts.len() {
                    let Id::Open(p) = it.id else { panic!("{addr}: {name} is no list") };
                    page = p;
                }
            }
        }
    }

    #[test]
    fn old_profile_lines_become_todays() {
        let rows = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        // Percent on a 2560 x 1440 screen: as the menu sat (64% of 2560 is 1638).
        let old = rows(&[
            ("Menu > Settings > Appearance > Position > Menu Left / Right", "64.0"),
            ("Menu > Settings > Appearance > Position > Menu Up / Down", "30"),
            ("Self > God Mode", "On"),
        ]);
        // X was the bar: plus the help boxes' 283 px at 1440p, Menu Size 100%; X is the wall since
        // evening: plus the bar to the wall, kept on the screen (2560).
        assert_eq!(upgrade_rows(old, (2560, 1440)), rows(&[("Self > God Mode", "On"), (POS_X_ADDR, "2560"), (POS_Y_ADDR, "432")]));
        // From the oldest address too (960 + 212 + 602 at 1080p); not a number: left out.
        assert_eq!(upgrade_rows(rows(&[("Settings > Appearance > Menu Left / Right", "50")]), (1920, 1080)), rows(&[(POS_X_ADDR, "1774")]));
        // The player's Menu Size counts: at 200% the room is 424 px, the bar to the wall 1204; the
        // Menu Size line becomes List Width / Height and doubles the notifications.
        assert_eq!(
            upgrade_rows(rows(&[("Menu > Settings > Appearance > Menu Size", "200"), ("Settings > Appearance > Menu Left / Right", "50")]), (1920, 1080)),
            rows(&[(LIST_W_ADDR, "1059"), (LIST_H_ADDR, "75"), (POS_X_ADDR, "1920"), (NOTE_SIZE_ADDR, "200"), (NOTE_TEXT_ADDR, "200")])
        );
        assert!(upgrade_rows(rows(&[("Menu > Appearance > Menu Up / Down", "high")]), (1920, 1080)).is_empty());
        // A pixel line already there wins over the percent one.
        let both = rows(&[(POS_X_ADDR, "100"), ("Menu > Settings > Appearance > Position > Menu Left / Right", "50")]);
        assert_eq!(upgrade_rows(both, (1920, 1080)), rows(&[(POS_X_ADDR, "100")]));
        // Disable Warnings On: both Automatically Proceed switches and Skip Already-Accepted.
        let on = upgrade_rows(rows(&[("Menu > Settings > Warnings > Disable Warnings", "On")]), (1920, 1080));
        assert_eq!(on, rows(&[(AUTO_BOX_ADDR, "On"), (AUTO_KEY_ADDR, "On"), (SKIP_ACCEPTED_ADDR, "On")]));
        assert_eq!(upgrade_rows(rows(&[("Settings > Disable Warnings", "on")]), (1920, 1080)).len(), 3);
        // Off is today's default: nothing.
        assert!(upgrade_rows(rows(&[("Menu > Disable Warnings", "Off")]), (1920, 1080)).is_empty());
        // The player's own new line wins.
        let mine = rows(&[(AUTO_KEY_ADDR, "Off"), ("Menu > Settings > Warnings > Disable Warnings", "On")]);
        let got = upgrade_rows(mine, (1920, 1080));
        assert_eq!(got.iter().filter(|r| r.0 == AUTO_KEY_ADDR).collect::<Vec<_>>(), [&(AUTO_KEY_ADDR.to_string(), "Off".to_string())]);
        // Load State and Save State read them the same way (1920 x 1080 in tests; 960 + the help
        // boxes' 212 px + the bar to the wall's 602, once: the saved version 4 text is not moved
        // again).
        let text = "Tree Compatibility Version: 2\nMenu\n\tSettings\n\t\tAppearance\n\t\t\tPosition\n\t\t\t\tMenu Left / Right: 50.0\n\t\tWarnings\n\t\t\tDisable Warnings: On\n";
        assert_eq!(line_of(text, POS_X_ADDR).as_deref(), Some("1774"));
        assert_eq!(line_of(text, AUTO_BOX_ADDR).as_deref(), Some("On"));
        let saved = set_lines(text, &[("Self > God Mode".to_string(), "On".to_string())]);
        assert!(!saved.contains("Menu Left / Right") && !saved.contains("Disable Warnings"), "{saved}");
        assert_eq!(line_of(&saved, POS_X_ADDR).as_deref(), Some("1774"));
        // Old toast lines land on the Notifications rows with the same values.
        let toast = "Tree Compatibility Version: 2\nMenu\n\tSettings\n\t\tAppearance\n\t\t\tToasts\n\t\t\t\tToast Position: Top Right\n\t\t\t\tToast Time: 7\n\t\t\t\tToast Colour: Teal\n";
        assert_eq!(line_of(toast, "Aurora > Settings > Appearance > Notifications > Position").as_deref(), Some("Top Right"));
        assert_eq!(line_of(toast, "Aurora > Settings > Appearance > Notifications > Timing > Time").as_deref(), Some("7"));
        assert_eq!(line_of(toast, "Aurora > Settings > Appearance > Notifications > Border Colour").as_deref(), Some("Teal"));
        // Old menu.json's corners are the first four Positions.
        let v = old_values(None, Some(&serde_json::json!({"toast_corner": 2, "toast_colour": 4, "toastseconds": 9})), (1920, 1080));
        assert_eq!(v, [(Id::Layout(TOAST_TIME), "9".to_string()), (Id::ToastPosition, "Top Left".to_string()), (Id::ToastColour, "Teal".to_string())]);
    }

    #[test]
    fn menu_position_is_pixels() {
        let d = Prefs::default();
        // Not set: where it always sat, its picture at 64% / 52% of the screen (1920 x 1080 in
        // tests). X is the wall: where the list's right edge was drawn at the default
        // X and Menu Size 85% (1228 + 212 + 511), kept on the screen: 1920.
        assert_eq!([menu_pos(&d, 0), menu_pos(&d, 1)], [1920, 561]);
        assert_eq!(default_state(Id::PosX).as_deref(), Some("1920"));
        let p = Prefs { x: Some(-20), y: Some(32767), ..d };
        assert_eq!([menu_pos(&p, 0), menu_pos(&p, 1)], [-20, 32767]);
        assert_eq!(current(Id::PosX, p), "-20");
        assert_eq!(limits(Id::PosX, test_prefs()), Some((-32768.0, 32767.0)));
        assert_eq!(limits(Id::NotifyY, test_prefs()), Some((-32768.0, 32767.0)));
        assert!(typed(Id::PosY) && typed(Id::NotifyX));
        // Steps of 1, round at the ends.
        assert_eq!(pixel(stepped(32767.0, 1.0, 1, PIXEL_MIN, PIXEL_MAX)), -32768);
        assert_eq!(pixel(stepped(1228.0, 1.0, -1, PIXEL_MIN, PIXEL_MAX)), 1227);
    }

    #[test]
    fn findc_finds_by_name() {
        let found = |section: usize, idx: &[usize], addr: &str, id: Id, kind: Kind| {
            let name = addr.rsplit(" > ").next().unwrap().to_string();
            Found { section, stack: idx.iter().map(|&i| (Page::World, i, 0)).collect(), addr: addr.into(), item: item(id, name, kind, "") }
        };
        // As all_rows gives them: a list's rows before the list itself.
        let rows = vec![
            found(0, &[0], "Self > God Mode", Id::God, Kind::Toggle(false)),
            found(1, &[1, 2, 0], "World > ESP > Targets > Hostile", Id::EspEnemies, Kind::Toggle(true)),
            found(1, &[1, 2], "World > ESP > Targets", Id::Open(Page::EspTargets), Kind::Sub),
            found(1, &[1, 0], "World > ESP > ESP", Id::EspOn, Kind::Toggle(true)),
            found(1, &[1, 1], "World > ESP > Range", Id::EspRange, Kind::Choice("150 m".into())),
            found(1, &[1], "World > ESP", Id::Open(Page::Esp), Kind::Sub),
            found(2, &[0, 0], "Game > Disables > Screen Shake", Id::NoShake, Kind::Toggle(false)),
            found(2, &[0, 9], "Game > Disables > Esp Title", Id::Header, Kind::Header),
            found(3, &[5], "Aurora > Settings > Appearance > Colours > Menu Colour", Id::MenuColour, Kind::Choice("Pink".into())),
        ];
        let hits = |t: &str| find_hits(rows.clone(), t).into_iter().map(|h| h.0).collect::<Vec<_>>();
        // "esp": by the shown name only, any case: the ESP list stands for its rows, so ESP's own
        // switch is not listed again under it; Range and Hostile are not named esp (only their
        // commands are); titles are no commands.
        assert_eq!(hits("esp"), ["World > ESP"]);
        assert_eq!(hits("ESP"), ["World > ESP"]);
        assert_eq!(hits("  eSp "), ["World > ESP"]);
        // Commands are not looked at: noscreenshake is not found by its command.
        assert!(hits("noscreen").is_empty());
        assert_eq!(hits("screen"), ["Game > Disables > Screen Shake"]);
        // A row in a list that does not match is found on its own; a matching list without its
        // rows.
        assert_eq!(hits("hostile"), ["World > ESP > Targets > Hostile"]);
        assert_eq!(hits("targets"), ["World > ESP > Targets"]);
        // By name with a space.
        assert_eq!(hits("god mode"), ["Self > God Mode"]);
        assert_eq!(hits("colour"), ["Aurora > Settings > Appearance > Colours > Menu Colour"]);
        // Each hit keeps its full address (the help line) and its own row, so it works as there.
        let all = find_hits(rows.clone(), "range");
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].0.as_str(), all[0].1.id), ("World > ESP > Range", Id::EspRange));
        assert!(hits("nothinglikethis").is_empty());
        assert!(hits("   ").is_empty());
    }

    /// A list opened from findc lives where its row says (as the usual way there),
    /// so its rows show their hotkeys there too.
    #[test]
    fn list_opened_from_findc_keeps_its_address() {
        // Findc's rows come from every page, which reads (and may write) Hotkeys.txt and Places:
        // only with a test APPDATA, never in the player's real Aurora folder.
        if !std::env::var("APPDATA").is_ok_and(|a| a.contains("aurora-test-appdata")) {
            return;
        }
        let mut st = on_notifications();
        st.section = 1;
        st.stack = vec![(Page::World, 1, 0), (Page::Esp, 0, 0)];
        assert_eq!(list_address(&st), "World > ESP");
        // No other test reads FIND.
        *FIND.lock().unwrap() = "esp".into();
        let k = items(Page::Find, st.prefs).iter().position(|i| i.id == Id::Open(Page::Esp)).unwrap();
        st.section = SECTIONS.iter().position(|s| s.0 == Page::Menu).unwrap();
        st.stack = vec![(Page::Menu, 0, 0), (Page::Find, k, 0), (Page::Esp, 0, 0)];
        assert_eq!(list_address(&st), "World > ESP");
        // Find itself: the menu's own place, as before.
        st.stack.pop();
        assert_eq!(list_address(&st), format!("Aurora > {}", items(Page::Menu, st.prefs)[0].row.text));
    }

    #[test]
    fn save_and_load_commands() {
        let profiles = vec!["Main".to_string(), "My Run".to_string(), "thisspot".to_string()];
        let others: Vec<String> = ["savethisspot", "godmode", "esprange"].map(String::from).to_vec();
        let pc = |n: &str| profile_command(n, &profiles, &others);
        assert_eq!(pc("savemain"), Some(ProfileCmd::Save("Main".into())));
        assert_eq!(pc("SAVEMAIN"), Some(ProfileCmd::Save("Main".into())));
        assert_eq!(pc("loadMain"), Some(ProfileCmd::Load("Main".into())));
        assert_eq!(pc("loadmyrun"), Some(ProfileCmd::Load("My Run".into())));
        // Unknown name: said, not run.
        assert_eq!(pc("savenope"), Some(ProfileCmd::Unknown("nope".into())));
        assert_eq!(pc("loadnope"), Some(ProfileCmd::Unknown("nope".into())));
        // Not one: no name, another start, another command (even with a profile of that name), or
        // the start of another command.
        assert_eq!(pc("save"), None);
        assert_eq!(pc("load"), None);
        assert_eq!(pc("godmode"), None);
        assert_eq!(pc("savethisspot"), None);
        assert_eq!(pc("savethis"), None);
        assert_eq!(pc("loadthisspot"), Some(ProfileCmd::Load("thisspot".into())));
    }

    /// A request file is taken once, and only after it is removed; a byte-order mark
    /// at its start is dropped. A test file, never the real one.
    #[test]
    fn request_file_runs_once_when_removed() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let dir = std::env::var_os("AURORA_TEST_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("aurora-test-{}-command.request", std::process::id()));
        std::fs::write(&path, "\u{feff}findc esp\r\nesp").unwrap();
        assert_eq!(take_request(&path).as_deref(), Some("findc esp\r\nesp"));
        assert!(!path.exists());
        assert_eq!(take_request(&path), None, "gone: nothing runs again");
        // Still open in the script that writes it (no delete sharing): it waits, nothing runs.
        std::fs::write(&path, "findc esp").unwrap();
        let held = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();
        assert_eq!(take_request(&path), None);
        assert_eq!(take_request(&path), None);
        assert!(path.exists());
        drop(held);
        assert_eq!(take_request(&path).as_deref(), Some("findc esp"));
        assert!(!path.exists());
    }

    #[test]
    fn warnings_logic() {
        let d = Prefs::default();
        assert!(d.force_read && d.skip_accepted && !d.auto_box && !d.auto_key);
        // Automatically Proceed On Warnings: only for where the action came from.
        let boxed = Prefs { auto_box: true, ..d };
        assert!(warn_skipped(&boxed, FROM_BOX, false));
        assert!(!warn_skipped(&boxed, FROM_KEY, false));
        assert!(!warn_skipped(&boxed, FROM_MENU, false));
        let keyed = Prefs { auto_key: true, ..d };
        assert!(warn_skipped(&keyed, FROM_KEY, false));
        assert!(!warn_skipped(&keyed, FROM_BOX, false));
        assert!(!warn_skipped(&d, FROM_BOX, false) && !warn_skipped(&d, FROM_KEY, false) && !warn_skipped(&d, FROM_MENU, false));
        // Skip Already-Accepted Warnings: once accepted this inject, not again (from anywhere).
        assert!(warn_skipped(&d, FROM_MENU, true));
        assert!(warn_skipped(&d, FROM_KEY, true));
        let ask_again = Prefs { skip_accepted: false, ..d };
        assert!(!warn_skipped(&ask_again, FROM_MENU, true));
        // Force Me To Read Warnings: Yes after 2 s; off, at once; never before it was asked.
        let ms = Duration::from_millis;
        assert!(!yes_shows(true, Some(ms(1999))));
        assert!(yes_shows(true, Some(ms(2000))));
        assert!(yes_shows(false, Some(ms(0))));
        assert!(!yes_shows(false, None) && !yes_shows(true, None));
    }

    #[test]
    fn notifications_stack() {
        // Bottom corners: newest lowest; top corners and Custom: newest highest. 0 is the oldest.
        assert_eq!(note_order(3, 0), [0, 1, 2]);
        assert_eq!(note_order(3, 1), [0, 1, 2]);
        assert_eq!(note_order(3, 2), [2, 1, 0]);
        assert_eq!(note_order(3, CUSTOM), [2, 1, 0]);
        let f = |w: u32, h: u32, v: u8| aurora_core::menuview::Frame { w, h, rgba: vec![v; (w * h * 4) as usize], panels: vec![[0, 0, w, h]] };
        let s = aurora_core::menuview::stack(&[f(4, 2, 1), f(3, 1, 2)], 5, false);
        assert_eq!((s.w, s.h), (4, 8));
        assert_eq!(s.panels, [[0, 0, 4, 2], [0, 7, 3, 8]]);
        assert_eq!(s.rgba[(7 * 4) * 4], 2);
        assert_eq!(s.rgba[(3 * 4) * 4], 0, "the gap stays empty");
        // Right corners: the narrow one sits against the right edge.
        let r = aurora_core::menuview::stack(&[f(4, 2, 1), f(3, 1, 2)], 5, true);
        assert_eq!(r.panels, [[0, 0, 4, 2], [1, 7, 4, 8]]);
        assert_eq!(r.rgba[(7 * 4) * 4], 0, "left of the narrow one stays empty");
        assert_eq!(r.rgba[(7 * 4 + 1) * 4], 2);
        // Background and border colours.
        assert_eq!(choice_names(Id::BackColour)[0], "Black");
        assert_eq!(note_colour(0, 2), ACCENTS[2].1);
        assert_eq!(note_colour(4, 0), ACCENTS[3].1);
    }

    /// The menu open on Aurora > Settings > Appearance > Notifications.
    fn on_notifications() -> State {
        let mut st = new_state(None);
        st.open = true;
        st.section = 3;
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 0, 0), (Page::Appearance, 0, 0), (Page::NoteLook, 0, 0)];
        st
    }

    /// Mouse clicks: left is Enter on a row and switches tabs; right is Back anywhere, never into a
    /// list.
    #[test]
    fn clicks_work_like_keys() {
        // On Warnings: a list, then switches.
        let mut st = on_notifications();
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 0, 0), (Page::Warnings, 0, 0)];
        let rows = items(Page::Warnings, st.prefs);
        assert_eq!((rows[0].row.kind.clone(), rows[1].row.text.as_str()), (Kind::Sub, "Force Me To Read Warnings"));
        let read = st.prefs.force_read;
        // Left on a switch: Enter on it, and it is the picked row.
        assert!(mouse_click(&mut st, Some(Spot::Row(1)), false));
        assert_eq!(st.prefs.force_read, !read);
        assert_eq!(st.stack.last().unwrap().1, 1);
        // Left on a row that opens a page: into it.
        assert!(mouse_click(&mut st, Some(Spot::Row(0)), false));
        assert_eq!(st.stack.last().unwrap().0, Page::AutoProceed);
        // Right on a row is only Back: no Enter, the pick and the switches stay, also on a row that
        // opens a page.
        let key = st.prefs.auto_key;
        assert!(mouse_click(&mut st, Some(Spot::Row(1)), true), "right on Hotkey");
        assert_eq!(st.prefs.auto_key, key, "Hotkey is not pressed");
        assert_eq!(st.stack.last().unwrap().0, Page::Warnings);
        assert_eq!(st.stack.last().unwrap().1, 0, "the pick stays where the left click put it");
        assert!(mouse_click(&mut st, Some(Spot::Row(0)), true), "right on Automatically Proceed");
        assert_eq!(st.stack.last().unwrap().0, Page::MenuSettings, "Back, not into the list");
        assert_eq!(st.prefs.force_read, !read, "nothing pressed");
        st.stack.push((Page::Warnings, 0, 0));
        assert!(mouse_click(&mut st, Some(Spot::Row(1)), true), "right on a switch");
        assert_eq!(st.prefs.force_read, !read, "the switch is not pressed");
        assert_eq!(st.stack.last().unwrap().0, Page::MenuSettings);
        st.stack.push((Page::Warnings, 0, 0));
        assert!(mouse_click(&mut st, None, true));
        assert_eq!(st.stack.last().unwrap().0, Page::MenuSettings);
        // Left off the rows, on the bar, past the list's end: nothing.
        let before = st.stack.clone();
        assert!(!mouse_click(&mut st, None, false));
        assert!(!mouse_click(&mut st, Some(Spot::Bar), false));
        assert!(!mouse_click(&mut st, Some(Spot::Row(999)), false));
        assert_eq!(st.stack, before);
        // Left on a tab shows it; back on Menu the player is where the player was.
        assert!(mouse_click(&mut st, Some(Spot::Tab(0)), false));
        assert_eq!((st.section, st.stack.last().unwrap().0), (0, Page::SelfTab));
        assert!(!mouse_click(&mut st, Some(Spot::Tab(0)), false), "the tab already shown");
        assert!(mouse_click(&mut st, Some(Spot::Tab(3)), false));
        assert_eq!((st.section, st.stack.clone()), (3, before));
        // Right on a tab is Back, not a switch.
        assert!(mouse_click(&mut st, Some(Spot::Tab(1)), true));
        assert_eq!((st.section, st.stack.last().unwrap().0), (3, Page::Menu));
        // A title row does nothing on a left click.
        st.stack = vec![(Page::Menu, 0, 0), (Page::Profiles, 0, 0)];
        assert_eq!(items(Page::Profiles, st.prefs)[2].row.kind, Kind::Header);
        assert!(!mouse_click(&mut st, Some(Spot::Row(2)), false));
        assert_eq!(st.stack.last().unwrap().1, 0);
    }

    /// Mouse Support: off by default, so the open menu never takes the mouse; on, it has it only
    /// while open with the game in front; turned off with the menu open, it is the game's again.
    /// Apply Default State turns it off. Move With Mouse is never greyed out:
    /// `move_with_mouse_takes_the_mouse`.
    #[test]
    fn mouse_support_toggle() {
        let mut st = on_notifications();
        assert!(!Prefs::default().mouse_support);
        assert!(!menu_takes_mouse(&st, true), "off by default: no mouse taken");
        // On, from Menu > Settings with Enter (as the player's click would).
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 3, 0)];
        press(&mut st, Key::Enter);
        assert!(st.prefs.mouse_support);
        assert!(menu_takes_mouse(&st, true));
        assert!(!menu_takes_mouse(&st, false), "game behind");
        st.open = false;
        assert!(!menu_takes_mouse(&st, true), "menu closed");
        st.open = true;
        // Off again with the menu open: the mouse is the game's at once.
        assert!(mouse_click(&mut st, Some(Spot::Row(3)), false));
        assert!(!st.prefs.mouse_support);
        assert!(!menu_takes_mouse(&st, true));
        // Saved like the other settings: its own address in profiles and Save/Load State, off for a
        // profile without its line (the player's old profiles), set by a profile line or command.
        let addr = "Aurora > Settings > Mouse Support";
        let f = resolve(addr, st.prefs).expect("its address");
        assert_eq!(state_of(&f.item, st.prefs).as_deref(), Some("Off"));
        assert!(!nondefault_rows(st.prefs).iter().any(|(a, _)| a == addr), "off is the default: not saved");
        assert!(apply_state(&mut st, &f, "On"));
        assert!(st.prefs.mouse_support);
        assert!(nondefault_rows(st.prefs).iter().any(|(a, _)| a == addr), "on is saved");
        let f = resolve(addr, st.prefs).unwrap();
        run_found(&mut st, f, None);
        assert!(!st.prefs.mouse_support, "its command switches it");
        // Its command, its default, and Apply Default State turns it off.
        assert_eq!(command(Id::MouseSupport).as_deref(), Some("mousesupport"));
        assert_eq!(default_state(Id::MouseSupport).as_deref(), Some("Off"));
        assert!(has_default(Id::MouseSupport));
        st.prefs.mouse_support = true;
        default_of(&mut st, Id::MouseSupport);
        assert!(!st.prefs.mouse_support);
    }

    /// Move With Mouse: never greyed out; on, the menu has the mouse for as long as it is on, with
    /// Mouse Support off; off again, the game has its mouse back at once. Mouse Support keeps its
    /// own setting all along.
    #[test]
    fn move_with_mouse_takes_the_mouse() {
        let mut st = on_notifications();
        assert!(!st.prefs.mouse_support && !st.prefs.move_mouse);
        assert!(!menu_takes_mouse(&st, true));
        let mm = |p: Prefs| items(Page::Position, p).into_iter().find(|i| i.id == Id::MoveMouse).unwrap();
        for p in [Prefs::default(), Prefs { mouse_support: true, ..Prefs::default() }] {
            let it = mm(p);
            assert!(!it.row.dim && !skipped(&it) && it.help.is_empty(), "never greyed out, no help");
        }
        // On from Position with Enter (as the player's click would), Mouse Support still off.
        st.stack = vec![(Page::Menu, 0, 0), (Page::MenuSettings, 0, 0), (Page::Appearance, 0, 0), (Page::Position, 2, 0)];
        press(&mut st, Key::Enter);
        assert!(st.prefs.move_mouse);
        assert!(!st.prefs.mouse_support, "Mouse Support keeps its own setting");
        assert!(menu_takes_mouse(&st, true), "the mouse is on while Move With Mouse is");
        assert!(!menu_takes_mouse(&st, false), "game behind");
        st.open = false;
        assert!(!menu_takes_mouse(&st, true), "menu closed");
        st.open = true;
        // Off again by a click on it: the mouse is the game's at once.
        assert!(mouse_click(&mut st, Some(Spot::Row(2)), false));
        assert!(!st.prefs.move_mouse);
        assert!(!menu_takes_mouse(&st, true));
    }

    /// Hovering moves the pick, nothing else; not onto titles; tabs are not hovered.
    #[test]
    fn hover_moves_only_the_pick() {
        let mut st = on_notifications();
        let prefs = st.prefs;
        assert!(hover(&mut st, 2));
        assert_eq!(st.stack.last().unwrap().1, 2);
        assert!(!hover(&mut st, 2));
        assert!(!hover(&mut st, 999));
        assert_eq!((st.prefs, st.stack.len(), st.section), (prefs, 4, 3));
        st.stack = vec![(Page::Menu, 0, 0), (Page::Profiles, 0, 0)];
        assert!(!hover(&mut st, 2), "a title");
        assert!(hover(&mut st, 1));
    }

    /// The player's bug: on Teleport, Where Am I (a row the cursor jumps over) went out of view as
    /// the player went down the list, and stayed hidden when the player came back up to Go To
    /// Erewhon. The player's list: Where Am I, two spots, Save This Spot, Undo Last Teleport, the
    /// Coordinates title, X, Y, Z, Go To Coordinates; 8 rows shown. The arrow keys' walk, row by
    /// row, through `scroll_to_show` (what `press` does with each pick).
    #[test]
    fn where_am_i_shows_again_at_the_top() {
        let (n, shown, lead, title) = (10, 8, 1, 5);
        // Down from Go To Erewhon to the last row, over the title.
        let walk: Vec<usize> = (lead..n).filter(|&p| p != title).collect();
        let mut scroll = 0;
        for &p in &walk {
            scroll = scroll_to_show(p, scroll, shown, lead);
            assert!(scroll <= p && p < scroll + shown, "row {p} in view");
        }
        assert_eq!(scroll, n - shown, "the last row at the bottom");
        // Back up to Go To Erewhon: the list is at its top again, Where Am I shows.
        for &p in walk.iter().rev().skip(1) {
            scroll = scroll_to_show(p, scroll, shown, lead);
            assert!(scroll <= p && p < scroll + shown, "row {p} in view");
        }
        assert_eq!(scroll, 0, "Where Am I shows again");
        // Down from the last row wraps to Go To Erewhon: the top shows too.
        assert_eq!(scroll_to_show(lead, n - shown, shown, lead), 0);
        // Rows in between scroll as before.
        assert_eq!(scroll_to_show(4, 2, shown, lead), 2);
        assert_eq!(scroll_to_show(9, 0, shown, lead), 2);
        assert_eq!(scroll_to_show(2, 3, shown, lead), 2);
        // One row shown: only the pick fits.
        assert_eq!(scroll_to_show(lead, 5, 1, lead), lead);
        // Nothing to jump over at the top.
        assert_eq!(scroll_to_show(0, 3, shown, 0), 0);
    }

    /// The wheel scrolls a long list three rows a notch, stops at its ends, and never changes a
    /// value or the pick; a short list does not move.
    #[test]
    fn wheel_only_scrolls() {
        let mut st = on_notifications();
        // Gear Score is longer than the menu shows. Not all_rows: that reads (and may write)
        // Hotkeys.txt.
        let long = Page::GearScore;
        let n = items(long, st.prefs).len();
        // Max Visible Commands.
        let shown = shown_rows(&st.prefs);
        assert_eq!(shown, 10);
        assert!(n > shown, "Gear Score has {n} rows");
        let one = (n - shown).min(3);
        st.stack = vec![(Page::Menu, 0, 0), (long, 1, 0)];
        let prefs = st.prefs;
        // Down one notch (the wheel turned towards the player: less than 0).
        assert_eq!(wheel_scroll(&mut st, -1), one as i64);
        assert_eq!(*st.stack.last().unwrap(), (long, 1, one));
        // Up past the top: back to 0, no further.
        assert_eq!(wheel_scroll(&mut st, 5), -(one as i64));
        assert_eq!(wheel_scroll(&mut st, 1), 0);
        // Far down: stops with the last row at the bottom.
        wheel_scroll(&mut st, -100);
        assert_eq!(st.stack.last().unwrap().2, n - shown);
        assert_eq!(wheel_scroll(&mut st, -1), 0);
        assert_eq!(st.prefs, prefs);
        assert_eq!(st.stack.last().unwrap().1, 1, "the pick stays");
        // A list that fits never scrolls.
        st.stack = vec![(Page::Menu, 0, 0), (Page::Profiles, 0, 0)];
        if items(Page::Profiles, st.prefs).len() <= shown {
            assert_eq!(wheel_scroll(&mut st, -1), 0);
        }
    }

    /// The mouse is tested against the menu picture where it is on screen and at the size it was
    /// drawn: a 2x menu (the old Menu Size 200% as List Width / Height) at (100, 50).
    #[test]
    fn mouse_uses_the_drawn_menu() {
        let mut st = on_notifications();
        let v = View {
            path: "Aurora > Menu".into(),
            sections: SECTIONS.iter().map(|s| s.1.to_string()).collect(),
            section: 3,
            rows: items(Page::NoteLook, st.prefs).into_iter().map(|i| i.row).collect(),
            pick: Some(0),
            scroll: 0,
            help: String::new(),
            accent: [0, 0, 0],
            layout: aurora_core::menuview::LAYOUT,
            list: aurora_core::menuview::list_at(200.0),
            visible: 10,
            spacer: aurora_core::menuview::SPACER,
        };
        assert_eq!(spot_at(&st, (900.0, 280.0)), None, "nothing drawn yet");
        // 1628 px wide (the help boxes' 660, the gap and the 960 px list): the wall at 1728.
        assert_eq!(aurora_core::menuview::menu_width(&v, 1.0), 1628.0);
        st.last = Some((v, (1728, 50)));
        st.drawn = Some(Drawn { x: 100, y: 50, scale: 1.0, w: 1628, h: 800, room: f32::INFINITY, wall: 1728, list_w: 960 });
        // At 2x the list starts at x 668 and y 60, rows 68 high; plus the menu's spot.
        assert_eq!(spot_at(&st, (100.0 + 800.0, 50.0 + 60.0 + 68.0 * 2.5)), Some(Spot::Row(2)));
        assert_eq!(spot_at(&st, (100.0 + 500.0, 50.0 + 60.0 + 68.0 * 3.5)), Some(Spot::Tab(3)));
        assert_eq!(spot_at(&st, (100.0 + 600.0, 50.0 + 20.0)), Some(Spot::Bar));
        assert_eq!(spot_at(&st, (50.0, 20.0)), None, "left of and above the menu");
    }

    /// Move With Mouse: the menu follows the drag from where it was, kept on screen, and X / Y say
    /// where it is.
    #[test]
    fn dragging_moves_the_menu() {
        let mut st = on_notifications();
        // The drag moves the wall: a 300 px list, its wall at 500.
        st.drawn = Some(Drawn { x: 100, y: 100, scale: 1.0, w: 400, h: 300, room: 500.0, wall: 500, list_w: 300 });
        assert!(drag_menu(&mut st, (500, 100), (50.0, -20.0), (1920, 1080)));
        assert_eq!((st.prefs.x, st.prefs.y), (Some(550), Some(80)));
        assert!(!drag_menu(&mut st, (500, 100), (50.0, -20.0), (1920, 1080)), "no move");
        // The wall stops at the screen's right edge, the bottom on the screen's bottom.
        assert!(drag_menu(&mut st, (500, 100), (5000.0, 5000.0), (1920, 1080)));
        assert_eq!((st.prefs.x, st.prefs.y), (Some(1920), Some(780)));
        // Left: the wall stops where the list still shows whole.
        assert!(drag_menu(&mut st, (500, 100), (-5000.0, -5000.0), (1920, 1080)));
        assert_eq!((st.prefs.x, st.prefs.y), (Some(300), Some(0)));
        assert_eq!((pixel_of(&st.prefs, Id::PosX), pixel_of(&st.prefs, Id::PosY)), (300, 0));
    }

    /// The menu as drawn at List Width / Height `list`, with a long help line.
    fn placed_view(list: [f32; 2]) -> View {
        let prefs = test_prefs();
        View {
            path: "Aurora > Aurora".into(),
            sections: SECTIONS.iter().map(|s| s.1.to_string()).collect(),
            section: 3,
            rows: items(Page::Fly, prefs).into_iter().map(|i| i.row).collect(),
            pick: Some(1),
            scroll: 0,
            help: ["Command: flyspeed [0.01 to 10000.00]", "How fast you fly, times 8 m/s.", "Click to input a value."].join(NEW_BOX),
            accent: [0, 0, 0],
            layout: aurora_core::menuview::LAYOUT,
            list,
            visible: 10,
            spacer: aurora_core::menuview::SPACER,
        }
    }

    /// X is the wall, the menu's right edge, Y the address bar's top, on the player's screens at
    /// several List Widths / Heights: the picture's right edge (the list's and the bar's) is at X
    /// whatever the widths, what sticks out on the left is cut at the screen's edge, and the wall
    /// never comes closer to it than the list is wide.
    #[test]
    fn x_is_the_wall() {
        let p = painter();
        for (sw, sh) in [(1920, 1080), (2560, 1440), (3440, 1440)] {
            let scale = sh as f32 / 1080.0;
            for list in [[200.0, 20.0], [450.0, 32.0], [529.0, 38.0], [1059.0, 75.0]] {
                let v = placed_view(list);
                let list_w = aurora_core::menuview::list_width(&v, scale).round() as i32;
                for x in [0, 1, 37, list_w, sw / 2, pct_px(64.0, sw), sw, sw + 500] {
                    let wall = wall_at(x, list_w, sw);
                    // Kept on screen, the list always whole.
                    assert!(wall <= sw && wall >= list_w.min(sw), "{sw}x{sh} {list:?} X {x}");
                    if (list_w..=sw).contains(&x) {
                        assert_eq!(wall, x, "{sw}x{sh} {list:?} X {x}");
                    }
                    let room = wall as f32;
                    let Some(p) = p else { continue };
                    let f = p.render_in(&v, scale, room);
                    let (px, py) = menu_place(wall, 100, (f.w, f.h), sh);
                    // The picture's right edge is the wall, its left edge on the screen.
                    assert_eq!(px + f.w as i32, wall, "{sw}x{sh} {list:?} X {x}");
                    assert!(px >= 0 && py >= 0, "{sw}x{sh} {list:?} X {x}");
                    // The bar's and the list's right edges are on the wall.
                    assert_eq!(f.panels[0][2], f.w, "bar {sw}x{sh} {list:?} X {x}");
                    assert_eq!(f.panels[2][2], f.w, "list {sw}x{sh} {list:?} X {x}");
                    // A part cut whole at the left edge is empty, never past the left edge.
                    assert!(f.panels.iter().all(|q| q[0] <= q[2]), "{sw}x{sh} {list:?} X {x}");
                }
            }
        }
        // Old X as the wall: plus tabs, gap and list at the old Menu Size, kept on the screen.
        assert_eq!(old_bar_to_wall(100, 1080), 602);
        assert_eq!(old_bar_to_wall(85, 1440), 683);
        assert_eq!(old_x_to_wall(1000, 100, (1920, 1080)), 1602);
        assert_eq!(old_x_to_wall(1500, 100, (1920, 1080)), 1920);
        assert_eq!(old_x_to_wall(-50, 100, (1920, 1080)), 602);
        // The player's own screen with no X line: the default wall is the right edge, where the
        // player's menu is.
        assert_eq!(old_x_to_wall(pct_px(64.0, 2560) + old_x_shift(100, 118, 1440), LOOK_SIZE, (2560, 1440)), 2560);
        // The shift itself: 330 - 118 px at 1080p and 100%, scaled with the screen and Menu Size.
        assert_eq!(old_x_shift(100, 118, 1080), 212);
        assert_eq!(old_x_shift(100, 118, 1440), 283);
        assert_eq!(old_x_shift(200, 118, 1440), 565);
        assert_eq!(old_x_shift(60, 118, 1080), 127);
        // Notifications' Custom Position: X / Y is the picture's top-left, kept on screen.
        for (sw, sh) in [(1920, 1080), (2560, 1440), (3440, 1440)] {
            assert_eq!(note_place(CUSTOM, (300, 40), (sw, sh), (0, 0)), (0, 0));
            assert_eq!(note_place(CUSTOM, (300, 40), (sw, sh), (123, 456)), (123, 456));
            assert_eq!(note_place(CUSTOM, (300, 40), (sw, sh), (sw, sh)), (sw - 300, sh - 40));
            assert_eq!(note_place(CUSTOM, (300, 40), (sw, sh), (-50, -50)), (0, 0));
            // The corners as they always sat: 30 px from the side, 90 from the bottom at 1080p.
            let s = sh as f32 / 1080.0;
            assert_eq!(note_place(0, (300, 40), (sw, sh), (0, 0)), ((30.0 * s) as i32, sh - 40 - (90.0 * s) as i32));
            assert_eq!(note_place(3, (300, 40), (sw, sh), (0, 0)), (sw - 300 - (30.0 * s) as i32, (30.0 * s) as i32));
        }
    }

    /// Version 3 profiles: Sprint is its own speed, Speed x the old Sprint, rounded to 0.01 and
    /// kept to 0.01 - 10000; version 1 lands where 1 -> 2 -> 3 does; old X lines get the help
    /// boxes' room, only lines named X.
    #[test]
    fn version_three_profiles() {
        let fly = |speed: &str, sprint: &str| {
            let mut t = "Tree Compatibility Version: 2\nSelf\n\tMovement\n\t\tFly\n".to_string();
            if !speed.is_empty() {
                t.push_str(&format!("\t\t\tSpeed: {speed}\n"));
            }
            if !sprint.is_empty() {
                t.push_str(&format!("\t\t\tSprint: {sprint}\n"));
            }
            t
        };
        let sprint_of = |t: &str| read_tree(&upgrade_profile(t, (1920, 1080))).into_iter().find(|(a, _)| a == FLY_SPRINT_ADDR).map(|(_, v)| v);
        assert_eq!(sprint_of(&fly("2.50", "1.60")).as_deref(), Some("4.00"));
        assert_eq!(sprint_of(&fly("0.33", "0.33")).as_deref(), Some("0.11"));
        assert_eq!(sprint_of(&fly("10000", "5")).as_deref(), Some("10000.00"));
        assert_eq!(sprint_of(&fly("0.01", "0.01")).as_deref(), Some("0.01"));
        // No Speed line: Speed was 1.00.
        assert_eq!(sprint_of(&fly("", "3")).as_deref(), Some("3.00"));
        // No Sprint line: it was 4.00 x Speed, so it gets one; at Speed 1 the default is it.
        assert_eq!(sprint_of(&fly("2", "")).as_deref(), Some("8.00"));
        assert_eq!(sprint_of(&fly("1.00", "")), None);
        assert_eq!(sprint_of(&fly("", "")), None);
        // Sprint m/s as before: Speed x Sprint x 8 then, Sprint x 8 now.
        let t = upgrade_profile(&fly("3.00", "2.00"), (1920, 1080));
        assert_eq!(line_of(&t, FLY_SPRINT_ADDR).as_deref(), Some("6.00"));
        assert_eq!(fly::metres_per_second(3.0, 6.0, true), 3.0 * 2.0 * 8.0);
        // Version 1 lands on the same result as 1 -> 2 -> 3.
        for old in [
            "Tree Compatibility Version: 1\nSelf\n\tMovement\n\t\tFly\n\t\t\tSpeed: 20\n\t\t\tSprint: 8\n",
            "Self > Movement > Fly > Speed: 13\nSelf > Movement > Fly > Sprint: 50\n",
            "Self > Movement > Fly > Speed: 4\n",
            "Self > Movement > Fly > Sprint: 15\n",
        ] {
            assert_eq!(upgrade_profile(old, (1920, 1080)), upgrade_v3(&upgrade_v2(&upgrade_v1(old), 1080), (1920, 1080)), "{old}");
        }
        // X: plus 212 at 1080p (and Menu Size 100%) for version 3, plus the bar to the wall's 602
        // for version 4; Y, and a percent line, as they were.
        let pos = "Tree Compatibility Version: 2\nMenu\n\tSettings\n\t\tAppearance\n\t\t\tPosition\n\t\t\t\tX: 1000\n\t\t\t\tY: 561\n";
        let t = upgrade_profile(pos, (1920, 1080));
        assert_eq!(line_of(&t, POS_X_ADDR).as_deref(), Some("1814"));
        assert_eq!(line_of(&t, POS_Y_ADDR).as_deref(), Some("561"));
        assert_eq!(upgrade_profile(&t, (1920, 1080)), t, "once only");
        let big = "Tree Compatibility Version: 2\nMenu\n\tSettings\n\t\tAppearance\n\t\t\tMenu Size: 200\n\t\t\tPosition\n\t\t\t\tX: 0\n";
        // 565 at 1440p and 200%; the wall is 1606 right of the bar there (315 + 11 + 1280): 2171.
        assert_eq!(line_of(&upgrade_profile(big, (2560, 1440)), POS_X_ADDR).as_deref(), Some("2171"));
        let pct = "Tree Compatibility Version: 2\nMenu\n\tSettings\n\t\tAppearance\n\t\t\tPosition\n\t\t\t\tMenu Left / Right: 50\n";
        assert!(upgrade_profile(pct, (1920, 1080)).contains("Menu Left / Right: 50\n"), "percent lines are upgrade_rows' job");
        // Notifications' rows that went are left out quietly.
        let gone_lines = "Tree Compatibility Version: 3\nMenu\n\tSettings\n\t\tNotifications\n\t\t\tInvert Flow: On\n\t\t\tPadding: 20\n\t\t\tFlash Colour: Blue\n\t\t\tWidth: 500\n";
        assert_eq!(profile_rows(gone_lines), vec![("Menu > Settings > Notifications > Width".to_string(), "500".to_string())]);
        for addr in ["Aurora > Settings > Notifications > Invert Flow", "Menu > Settings > Notifications > Padding", "Aurora > Settings > Notifications > Flash Colour", "Self > Movement > Fly > Stand While Flying"] {
            assert!(gone(addr), "{addr}");
        }
        assert!(!gone("Aurora > Settings > Notifications > Text Size"));
        for c in ["notifyinvertflow", "notifypadding", "notifyflashcolour", "standwhileflying"] {
            assert!(GONE_COMMANDS.iter().any(|g| g.0 == c), "{c}");
        }
    }

    /// Damage Multiplier and Fire Rate: 0.00 to 1000000.00, two decimals, steps of 1.00 that go
    /// round at the ends (Off, x1.00, x2.00 .. the top, Off); 0 is Off; a typed x2.37 keeps its
    /// decimals. Teleport's Coordinates keep two decimals typed, Left / Right step whole numbers.
    #[test]
    fn weapon_values_and_coordinates_take_decimals() {
        for id in [Id::Damage, Id::FireRate] {
            let (lo, hi) = limits(id, test_prefs()).unwrap();
            assert_eq!((lo, hi), (0.0, 1_000_000.0));
            assert_eq!(range(id, test_prefs()), "0.00 to 1000000.00 (0 is off)");
            assert!(typed(id));
            assert_eq!(stepped(0.0, TIMES_STEP, 1, lo, hi), 1.0, "Off goes to x1.00, not x0.10");
            assert_eq!(stepped(1.0, TIMES_STEP, 1, lo, hi), 2.0);
            assert_eq!(stepped(2.0, TIMES_STEP, -1, lo, hi), 1.0);
            assert_eq!(stepped(1000.0, TIMES_STEP, 1, lo, hi), 1001.0, "1000 is no top now");
            assert_eq!(stepped(hi, TIMES_STEP, 1, lo, hi), 0.0, "round at the top");
            assert_eq!(stepped(0.0, TIMES_STEP, -1, lo, hi), hi, "round at the bottom");
            assert_eq!(cents(stepped(2.37, TIMES_STEP, 1, lo, hi)), 3.37);
            assert_eq!(cents(stepped(hi - 0.5, TIMES_STEP, 1, lo, hi)), hi);
        }
        assert_eq!(times_text(0.0), "Off");
        assert_eq!(times_text(2.5), "x2.50");
        assert_eq!(times_text(1000.0), "x1000.00");
        assert_eq!(times_text(1_000_000.0), "x1000000.00");
        assert_eq!(times_value(2.345_6, DAMAGE_MAX), 2.35);
        assert_eq!(times_value(5000.0, FIRE_MAX), 5000.0);
        assert_eq!(times_value(5_000_000.0, FIRE_MAX), 1_000_000.0);
        assert_eq!(times_value(2_000_000.0, DAMAGE_MAX), 1_000_000.0);
        assert_eq!(times_value(-3.0, FIRE_MAX), 0.0);
        // Coordinates: typed decimals kept to two, shown as typed; whole steps.
        assert_eq!(cents(-6958.254), -6958.25);
        assert_eq!(number(cents(-6958.25)), "-6958.25");
        assert_eq!(number(cents(-6958.0)), "-6958");
        assert_eq!(step(-6958.25, 1), 5.0);
        assert_eq!(step(12.5, 1), 1.0);
        assert_eq!(range(Id::Coord(0), test_prefs()), "-100000.00 to 100000.00");
    }

    /// Text Size: its own row under Size, percent of the text's old size; Size scales the box, Text
    /// Size the text.
    #[test]
    fn notification_text_size() {
        let d = test_prefs();
        assert_eq!(LAYOUT_ROWS[NOTE_TEXT].0, "Text Size");
        assert_eq!(d.layout[NOTE_TEXT], 100);
        assert!((note_font(d, 1080) - aurora_core::menuview::NOTE_FONT).abs() < 1e-4);
        let big = Prefs { layout: { let mut l = d.layout; l[NOTE_TEXT] = 200; l }, ..d };
        assert!((note_font(big, 1080) - 2.0 * aurora_core::menuview::NOTE_FONT).abs() < 1e-4);
        // Size does not change the text.
        let wide = Prefs { layout: { let mut l = d.layout; l[TOAST_SIZE] = 200; l }, ..d };
        assert_eq!(note_font(wide, 1080), note_font(d, 1080));
        // The box's height follows the text size.
        let Some(p) = painter() else { return };
        let look = |font: f32| aurora_core::menuview::NoteLook { width: 400.0, border: [255, 255, 255], back: [0, 0, 0], font };
        let small = p.render_note("Fly on", &look(note_font(d, 1080)), 1.0);
        let large = p.render_note("Fly on", &look(note_font(big, 1080)), 1.0);
        assert!(large.h > small.h * 3 / 2, "{} {}", small.h, large.h);
    }
}
