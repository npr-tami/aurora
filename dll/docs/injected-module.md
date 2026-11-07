# The injected module (aurora-dll)

How the half of Aurora that runs inside the game works, from the moment Windows loads it to the moment it frees itself. Written from the code as it is at that date (repo `code\`, `dll\src`). Nothing here was run in the game while writing it; it is a reading of the code and the notes.

Read [architecture.md](../../docs/architecture.md) first for the big picture. This page goes one level deeper on the DLL only. The Launchpad side (finding the game, `LoadLibraryW` injection) is in [launchpad.md](../../launchpad/docs/launchpad.md). What each feature does is in [features.md](features.md).

## 1. What it is, in one paragraph

`aurora.dll` is a normal Windows DLL. The Launchpad asks the game to load it with Windows' own `LoadLibraryW` (a `CreateRemoteThread` that calls it), so it shows in the game's module list like any overlay. From then on it is part of the game's process: same memory, same threads, same window. It reads and writes the game's memory directly (it *is* the game's process), puts small jumps into the game's code to change one decision, hooks the game's "show this frame" call to draw the menu and ESP, and listens to the keyboard. It talks to the outside only through files in `%APPDATA%\Aurora` and one named pipe that carries two messages. It never hides itself, never touches anti-cheat, the network or accounts, and acts on the player's own character except for Undetected (enemy NPC eyesight, a single-player difficulty switch).

## 1b. How to use it, start to finish

1. **Build.** `cargo build --release` (or `tools\package.ps1` for the Launchpad exe). The exe carries no DLL. Put the new `aurora.dll` into `%APPDATA%\Aurora\Bin` with `tools\swap-dll.ps1` (unload first; see `swap-dll.md`). Unit tests: `cargo test`.
2. **Either build** works since Vulkan (`GRB_vulkan.exe`) or DirectX 11 (`GRB.exe`). The mod picks its drawing from the exe's imports; see [dx11.md](dx11.md). DX11 is
3. **Open Aurora.Launchpad.** It finds the game (Steam/Ubisoft registry, or Options > Locate), makes the folders and injects Bin's DLL as it is (it says so when Bin has none). Options > where to hook (Auto / Driver / Off) is read by the DLL once at inject.
4. **Launch, then Inject.** The one button follows the game: Launch → Inject → Injected (greyed). Advanced has an auto-inject delay. Inject is the remote `LoadLibraryW` described in [launchpad.md](../../launchpad/docs/launchpad.md) §4: the Launchpad writes the DLL path into the game, starts a thread at `LoadLibraryW`, and checks the module list. Windows then calls `DllMain` (section 2 below).
5. **Check it is in.** The Launchpad sends one `Ping` and shows the answer. `Dumper\dll.log` gets `aurora-dll <version> (<build>) attached` (the build's name, such as `26.11.1, commit <hash>`; `dev build` for a plain cargo build) and, a few seconds later, `code searches done in N ms`. Any `pattern found N times` line means that feature is off for this build.
6. **Close the Launchpad if you like.** The mod keeps running and keeps everything it holds.
7. **In game:** the menu key (default **F4**, change it in Menu > Hotkeys) opens the menu. The drawing hook goes in the first time the menu or ESP shows; `Bin\hook.txt` then says where (Steam overlay or GPU driver). Features are switched from the menu, the command box, row hotkeys, or a `command.request` file (one command per line).
8. **Unload:** Menu > Unload > Yes (or an `unload.request` file). Wait a few seconds after inject first, and not on a loading screen (see 2.3). The log ends with `unloading itself`.

## 2. Life cycle

```
LoadLibraryW (Launchpad)
   └─ DllMain(PROCESS_ATTACH) loader lock held: do nothing heavy
        └─ spawns the START THREAD and returns TRUE at once
             ├─ panic hook → dll.log
             ├─ vkdraw::hook_start read the "hook" setting, write Bin\hook.txt
             ├─ menu::start menu worker thread (keys, files, toasts)
             ├─ entities::warm_up first scan for characters near the player
             ├─ WARM-UP THREAD globals::find_all, then code searches
             │ (unseen, weapon, nvg, airwalk, noclip, credits) → WARMED
             ├─ godmode::learn_background(10 min) waits for one hit to learn the player's character
             ├─ watchdog::start 1 s tick: weapon/airwalk refresh, hang report
             └─ server::run named pipe; blocks until Bye
                    … the game runs, the mod runs …
             menu > Unload > Yes or unload.request
                    └─ server::unload_self → UNLOADER THREAD sends Bye to its own pipe
             server::serve gets Bye → shuts every part down in order → answers Ok
             start thread: join unloader, sleep 0.5 s, FreeLibraryAndExitThread(self)
```

### 2.1 `DllMain` (`lib.rs`)
- Windows calls it while holding the *loader lock*. Anything that loads a library, waits on another thread, or takes a lock another thread may hold while loading, can deadlock the whole game. So it only:
  - calls `DisableThreadLibraryCalls` (no more calls for each new game thread),
  - stores its own module handle in `MODULE` (needed later to free itself),
  - starts one thread and returns `TRUE`.
- Everything real happens on that thread, after the loader lock is released.

### 2.2 The start thread (`lib.rs`)
In this order:
1. **Log line** `aurora-dll <version> (<build>) attached, pid <pid>` in `%APPDATA%\Aurora\Dumper\dll.log`. The log is opened once, appends, and never panics. It is not written when `Aurora` does not exist yet, see `paths::dumper_for_writing`. Unit tests never write it, nor flytrace.log or the target mark. Nor anything else of hers: in tests the menu has no Aurora folder (`menu::folder`: no Hotkeys.txt, Places, Profiles, Saved Commands, Meta State.txt or command.request), and `Offsets.bin` is never saved.
2. **Panic hook**: a Rust panic on any of the mod's threads is written to the log before the default handler runs. Added after the ESP stopped updating with nothing logged.
3. **`vkdraw::hook_start`**: reads the `Hook:` line of the Launchpad's `Settings.txt` (`%LOCALAPPDATA%\Borealis\Aurora.Launchpad`) once (Auto / Driver / Off) and writes the current state to `Bin\hook.txt`, which the Launchpad's Options shows. The drawing hook itself goes in later, the first time the menu or the ESP needs it.
4. **`menu::start`**: resets "on by default" things to off, loads the ESP style and preferences, finds a profile marked *Load On Inject*, and starts the menu worker.
5. **`entities::warm_up`**: first heap scan for character bodies near the player, so the Entities page and the ESP have a list early.
6. **Warm-up thread** (stored in `WARM`, joined on unload):
   - `globals::find_all`: finds the 9 fixed game places (the player's body root, camera root, gear roots, class tables, a return address) by pattern (see 5.2).
   - `unseen::warm`, `weapon::warm`, `silentaim::warm`, `nvg::warm`, `airwalk::warm`, `noclip::warm` (fly through), `credits::warm`: the slow code searches for features, done now so switching them on later is instant. None of them changes the game; they only find addresses.
   - sets `WARMED`, logs `code searches done in N ms`. A *Load On Inject* profile waits for this.
7. **`godmode::learn_background(10 min)`**: arms a hardware watchpoint on the damage check and waits for the player to take one hit, only to learn which entity is the player's. God mode stays off.
8. **`watchdog::start`**: the freeze guard (see 4).
9. **`server::run`**: the pipe server. This call blocks until unload and only returns after `Bye`.
10. After `Bye`, if `SELF_UNLOAD` is set: join the unloader thread, sleep 0.5 s, log `unloading itself`, then `FreeLibraryAndExitThread(MODULE, 0)`. That one call frees the DLL and ends the thread without returning into freed code.

### 2.3 Unload (`server.rs`, `menu.rs`)
- Only the mod unloads itself. Two triggers, both end in `menu::unload_now` → `server::unload_self`:
  - menu > Unload > Yes,
  - a file `%APPDATA%\Aurora\unload.request` (test scripts). The menu worker checks for it every 250 ms and deletes it before acting.
- `unload_self` sets `SELF_UNLOAD` (only once) and starts the **unloader thread**: it opens its own pipe (tries for 3 s), writes `Bye`, reads the answer, logs `own bye answered`. It runs on its own thread because the shutdown joins the menu worker, which is the thread that asked.
- When `serve` reads `Bye` it shuts down, in this order:
  1. `menu::shutdown` – stop the worker, put the game's window procedure back, remove the DirectInput hooks, clear the pictures.
  2. `vkdraw::shutdown` – stop the entity scanner, remove the pose hook, disable the Vulkan detours, wait up to 3 s for `IN_HOOK` to reach 0, wait 150 ms for frames in flight, destroy the mod's GPU objects.
  3. `watchdog::shutdown`, `fly::shutdown`.
  4. Undetected off, Ignore only me off, then Silent Aim, weapon, NVG, airwalk, fly through (`noclip`), credits, gear score shutdowns: each puts the game's original code word back.
  5. God mode off and its threads joined; `freeze::shutdown` (stop the 15 ms writer).
  6. `join_warm` – wait for the warm-up thread.
  7. Answer `Ok`, flush, disconnect, return `true` → `run` returns → step 10 above.
- **Known weak spots**: the hook drains are timeouts (3 s / 2 s) and the unload carries on when they expire; god mode's learn thread and the entity warm-up don't see the shutdown (unload right after inject can hang or crash); the warm-up thread is joined last, so it can put the credits hook in after credits was switched off; a watchpoint clear that fails is ignored. Safe habit: wait a few seconds after inject before unloading, and never unload on a loading screen.

## 3. Threads

| Thread | Started by | Runs | Ends on | Joined on unload? |
|---|---|---|---|---|
| start / pipe server | `DllMain` | everything in 2.2, then the pipe | `Bye` | it is the one that frees the DLL |
| warm-up | start thread | pattern searches | done | yes, last (`join_warm`) |
| menu worker | `menu::start` | keys, hotkeys, command files, toasts, menu picture, gear hold | `menu::shutdown` | yes |
| unloader | `unload_self` | sends `Bye` to its own pipe | answer read | yes |
| freeze worker | first `freeze::set` | writes every hold back every 15 ms | `freeze::shutdown` | yes |
| fly worker | `fly::set(true)` | holds and moves the player's body every 4 ms | fly off, new body, body gone | yes |
| fly after phase | fly off, when the game was moving the player or Wait For Calm / Snap Guard is on | holds the player's until calm (Wait For Calm), watches the player 2 s (Snap Guard, trace), then starts the landing check | done, Fly on, a teleport, a second Fly off, the freeze guard, shutdown | yes |
| fly landing check | fly off (after the after phase, if any) | turns fly's god mode off once the player has landed | landed, 90 s, shutdown | yes |
| watchdog | `watchdog::start` | 1 s tick (see 4) | `watchdog::shutdown` | yes |
| god mode learn / arm | god mode | waits on a hardware watchpoint | hit, time out, shutdown | yes, but learn doesn't see the stop flag (U1) |
| entity scanner | ESP | lists characters near the player | `vkdraw::shutdown` | yes |
| entity warm-up | `entities::warm_up` | first scan | done | **no** (U2) |
| credits 20 s watch, Bring/Yeet timer | menu rows | short timers | done | **no** (audit R6) |
| game threads in the mod's hooks | the game | present hook, DirectInput hooks, window procedure, watchpoint handler | hook removed | counted with `IN_HOOK` / `IN_PROC`, timed drain |

The one rule: no thread may be running DLL code when the DLL is freed. A thread that returns into freed code crashes the game.

## 4. The freeze guard (`watchdog.rs`)
Every second:
- `weapon::tick` and `airwalk::refresh` re-read the player's body and the player's network part, so the weapon and air-walk caves compare against the current ones (a respawn gives new objects).
- Finds the game's biggest visible window (again every 10 s) and asks Windows `IsHungAppWindow`.
- 1 s hung: writes a report to the log (fly on/off and tick count, god mode entity and arm state, watched address, running holds, last pipe request), turns fly off, cancels any watchpoint.
- 6 s hung: drops every hold, god mode included.
- It logs again when the window answers.

## 5. How it touches the game

### 5.1 Memory reads and writes (`memory.rs`)
- `find_module("")` = the game exe (base and size), looked up once with a Toolhelp snapshot and cached.
- `readable(addr, len)` asks `VirtualQuery` whether every page is committed and not `NOACCESS`/`GUARD`.
- `read` / `write` go through `ReadProcessMemory` / `WriteProcessMemory` **on its own process**. That sounds odd, but it is the point: if the game frees the memory between the check and the copy (a world change, joining a friend), the kernel call fails and returns an error instead of the game crashing. A plain pointer copy can crash the game.
- `write` also refuses pages that are not already writable. Code pages are changed only through `swap_code_u64` (below).
- Reads are capped at 16 MB.

### 5.2 Finding addresses: patterns and `Offsets.bin`
- An address is never typed in by hand. Each one comes from a **byte pattern** (a short run of the game's instructions with `??` for parts that change) that must match **exactly once** in the exe. Zero or two matches = the feature stays off and the log says `pattern found N times, want 1 (game updated?)`.
- `scan_module` walks the exe region by region with `VirtualQuery`, because the exe's copy protection leaves unreadable holes; reading it as one block would fault. It reads, it never changes those regions.
- `find_code(exe, name, pattern)` keeps results in `%APPDATA%\Aurora\Bin\Cache\Offsets.bin` (binary, versioned, with a check sum; any mismatch or damage means search again), keyed by **build id** (the exe's PE link time + image size). Next inject: the saved offset is checked against the pattern and used at once. New build or mismatch: scan again, save.
- `globals.rs` finds the 9 fixed *data* places the same way: the pattern is code that reads the place with a `mov/lea reg, [rip+x]`, and the address is decoded from that instruction (`rip_target`). Until found, `rva` is `None` and every user treats it like an unreadable pointer (skip, try later).

### 5.3 Holding a value (`freeze.rs`)
- `freeze::set(id, addr, bytes)` writes once and then a worker writes the same bytes back every 15 ms, so the game's own writes never stick (god mode's immune byte, for example).
- `set_guarded` adds a guard: bytes that must stay the same (usually the object's class tag). When they change, the object was freed or replaced, and the hold drops itself (`freeze <id> dropped: address went away`). A failed write drops it too.

### 5.4 Patching code: the 8-byte swap (`memory.rs`)
- `swap_code_u64(addr, new)` changes one **aligned 8-byte word** of game code with one atomic store, so no game thread can ever run half-old, half-new bytes. Steps: `VirtualProtect` the word to read-write-execute, `AtomicU64::swap`, put the old protection back, `FlushInstructionCache`. It returns the word that was there.
- Every caller compares the returned word with what it expected. On a mismatch it swaps the old word back, marks the feature bad until unload and logs `left alone`. So if anything else changed that code, Aurora backs off.
- Only one swap runs at a time (`SWAP` lock), because two swaps on one page could each restore the other's protection.
- Used for: short-jump → long-jump flips (Undetected's meter fill, the clamp), and for writing the 5-byte jump into a code cave (below).

### 5.5 Code caves (Undetected, weapons, NVG, airwalk, credits, pose hook, inbox draft, Silent Aim)
- A cave is a few bytes of Aurora's own machine code in memory from `alloc_code_near`: within 2 GB of the exe so a 5-byte `jmp rel32` reaches it.
- The game's instruction is replaced (through 5.4) with `jmp cave`. The cave runs the instruction(s) it replaced, adds one decision, and jumps back to the next game instruction.
- Caves are **never freed** and **never call into the DLL**. So a game thread that is inside a cave when the DLL unloads is still safe: the cave and the original code are both still there.
- Each cave checks the owner first: if it is not the player, the game's own code runs unchanged.
- **One-instruction rule**: a 5-byte jump is safe without pausing only if it replaces exactly one whole instruction. If it covers two (the inbox: a 4-byte `sub rsp` then a `test`), a thread stopped between them would run the tail of the jump as code. Those go in with `swap_code_u64_paused`: every other thread is suspended, each one's instruction pointer is read, and the swap happens only if none sits inside the covered bytes (tries for about a second, then leaves the code alone). While threads are paused nothing allocates or logs, because a paused thread may hold the heap lock. Taking such a jump out is safe without the pause. (Lesson: *A Jump Over Two Instructions Needs A Pause*.)
- **Known gap** (audit B1/B2, R4/P4, not fixed): four feature patches and the drawing/keyboard hooks are put in without that pause.
- **Silent Aim**: its own 0x280-byte block at the shot-build entry. The jump covers three instructions, so it goes in with the paused swap. For the player's shots the cave calls the game's function itself, from a frame of its own, with a new direction (Infinite Reserve's and Fire Rate's caves call game code the same way). With Magic Bullets, No Range Loss or Instant Hit on, it also writes into the player's shot after the call (`+0x378`, `+0x158`, `+0xC`, `+0x8`). It never calls the DLL. Details: offsets.md 3.21.

### 5.6 Function hooks (drawing and keyboard)
These use the `retour` crate (`RawDetour`): it copies the first bytes of a function into a *trampoline*, writes a jump to the mod's function, and the mod's function calls the trampoline to run the original.
- **DirectX 11 present**: hooks `IDXGISwapChain::Present`, `Present1`, `ResizeBuffers` and `ResizeBuffers1` in dxgi.dll. See [dx11.md](dx11.md).
- **Vulkan present** (`vkdraw/vulkan.rs`; shared parts in `vkdraw/mod.rs`): hooks `vkQueuePresentKHR` (show this frame), `vkAcquireNextImageKHR` and `vkCreateSwapchainKHR` at the first function in the chain: the Steam overlay layer when it is on, else the GPU driver (setting *Auto*), or always the driver (*Driver*), or never (*Off*). Where it hooked goes to `Bin\hook.txt` in plain words (`Steam overlay (SteamOverlayVulkanLayer64.dll)`).
  - In present, it reads the camera at that moment, projects the ESP boxes, and copies the menu, toast and typing box pictures into the frame with blits of 1×1 coloured images, so it never needs to know the screen's pixel format. Then it calls the original present.
  - Its own command buffers (4 frames in flight) and images are made from the game's device (`vkGetDeviceProcAddr`).
  - Swapchains made while the mod is loaded get the transfer flag added (needed to blit into them); if the driver refuses, the game's own request goes through unchanged.
  - `IN_HOOK` counts game threads inside the mod's hook functions for the unload drain. On an error the drawing turns itself off until unload (`BROKEN`).
- **DirectInput** (`dinput.rs`): the game reads the keyboard through DirectInput 8, past its window messages. Aurora makes a throwaway keyboard device of its own just to read the function table, then hooks `GetDeviceState` and `GetDeviceData` (W and A versions, once if they share code). While the menu is open, the menu's keys read as up and their presses are taken out of the event list; releases still pass, so no key gets stuck. Hotkeys and fly's down key are kept from the game the same way.
- **Window procedure** (`menu.rs`, `subclass`): `SetWindowLongPtrW(GWLP_WNDPROC)` puts the mod's procedure in front of the game's. While the menu is open it eats the menu's keys and raw input; everything else goes to the game's procedure with `CallWindowProcW`. `unsubclass` puts the game's back (and logs if someone else wrapped the window after the mod).

### 5.7 Hardware watchpoints (`hwbp.rs`)
- Purpose: learn "who writes this address" once, without changing code. Used by god mode (learn the player's character from one hit) and credits (find the player's balance).
- How: for every other game thread, suspend it, set debug register DR0 to the address (and the matching bits in DR7) with `SetThreadContext`, resume. A vectored exception handler catches the single-step trap, notes the instruction pointer and registers in a fixed array (no allocation inside the handler), and lets the game carry on.
- Only one watch at a time; a new one cancels the old. Clearing happens twice with a stopping flag, after a thread re-armed itself while being cleared. A named lock `Local\aurora-hwbp-<pid>` is shared with the research DLL's DR1 watch so neither writes back a stale copy of the other's registers.
- **Known gap** (audit H1/R7, not fixed): a failed clear is ignored and the handler is removed anyway; a thread left armed then crashes the game on its next hit.

## 6. Talking to the outside

### 6.1 The pipe (`server.rs`, `aurora-core::proto`)
- `\\.\pipe\aurora`, one client at a time, JSON lines, protocol version 4.
- Two requests only:
  - `Ping` → `Pong { pid, proto, dll_version }`. The Launchpad sends it once after inject to see the mod is in.
  - `Bye` → shuts down (2.3) → `Ok`. Sent only by the mod's own unloader thread.
- A client that disconnects changes nothing: the mod keeps everything running.
- Known small bug (U6): the `Ok` answer to its own `Bye` can be lost; harmless today because nobody waits on it for anything.

### 6.2 Request files (`menu.rs`, `command_file`)
Checked by the menu worker every 250 ms in `%APPDATA%\Aurora`:
- `unload.request` → same as Unload > Yes. Deleted before acting.
- `command.request` → each line runs as if typed in the command box (with Suppress Generic Responses). Deleted once read. Every line is logged as `command file: …`.
- Known gap (audit): a request file left over from a crash runs again at the next inject.

### 6.3 Files it owns
Everything in `%APPDATA%\Aurora` except `Bin\aurora.dll` (the Launchpad's): `Profiles\` (every setting, the menu's look included, as Stand's), `Meta State.txt`, `Hotkeys.txt`, `Places\`, `Bin\Cache\Offsets.bin`, `Bin\hook.txt`, `Dumper\dll.log`. At the first inject of the build, the old `settings.json` and `menu.json` go into the Load On Inject profile (Main when none is marked; only lines it lacks), then move to `Dumper\old` with the old `Offsets.json` (`menu::carry_over_old_files`). Saves go to `Bin\Temp` first and are renamed into place, so a crash never leaves half a file. Full table: top of `shared\aurora-core\src\paths.rs`.

## 7. Safety rules the module follows (and where in the code)
| Rule | Where |
|---|---|
| No work under the loader lock | `DllMain` only spawns a thread |
| Address only from a unique pattern or pointer path, per build | `memory::find_code`, `globals.rs` |
| Every read/write checked, through the kernel | `memory::read` / `write` |
| Re-check an address right before writing | each feature's `set` (e.g. `gearscore::set_slot`) |
| Code changes are one aligned atomic 8-byte swap, verified, rolled back on mismatch | `memory::swap_code_u64` + callers |
| Jumps over two instructions only with threads paused | `memory::swap_code_u64_paused` |
| Caves never freed, never call the DLL | `memory::alloc_code_near`, every `cave_code` |
| The player's character only (Undetected: enemy NPC sight; documented exception) | owner checks in every cave |
| Never edits game files; never Ghost Coins | nothing writes under the game folder |
| No anti-cheat evasion, no hiding the mod, no multiplayer use | the DLL is loaded with `LoadLibraryW` and stays in the module list; there is no code for any of those |

## 8. Where to look when something goes wrong
1. `%APPDATA%\Aurora\Dumper\dll.log` – start line, `code searches done`, every pattern miss, every hold on/off/dropped, every patch on/off/left alone, freeze guard reports, the unload steps. Beside it, `flytrace.log`: what the game did to the player around a Fly off.
2. `%APPDATA%\Aurora\Bin\hook.txt` – where the drawing hook went, or why it failed.
3. `%APPDATA%\Aurora\Bin\Cache\Offsets.bin` – what was found for this build. Deleting it only costs one slow start.

## 9. Not covered
- Two things are *not* in the code and are *not* documented, because they are outside this project's rules: getting round the exe's copy protection or integrity checks (the early-inject note mentions they could be tripped; the answer there is to wait until the code is ready, never to work around them), and anything that hides the module or targets multiplayer or anti-cheat.

## The wait for the go
After `DllMain` has started the start thread and logged "attached", the thread waits on `GO` (a mutex and condvar in `lib.rs`) for up to 15 s (`GO_WAIT`) before anything else: no drawing hook, no menu, no warm-up, no pipe. Two exports, run by the Launchpad on short threads it starts in the game (the way it runs LoadLibraryW):
- `aurora_hello(p)`: `p` is the Launchpad's 180-byte buffer (`aurora_core::hello`). The DLL checks the label, format and size, then writes its answer in place: the challenge back, the protocol, `DLL_VERSION`, `BUILD`, its pid and `MODULE`. Returns `HELLO_OK` (0), or 1 (null), 2 (not a hello), 3 (a newer hello format). It changes nothing else and remembers nothing.
- `aurora_go(_)`: sets `GO`; the start thread goes on as before ("go from the Launchpad after N ms"). A second go changes nothing. Returns 1.
No go within 15 s: "no go from the Launchpad within 15 s (...): unloading, nothing was started", the panic hook is put back, and `FreeLibraryAndExitThread` frees the DLL. Nothing was hooked or started, so nothing is undone. An older Launchpad without the hello can load this DLL but never gives it a go: it unloads after 15 s. The Launchpad's side: `launchpad\docs\launchpad.md`.
