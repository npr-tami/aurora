# grbresearch-dll

Research-only recorders that have to run inside the game. Never part of the mod: aurora-dll does not use it, and the launcher never loads it by itself. Load it only for a research step that needs it, and unload it after.

What it does:
- `watch.request`: who writes, reads or runs one address (a hardware watchpoint).
- `regs.request`: who runs a few code spots, and with which registers (short jumps to a ring recorder).

Everything else (memory reads, scans, diffs) belongs in `grbresearch`, which reads the game from outside.

## Files

All request files go in `%APPDATA%\Aurora\Dumper\research\` (shared with grbresearch.exe). The DLL checks twice a second, reads a request once and deletes it. Results land in the same folder.

- Write a request as `name.tmp` first, then rename it to `name` (for example `watch.request.tmp` -> `watch.request`). Only the exact name is read, so a half-written file is never parsed.
- A request is always deleted once read, even with broken bytes in it (each becomes the replacement mark U+FFFD). UTF-8 and UTF-16 with a BOM (Windows PowerShell 5's `>`) both work.

Log: `%APPDATA%\Aurora\Dumper\research-dll.log`.

### watch.request -> watch.txt

One line: `addr millis [w|a|x key]`

- `addr`: hex (`0x1d2c3b4a5960`) or `exe+0x2a5a4c0`.
- `millis`: 100 to 30000, default 5000.
- `w` (default): who writes the 4 bytes at `addr`. Up to 40 writers.
- `a`: who reads or writes them. Up to 40.
- `x key`: who runs the code at `addr`, one hit per value of register `key` (0 rax, 1 rbx, 2 rcx, 3 rdx, 4 rsi, 5 rdi, 6 rbp, 7 rsp, 8..15 r8..r15; default 1). Up to 256.

The catch list holds 256 hits. Once 256 are kept, the watch ends early, before `millis` is up.

`watch.txt` is JSON: `{"addr", "total", "ended_early", "hits": [{"rip", "module", "offset", "thread", "code", "regs", "count", "ret"}]}`, most frequent first. `code` is 16 bytes before rip and 32 after. `regs` are in the order above. On a failure: `{"addr", "error"}`.

- `total`: every hit while the watch was on.
- `ended_early: true`: the list filled up (or the watch was cancelled), so `total` covers only part of `millis`. Don't read it as a rate over the whole time.

It uses debug register DR1. The mod's god mode and credits catches use DR0, so both work at the same time and neither takes the other's away. Each handler clears only its own DR6 bit, and both take one shared lock (`Local\aurora-hwbp-<pid>`) while they change a thread's debug registers, so neither writes back a stale copy of the other's.

When a watch ends, DR1 is cleared on every thread, then read back and cleared again where it is still on. If any thread still has it on after that, the log says so, the exception handler stays in, and the DLL refuses to unload until the game closes (unloading then would crash the game on the next hit).

### regs.request -> regs.txt

One item per line, `#` starts a comment:

```
secs 60
spot damage; 83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8; 0; 7
spot regen; F3 0F 10 84 81 CC 08 00 00; 0; 9
spot mine; exe+0x2a5a4c0; 0; 7
value character 0x1f2e3d4c5b60
```

- `secs`: how long the spots stay on, 5 to 300 (default 60).
- `spot name; where; offset; bytes`:
  - `where` is a byte pattern (`??` for any byte; every hit up to 4 gets hooked) or an address (`exe+0x...`, `0x...`).
  - The hook goes `offset` bytes after it. `offset` is hex, like every address: `10` and `0x10` both mean 16.
  - `bytes` (decimal, 5 to 32) is how many bytes of whole instructions run in the recorder instead.
  - A decoder checks those bytes. The spot is refused (with a log line) when they cut an instruction in half, use rip-relative addressing, or hold any jump, call or return.
  - The first 5 bytes must sit inside one aligned 8-byte word.
  - Every jump goes in while all other threads are stopped and none sits inside its 5 bytes (it tries for about a second, then skips the spot with a log line). Without that, a thread stopped between two covered instructions would run the jump's last bytes as code.
  - Two hooks may not share a byte. A hook whose bytes overlap another one's is refused.
  - Never pick a spot the mod changes itself (its weapon, Undetected and bone ESP hooks).
- `value name addr`: also count this value. "the player's body" (`[exe+0x63EBEB0]`) is always counted.
- `ring calls`: how many calls each hook keeps, a power of two, 2048 (default) to 16384.
- A spot may end with a 5th field, reads made at the moment of each call: `spot inbox; <where>; 0; 11; rsp+0 rdx+0 rdx+8:4`.
  - Each read is `register+hex` (or `-hex`), then `:1` `:2` `:4` `:8` (default) or `:f` (4-byte float). The register is its value when the spot runs; `rsp+0` at a function's first byte is the return address (who called).
  - **The read runs inside the game's thread. A bad pointer crashes the game.** Read only what the game itself reads at that spot, or its stack.
  - At most 15 per spot.
- At most 16 spots.

`regs.txt`, per spot: how many calls, then per register how often it equalled each value, how often it pointed at something holding the value in its first 8 words ("holds"), and its 4 most common values. The ring keeps the last `ring` calls per hit. A bad request writes `error: ...` into `regs.txt` instead.

Spots with reads also get:
- In `regs.txt`: the 40 most common read combinations, with how many calls and how many different `rcx`. 8-byte values get a note: `(exe+0x...)` when in the exe, `(the player's body)` when equal, `(->the player's body)` when they point at it (a handle).
- `regs-rows.csv`: every kept call in time order: `spot,ms,rcx,<reads>` (a `#` line per spot names the columns). `ms` counts from when the spots went on; each call is stamped with the CPU's time stamp counter, measured against the clock over the run.

Ready-made request: `requests\noise.request` (the enemy inbox and the noise event, 300 s; see `research\docs\perception.md`). Read its rows with `python research\scripts\noise_report.py regs-rows.csv`: it names each sender (also the one behind the generic helper, the sight wrapper or the broadcast, from three stack reads), lists the most common sources with when they were seen, and stops with a clear message if the reads are spelled differently.
- "the player" in the report and in `regs.txt` is worked out when the recording ends, from the player's body then. If the player dies or respawns during the run, the labels can be wrong; the source list with times is there to check.
- Rows caught half-written when the hooks come out are dropped.

The jumps go in and out with one atomic 8-byte compare-and-swap. If someone else changed that word meanwhile, it is left alone. The recorder code is never freed, so a jump left behind stays safe after unloading.

### research-unload.request

Any content. The DLL ends a running watch (the watchpoint comes off every thread), takes the regs jumps out (that report is still written), waits a moment, then unloads itself with `FreeLibraryAndExitThread`. The log ends with `unloading`. Then the DLL file is free, so a new build can go in without restarting the game.

It refuses (and logs why) in two cases:
- It was loaded more than once. Windows only counts a second load, so one unload would leave it in with no worker. It keeps working instead, until the game closes.
- A watch could not clear DR1 on every thread (see above). It stays loaded until the game closes.

## Build and load

`cargo build -p grbresearch-dll --release` -> `target\release\grbresearch_dll.dll`.

Load it only when a research step needs it. Load it once per game.
- In the launcher, Advanced > DLL list: add `grbresearch_dll.dll`. **A ticked DLL loads into every game, each time, until it is unticked.** It does not switch itself off after one research step. Untick it (or remove it) as soon as the step is done.
- Or load it with any injector, into `GRB_vulkan.exe` only.

Check the log for `attached`, then write request files. Unload with `research-unload.request` before rebuilding.

Rules as for the mod: it only watches and records, it never writes game data, and it never acts on other players.
