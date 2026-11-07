# Aurora Launchpad and injection

The Launchpad (`launchpad` crate, window title **Aurora Launchpad <version>**. The exe is named `Aurora.Launchpad <version>.exe` (`tools\package.ps1`)) does five things and nothing else: find the game, launch it, make the folders, install the DLL, inject it. It has settings, but no Unload: the mod unloads itself from its own menu. Everything the mod does in game, it does on its own from its own files.

Code: `launchpad\src\main.rs` (app logic), `launchpad.rs` (the window), `platform.rs` (stores), `process.rs` (game process, injection), `install.rs` (folders, DLL), `settings.rs` (`Settings.txt`), `pipe.rs` (talking to the mod).

## 1. Finding the game (`platform.rs`)

Never a fixed folder: each store's own records, then a folder the player picks.

**Steam**
1. `HKCU\Software\Valve\Steam`, value `SteamPath` (fallbacks: `HKLM\SOFTWARE\WOW6432Node\Valve\Steam` / `HKLM\SOFTWARE\Valve\Steam`, `InstallPath`).
2. `<steam>\steamapps\libraryfolders.vdf` lists every library folder.
3. The library that has `steamapps\appmanifest_2231380.acf` holds the game; its `installdir` value names `steamapps\common\<installdir>`.
4. 2231380 is Breakpoint's Steam app id.

**Ubisoft Connect**
1. `HKLM\SOFTWARE\WOW6432Node\Ubisoft\Launcher` exists when Ubisoft Connect is installed.
2. `...\Launcher\Installs\<id>` has `InstallDir` for each installed game. The id is read, not assumed: the one whose folder holds a game exe. (The player's Steam copy shows up as `62412` with no folder.)
3. The Ubisoft-store id of Breakpoint is **unknown** (11903 is unverified); the code never hardcodes it.

**Fallback**: Advanced > **Locate...** opens a folder picker; a folder counts only if it holds one of `GRB.exe`, `GRB_vulkan.exe`, `GRB_UPP.exe`, `GRB_UPP_vulkan.exe`. Saved per store in `Settings.txt`.

Small `.vdf` parser: `vdf_values` reads `"key" "value"` pairs.

## 2. Launching

The store's own way, so the store and Ubisoft Connect start as they normally would:
- Steam: open `steam://rungameid/2231380`.
- Ubisoft with an id: open `uplay://launch/<id>/0`.
- Ubisoft without an id: run the exe from its folder; it hands over to Ubisoft Connect.

The game restarts itself once while starting (store, then Ubisoft Connect). The Launchpad treats a game that closes within 2 minutes of appearing as that restart and keeps waiting (`poll_game`).

**DirectX 11 vs Vulkan**: both are injected. The status line and the log say which one runs (`Game found (DirectX 11)`). Advanced > Where to hook > GPU driver is Vulkan only: on DirectX 11 it is greyed and the mod hooks as Auto. A Ubisoft launch with no store id starts `GRB.exe`, the DX11 build. See [dx11.md](../../dll/docs/dx11.md).

## 3. Folders and the DLL (`install.rs`)

- At start (before its settings are read), before each inject and on Open Aurora folder: `ensure_folders` first moves the old folders in, each in one rename so every file moves together or none does: `%APPDATA%\grbmod` to `%APPDATA%\Aurora`, then `%LOCALAPPDATA%\grbmod` (logs, research) to `%APPDATA%\Aurora\Dumper`, then the old launcher's `dumps` (now at `Aurora\dumps`) to `Dumper\dumps`. With both there, the new one is used and the old one is left alone.on test folders, including a locked one (`install.rs` tests). If the rename fails (the game still has the old DLL loaded, or a window shows the folder), nothing is made, inject is refused, and the status line says "Old grbmod folder in use: close the game, reopen." (an empty `Aurora` would hide the player's profiles and hotkeys).
- Then it makes `%APPDATA%\Aurora`, `Bin`, `Bin\Temp`, `Places`, `Profiles`, `Dumper` when missing. Existing ones are left alone, so it costs nothing when all are there and can never be skipped when one is missing.
- The DLL is always loaded from `%APPDATA%\Aurora\Bin\aurora.dll`; the exe can run from anywhere.
- `ensure_dll`:
  - The exe carries no DLL and never writes Bin's. It only reads it: the DLL row shows its age, and with none in Bin it says "no aurora.dll: copy aurora.dll from the build folder into <Bin>" and does not inject.
  - A new DLL goes in with `tools\swap-dll.ps1` (`swap-dll.md`).
  - Only stamped: the last 80 bytes of Bin's DLL are its stamp, an ECDSA P-256 signature (SHA-256) over the rest of its bytes, checked with Windows' CNG against the public key built into the exe (`sign_key.rs`). Windows ignores bytes after a DLL's last section, so a stamped DLL loads as before. One file to share; `aurora.dll.sig` is no longer read. No stamp: "not injected: aurora.dll carries no Aurora stamp (an unsigned or older build); copy aurora.dll from the build folder into <Bin>". Changed bytes: "not injected: aurora.dll was changed or isn't an official Aurora build; copy...". A stamp of a later format: "needs a newer Launchpad". The words are for someone without the repo; on the build PC `tools\swap-dll.ps1` puts it in. Auto inject stops then (the short lines "Not injected: aurora.dll has no stamp.", "Not injected: aurora.dll's stamp doesn't match." are kept in `Refused::short` for the tests and the log). The whole message opens in a window of its own, with Copy (to the clipboard), Open Bin folder (the player's folder app, as Open Aurora folder) and Close; The status line shows nothing of a refusal, no red line and no hover, before or after Close; Close keeps the window away until the next Inject that fails. The same window shows for "Not accepted" and for a missing DLL. A missing DLL shows there from the start ("No aurora.dll in Aurora\Bin."). From the check until LoadLibraryW has returned, the Launchpad holds the DLL open read only (others may read it, not write, rename or delete it), so it can't be swapped in between; after a load that timed out (15 s) it stays held until the game has the mod, the game is gone, or 5 minutes passed. DLLs from Advanced's Add button are not checked unless they are Aurora (the Add rule below). Key and scripts: `swap-dll.md`.
  - Then the build card: 96 bytes in the verified part of the DLL saying which Launchpad protocols it speaks (`PROTO_VERSION`, 5 since the stamp), its version and its build name. No card ("a build from before the cards"), a broken one, or one of a later format: not injected. A DLL whose range does not hold this Launchpad's protocol: "not injected: aurora.dll 0.2.0 speaks protocols 6 to 7, this Launchpad protocol 5; get the Launchpad from the same build" (short: "needs a newer Launchpad"), or "... speaks protocol 4 ...; copy aurora.dll from the build folder into <Bin>" (short: "is too old for this Launchpad"). A DLL that passes is named in the log: "aurora.dll checked: 26.11.1 (26.11.1, commit …), protocol 5 (stamp and build card)". Future DLLs work without a new Launchpad as long as their card's range still holds its protocol; the number changes only when the check itself does. A mod already in the game (after a Launchpad restart) gets the same check on Bin's file.
  - Then the module in the game against the checked file: its size must be the file's image size, its headers (as the loader maps them) and the card at the card's own address must be the file's bytes. Else "Not accepted: aurora.dll in the game isn't the checked file." and no go, no pipe, no extras. Why: a copy with the same code and a changed stamp, added with the Add button, answered the hello as Bin's file would. A copy that is the same bytes but came in another way still passes (by design: "anything by a program running as the player").
  - The Add rule: an Add DLL is Aurora when its name is aurora.dll (any case) or its bytes carry the card's label or the stamp; it then goes in the gatekeeper's way (`load_aurora`, the same as Bin's: file check, load, match, hello, go), and never beside another Aurora: with Bin's DLL in, or once one Add Aurora is in, the next is skipped ("not loaded: it is Aurora, and an Aurora is already in the game (never two)"). Other Add DLLs load unchecked, as before. Old test DLLs such as aurora.sa.dll from before the card carry neither and load unchecked; a copy of a stamped build under any name is caught.
  - Then the hello: the Launchpad takes 32 random bytes from CNG, writes a 180-byte ask into the game's memory and runs the DLL's `aurora_hello` export there on a short thread (found in the checked file's export table; run as LoadLibraryW is, `Proc::call_export`). The DLL writes back the challenge, the protocol, its version, build, pid and module address; every field must equal what the Launchpad has (the file's card, the game's pid, where the module list has aurora.dll), else "hello try N: ..." in the log and another try (3 tries, 1 s apart, within the DLL's 15 s window). A right answer: "aurora.dll accepted: 26.11.1 answered the hello from pid … at 0x… (try 1, N ms)", then `aurora_go` the same way, then the pipe's Ping and the extras. No right answer: status line "Not accepted: aurora.dll failed the hello.", no go, no pipe, no extras; the DLL unloads itself. A mod already in the game gets the hello without the go (it had its go); failing it loads no extras. Nothing to keep secret: the challenge is new each time, so an old answer is of no use, and an answer from another program fails the pid and module fields. A DLL from before the hello has no `aurora_hello` export: "Not accepted".
  -
- The mod makes its own files (`Hotkeys.txt`, `Profiles\`, `Bin\Cache\Offsets.bin`, ...). The Launchpad makes folders only (`Bin\Cache` among them), plus its own `Settings.txt`, `Bin\Launchpad.lock` (there only while a Launchpad is open, as Stand's; a second Launchpad brings the open one forward and closes) and `Dumper\launchpad.log`. Table: `shared\aurora-core\src\paths.rs`.

## 4. Injection (`process.rs`, `Proc::inject`)

The classic **remote LoadLibrary** method:
1. `OpenProcess` on the game with only the rights needed: create thread, query, VM operation/read/write, synchronize.
2. `VirtualAllocEx`: a small block in the game's memory; `WriteProcessMemory` puts the DLL's full path (UTF-16) there.
3. `GetProcAddress(kernel32, "LoadLibraryW")`. Kernel32 is at the same address in every process of a session, so this address is valid in the game too.
4. `CreateRemoteThread(game, start = LoadLibraryW, arg = the path)`. On x64 `LoadLibraryW(path)` has the shape of a thread start routine, so the new thread simply loads the DLL.
5. Wait up to 15 s. Success is judged by the module list (`has_module`), not the thread's exit code (which is only 32 bits of a 64-bit handle).
6. Free the path block, except after a time-out (LoadLibraryW may still be reading it).

Then Windows calls the DLL's `DllMain`, which starts the mod's thread (see [architecture.md](../../docs/architecture.md)).

### Methods compared

| Method | How | Verdict |
|---|---|---|
| **Remote LoadLibraryW** (used) | as above | **Kept.** Simple, well understood, the DLL is a normal module that can free itself cleanly (`FreeLibraryAndExitThread`). |
| Mod waits for the renderer | same injection; the DLL waits until the Vulkan device and swapchain exist before hooking | Possible later, only if early inject proves flaky. |
| Vulkan implicit layer | register a layer DLL in the registry; the Vulkan loader loads it into every Vulkan app | Possible but Vulkan-wide and needs a registry entry; no gain now. |
| Proxy DLL | put a fake system DLL in the game folder | **Rejected**: edits the game's folder (rule: never touch game files). |
| Start suspended + inject | `CreateProcess(SUSPENDED)` then inject | **Rejected**: the store must start the game; no benefit. |
| AppInit_DLLs / IFEO | system-wide registry hooks | **Rejected**: system-wide side effects. |
| APC / manual mapping | queue the load on a game thread / map the DLL by hand | **Rejected**: harder, hides the module, nothing to gain; hiding is not a goal. |

No method here tries to hide from or get around anti-cheat; that is out of scope by design. (The menu's "Undetected" row is about enemy NPC eyesight in the game, not about hiding the mod: see `dll\src\unseen.rs`.)

## 5. Talking to the mod (`pipe.rs`, `shared\aurora-core\src\proto.rs`)

Named pipe `aurora-core::PIPE_NAME`, one JSON object per line. Two requests:
- `{"t":"ping"}` -> `{"t":"pong","pid":..,"proto":4,"dll_version":"26.11.1"}`. The Launchpad sends this once after inject, to see the mod is in, then lets go of the pipe. A different `proto` means an older/newer mod is loaded: unload it from its menu and inject again.
- `{"t":"bye"}` -> `{"t":"ok"}`. Only the mod sends this, to itself (see 6).

Closing the Launchpad does not affect the mod: it keeps every hold and setting.

## 6. Unloading (the mod's job, not the Launchpad's)

The Launchpad has no Unload. The mod unloads itself:
1. Menu > Unload Aurora > Yes (or a test script's `unload.request` file, which the mod checks every 0.25 s) calls `server::unload_self`.
2. That starts a thread which opens the mod's own pipe (retrying for up to 3 s if it is busy) and sends `Bye`.
3. The pipe server shuts every part down in order (`dll\src\server.rs`): menu window hook, Vulkan hooks, watchdog, fly, Undetected, Silent Aim, weapons, NVG, airwalk, fly through, credits, gear, god mode, freezes. Then it answers `Ok`.
4. The start thread waits for the unloader, sleeps 0.5 s, and calls `FreeLibraryAndExitThread`: the DLL is freed and that thread ends in one call (`dll\src\lib.rs`).

If the Launchpad is still open, it notices the mod is gone, says "the mod unloaded itself (from its menu)" and goes back to Inject without injecting again by itself. Test scripts' `quit.request` only closes the Launchpad; the mod keeps running.

## 7. Window

Laid out like Stand's launchpad. One button follows the game: **Launch** (not running), **Inject** (running without the mod), and **Inject** still once the mod is in. The status line then says "Injected 1/2 DLLs.": X is how many of Bin's `aurora.dll`, the DLLs in the Advanced list (ticked or not) and the DLLs this Launchpad loaded into this game are in the game's module list (one snapshot per poll, also when only other DLLs are in). Never two Auroras, under any name: an Add DLL is Aurora when named aurora.dll or starting with "aurora", or when it carries the stamp, the card or the pipe name (ASCII or UTF-16LE), and goes through the full check (an unstamped one is refused); with another-named Aurora in the game Bin's DLL is not loaded (`aurora_running`). Button text is centred on its ink (`mesh_bounds`) and snapped to whole pixels, counted after each inject and every 2 s (`count_dlls`); hovering it shows the menu key and the DLLs by name. Store drop-down: Steam / Ubisoft Connect. The row under the checkbox, as Stand's: **Check for updates** (greyed: nowhere to check yet) and **Changelog**. Every button's text is centred, as Stand's. **Advanced**: auto-inject delay, the ↑ ↓ Add Remove row and the DLL list (4 rows), a line, then the game folder + Locate, Where to hook as a drop-down (Auto / GPU driver / Off; it opens upward over the list) with one grey line about the pick ("Takes effect at the next inject." when the pick differs from what the mod in the game runs), and "Hooked at: ...". The window's title is "Aurora Launchpad <version>". Every button, box, row and arrow lights and reacts to a click only when Windows says the mouse is on it (`hot`: the window under the mouse is the Launchpad's own, measured with ScreenToClient; egui's 5 px slack round widgets is off).

The Launchpad can be closed right after inject: the mod keeps running and keeps everything it holds.
