# The in-game menu

How Aurora's in-game menu works, end to end: what it shows, how a key press becomes a change, how the picture gets into the game's frames, and which files it keeps.

For: someone who knows some programming but not this code.
Everything here was read from the code. Nothing was run for this page. Where the code was unclear, it says "unclear".

Line numbers drift. If one is off, search for the name next to it.
Short forms used below:

| Short form | File |
|---|---|
| `menu.rs` | `dll/src/menu.rs` |
| `vkdraw.rs` | `dll/src/vkdraw.rs` |
| `dinput.rs` | `dll/src/dinput.rs` |
| `dll/keys.rs` | `dll/src/keys.rs` |
| `menuview.rs` | `shared/aurora-core/src/menuview.rs` |
| `core/keys.rs` | `shared/aurora-core/src/keys.rs` |
| `core/lib.rs` | `shared/aurora-core/src/lib.rs` |
| `paths.rs` | `shared/aurora-core/src/paths.rs` |

## Contents

0. [The short version](#0-the-short-version)
1. [Map of the menu](#1-map-of-the-menu)
2. [Code map of menu.rs](#2-code-map-of-menurs)
3. [Input path](#3-input-path)
4. [Drawing path](#4-drawing-path)
5. [Files](#5-files)
6. [Profiles, defaults, saved commands, non-default state](#6-profiles-defaults-saved-commands-non-default-state)
7. [Clarity notes](#7-clarity-notes)
8. [Design history](#8-design-history)

---

## 0. The short version

- The menu is a copy of the GTA mod menu "Stand": tabs on the left, a list on the right, a bar with the path on top, help boxes under the tabs.
- It lives in the DLL inside the game. No outside window.
- One thread, the **worker** (`menu.rs:2053`), wakes every 12 ms. It reads the keyboard, changes things, and redraws.
- The picture is drawn on the CPU into a plain RGBA image (`menuview.rs`). `vkdraw.rs` copies that image into every frame the game shows, by hooking Vulkan's present call.
- While the menu is open, its keys are hidden from the game in two places: the game window's messages (`menu.rs:2320`) and DirectInput (`dinput.rs`).
- Every row has an **address**, its path as text: `World > ESP > Range`. Hotkeys, saved commands and profiles all store rows by that address.
- Every row can have a **command** for the command box: `esprange 250`.

A key press, in one line:
worker sees the key -> `press` -> `activate` / `adjust` / `set_typed` -> the feature changes and a file is saved -> next tick, `view` -> `Painter::render` -> `vkdraw::set_pic` -> the next game frame shows it.

---

## 1. Map of the menu

### Row kinds

Each row is an `Item` (`menu.rs:264`) holding a `Row` with a `Kind` (`menuview.rs:11`). How each kind looks comes from `Painter::render` (`menuview.rs:231`).

| Word used below | `Kind` | Looks like | Left / Right | Enter |
|---|---|---|---|---|
| toggle | `Toggle(bool)` | a rounded box on the right; filled with a tick when on, outline when off (`menuview.rs:303`) | Left goes back | flips it |
| number | `Choice(text)` | the value on the right; `< value >` while picked (`menuview.rs:334`) | one step down / up | opens the command box filled in (`menu.rs:1449`) |
| list | `Choice(text)` | same as number | previous / next name | next name (`menu.rs:1453`) |
| value | `Value(text)` | the value on the right, no arrows (`menuview.rs:330`) | see note below | depends on the row |
| action | `Action` | the name only | Left goes back | does it once |
| sub-page | `Sub` | a `>` chevron on the right (`menuview.rs:297`) | Right opens, Left goes back | opens it |
| header | `Header` | a centred title (`menuview.rs:289`) | the cursor skips it | - |
| info | `Info` | plain text | Left goes back | nothing (a gone saved command says so, `menu.rs:1502`) |
| waiting | `Waiting(step)` | a ring of 8 dots, one bright, moving one step every 250 ms (`menuview.rs:316`, `menu.rs:1883`) | - | turns the waiting switch off (`menu.rs:1270`) |
| dim | `row.dim = true` | grey text (`menuview.rs:287`) | the cursor skips it (`menu.rs:3525`) | nothing (`menu.rs:1266`) |

Note on value rows: Left steps down unless the row's `Id` is in a hand-kept list (`menu.rs:1837`); those go back instead. See clarity note 3.

The kind decides the look. What a row *does* is decided by its `Id` in `activate`, `adjust` and `set_typed`.

Rows with hotkeys show them after the name, as Stand does: `God Mode [ F5 ]` (`menu.rs:1923`).

### Order on screen

`items_of(page)` (`menu.rs:816`) builds a page's rows. `items(page)` (`menu.rs:804`) then moves sub-pages to the top. It skips that sort on the "O" pages, Profiles, Credits and ESP (`menu.rs:810`).
So the order in `items_of` is not always the order on screen. The tree below is the order on screen.

### Tabs

`SECTIONS`: four tabs, top to bottom.
Right Ctrl goes to the next tab, Right Shift to the one before; both wrap round (`menu.rs:1774`).

| Tab | Page |
|---|---|
| Self | `Page::SelfTab` |
| World | `Page::World` |
| Game | `Page::Game` |
| Aurora | `Page::Menu` |

### The tree

Format: `Row name` [kind] `(Id)`, then `-> Page::X` for sub-pages. Page titles come from `page_name` (`menu.rs:108`).

- **Self** `Page::SelfTab`
  - God Mode [toggle] `(God)`
  - Undetected [toggle] `(Unseen)`: enemy NPC eyesight and alert meter only (`unseen.rs`). Only the label says "Undetected"; it is not a bypass and has nothing to do with anti-cheat.
  - Infinite Stamina [toggle] `(Stamina)`
  - Movement [sub-page] -> `Page::Movement`
    - Fly [sub-page] -> `Page::Fly`
      - Fly [toggle] `(Fly)`. Help: one box, four lines, the keys as bound with Left / Right as L / R: "W A S D move / L Shift up / L Ctrl down / Space sprint".
      - Fly Speed [number, "1.00", 0.01-10000.00, steps of 0.10, goes round at the ends] `(FlySpeed)`, command `flyspeed`. A multiplier: 1.00 is 8 m/s. "Speed" before; old addresses move.
      - Sprint [number, "4.00", 0.01-10000.00, as Fly Speed] `(FlySprint)`. Its own speed since Sprint x 8 m/s while the fast key is held (it was times Speed).
      - Hop Up [toggle, default on] `(HopUp)`, command `flyhopup`. The first press of up hops, only while the player stands on the ground.
      - Hop Height [number, "3.75", 0.50-15.00 m, steps of 0.10, goes round at the ends] `(HopHeight)`, command `flyhopheight`. In the HUD's metres (world units x 1.5); 3.75 is the old 2.5 units.
      - Safe Fly [toggle] `(SafeFly)`. Help, two lines: "Turns on God Mode while you fly, / until you land."
      - Stand While Flying has no row since it is always on while Fly is on. Its old profile lines, hotkeys and Saved Commands are dropped quietly (`GONE`, `gone` in `menu.rs`); the old command `standwhileflying` says it is always on.
  - Weapons [sub-page] -> `Page::Weapon` (the old Weapon tab)
    - No Recoil [toggle] `(NoRecoil)`
    - No Spread [toggle] `(Accuracy)`
    - No Scoped Sway [toggle] `(NoSway)`
    - Infinite Mag [toggle] `(Ammo)`
    - Infinite Reserve [toggle] `(Reserve)`
    - Damage Multiplier [number, "Off" or "x2.50", 0.00-1000000.00, Left / Right steps of 1.00, typed values keep two decimals, 0 = off] `(Damage)`
    - Fire Rate [number, "Off" or "x2.50", 0.00-1000000.00, as Damage Multiplier] `(FireRate)`. The hook keeps the wait between shots at 2 ms at least (`FIRE_MIN_S`, `weapon.rs`), so a huge value can't stall the gun.
- **World** `Page::World` (`menu.rs:897`)
  - Teleport [sub-page] -> `Page::Teleport` (`menu.rs:881`)
    - Where Am I [value, read only, two decimals; the cursor skips it] `(WhereAmI)`
    - Go To *name* [action], one per file in `Places\` `(Spot(i))`. Delete removes the spot (`menu.rs:1842`).
    - Save This Spot [action] `(SaveSpot)`
    - Undo Last Teleport [action] `(Undo)`
    - "Coordinates" [header]
    - X, Y, Z [number, -100000.00 to 100000.00] `(Coord(0..=2))`: Left / Right step whole numbers, typed values keep two decimals and show them
    - Go To Coordinates [action] `(GoCoords)`
  - ESP [sub-page] -> `Page::Esp` (not sorted, `menu.rs:903`)
    - ESP [toggle] `(EspOn)`
    - Range [number, "375 m", 25-1500, steps of 25] `(EspRange)`
    - Targets [sub-page] -> `Page::EspTargets`
      - Hostile, Friendly, Players, Drones, Hide Dead [toggle] `(EspEnemies, EspFriendlies, EspPlayers, EspDrones, EspHideDead)`
        - Drones: NPC drones as diamonds, on by default; command `espdrones`.
    - Drawing [sub-page] -> `Page::EspDrawing`
      - Colours [sub-page] -> `Page::EspColours` (`menu.rs:932`)
        - Hostile [list of `BOX_COLOURS`] `(EspColour(0))`
        - Hex Code [value, typed only; empty for a listed colour] `(EspHex(0))`
        - Friendly [list] `(EspColour(1))`, Hex Code `(EspHex(1))`
        - Players [list] `(EspColour(3))`, Hex Code `(EspHex(3))`
        - Drones [list] `(EspColour(5))`, Hex Code `(EspHex(5))`: Purple by default, kept in `EspStyle::drone`; command `espdronescolour`.
      - Style [list: Box / Bones] `(EspBones)`
      - Lines [number, "auto" or "2 px", 0-6] `(EspLines)`
      - Black Edge [toggle] `(EspEdge)`
      - Distance [toggle] `(EspDistance)`
  - Target [sub-page] -> `Page::Target` (`menu.rs:944`)
    - Pick Target [value: "none" or "12.3 m"; Enter picks the NPC you look at; no help text since] `(Aim)`
    - Yeet Target, Bring Target [action; dim with no target] `(Yeet, Bring)`
- **Game** `Page::Game`
  - Disables [sub-page] -> `Page::Disables`
    - Screen Shake [toggle] `(NoShake)`, command `noscreenshake`
    - NVG In Erewhon [toggle] `(NvgZone)`, command `nvginerewhon`
  - Rendering [sub-page] -> `Page::Rendering`
    - No NVG Overlay [toggle] `(NoNvgOverlay)`, command `nonvgoverlay`. "NVG Overlay"; `MOVED` takes the old address (and every older one) here, so hotkeys, Saved Commands and profile lines keep working.
  - Progress [sub-page] -> `Page::Progress`
    - Editor [sub-page] -> `Page::Editor`
      - Gear Score [sub-page] -> `Page::GearScore`
        - Dropped Item Gear Score [number, 0-1000, 0 = the game's own; Left / Right go round 1 to the top] `(GearValue)`
        - Total Gear Score [number] `(GearTotal)`
        - "Weapons" [header]
        - Primary, Secondary, Pistol [number] `(GearSlot(0..=2))`
        - "Gear" [header]
        - Head, Hands, Vest, Pants, Shoes [number] `(GearSlot(3..=7))` (names from `gearscore::SLOTS`)
        - "Everything You Own" [header]
        - Set All Owned Items [value, typed only] `(GearOwned)`
        - While Enable Experimental Features is on: every number here goes up to 1000000 (`gear_max`, `gearscore::HIGH_MAX`), the ranges say so, and each row has the help "Recommended max is 600. / If you set an absurd number, / use at your own risk." Off: 0-1000 and no help. Turning it off brings Dropped Item Gear Score back to the player's last value up to 1000 (`gear_back`); with none, a drop score over 1000 left in the game goes to 1000 once in the world, also after every inject (`gear_cap_check`). Items already set keep their score. Profiles keep only that value, never one over 1000; a hand-edited line over 1000 loads as 1000 (`profile_value`). Set All Owned Items checks the cap before each item it writes (`gearscore::cap`), so the switch going off during its search or its writes keeps it to 1000.
      - Credits [sub-page] -> `Page::Credits` (not sorted)
        - Credits [value, typed only] `(Credits)`. First Enter shows the warning page `Page::Risk` (see Warnings below). Then it may go to `Page::CreditsPicker`.
        - Pick Other Points [sub-page look, but `Id::PickOther`, not `Id::Open`] -> `Page::OtherPick`. Help, three lines in one box: "Open a shop within 20 s. / Pick the amount you want. / Credits then changes that one."
          - Re-scan [action] `(Rescan)`
          - a status line [info]
          - one row per amount seen [action] `(OtherPick(o))`
- **Aurora** `Page::Menu`
  - Settings [sub-page] -> `Page::MenuSettings`
    - Appearance [sub-page] -> `Page::Appearance` (`menu.rs:1064`)
      - Position [sub-page] -> `Page::Position`
        - X [number, screen pixels, -32768-32767] `(PosX)`, command `menux`. X: the menu's right edge, the wall. The list's and the address bar's right edges are on it, the tabs sit left of the list, the help boxes under the tabs. A wider list, tabs or bar grows to the left only; what would go past the screen's left edge is cut there, the wall never moves. On screen the wall is kept between the list's width and the screen's right edge (`wall_at`), so the list always shows whole. Not set: where the list's right edge was drawn at the old default (64% of the screen plus the help boxes' room, Menu Size 85%), kept on screen: the right edge on 16:9 screens (1920 at 1920 x 1080, 2560 at 2560 x 1440). Help: "The menu's right edge, in screen pixels."
        - Y [number, screen pixels] `(PosY)`, command `menuy`: the address bar's top edge. Help: "The address bar's top, in screen pixels."
        - Move With Mouse [toggle] `(MoveMouse)`: drag the menu by its top bar; the drag moves the wall (X) and Y. Never greyed out: on, the menu has the mouse for as long as it is on, Mouse Support or not. No command; the old `menumovewithmouse` still works quietly (`old_row_command`).
      - Colours [sub-page] -> `Page::MenuColours`
        - Menu Colour [list of `ACCENTS`, "Pink" first] `(MenuColour)`
      - Tabs [sub-page] -> `Page::Tabs`
        - Tabs Width, Tabs Height [number] `(Layout(0), Layout(1))`
      - Address Bar [sub-page] -> `Page::AddressBar`
        - Address Bar Height, Address Bar Width [number] `(Layout(2), Layout(3))`
      - Notifications [sub-page] -> `Page::NoteLook`. `MOVED` takes the old addresses here.
        - Lists on top: Custom Position, Timing, then the rest (see below).
      - Max Visible Commands [number, "10", 3-255] `(Layout(10))`, command `menuheight`: rows shown before the list scrolls; its own thing, not tied to List Width / Height. Never more than fit on the screen (`shown_rows`, `menuview::rows_fit`).
      - List Width [number, "450", 0-32767] `(Layout(8))`, command `listwidth`
      - List Height [number, "32", 0-32767] `(Layout(9))`, command `listheight`: one row's height
      - Spacer Size [number, "3", 0-50] `(Layout(11))`, command `spacersize`: the see-through gap between the bar, tabs, list and help boxes (3 is the gap it always had, 4 px at 1080p and 100%; 0 is none; at least 1 px otherwise, so 1 and 2 differ).
      - List Width / Height. Units: 450 / 32 is exactly the menu as it was at Menu Size 85%. One List Width unit is 480 x 0.85 / 450 px at 1080p (the list is 408 px wide at 1080p, 544 at 1440p); one List Height unit is 34 x 0.85 / 32 px at 1080p (a row is 29 px at 1080p, 39 at 1440p). Both grow with screen height / 1080. List Width scales everything across (list, tabs, address bar, help boxes), List Height everything down and the text; the mouse uses the same sizes (`menuview::axes`, kept to 0.2-4 times the old 100%). The command box follows them too.
    - Notifications [sub-page] -> `Page::Notifications`: only Suppress Generic Responses
      - Suppress Generic Responses [sub-page] -> `Page::Suppress`. Help: "Hides plain replies like "Fly on"."
        - Command Box [toggle] `(QuietBox)`, help "Includes the command box."; Hotkey [toggle] `(QuietKey)`, no help. The Menu switch went: doing something by hand in the menu never shows a plain "on" / "off" / "set to" now. Its old line, hotkey and command are dropped quietly (`GONE`, `GONE_COMMANDS`).
    - Appearance > Notifications `Page::NoteLook`
      - Custom Position [sub-page] -> `Page::CustomPosition`
        - X, Y [number, screen pixels] `(NotifyX, NotifyY)`, commands `notifyx`, `notifyy`: the notifications' top-left, kept on screen
      - Timing [sub-page] -> `Page::Timing`
        - Time [number, "3 s", 1-15] `(Layout(5))`, command `notifytime`
      - Position [list: Bottom Left, Bottom Right, Top Left, Top Right, Custom] `(ToastPosition)`, command `notifyposition`
      - Width [number, "400", 100-1600, pixels at 1080p] `(Layout(6))`, command `notifywidth`: the widest a notification grows before its text wraps. Capped so a notification is never wider than the 2048-pixel picture slot.
      - Size [number, "100%", 50-200] `(Layout(4))`, command `notifysize`: the box (padding, bar, Width)
      - Text Size [number, "100%", 50-300] `(Layout(7))`, command `notifytextsize`: the text
      - Border Colour [list: Menu Colour, then the `ACCENTS` names] `(ToastColour)`, command `notifybordercolour`
      - Background Colour [list] `(BackColour)`, command `notifybackgroundcolour`
      - Flash Notification [action] `(FlashNote)`, command `flashnotification`
      - Invert Flow, Padding and Flash Colour went: rows, commands and prefs. Their old profile lines, hotkeys and Saved Commands are dropped quietly (`GONE`); the old commands say they are gone (`GONE_COMMANDS`).
    - Warnings [sub-page] -> `Page::Warnings`
      - Automatically Proceed On Warnings [sub-page] -> `Page::AutoProceed`
        - Command Box, Hotkey [toggle, off] `(AutoBox, AutoKey)`: no question when the action came from there
      - Force Me To Read Warnings [toggle, on] `(ForceRead)`: on, Yes shows only 2 s after the question. Off: no warning page at all, from anywhere: Unload, the Credits warning, Enable Experimental Features and a profile's Delete go ahead at once (`warn_skipped`). Warning text rows (`Id::WarnText`) are never picked, highlighted, hovered or clicked.
      - Skip Already-Accepted Warnings [toggle, on] `(SkipAccepted)`: a warning answered Yes since this inject is not asked again. Each profile's Delete counts on its own.
    - Mouse Support [toggle, off] `(MouseSupport)`, command `mousesupport`: under the lists. Off, the menu never takes the mouse. Profiles without its line load it off. A mouse button still down as the menu lets go of the mouse (a click closed it, or Yes under Unload Aurora) stays the menu's until let go, so the game never sees a fresh press; Unload waits for it too.
    - Enable Experimental Features [toggle, off] `(Experimental)`, command `enableexperimentalfeatures`: under Mouse Support. Help: "Shows Aurora > Experiments. / Off turns all experiments off." On: the Experiments warning first (`Page::ExpWarn`: "Experiments are untested. / They can crash your game or / cause unwanted behaviour.", Yes after the read wait, `ExpYes`), skipped as the other warnings are (Warnings' switches); a profile turns it on without asking. Off: every experiment that is on goes off (`experiments_off`: Experiments' switches that are off by default; values and Show FOV Circle stay) and Experiments closes wherever it is open. A profile has it on only with its line On (`wants_experimental`), set before the other rows load; older profiles load it off. Profiles keep the switch (Automatically Save too) but nothing inside Experiments: Save, Automatically Save and Save State leave those rows out (`profile_state`; Save State on them says "Experiments are never saved"), Load and Load State put them at their default, also while the switch is off, and lines an older build saved there are left out at Load (`loadable`: logged, not counted as skipped). Hotkeys and Saved Commands on them stay, without their states. On also lets every gear score go up to 1000000; Off brings Dropped Item Gear Score back to the player's last value up to 1000 (`gear_back`, see Gear Score). While it is off, a hotkey or command on an Experiments row says "Turn on Enable Experimental Features first". Off also closes Silent Aim's list where Find or Saved Commands opened it (`experiment_pages`), and the Aurora list's cursor and Back stay on their rows.
  - Profiles [sub-page] -> `Page::Profiles` (not sorted, `menu.rs:1091`)
    - Create Profile [action; opens the box with `createprofile `] `(NewProfile)`
    - Open Folder [action; opens the default folder app, `open_folder`] `(OpenProfiles)`
    - "Profiles" [header]
    - one row per `Profiles\*.txt` [sub-page] -> `Page::Profile(n)` (`menu.rs:1100`)
      - Save, Load [action] `(ProfileSave(n), ProfileLoad(n))`
      - Load On Inject, Automatically Save [toggle] `(ProfileOnInject(n), ProfileAuto(n))`. Load On Inject's help: "Loads this profile when Aurora injects."
      - Delete [action] `(ProfileDelete(n))` -> `Page::ProfileDelete(n)`: the question, then Yes after 2 s
  - Hotkeys [sub-page] -> `Page::HotkeyList` (`menu.rs:3488`)
    - "Menu Actions" [header]
    - Menu, Command Box [value: the key] `(Bind(Bind::Action("menu" / "cmdbox")))`
    - "Commands" [header]
    - one row per line in Hotkeys.txt, by the row's own name [value: its keys] `(KeyRow(i))`. Enter opens that row's "O" > Hotkeys page.
  - Experiments [sub-page] -> `Page::Experiments`, in Aurora only while Settings > Enable Experimental Features is on. Row help: "Untested. / Can crash your game or / cause unwanted behaviour." It opens straight in: the warning comes when the switch goes on. Nothing inside it is kept in profiles: it starts at its defaults at every inject and profile load.
    - Silent Aim [list] `(Open(SilentAim))`, on top as every list. Old addresses follow it (`MOVED`: `Self > Weapons > Silent Aim`, and its FOV and Range rows before that).
      - Silent Aim [toggle, off at inject] `(SilentAim)`, command `silentaim`. Help: "Your shots go to an AI soldier or drone inside the circle. / Never players, co-op players included. Never animals." Profiles never turn it on. Not tested in game.
      - Silent Aim FOV [number, "20 deg", 1-90, the circle's full width, default 20] `(SilentFov)`, command `silentaimfov`.
      - Silent Aim Range [number, "300 m", 10-1000 m as the HUD counts, steps of 10, default 300] `(SilentRange)`, command `silentaimrange`.
      - Show FOV Circle [toggle, on at inject] `(SilentCircle)`, command `silentaimcircle`. The circle shows while Silent Aim is on, ESP on or off.
      - Priority [choice: Crosshair (default), Nearest, Farthest] `(SilentPriority)`, command `silentaimpriority <word>`. Which target inside the circle and range goes first.
      - Magic Bullets [toggle, off at inject] `(SilentMagic)`, command `magicbullets`. Help: "Steered shots go through walls to the target. / Not when a player is near the line. Needs Silent Aim on."
      - No Range Loss [toggle, off at inject] `(SilentNoRange)`, command `norangeloss`. Help: "Your shots keep their damage at any distance. / Not while a player is near your crosshair. Needs Silent Aim on."
      - Instant Hit [toggle, off at inject] `(SilentInstant)`, command `instanthit`. Help: "Your shots arrive at once. / Not while a player is near your crosshair. Needs Silent Aim on."
    - Ignore only me (untested) [toggle, off] `(UnseenInbox)`, command `ignoreonlyme`.
    - "Fly Water Fixes" [header], then Wait For Calm, Snap Guard, Let Game Fall (each "(untested)") [toggles, off] `(FlyCalm, FlyGuard, FlyLetFall)`, commands `flywaitforcalm`, `flysnapguard`, `flyletgamefall` (features.md, Fly Water Fixes).
  - Saved Commands [sub-page] -> `Page::Saved`: the saved rows themselves, working in place. A saved row behind Enable Experimental Features shows `(hidden)` while the switch is off, and Enter on it says to turn the switch on.
  - Commands With Non-Default State [sub-page] -> `Page::NonDefault`: every changed row, working in place. No command; the old `commandswithnondefaultstate` still works quietly.
  - Apply Default State [action] `(ApplyDefaults)`, command `default`. It resets Experiments' rows too, also while the switch is off.
  - Unload Aurora [action] `(Unload)`, command `unload` -> `Page::Unload`: the question, then Yes after 2 s


Pages that are not in the tree:

- **The "O" menu** of any row: press O on a row (`menu.rs:1786`). Pages `Page::Ctx`, `Page::RowHotkeys`, `Page::CtxKey(vk)` (`menu.rs:959`).
  - Apply Default State To Children, on a sub-page row; Apply Default State, on a row that has a default (`menu.rs:962`-`967`)
  - Rename [action], only on a teleport spot
  - Hotkeys [sub-page] -> `Page::RowHotkeys`: Add Hotkey [action], "Hotkeys" [header], one sub-page per key -> `Page::CtxKey(vk)`: Remove Hotkey [action]
  - Save [toggle]: in or out of Saved Commands
  - Copy Address [action]: puts the address on the clipboard
- **Confirmation and pick pages**: `Page::Unload`, `Page::ProfileDelete(n)`, `Page::Risk` (credits warning; Back is blocked for 2 s, `menu.rs:1857`), `Page::CreditsPicker`, `Page::OtherPick`.

---

## 2. Code map of menu.rs

### The main types

| Name | Line | What it is |
|---|---|---|
| `Page` | `menu.rs:35` | Every page the menu can show. Some carry a number: `Profile(u32)`, `CtxKey(u32)`. |
| `SECTIONS` | `menu.rs:105` | The four tabs and their top page. |
| `page_name` | `menu.rs:108` | A page's title in the path bar. |
| `Id` | `menu.rs:150` | What a row *is*. `activate`, `adjust`, `command` and friends all `match` on it. |
| `Bind` | `menu.rs:252` | What a "press a key" prompt sets: a menu key (`Action("menu")`) or a new row hotkey (`RowAdd`). |
| `BIND_ROWS` | `menu.rs:261` | The two menu keys: Menu and Command Box. |
| `Item` | `menu.rs:264` | One row: its `Id`, its `Row` (text, kind, dim) and its help text. Made with `item` (`menu.rs:302`). |
| `BOX_COLOURS` | `menu.rs:271` | Names and RGB for the ESP colour lists. Any other colour shows as "Custom" plus a hex code. |
| `ACCENTS` | `menu.rs:306` | Names and RGB for Menu Colour and the notification colours. Index 0 (Pink) is the default. |
| `Prefs` | `menu.rs:319` | The menu's own settings. Profile rows like any other since (`menu.json` before); defaults at every inject. |
| `LAYOUT_ROWS` | `menu.rs:346` | Eight number rows: name, the old `menu.json` key (read once to carry the player's values over), default, least, most. 0-3 are tabs and address bar sizes, 4-7 are the notifications' Size, Time, Width and Text Size. |
| `LAYOUT_COMMANDS`, `LAYOUT_UNITS` | `menu.rs:356`, `358` | Their commands (`tabsw` ... `notifytextsize`) and units (`%`, ` s`). |
| `TABS_WIDTH`, `TOAST_SIZE`, `TOAST_TIME`, `NOTIFY_WIDTH`, `NOTE_TEXT` | `menu.rs` | Indexes 0 and 4 to 7 into `LAYOUT_ROWS` / `Prefs.layout`. `NOTE_GAP` (4 px at 1080p) is the gap between stacked notifications. |
| `NOTIFY_POSITIONS` | `menu.rs:468` | The four corners and Custom; index 0 (Bottom Left) is the default. |
| `State` | `menu.rs:373` | Everything the worker keeps: open or not, tab, page stack, typing, key prompts, history, prefs, profile. One copy in `STATE` (`menu.rs:408`). |
| `Found` | `menu.rs:2498` | A row found anywhere in the menu, with its tab, the stack to reach it, and its address. |
| `Key`, `NAV` | `menu.rs:1743`, `1758` | Navigation keys and their Windows key codes. |

`State` in more detail (`menu.rs:373`):

- `open`: the menu is shown.
- `section`: which tab.
- `stack`: the pages opened inside this tab, each with its picked row and scroll. The last one is on screen.
- `saved`: each other tab's stack, so switching back lands where you were.
- `edit`: `Some(text)` while the command box is open. `caret`: where its caret is, in bytes (None: at the end).
- `bind`: `Some(...)` while waiting for a new key.
- `bind_hold`: the key just picked; kept from everything until let go.
- `history`, `hist_pos`, `tab`: command box history and Tab completion.
- `last`: the last `View` drawn, so an unchanged picture is not drawn again.
- `profile`, `profile_text`, `inject_load`: profile bookkeeping (section 6).

### How a key press flows

1. **The worker loop** `worker` (`menu.rs:2053`). Every 12 ms, holding the `STATE` lock for the whole step:
   1. Is the game window in front? `crate::keys::game_focused` (`dll/keys.rs:22`). If not, no key counts.
   2. Menu key pressed? Flip `open` (`menu.rs:2075`). Opening calls `take_keys` (`menu.rs:2554`).
   3. Command box key pressed? `open_box` (`menu.rs:2085`).
   4. Publish who owns the keys right now (`menu.rs:2097`-`2103`; see section 3).
   5. Side jobs: `toast_tick`, `row_hotkeys`, `command_file`, `profile_tick`, `gear_tick`, `keep_hotkeys` (`menu.rs:2104`-`2109`).
   6. Typing? `box_step` (`menu.rs:2112`). Then `input_tick` draws the box.
   7. Menu closed? Hide its picture and stop here (`menu.rs:2115`).
   8. Waiting for a key? `binding_step` (`menu.rs:2122`).
   9. Navigation keys: a fresh press fires once; arrows held for 400 ms repeat every 70 ms (`menu.rs:2130`-`2149`). Each press calls `press`.
   10. Redraw (step 4 of this list).
2. **Navigation** `press` (`menu.rs:1772`):
   - Right Ctrl / Right Shift: switch tab, keeping each tab's stack.
   - O: push `Page::Ctx` for the picked row (`menu.rs:1786`).
   - Up / Down: move, skipping headers, dim rows and Where Am I. The pick stays in view; on the first row the cursor can be on, the list goes back to its top, so the rows above it (Where Am I, a title) show again.
   - Enter: `activate`.
   - Right: `activate` on a sub-page, `adjust(+1)` on a number, list or value.
   - Left: `adjust(-1)`, or `back`.
   - Backspace (and Numpad 0): `back` (`menu.rs:1856`).
   - Delete: remove a spot or a menu key.
3. **Doing it**:
   - `activate` (`menu.rs:1265`): Enter on a row. Toggles flip. Actions run. Sub-pages are pushed on the stack. Number rows open the command box filled in (`menu.rs:1449`). List rows step to the next name (`menu.rs:1453`).
   - `adjust` (`menu.rs:1653`): Left / Right on a number or list. Steps come from `step` (`menu.rs:1131`): 1, or 5 beyond 1000.
   - `set_typed` (`menu.rs:1567`): a typed number from the command box. Clamps it, sets it, saves it.
   - From the command box or a hotkey, `run_found` (`menu.rs:2588`) picks one of these three.
4. **Saving** (section 5 has the files):
   - Menu looks, ESP, fly speed, Safe Fly, gear score, coordinates: no file of their own since (`menu.json` and `settings.json` before). They live in memory and a profile keeps them, as every row (`prefs_changed` only mirrors the Suppress switches).
   - Hotkeys: `set_row_keys` (`menu.rs:3083`), or `aurora_core::save_keys` (`core/lib.rs:72`) for the two menu keys.
   - Saved Commands: `set_saved` (`menu.rs:3184`).
   - Nothing else is saved on its own: every inject starts from the defaults, and only a profile (Load On Inject) brings the player's values back.
5. **Saying so**: toasts through `note`, `done`, `confirm`, `switched`, `result` (`menu.rs:1196`-`1254`). See section 4.
6. **Redraw** (`menu.rs:2157`-`2179`): at most every 250 ms when nothing changed, at once when something did. `view` (`menu.rs:1865`) builds a `View`. If it equals the last one, nothing is drawn. Otherwise `Painter::render` -> `vkdraw::set_pic(MENU, ...)`.

### Where each concern lives

| Concern | Where |
|---|---|
| Start and stop | `start` (`menu.rs:1954`), `shutdown` (`menu.rs:1998`). `start` is called at inject from `dll/src/lib.rs:89`; `shutdown` from `dll/src/server.rs:105`, before vkdraw's. |
| Rows of every page | `items_of` (`menu.rs:816`), `items` (`menu.rs:804`) |
| Every row anywhere, with addresses | `all_rows` (`menu.rs:2506`), `resolve` (`menu.rs:2531`), `address_of` (`menu.rs:2536`) |
| Rows that moved | `MOVED` (`menu.rs:2895`), `MOVED_EXACT` (`menu.rs:2945`), `moved` (`menu.rs:2950`) |
| Help text, path bar, labels | `view` (`menu.rs:1865`), `range` (`menu.rs:1177`) |
| Command names | `command` (`menu.rs:2391`), `typed` (`menu.rs:2465`), `current` (`menu.rs:2470`) |
| Command box | `open_box` (`menu.rs:2580`), `box_step` (`menu.rs:2776`), `run_line` (`menu.rs:2679`), `run_found` (`menu.rs:2588`), `matches` (`menu.rs:2726`), `command_list` (`menu.rs:2748`), `input_tick` (`menu.rs:2871`) |
| Key prompts | `binding_step` (`menu.rs:2199`), `set_bind` (`menu.rs:772`), `bindable` (`menu.rs:2185`) |
| Row hotkeys | `row_keys` (`menu.rs:2978`), `row_hotkeys` (`menu.rs:3119`), `add_row_key` / `remove_row_key` (`menu.rs:3093`, `3108`) |
| Keeping keys from the game | `subclass` / `wndproc` / `eats` (`menu.rs:2333`, `2320`, `2284`), `keep_hotkeys` (`menu.rs:3418`), `dinput.rs` |
| Toasts | `toast` (`menu.rs:421`), `toast_tick` (`menu.rs:2013`) |
| Teleport spots | `spots` (`menu.rs:624`), `places` (`menu.rs:664`), `add_spot` / `remove_spot` / `rename_spot` (`menu.rs:699`, `709`, `723`) |
| Profiles | `menu.rs:3534`-`4017` |
| Defaults | `default_of` (`menu.rs:3230`), `default_state` (`menu.rs:3673`), `has_default` (`menu.rs:3344`), `default_all` (`menu.rs:3220`), `defaults` (`menu.rs:3196`) |
| Gear score held for drops | `gear_tick` (`menu.rs:1009`), `set_gear` (`menu.rs:929`) |
| Credits | `credits_text` (`menu.rs:4047`), `watch_others` (`menu.rs:4039`), `risk_read` (`menu.rs:4034`) |
| Unload | `unload_now` (`menu.rs:4021`) |
| Tests | `menu.rs:4081`-`4224` |

### If you want to change X, look at Y

| To change | Look at |
|---|---|
| Add a toggle row | `Id` (`menu.rs:150`), its page in `items_of`, `activate`, `command`, then `default_state`, `default_of`, `has_default`. Add it to `needs_world` (`menu.rs:3895`) if it touches the player's character or hooks game code. `state_of` handles toggles by kind, no change needed. |
| Add a number row | As above, plus `range`, `typed`, `current`, `set_typed`, `adjust`, the typed list in `activate` (`menu.rs:1449`). If Left must not step it, the list in `press` (`menu.rs:1837`). |
| Add a page | `Page` (`menu.rs:35`), `page_name` (`menu.rs:108`), an arm in `items_of`, an `Id::Open(Page::X)` row on the parent. If it is a tool page (lists other rows, a question, a picker), the four skip lists (clarity note 5). |
| Add or reorder tabs | `SECTIONS` (`menu.rs:105`). `State.saved` is sized from it. |
| Rename or move a row | Its text in `items_of`, then a line in `MOVED` (`menu.rs:2895`) so old hotkeys, saved commands and profiles still find it. The test `target_rows_follow_their_move` (`menu.rs:4100`) shows how. |
| Navigation keys | `NAV` (`menu.rs:1758`), and `MENU_KEYS` in `dinput.rs:29` (DirectInput scan codes, not Windows key codes). |
| Default menu / command box key | `Keys::default` (`core/keys.rs:61`-`63`): F4 and Y. |
| Sizes, fonts, layout of the picture | `Metrics`, `LAYOUT`, `LIST` (450 / 32, the old 85%), `axes`, `Geo` (the right wall), `render_in` in `menuview.rs`; Max Visible Commands is `View.visible` (`shown_rows` in `menu.rs`) |
| Menu colours | `ACCENTS` (`menu.rs:306`) |
| ESP colour list | `BOX_COLOURS` (`menu.rs:271`) |
| Default menu position | Three places: `Prefs::default`, `default_of` (`menu.rs:3321`-`3327`), `default_state` (`menu.rs:3691`-`3692`) |
| Toast look and place | `toast_tick` (`menu.rs:2013`), `render_toast` (`menuview.rs:393`), `TOAST_CORNERS` |
| Command box look and place | `render_input` (`menuview.rs:369`), `input_tick` (`menu.rs:2887`): centred, 7% from the top |
| What a typed command accepts | `run_found` (`menu.rs:2588`), `run_line` (`menu.rs:2679`) |
| Toast wording, or when it is hidden | `note` / `done` / `confirm` (`menu.rs:1196`-`1238`), `FROM_*` (`menu.rs:1207`) |
| What a profile saves | `state_of` (`menu.rs:3636`) |
| Profile file format | `write_tree` / `read_tree` (`menu.rs:3720`, `3743`) |
| What waits for the world after a profile load | `needs_world` (`menu.rs:3895`), `world_ready` (`menu.rs:3899`) |
| "Are you sure" pages | `Page::Unload` (`menu.rs:1024`), `Page::ProfileDelete` (`menu.rs:1110`), `Page::Risk` (`menu.rs:997`); timers `UNLOAD_ASKED`, `DELETE_ASKED`, `RISK_SHOWN` |
| Where files live | `paths.rs` |
| Where the drawing hooks in | `hook_at` (`vkdraw.rs:259`); set in the Launchpad's Options |

---

## 3. Input path

The mod reads keys with `GetAsyncKeyState` (`down`, `menu.rs:1949`). That asks Windows directly, so it works whatever the game sees.
Hiding keys from the game is a separate job. The game reads keys in two ways, so there are two blockers.

### Who owns the keys right now

The worker sets these flags every tick (`menu.rs:2097`-`2103`):

| Flag | True when | Read by |
|---|---|---|
| `menu.rs` `OPEN` and `dinput::OPEN` | menu open, or command box open, or a just-picked key is still held | `wndproc`, `dinput::blocked` |
| `menu.rs` `EDITING` and `dinput::EDITING` | command box open | `eats`, `is_nav`, dinput's mouse blocking |
| `dinput::BINDING` | command box open, or a just-picked key is held, or waiting for a new key with the menu open | `eats` ("all"), `dinput::blocked`, `fly.rs:59` |
| `dinput::MOUSE` (`set_mouse`, `menu_has_mouse`) | menu open, the game in front and Mouse Support or Move With Mouse on; lapses by itself 500 ms after the worker last said so | `eats` / `mouse_ours`, dinput's `state` and `data` |

When `BINDING` is true, every key is kept from the game.

### Blocker 1: the game window's messages

- `subclass` (`menu.rs:2333`) swaps the game window's message function for ours, `wndproc` (`menu.rs:2320`). The window is the game's biggest visible window, `vkdraw::find_window` (`vkdraw.rs:1221`).
- It is called from `take_keys` (when the menu or box opens) and from `keep_hotkeys` (every 500 ms, if not done yet, `menu.rs:3429`).
- `wndproc` swallows a message when `OPEN` and `eats(...)` says yes, or when it is a hotkey (`hotkey_msg`, `menu.rs:2268`). Swallowed `WM_INPUT` still goes to `DefWindowProcW` so Windows frees it. Everything else goes to the game's own function.
- `eats` (`menu.rs:2284`):
  - With `BINDING`: every key, char and system key message.
  - Key down / up: only navigation keys (`is_nav`, `menu.rs:2253`). Ctrl and Shift count only on the right side (`right_side`, `menu.rs:2258`).
  - `WM_CHAR`: Enter and Backspace; while typing, also digits, `.` and `a`-`f`.
  - Mouse: moves, presses and the wheel while the command box is open, or while the menu has the mouse (`mouse_ours`, `menu.rs:3120`). Releases always pass, so a held button is not stuck.
  - `WM_INPUT` (raw input): the same rules for keyboard and mouse.
- `unsubclass` (`menu.rs:2355`) puts the game's function back and waits up to 2 s for calls in flight.

### Blocker 2: DirectInput (`dinput.rs`)

The game also reads the keyboard through DirectInput 8, past its window messages (`dinput.rs:1`).

- `ensure` (`dinput.rs:284`) runs once. It makes its own throwaway keyboard device to find the functions (`table`, `dinput.rs:253`), then detours `GetDeviceState` and `GetDeviceData` (`install`, `dinput.rs:299`).
- `blocked(dik)` (`dinput.rs:113`) decides per key:
  1. A Aurora hotkey: always blocked, menu open or not (`HOTKEY_DIK`).
  2. Fly's down key while Fly is on: blocked, because it is also the game's crouch (`dinput.rs:120`).
  3. Menu closed: nothing else.
  4. Menu open: everything with `BINDING`; else the menu's keys (`MENU_KEYS`, `dinput.rs:29`: both Enters, Backspace, arrows, Right Shift, Right Ctrl, numpad 8 2 4 6 5 0, O, Delete); while typing, also `DIGIT_KEYS` (`dinput.rs:31`).
- `state` (`dinput.rs:188`): in the 256-byte keyboard state, blocked keys read as up. While the command box is open, a mouse state (16 or 20 bytes) reads as still.
- `data` (`dinput.rs:212`): in the event list, presses of blocked keys are removed, releases stay. While typing, mouse events go except button releases.
- Left Shift and Left Ctrl stay the game's (`dinput.rs:28`).

### The mouse



- Only with Menu > Settings > Mouse Support on, or Move With Mouse on. Both off: no DirectInput zeroing, no messages eaten, no pointer, no hover, clicks or wheel. Turned off with the menu open, the game has the mouse again on that same tick (`menu_takes_mouse`, `menu.rs:2839`, the same `set_mouse` path as closing the menu).
- The game has no raw input. It reads the mouse through DirectInput 8, like the keyboard. It also uses `SetCursorPos`, `ClipCursor`, `ShowCursor` and `GetCursorPos`.
- So the mouse is blocked in the same two places as the keys. No new hook, nothing in Windows is changed (no `ClipCursor`, `ShowCursor`, `SetCursorPos`, no device re-acquire):
  - `dinput.rs` `state` (`:318`): a mouse state (16 or 20 bytes) is read for the menu, then zeroed for the game.
  - `dinput.rs` `data` (`:359`): a mouse's events (device type checked, `kind`) are read for the menu (`note_mouse_events`, `:292`), then only button releases stay for the game. Peeks (`DIGDD_PEEK`) are not counted.
  - `eats` in the window procedure: mouse messages and raw mouse input are swallowed, releases pass.
- When: `set_mouse(menu_takes_mouse(st, focused))` every worker tick (`menu.rs:2714`): open, the game in front, Mouse Support or Move With Mouse on. Game behind, menu closed, Unload: the game gets the mouse on its next read.
- Never stuck: `menu_has_mouse` (`dinput.rs:44`) is true only if the worker said so in the last 500 ms. `menu::shutdown` and `dinput::shutdown` turn it off first; then the hooks come out as before.
- The pointer (`Pointer`, `menu.rs:2771`): moves by the game's mouse moves (mickeys, 1:1 to screen pixels). If the game did not read its mouse, Windows' pointer, buttons and wheel messages are used instead (`os_pointer`, `menu.rs:2905`). Buttons held when the menu took the mouse wait to be let go.
- What it does (`Pointer::tick`, `menu.rs:2815`):
  - Hover: the pick follows the pointer onto a row, only when the pointer or the list moved (`hover`, `menu.rs:2965`). Titles and greyed rows are skipped. Tabs are not hovered.
  - Left click: a row is Enter on it; a tab shows it (`mouse_click`, `menu.rs:2978`).
  - Right click: Back, anywhere, never into a row.
  - Wheel: scrolls the list 3 rows a notch, only if it is longer than Max Visible Commands; never a value or the pick (`wheel_scroll`).
  - Move With Mouse on: dragging the address bar moves the menu, kept on screen; X / Y follow, as the bar's own spot (`drag_menu`, `menu.rs:2938`).
  - Nothing but the pointer moves while the command box is open or a key is being picked.
  - Clicks go into the keys log written at Unload (`note_seen`).
- Hit-testing: `menuview::spot_in` uses `Geo` (`menuview.rs:147`), the same sizes `render_in` draws with, at the scale, room and spot the picture was drawn (`State::drawn`, a `Drawn`).

### Navigation keys

`NAV` (`menu.rs:1758`), with the number pad working like the arrows (as Stand):

| Key | Does |
|---|---|
| Up / Numpad 8, Down / Numpad 2 | move |
| Left / Numpad 4 | step down, or back |
| Right / Numpad 6 | step up, or open |
| Enter / Numpad 5 | do it |
| Backspace / Numpad 0 | back |
| Right Ctrl / Right Shift | next / previous tab |
| Delete | remove a spot or a key |
| O | the row's "O" menu |

The menu key itself (default F4) opens and closes (`menu.rs:2075`). There is no Escape-to-close for the menu; Escape only cancels the command box and key prompts.
Watch out: the top comment of `menu.rs` (line 4) still says Insert. The default is F4 (`core/keys.rs:61`).

### Hotkeys

- Two kinds:
  - **Menu keys**: Menu and Command Box (`BIND_ROWS`, `menu.rs:261`). Stored as the top two lines of `Hotkeys.txt`. Changed in Menu > Hotkeys.
  - **Row hotkeys**: any row, from its "O" > Hotkeys > Add Hotkey. Stored in `Hotkeys.txt` by the row's address.
- Fly's moving keys (W A S D, Shift, Ctrl, Space) are built in and kept nowhere (`core/keys.rs:51`-`57`, `core/lib.rs:27`).
- `row_hotkeys` (`menu.rs:3119`) runs every tick. It works with the menu closed too. It does nothing while typing or picking a key, or when the game is not in front.
  - For each bound key, on a fresh press, every row with that key runs (`menu.rs:3133`). One key can do several things.
  - It finds the row by address (`resolve`) and calls `run_found(st, f, None)` with `FROM_KEY`, so Suppress Generic Responses > Hotkey applies.
  - What happens depends on the row: a toggle flips; a sub-page opens the menu there (`navigate`, `menu.rs:2563`); a number row opens the command box with its value; an action runs.
  - A row that no longer exists toasts "Hotkey: ... is gone".
- `keep_hotkeys` (`menu.rs:3418`), every 500 ms, gives the list of all bound keys to `dinput::set_hotkeys`, so the game never sees them.

### Key prompts (picking a new key)

- Enter on Menu > Hotkeys > Menu or Command Box sets `st.bind = Some((Bind::Action(..), held_now))` (`menu.rs:1454`). "O" > Hotkeys > Add Hotkey sets `Bind::RowAdd` (`menu.rs:1485`).
- The row shows "press a key" and the help "Esc cancels." (`menu.rs:1930`).
- `binding_step` (`menu.rs:2199`) waits for a key that was not already down:
  - Escape cancels.
  - Delete removes the key (not for the menu key: "The menu always needs a key").
  - Any key that `bindable` allows (`menu.rs:2185`: not left / right mouse, not IME keys) is set.
- Then `bind_hold` keeps that key from the game and from hotkeys until it is let go (`menu.rs:2094`).
- `set_bind` (`menu.rs:772`) saves a menu key and toasts if the key is also used by another action.

### The command box

Stand's command box: a line at the top middle where you type a command.

- **Opening it**:
  - the command box key (default Y, `menu.rs:2085`);
  - Enter on a number row: filled with `command value`, e.g. `esprange 250` (`menu.rs:1449`);
  - "O" > Rename on a spot: `renamespot spot1 ` (`menu.rs:1487`);
  - Create Profile: `createprofile ` (`menu.rs:1506`);
  - a hotkey on a number or colour row (`run_found`, no value).
  - `open_box` (`menu.rs:2580`) remembers which keys were already down, so they do not type.
- **Typing** `box_step` (`menu.rs:2776`):
  - letters (Shift for capitals), digits, numpad digits, space, `.`, `-`, and Shift+3 for `#` (`typed_char`, `menu.rs:2762`). At most 60 characters.
  - Left / Right move the caret one character, and stop at the ends. Typing goes in at the caret.
  - Backspace erases before the caret; Ctrl+Backspace erases the word before it. Held Backspace, Left and Right repeat (after 400 ms, every 35 ms; `box_held`).
  - Up / Down walk the last 50 commands. They, Tab and opening the box put the caret at the end (`caret` None).
  - Tab goes through every command that starts with what was typed.
  - Escape closes. Enter runs it with `FROM_BOX`.
- **Under the box** `matches` (`menu.rs:2726`):
  - a full command shows only itself and its values, as `firerate 0.00 - 1000000.00` (`box_syntax`: the help box's range, "to" as " - ", without the note in brackets; rows without a value show the name only). What a row does is only in the help box beside the menu;
  - part of one lists up to 7 "command - Row", then "... and N more.";
  - otherwise "No matching commands".
- **Running** `run_line` (`menu.rs:2679`):
  - The first word is the command, the rest is the value.
  - Two commands are not rows: `tp x y z` and `renamespot oldname newname`.
  - Otherwise the exact command, or the only command that starts with it (`god` finds `godmode`). Several matches: "Several commands start with...".
- **Command names** `command(id)` (`menu.rs:2391`): the row's name, lower case, no spaces. Where the name alone says too little, what it changes comes first: Fly > Fly Speed is `flyspeed`.
  Examples: `godmode`, `fly`, `flyspeed`, `esprange`, `esphostilecolour`, `damagemultiplier`, `listwidth`, `menuheight`, `tabsw`, `notifytime`, `goto` + spot name, `createprofile`, `default`, `unload`.
  Rows without a command (sub-pages, most profile rows, "O" rows) return `None`.
- **Values** `run_found` (`menu.rs:2588`):
  - Number rows: a number. No number: the box opens with the current value (`current`, `menu.rs:2470`).
  - Toggles: `on` / `off` / `true` / `false` / `1` / `0`. Nothing: flip. "X is already on" when nothing changes.
  - ESP colours: a name from `BOX_COLOURS` or 6 hex characters (`FF8800`).
  - Menu Colour: a name from `ACCENTS`.
  - Notification Position and the colours: a name, spaces optional (`notifyposition topright`, `notifybordercolour menu`).
  - Old names still work, hidden (`old_command`): `toastposition`, `toastcolour`, `toastsize`, `toasttime`, `applydefaultstate` (now `default`), and `menuleftright` / `menuupdown`, whose percent becomes pixels (X as the wall: the old picture's left edge plus the help boxes' room plus the bar to the wall, at the 85% look, kept to the screen's width). `commandswithnondefaultstate` and `menumovewithmouse` still run their rows quietly. `menusize` and `suppressgenericresponsesmenu` say they went (`GONE_COMMANDS`).
  - Style: `box` or `bones`.
  - `createprofile name`.
- `typed(id)` (`menu.rs:2465`) lists the rows whose value is a typed number.
- Apply Default State's command is `default`; the old `applydefaultstate` still works.

### Scripted commands: `command.request`

`command_file` (`menu.rs:3399`) looks for `%APPDATA%\Aurora\command.request` every 250 ms.
It deletes the file first, then runs each line as if typed in the box (`FROM_BOX`), and logs each line. For test scripts.

---

## 4. Drawing path

### From state to picture

1. `view(st)` (`menu.rs:1865`) builds a `View` (`menuview.rs:40`):
   - `path`: `Aurora <version> > Self > Movement > Fly`.
   - `sections`, `section`: the tabs and which is lit.
   - `rows`: the page's rows, with hotkey labels added and waiting rows turned into `Kind::Waiting`.
   - `pick`, `scroll`.
   - `help`: stacked boxes (`help_lines`). In order: `Command: esprange [25 - 1500 m]` always on top, then the context: the row's help (or "Enables once your character loads." for a waiting row) and the row's address on Saved / Non-Default pages, then "Click to input a value." always at the bottom for number rows. Findc's rows show only where they live. Findc matches the shown name only; a matching list stands for its rows. While picking a key: "Esc cancels.".
   - `accent`: the menu colour.
   - `layout`: tabs width and height, address bar height and width (the first four of `Prefs.layout`).
   - `list`: List Width and List Height; `visible`: Max Visible Commands (capped to what fits, `shown_rows`); `spacer`: Spacer Size.
   - `help`: a blank line (`NEW_BOX`) starts the next box; a plain line break is a new line in the same box. Under the command box the lines become one (`one_line`: ", " between them, a space after a full stop).
2. `Painter::render(&view, scale)` (`menuview.rs:231`) draws it on the CPU:
   - The font is Segoe UI from `C:\Windows\Fonts\segoeui.ttf`, loaded once (`painter`, `menu.rs:427`). No font, no menu ("menu: Segoe UI not found" in the log).
   - `scale` = screen height / 1080. List Width / Height turn it into an across and a down scale (`menuview::axes`: scale x 0.85 x List Width / 450, and x List Height / 32), each kept to 0.2-4.
   - Layout, the right wall: the picture's right edge is X. The address bar on top and the list under it end on it; the tabs sit a gap left of the list; the help boxes under the tabs, ending at the tabs' right edge. Wider parts grow to the left only. The room left of the wall is X (and the 2048-pixel slot); what does not fit is cut at the picture's left edge (tab names keep their place in the whole tab, help boxes wrap narrower or go when there is no room for them). Max Visible Commands rows before the list scrolls, with a thin scroll bar.
   - The result is a `Frame` (`menuview.rs:64`): RGBA pixels plus a list of `panels` (rectangles). Every panel is opaque. Only panels are copied into the game, so there is no see-through blending.
3. The worker places it: Position X and Y are the screen pixel of the address bar's top-left. The picture is drawn with `render_in(v, scale, room = X)`: the help boxes get only the room left of the bar the screen has and narrow there, so they never move the menu (tabs wider than usual still stick out left of the bar). It goes at X - `bar_left` (`menu_place`), kept on screen. It wraps the frame in a `MenuPic { generation, frame, x, y }` and calls `vkdraw::set_pic(vkdraw::MENU, ...)` (`menu.rs:2178`). `generation` goes up with every new picture.

### Into the game's frames (`vkdraw.rs`)

- `set_pic(slot, pic)` (`vkdraw.rs:105`) stores a picture in one of three slots (`vkdraw.rs:95`-`99`):
  - `MENU` = 0: the menu;
  - `TOAST` = 1: the toast;
  - `INPUT` = 2: the command box;
  - `CURSOR` = 3: the mouse pointer. Its image is 128 x 128 at most (`slot_size`, `vkdraw/mod.rs:173`), so it costs next to nothing.
  - `None` hides it.
- `ensure_hooks` (`vkdraw.rs:309`) hooks the game once. `take_keys` calls it when the menu or box opens; `toast_tick` calls it for toasts.
  - It reads the Launchpad's `hook` setting once (`hook_at`, `vkdraw.rs:259`): Auto, Driver, or Off.
  - It makes a throwaway Vulkan device to learn where three functions live (`probe`, `vkdraw.rs:479`), then detours them (`detour`, `vkdraw.rs:518`):
    - `vkQueuePresentKHR` -> `hk_present` (`vkdraw.rs:653`): the game's "show this frame" call. All drawing happens here.
    - `vkAcquireNextImageKHR` -> `hk_acquire` (`vkdraw.rs:543`): only to learn the game's device.
    - `vkCreateSwapchainKHR` -> `hk_create` (`vkdraw.rs:562`): adds the transfer flag Vulkan needs for copying into frames, and notes the size.
  - It writes where it hooked to `Bin\hook.txt` for the Launchpad.
- Each frame, `hk_present` -> `draw` (`vkdraw.rs:731`) -> `record_and_submit` (`vkdraw.rs:960`):
  1. ESP boxes first.
  2. Then each picture in slot order: menu, toast, command box. So the box sits on top of the toast, the toast on top of the menu.
  3. A picture with a new `generation` is uploaded: pixels into a buffer in 64 KB pieces, then into the slot's own image (`vkdraw.rs:1080`-`1135`).
  4. Only the panels are copied (blitted) into the frame, clipped to the screen (`vkdraw.rs:1137`-`1156`).
  5. The present then waits for the mod's copy (`vkdraw.rs:704`-`711`).
- Limits and safety, from the code:
  - A picture larger than 2048 x 1536 is skipped without a word (`MENU_W`, `MENU_H`, `vkdraw.rs:84`, `974`).
  - Only frames made with the transfer flag are drawn into (`safe_swap`, `vkdraw.rs:619`). Others get the game asked to remake its swapchain (`nudge`, `vkdraw.rs:626`).
  - If a frame's slot is still busy after 2 ms, that frame is skipped, so the game is never held up (`vkdraw.rs:1010`).
  - Any error or panic while drawing sets `BROKEN`. All drawing then stops for the session (`vkdraw.rs:693`-`699`).

### Toasts

- `toast(msg)` stores the message and the time; up to 8 are kept and shown stacked, as Stand's notifications. Other modules call it too (`gearscore.rs:200`).
- `toast_tick` (`menu.rs:2013`), every worker tick:
  - Hides each one after Timing > Time seconds (`Prefs.layout[TOAST_TIME]`).
  - Draws them with `render_note`: the Background Colour box with a Border Colour bar on its left, fitting its text snugly; Width is only how wide it may grow before the text wraps; 4 px apart (`NOTE_GAP`). Width is capped and stacking stops at the 2048 x 1536 slot, so they never vanish.
  - Size: screen scale x Notifications Size %, for the box. Text Size: `NOTE_FONT` x screen scale x Text Size % (`note_font`), for the text. Menu Size went: an old profile's Menu Size P is multiplied into its notification Size and Text Size (`upgrade_rows`), so they look as before.
  - Where: `note_place`; Custom Position's X / Y is the top-left.
  - Corner: `TOAST_CORNERS`, 30 px from the side, 90 px from the bottom or 30 from the top, at 1080p (`menu.rs:2039`-`2048`).
- When a toast shows (`menu.rs:1196`-`1254`):
  - `note(msg)`: always shows. Used for errors, warnings and "what to do next". While a profile loads, it also goes to the log.
  - `done(msg)`: a plain "X on / off / set to". Never from the menu by hand, also a value typed after clicking its row, while the box still holds that row's command. From the command box or a hotkey, hidden by that Suppress Generic Responses switch. Always hidden while a profile loads (`quiet_from`). `note` (errors, warnings) shows everywhere except after the command box with Suppress > Command Box on: then nothing shows, "No command called x" included, and the line goes to dll.log as "command box (suppressed): ...".
  - `confirm(msg)`: other "it worked" messages ("Profile saved", "Address copied"). From the menu they show; from the command box or a hotkey, as `done`.
  - `switched(result, ok)` and `result(result, ok)`: `done` / `confirm` on success, `note` on failure.
  - Where a command came from is the global `FROM` (`menu.rs:1211`): `FROM_MENU`, `FROM_BOX`, `FROM_KEY`, `FROM_PROFILE`. It is set around `run_line`, `row_hotkeys`, `command_file` and profile loads.
  - The two Suppress switches are copied into atomics (`QUIET_BOX`, `QUIET_KEY`) by `mirror_quiet`.


### The command box picture

`input_tick` (`menu.rs:2871`) draws it with `render_input` (`menuview.rs:369`): a bar titled "Aurora Command Box", the text with a `|` caret, then one strip per line from `matches`. The caret is drawn over the gap before its character (`pen_at`), so the text stays still when the caret moves. It sits centred, 7% from the top (`menu.rs:2887`). It is redrawn only when the text, the caret, the colour or the size changes.

---

## 5. Files

All in `%APPDATA%\Aurora\` (`paths::root`, `paths.rs:36`). `paths.rs:7`-`24` has the full table of who makes, writes and reads each one.
The log is `%APPDATA%\Aurora\Dumper\dll.log` (`dll/src/lib.rs:42`).

### The safety rules

1. **Whole-file writes through `Bin\Temp`.** `paths::write_file` (`paths.rs:97`) writes the new text to `Bin\Temp\<name>.<hash>.tmp`, then renames it over the real file. A crash mid-write leaves the old file whole.
2. **An unreadable file is never overwritten.** `read_text` (`menu.rs:488`) tells three cases apart: there (`Ok(Some)`), not there yet (`Ok(None)`), there but unreadable, e.g. Locked or not UTF-8 (`Err`). On `Err` the callers do not write:
   - `Hotkeys.txt`: `HOTKEYS_UNREADABLE` is set at load (`menu.rs:2985`); `write_row_keys` then refuses for the session (`menu.rs:3055`). `core::save_keys` refuses too (`core/lib.rs:79`-`83`).
   - `Saved Commands.txt`: `SAVED_UNREADABLE` (`menu.rs:3151`, `3185`).
3. **Old files are read once and left on disk**: `row_keys.json`, `saved.json`, `teleports.json` / `spots.json`, `keys.json` (`paths.rs:24`). `settings.json`, `menu.json` and `Offsets.json` are carried over once and moved to `Dumper\old` (`carry_over_old_files`); never deleted.

Where the rules do not apply (read from code):

- `Meta State.txt` is read without the check (`meta`, `menu.rs:3599`). A locked file would read as empty, and the next flag change would write just that flag.
-

### The files

**`Hotkeys.txt`**. Row hotkeys plus the two menu keys.
- Written by `write_row_keys` (`menu.rs:3054`) whenever a row hotkey changes, and by `core::save_keys` (`core/lib.rs:72`) when a menu key changes.
- Read by `row_keys` once per session (`menu.rs:2978`), and by `core::read_keys` (`core/lib.rs:53`), which the Launchpad also uses.
- Format: Stand's tree (`write_tree`, `menu.rs:3720`). The two menu keys always sit right under the version line (`with_menu_keys`, `core/lib.rs:89`). Keys are written by name (`key_name`, `core/keys.rs:159`), several joined by `, `. An unknown key name is logged and dropped (`menu.rs:2996`).

```
Tree Compatibility Version: 1
Open Menu: F4
Command Box: Y
Self
	God Mode: F5
World
	ESP
		Range: F7
	Teleport
		Go To spot 1: Numpad 2, F8
```

**`Saved Commands.txt`**. Rows saved with "O" > Save.
- Written by `set_saved` (`menu.rs:3184`).
- Read by `saved_list` (`menu.rs:3146`) each time it is needed.
- Format: the same tree, every value `Saved` (test at `menu.rs:4178`).

```
Tree Compatibility Version: 1
Self
	God Mode: Saved
World
	ESP
		Range: Saved
```

**`settings.json`, `menu.json`**. The menu's look and the feature settings (ESP, fly speed, Safe Fly, gear score, coordinates) are profile rows now, as Stand's: nothing writes these files.
- At the first inject of that build, `carry_over_old_files` turns their values into lines of the Load On Inject profile (Main when none is marked, made if needed), adding only the lines it lacks, so the player's profile wins. `old_values` lists what is carried; defaults are left out.
- Not carried: ESP's switch and the gear score hold. Old builds switched both off at every inject, so the file only said how the last game ended.
- Then both files, and the old `Offsets.json`, move to `Dumper\old` (`paths::move_aside`, never replacing a file there). A file that can't be read, or a profile that can't be written, leaves them for the next inject. Each step is in `dll.log`.

**`Meta State.txt`**. Which profile loads on inject, and which save automatically.
- Written by `write_meta` (`menu.rs:3608`) when a flag changes or a profile is deleted.
- Read by `meta` (`menu.rs:3599`).
- Only one profile can have Load On Inject (`set_profile_flag`, `menu.rs:3626`).

```
Load On Inject: Main
Automatically Save: Main
```

**`Places\<name>.txt`**. One teleport spot per file.
- Made by Save This Spot (`add_spot`, `menu.rs:699`), named `spot 1`, `spot 2`, ...
- Renamed by "O" > Rename or `renamespot` (`rename_spot`, `menu.rs:723`); its hotkeys and saved-command entry move with it.
- Deleted by Delete on its row (`remove_spot`, `menu.rs:709`).
- Read by `spots` (`menu.rs:624`) whenever the Teleport page is built. Sorted by name.

```
X: -3857.6
Y: 3057.7
Z: 457.9
```

**`Profiles\<name>.txt`**. A profile.
- Written by Save, Create Profile, and Automatically Save (`save_profile`, `menu.rs:5878`; `profile_tick`, `menu.rs:6441`).
- Read by Load (`load_profile`, `menu.rs:5979`).
- Format: the same tree, only rows not at their default (`profile_text`, `menu.rs:5778`), as `profile_state` keeps them: never rows inside Aurora > Experiments, Dropped Item Gear Score up to 1000. Values: `On` / `Off`, a name as the row shows it, or a number (2 decimals only when it has a fraction, `number`, `menu.rs:5727`).
- Version 4: X is the wall, the menu's right edge (version 3's X plus the bar to the wall, the tabs, the gap and the list at that profile's Menu Size or 100%, kept on the screen: `upgrade_v3`, `old_x_to_wall`). Version 3: Sprint is its own speed (version 2's Speed x Sprint, rounded to 0.01, 0.01-10000; no Sprint line and Speed not 1 gets one, Speed x 4), and X the address bar (version 2's X plus the help boxes' room at that profile's Menu Size and Tabs Width, `old_x_shift`). Version 2: Fly's multipliers. Version 1 holds m/s. Every read upgrades one step at a time (`upgrade_v1`, `upgrade_v2`, `upgrade_v3`), so version 1 lands where 1 -> 2 -> 3 -> 4 does.
- Menu Size lines: Menu Size P becomes List Width / Height = 450 x P / 85 and 32 x P / 85, rounded (no lines when that is 450 / 32, so 85% needs none), and the notification Size and Text Size are multiplied by P / 100 (a missing one counts as 100%). The Menu Size line itself is then dropped quietly (`GONE`). The player's Main profile (Menu Size 85, no X line): List Width / Height stay 450 / 32, Size and Text Size become 85%, X stays the default (the right edge on the player 2560 x 1440 screen, where the list ended before), NVG Overlay On becomes No NVG Overlay On. A profile with no Menu Size line was 100% and now gets 450 / 32, the 85% look.
- Save State / Load State ("O" menu) write or read only that row, or every row in that list, in the active profile (`save_state`, `load_state`). A row with no line loads its default, as Load. Experiments rows: Save State writes nothing ("Experiments are never saved"), Load State puts them at their default.

```
Tree Compatibility Version: 3
Self
	Movement
		Fly
			Fly Speed: 2.50
	God Mode: On
World
	ESP
		ESP: On
		Range: 700
		Drawing
			Style: Bones
```

**`command.request`**. Commands for test scripts, one per line (section 3).
- Written by scripts. Run and deleted by the mod within 250 ms.

```
esprange 250
godmode on
tp 100 200 50
```

**`unload.request`**. An empty file, for test scripts.
- Written by scripts. The mod checks for it with `command.request` (every 250 ms), deletes it, and unloads itself exactly like Menu > Unload Aurora > Yes (`unload_now` -> `server::unload_self`).
- The Launchpad never reads it and never unloads.

### Addresses: how files point at rows

Hotkeys, saved commands and profiles store a row by its address: the tab, each page's row text, the row's text, joined by ` > `.
`all_rows` (`menu.rs:2506`) walks every page to build them. It skips "O" pages, tool pages and pickers (`menu.rs:2516`).
So **renaming or moving a row breaks every file that points at it**, unless `MOVED` gets a line (`menu.rs:2895`). Old addresses are moved on load:

- `row_keys` rewrites `Hotkeys.txt` (`menu.rs:3043`);
- `saved_list` rewrites `Saved Commands.txt` (`menu.rs:3177`);
- `load_profile` maps old lines on the fly (`menu.rs:3813`).

Profile rows get addresses too, e.g. `Aurora > Profiles > Main > Load`, so they can have hotkeys (test at `menu.rs:4131`).

---

## 6. Profiles, defaults, saved commands, non-default state

### Four functions that fit together

| Function | Line | Gives |
|---|---|---|
| `state_of(item, prefs)` | `menu.rs:3636` | The row's state now, as a profile writes it (`"On"`, `"Bones"`, `"250"`). `None` for rows that profiles do not keep. |
| `default_state(id)` | `menu.rs:3673` | The default, in the same words. |
| `default_of(st, id)` | `menu.rs:3230` | Actually puts the row back to its default (turns a hook off, writes a file). |
| `has_default(id)` | `menu.rs:3344` | Whether `default_of` can reset this row. Decides if "O" shows Apply Default State (`menu.rs:964`). |

Plus `same_state` (`menu.rs:3699`): numbers compare as numbers (8 = 8.00), names ignore case.

Rule of thumb: a row is "changed" when `state_of` is `Some` and differs from `default_state`.

How they are used:

- **Commands With Non-Default State** (`nondefault_rows`, `menu.rs:3459`): every row where `state_of != default_state`. The rows work in place. `refit_nondefault` (`menu.rs:3448`) keeps the cursor inside the list as it shrinks.
- **Profile Save** (`profile_text`, `menu.rs:5778`): the same rows, written as a tree (not the rows inside Experiments, `profile_state`).
- **Profile Load** (`load_profile`, `menu.rs:5979`): every row with a state gets the profile's value, or its default if the profile does not list it. So you get exactly what was saved.
  - Old addresses go through `moved`.
  - Lines that match no row are skipped and logged once.
  - Fly is applied last, so Safe Fly and God are set first.
  - Toggles go through `activate`. Other rows go through `run_found` with the value, as if typed (`apply_state`, `menu.rs:3934`). So a load takes the same path as the player's own changes.
  - `FROM_PROFILE` hides the plain toasts; one "Profile X loaded" shows at the end.
- **Apply Default State** (Menu tab, `default_all`, `menu.rs:3220`): `default_of` on every row with a state, **and** on every menu setting. Hotkeys stay.
- **Apply Default State To Children** ("O" on a sub-page row, `defaults`, `menu.rs:3196`): every row inside that page, at most 6 levels deep. On Commands With Non-Default State, every row it lists.

### Rows profiles do not keep

`state_of` returns `None` for these (`menu.rs:3642`-`3645`):

- (the menu settings and Coordinates X / Y / Z are no longer here: Menu Colour, Size, Position, the Tabs, Address Bar and Notifications rows, Suppress Generic Responses, the Warnings switches and X / Y / Z are profile rows like any other, as Stand's.)
- **One-offs the game keeps**: Credits, Total Gear Score, the gear slots, Set All Owned Items.
- **Others**:
  - Hex Code (its colour is kept through the colour row);
  - Load On Inject and Automatically Save (kept in `Meta State.txt`).
- Actions, sub-pages, info and value-only rows have no state either (`menu.rs:3654`).

`default_state` has an entry for every row with a state, the menu settings and Coordinates included.

### Waiting for the world

Some switches can only turn on once the player is in the world and the start-up code searches are done: `needs_world` (`menu.rs:3895`), `world_ready` (`menu.rs:3899`, uses `crate::WARMED` from `dll/src/lib.rs:65`).

- When a profile loads before that, a line that would turn such a switch on is parked in `WAITING` (`menu.rs:412`) instead.
- Its row shows the waiting ring. Its help says "Enables once your character loads." or "Enables once Aurora has started." (`wait_text`, `menu.rs:3912`).
- For profiles, its state is the parked value, so Save keeps it (`menu.rs:3637`).
- Enter on a waiting switch, or `off`, cancels the wait (`unwait`, `menu.rs:3886`). For Damage and Fire Rate, a new number waits in its place and 0 cancels (`requeue`, `menu.rs:3870`).
- `apply_waiting` (`menu.rs:3917`) applies them once a second, as soon as the world is ready.

### Load On Inject and Automatically Save

- `start` (`menu.rs:1954`) looks for the profile flagged Load On Inject and keeps its name in `st.inject_load`.
- `profile_tick` (`menu.rs:3984`), once a second:
  1. Applies waiting lines;
  2. Loads the Load On Inject profile once (about a second after inject);
  3. If the last loaded, saved or created profile (`st.profile`) has Automatically Save, writes it again whenever `profile_text` changed.
- A profile deleted outside the menu is not made again (`menu.rs:4006`).

### Profile names

- `new_profile` (`menu.rs:3776`) accepts up to 40 characters.
- Not allowed: `< >: " / \ | ? *`, control characters, a trailing dot, Windows device names (CON, NUL, COM1 ...), or a name already taken.
- Pages carry a number, not the name: `PROFILE_NAMES` (`menu.rs:3541`) maps numbers to names for the session.

### Saved commands

- "O" > Save adds or removes the row's address in `Saved Commands.txt` (`menu.rs:1466`).
- Menu > Saved Commands (`saved_rows`, `menu.rs:3472`) shows the rows themselves, working in place, sub-pages first.
- The help box adds where each row lives.
- A saved row that no longer exists shows as "<address> (gone)".

---

## 7. Clarity notes

Spots that confuse a newcomer, with a suggested fix. Nothing here was changed.

1. **`LAYOUT_ROWS` is not only layout** (`menu.rs:346`).
   - It also holds the notifications' Size, Time, Width and Padding (indexes 4 to 7).
   - `Prefs.layout` has 6 numbers but `View.layout` has 4 (`menuview.rs:54`); `menu.rs:1945` silently takes the first four.
   - Timing > Time has three names: row "Time", command `notifytime`, old `menu.json` key `toastseconds`.
   - Suggest: split into `LAYOUT_ROWS: [_; 4]` and `TOAST_ROWS: [_; 2]` with their own `Prefs` fields (`toast_size`, `toast_secs`), or rename the whole thing `NUMBER_PREFS`. Use one name for toast time.
2. **Two pages share a title, and names differ from titles.**
   - "Colours" is both `Page::MenuColours` (Menu > Settings > Appearance > Colours, the menu colour) and `Page::EspColours` (World > ESP > Drawing > Colours).
   - "Hotkeys" is both `Page::HotkeyList` (Menu > Hotkeys) and `Page::RowHotkeys` ("O" > Hotkeys). `KeyList` is called "Hotkey List" in its comments (`menu.rs:91`, `3486`).
   - The "Self" tab is `Page::SelfTab`.
   - Suggest: `Page::MenuColours` and `Page::EspColours`, `Page::HotkeyList` -> `Page::Hotkeys` with matching comments, `Page::SelfTab` -> `Page::SelfTab`.
3. **`Kind::Choice` and `Kind::Value` do not say what they do.**
   - `Choice` is used both for number sliders (Speed, Range, Size) and for name lists (colours, Style).
   - `Value` is used both for read-only lines (Where Am I, Pick Target, key names) and for typed-only numbers (Credits, Hex Code).
   - So what Left does is decided by a hand-kept `Id` list in `press` (`menu.rs:1837`), not by the kind. That list already misses `KeyRow`: Left on a Menu > Hotkeys > Commands row seems to do nothing, where other value rows go back (read from code).
   - Suggest: kinds `Number`, `List`, `Readout`, `TypedValue`, and let the kind decide Left / Right / Enter.
4. **The key-owner flags have two copies and misleading names.**
   - `OPEN` and `EDITING` exist in both `menu.rs:2247` and `dinput.rs:19`, set side by side (`menu.rs:2100`-`2103`).
   - `OPEN` means "menu, box, or held key", not "menu open" (`menu.rs:2098`); the real "menu open" is `st.open`.
   - `dinput::BINDING` is documented as "a new hotkey is being picked" (`dinput.rs:22`), but it is also true whenever the command box is open (`menu.rs:2099`). As a result the "while typing" digit lists (`is_nav` `menu.rs:2254`, `eats` `menu.rs:2292`, `DIGIT_KEYS` `dinput.rs:31`) look redundant: `BINDING` already blocks every key then.
   - Suggest: one shared set in `dinput.rs`, named `KEYS_TAKEN` (was `OPEN`), `TYPING` (was `EDITING`), `TAKE_ALL_KEYS` (was `BINDING`). Then drop the digit lists if a test confirms they are unused.
5. **The "special pages" list is copied four times, slightly different each time.**
   - `press` for O (`menu.rs:1787`), `view` for labels (`menu.rs:1900`), `all_rows` (`menu.rs:2516`), `defaults` (`menu.rs:3210`; this one lacks `Risk`, `CreditsPick`, `OtherPick`).
   - A new tool page must be added to each by hand.
   - Suggest: one `fn is_tool_page(p: Page) -> bool` (or a small enum of page roles), used in all four. `items` has its own sort-exception list (`menu.rs:810`) that could become a `keeps_order(p)` helper next to it.
6. **Two names for one thing.**
   - `confirm` only calls `done` (`menu.rs:1236`).
   - `hotkeys_of` only calls `keys_of` (`menu.rs:3379`).
   - A reader looks for a difference that is not there.
   - Suggest: keep `done` and `keys_of` and delete the wrappers. Or, if `confirm` is meant to differ one day, say so in its comment.

Stale comments worth fixing at the same time (not counted above):

- `menu.rs:4`: the menu key "Insert"; it is F4 (`core/keys.rs:61`).
- `menu.rs:231`: the command `default` is real.

- `menu.rs:250`: hotkeys in "keys.json" and "spots.json"; they are in `Hotkeys.txt` now.
- `menu.rs:316`, `menu.rs:572`, `vkdraw.rs:127`, `dll/keys.rs:2`: "the window", meaning the old launcher window, which was removed.

---

## 8. Design history

