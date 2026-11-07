# Architecture

How Aurora is put together: two programs, one shared library, and the files between them.

## The two halves

```
 Aurora.Launchpad (aurora-launchpad.exe) Ghost Recon Breakpoint (GRB_vulkan.exe)
 ------------------------------ -----------------------------------------
 finds the game (Steam / Ubisoft) aurora.dll <- loaded by the Launchpad
 launches it menu + ESP drawn into the game's frames
 makes folders, installs the DLL god mode, fly, teleport, weapons, ...
 injects the DLL ------- LoadLibraryW -----> reads/writes its own files
 asks "are you there?" -- named pipe -------> answers Ping
 (can be closed now; the mod runs on) menu > Unload: shuts itself down and
                                                frees its own DLL (FreeLibraryAndExitThread)
```

- The **Launchpad** runs outside the game. It never reads or writes game memory except to load the DLL, and it never unloads it. Code: `launchpad\src`. See [launchpad.md](../launchpad/docs/launchpad.md).
- The **mod** (`aurora-dll`) runs inside the game's process, on its own threads. Everything you see in game comes from here. It keeps running when the Launchpad is closed, and only it unloads itself.
- **aurora-core** is shared by both: file paths, the pipe protocol, the byte-pattern parser, key names, the menu's picture drawing. It has no Windows-only game code and is.
- **grbresearch** and **grbresearch-dll** are separate research tools (read-only scans, recorders). The mod never needs them. See `research\grbresearch-dll\README.md`.

## Inside the mod: start-up

`dll\src\lib.rs`, `DllMain`:
1. Windows calls `DllMain` while holding the loader lock, so it only starts a thread and returns at once. Doing real work inside `DllMain` can deadlock the game.
2. That thread installs a panic hook (panics go to the log), then:
   - `vkdraw::hook_start`: picks Vulkan or DirectX 11 from the game exe's imports and reads the hook setting; the present call is hooked when the menu or ESP first shows, so the mod can draw on each frame ([dx11.md](../dll/docs/dx11.md)).
   - `menu::start`: the menu's own thread (keys, state, files).
   - `entities::warm_up`: starts the scan for characters around the player.
   - A warm-up thread runs the slow code searches (Undetected, weapons, Silent Aim, NVG, airwalk, fly through, credits) so switching those on later is instant. `WARMED` is set when done.
   - `godmode::learn_background`: learns the player's character in the background; god mode stays off.
   - `watchdog::start`: once a second, refreshes weapon hooks and logs if the game window stops answering.
   - `server::run`: the named-pipe server. The Launchpad only pings it; the mod's own unloader (`server::unload_self`, started by menu > Unload > Yes or an `unload.request` file) sends `Bye`. It loops until it gets `Bye`.
3. After `Bye`: the start thread waits for the unloader thread, sleeps 0.5 s so every hook has returned, and calls `FreeLibraryAndExitThread(its own module)`: the DLL frees itself and the thread ends in the same call, so no code of the DLL's runs after the free.

## Inside the mod: threads

| Thread | Started by | Does | Stops on |
|---|---|---|---|
| start thread / pipe server | `DllMain` | answers pings; on `Bye` shuts down and frees the DLL | `Bye` |
| unloader | `server::unload_self` | sends `Bye` to its own pipe | answer read |
| menu worker | `menu::start` | keys, hotkeys, toasts, menu picture, gear hold | `menu::shutdown` |
| freeze worker | first `freeze::set` | writes held values back every 15 ms | `freeze::shutdown` |
| fly worker | `fly::set(true)` | holds and moves the player's body every 4 ms | fly off, new body, body gone |
| fly landing check | fly off | turns fly's god mode off once the player has landed | landed, 90 s, shutdown |
| watchdog | `watchdog::start` | weapon refresh, frozen-window report | `watchdog::shutdown` |
| god mode learn / arm | god mode | waits for a hit with a hardware watchpoint | hit, time out, shutdown |
| entity scanner | `entities::warm_up` / ESP | lists characters near the player | shutdown |
| game render thread | the game | runs the mod's present hook (draws menu, toasts, ESP) | hook removed |

Rule for every thread: it must be finished before the DLL is freed. A thread still running code from a freed DLL crashes the game. That is why `Bye` shuts every part down in order (`server.rs`) before answering, and why the DLL is freed only by its own start thread, after that.

## How the mod talks to the game

Four ways, from gentlest to strongest. [features.md](../dll/docs/features.md) says which feature uses which.

1. **Read memory**: follow pointers from a fixed place in the exe to the player's body, camera and so on. Example: the player's position is three doubles at `[exe+0x63EBEB0] + 0x20`.
2. **Hold a value** (`freeze.rs`): write the same bytes back every 15 ms, so the game's own writes never stick. Each hold can have a guard: bytes that must stay the same (for example an object's class tag). When the guard changes, the object was freed and the hold stops on its own.
3. **Patch code**: change a few bytes of the game's code, for example a conditional jump into a plain jump. Always one aligned 8-byte word swapped in one atomic store (`memory::swap_code_u64`), so no game thread ever runs half-changed code. The old word is kept to undo it.
4. **Hook / code cave**: replace an instruction with a jump to a few bytes of the mod's own code (allocated within 2 GB of the exe), which does the original work plus a change, then jumps back. Rendering hooks use the `retour` crate.

Plus **hardware watchpoints** (`hwbp.rs`): the CPU's debug registers stop a thread when it touches an address, so the mod can learn "who writes here" (used once to learn the player's character from a hit).

## Saved offsets (`Bin\Cache\Offsets.bin`)

Code searches (finding a function by its bytes) take seconds over a ~100 MB exe. `memory::find_code(exe, name, pattern)` keeps what it found in `%APPDATA%\Aurora\Bin\Cache\Offsets.bin`, per game build (the exe's link time and size from its PE header):
- next inject: the saved offset is checked against the pattern (a few bytes) and used at once;
- a game update (new build id) or a mismatch: scan again and save the new place.

The file is binary: `AURORAOF`, a layout version, the build id, then per search its name, pattern and offset from the exe base, and a check sum. Another build, another layout, or a damaged file is ignored and the searches run again; with no build id nothing is read or written. Static pointers like `exe+0x63EBEB0` are constants in the code, not in this file: they change only with a new game build and are listed in [offsets.md](../dll/docs/offsets.md).

## Safety rules in the code

- Every read and write goes through `memory::read` / `memory::write`: they check the range is mapped, and copy through the kernel (`ReadProcessMemory` on its own process), so memory freed in between fails the call instead of crashing the game.
- Addresses found earlier are re-checked right before each write (a world change frees everything).
- Code patches check the bytes are what was expected before and after the swap; on a mismatch they put it back and stop.
- Changes act on the player's own character only. Undetected is the exception: it changes how every enemy NPC perceives (their eyesight and alert meter), and its docs say so. It is game AI only: not a bypass, not anti-cheat evasion, not hiding anything (see `unseen.rs`).
- Nothing in Aurora hides the mod, touches anti-cheat, or works in multiplayer. Those are out of scope by design: this is a learning project.
- Never touches the game's files. Never Ghost Coins.

## Files

Every file and folder, who makes it, who writes it, who reads it: the table at the top of `shared\aurora-core\src\paths.rs`. Short version:
- `%APPDATA%\Aurora\`: the Launchpad makes the folders (`Bin`, `Bin\Temp`, `Bin\Cache`, `Places`, `Profiles`, `Dumper`) and never writes Bin's `aurora.dll`; it injects it only with its signature `aurora.dll.sig`. Everything else there is the mod's.
- The Launchpad's own settings: `%LOCALAPPDATA%\Borealis\Aurora.Launchpad\Settings.txt`, one `Key: Value` per line. `Borealis` is a working name, one constant in `paths.rs`.
- Every setting of the mod, the menu's look included, lives in profiles, as Stand's. Each inject starts from the defaults; the Load On Inject profile puts the saved settings back. Old files that are not used any more go to `Dumper\old`, never deleted.
- Saves go to `Bin\Temp` first and are renamed into place (`paths::write_file`), so a crash never leaves half a file.
- `%APPDATA%\Aurora\Dumper\`: `dll.log`, the mod's log (start here when something goes wrong), `flytrace.log`, `target.txt`, the research tools' `research\` and `research-dll.log`, and `dumps\` (the old launcher's dumps, was `grbmod\dumps`; nothing writes there now). Folders inside `Dumper` stay lowercase. Nothing writes there before `Aurora` exists (`paths::dumper_for_writing`), so a log line can never make an `Aurora` that stops the Launchpad's move.

## Where to read next

- [launchpad.md](../launchpad/docs/launchpad.md): finding the game, launching, injection.
- [menu.md](../dll/docs/menu.md): the in-game menu, input, drawing, files.
- [features.md](../dll/docs/features.md): every feature and how it works.
- [offsets.md](../dll/docs/offsets.md): every game address and pattern, with its status.
