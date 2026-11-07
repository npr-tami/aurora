# grbresearch

A small command-line memory inspector for Ghost Recon Breakpoint research, like a tiny Cheat Engine / ReClass you can run from a shell. It attaches from outside to the game that is already running, so finding a new offset never needs the mod rebuilt or reinjected.

**Read-only.** It opens the game with `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ` and nothing else. There is no `WriteProcessMemory`, `VirtualProtectEx`, `CreateRemoteThread` or thread access anywhere in this crate. It reads memory, lists modules and regions, and asks which pages are in RAM. That is all.

The mod (`aurora-dll`) holds only features. Research that has to run inside the game (who writes, who reads, who runs this address) lives in `grbresearch-dll`.

## Build

```
cargo build -p grbresearch --release
cargo test -p grbresearch
```

The exe is `target\release\grbresearch.exe`. The tests read only the test binary's own process.

## Using it next to Aurora

1. Start the game and the mod as usual. Both keep running.
2. Run `grbresearch <command>`. It finds `GRB_vulkan.exe` by name (`--exe NAME` for another, `--pid N` if several run), reads, prints and exits.
3. For many steps in a row, `grbresearch shell` keeps the process open and remembers names.

Global flags: `--exe NAME`, `--pid N`, `--dir FOLDER` (research folder, default `%APPDATA%\Aurora\Dumper\research`).
Exit codes: 0 ok, 1 error, 3 the game closed.

## Addresses

Numbers are always hex. Every result prints both the runtime address and the static form (`module+rva`, or a note like `heap rw- (alloc 0x1D2C0000 +0x490)`).

| Form | Meaning |
|---|---|
| `7FF6A000`, `0x7FF6A000` | plain address |
| `exe+0x63EBEB0` | main module + offset |
| `ntdll.dll+0x10`, `ntdll+0x10` | any module, case-insensitive |
| `[exe+0x63A0D20]+0x490` | read the pointer, then add |
| `[[exe+0x63EDAA0]+0x1F0]+0x18` | nested pointer chain |
| `$body+0x20` | a name remembered in the shell |
| `a+b-c` | plus and minus |

A module name made only of hex letters (`d3d11`) reads as a number: write `d3d11.dll`.

## Read errors

A failed read never turns into zeros. Each read checks the region first (VirtualQueryEx) and says what went wrong:

| Kind | Meaning |
|---|---|
| `InvalidAddress` | null page (below 0x10000) or not a user-mode address |
| `Free` | nothing allocated there |
| `Reserved` | reserved, not committed |
| `NoAccess` | `PAGE_NOACCESS`, `PAGE_GUARD`, or the read was refused. Guard pages are never touched. |
| `PartialRead(n)` | only the first n bytes could be read |
| `ProcessGone` | the game has ended: the tool says so and stops |

## Commands

| Command | Example |
|---|---|
| `info` | `grbresearch info`: pid, path, image base and size, build (file version + PE timestamp) |
| `modules` | `grbresearch modules` |
| `regions [--prot rwx] [--range A B] [--committed]` | `grbresearch regions --prot w --committed --range exe exe+0x8000000` |
| `read <addr> <type> [count]` | `grbresearch read [exe+0x63EBEB0]+0x20 f64 3` |
| `dump <addr> <len>` | `grbresearch dump exe+0x1A72150 0x40` |
| `view <addr> [len]` | `grbresearch view [exe+0x63EBEB0] 0x120`: one row per 8 bytes: offset, hex, where it points (module+rva, "heap rw-", or "not a pointer"), u64, i32 x2, f32 x2, f64 |
| `chain <addr>` | `grbresearch chain [[exe+0x63EDAA0]+0x1F0]+0x18`: each step, and where and why it fails |
| `inspect <addr> [len] [--type T]` | `grbresearch inspect 0x1D2C3B4A5960`: region, vtable (module+rva) if the first qword points into an image, each qword labelled ptr-to-image / ptr-to-heap / small-int / float-like / zero / unknown, plus research.json labels with their status |
| `scan aob "<pattern>" [--module exe]` | `grbresearch scan aob "48 8B 01 33 D2 4C 8B C1"` |
| `scan value <type> <value>` | `grbresearch scan value f32 100 --range 0x1D000000000 0x1E000000000 --save hp` |
| `scan ptr <addr> [--within N]` | `grbresearch scan ptr 0x1D2C3B4A5960 --full` |
| `scan vtable <addr>` | `grbresearch scan vtable exe+0x49BFD70 --full` |
| `next <scan> <mode>` | `grbresearch next hp decreased` (modes: changed, unchanged, increased, decreased, eq V, ne V) |
| `refs <addr>` | `grbresearch refs 0x1D2C3B4A5960 --full`: hits inside images (static roots) vs heap, with the vtable of the object a heap hit seems to sit in (a guess, marked so) |
| `snap <name> <addr> <len>` | `grbresearch snap ground [exe+0x63EBEB0] 0x400` |
| `diff <a> <b> [--as f32\|i32\|u64\|f64\|auto]` | `grbresearch diff ground air --as f32`: changed offsets, old and new raw bytes, values beside them |
| `watch <addr> <len> [--ms 100] [--secs 10] [--as T]` | `grbresearch watch [exe+0x63EBEB0]+0x20 24 --as f64`: prints only fields that change, with the time |
| `chars [--all]` | `grbresearch chars`: NPC and player character components (see below) |
| `db list [section] [--all]` | `grbresearch db list offsets` |
| `db find <text>` | `grbresearch db find awareness`: searches every field of every entry, and saved scan / snapshot names |
| `db add <section> key=value...` | `grbresearch db add offsets id=off.hp object_type=Body object_offset=0x2A0 data_type=f32 name=health how_found="scan + next decreased"` |
| `db set-status <id> <status> --evidence "..."` | `grbresearch db set-status off.hp verified --evidence "watch showed it drop on every hit, 5/5"` |
| `export [--out file.json]` | `grbresearch export`: research db + saved scans + snapshots index |
| `shell` | `grbresearch shell`, then `body = [exe+0x63EBEB0]`, `view $body`, `vars`, `quit` |
| `selftest` | `grbresearch selftest`: read-only checks against its own process |

Types: `u8 i8 u16 i16 u32 i32 u64 i64 f32 f64 ptr bytes str wstr`. Lengths and counts are decimal, or hex with `0x`. Integer values are decimal, or hex with `0x`. A float matches within half a unit of its last typed digit (`12.5` matches 12.45 to 12.55), like Cheat Engine.

### Scans

- **Targeted by default.** Say where: `--range A B`, `--module NAME` (aob defaults to the exe), `--reuse SCAN` (rescan only that scan's hits, or its regions if it had none), or `--full` (all committed readable private and mapped memory; ptr/vtable/refs also include images).
- Skips guard pages, no-access pages and GPU memory, counted by reason. `--full` reads only pages that are in RAM now (`--paged-out` to include the rest), so the game does not grow.
- Reads in 1 MB chunks. Pages that vanish mid-scan are skipped and counted.
- Ctrl+C stops the scan cleanly and keeps what it found.
- `--limit N` (default 1,000,000 for value scans, 10,000 for others; 0 = none). Value scans are aligned to the type size unless `--unaligned`.
- Prints the time, MB read and skipped regions; shows 20 hits unless `--all`.
- `--save NAME` keeps the hits; without it they are kept as `_last`.

### chars

Finds the components the mod's ESP uses: heap objects whose first qword is the NPC vtable `exe+0x49BFD70` or the player vtable `exe+0x4A4C5F0`. Each is checked like the ESP does: marker+0 is the vtable, marker+8 is a body pointer, the body's first qword points into the exe, and body+0x20 is a sane position (finite, under 1e5). It shows marker, body, position, distance in game metres (units x 1.5), side (`[[marker+0x60]+0x160]+0x104`: 3 enemy, 0 friendly) and the dead flag (body+0x108 bit 0x4000). "you" is the body at `[exe+0x63EBEB0]`. These offsets are from GRB_vulkan.exe.

## research.json

`%APPDATA%\Aurora\Dumper\research\research.json`, created and seeded on first use (never re-seeded once it exists). Plain JSON, safe to edit by hand.

Sections: `profiles`, `vtables`, `object_types`, `offsets`, `relationships`, `functions`, `signatures`, `experiments`, `notes`. Each entry:

```json
{
  "id": "off.body_pos",
  "name": "position x y z",
  "status": "strong",
  "exe": "GRB_vulkan.exe",
  "build": "unknown",
  "module_rva": "",
  "object_offset": "0x20",
  "object_type": "Body",
  "data_type": "f64 x3",
  "how_found": "Taken from the mod source.",
  "how_tested": "",
  "evidence": "Used every session by a working mod feature. Feature: Fly, Teleport, ESP.",
  "limitations": "",
  "date": "2026-11-03"
}
```

Any other key (`pattern`, `source`, ...) is kept as is. `inspect` shows an `offsets` entry beside the qword where its `object_type` matches the object's type (found from `vtables` by its `module_rva`, or given with `--type`).

Profiles are keyed by exe and build. `GRB_vulkan.exe` is seeded with build "unknown": run `info` and record the build. `GRB.exe` (DX11) is an unverified placeholder: its addresses must not be assumed equal to the Vulkan build.

### Status rules

| Status | When |
|---|---|
| `verified` | a controlled test proved it (say which in `how_tested` and `evidence`) |
| `strong` | a working, live-used feature depends on it. The player's live use is evidence, not a controlled test. |
| `unverified` | a correlate, a guess, or a parked lead |
| `rejected` | ruled out, with the evidence (for example the ground/air word +0x474 is never read while flying, so it is not a control) |

The seed never says `verified`. `db set-status` needs `--evidence` and appends it with the date, so the history stays.

Saved scans go in `scans\NAME.json`, snapshots in `snaps\NAME.json`.

## How it is used

1. **Check first:** `db find <word>`. Past work, rejected leads and their evidence are there.
2. **Targeted before full:** scan a `--range` or `--module` you have reason for; use `--full` only when needed.
3. **Narrow, don't dump:** `scan value... --save x`, change the thing in game, `next x decreased`, repeat until a few hits are left.
4. **Snapshot around one action:** `snap before...`, do exactly one thing in game, `snap after...`, `diff before after`.
5. **Record it:** `db add` with `how_found`, then `db set-status` with evidence once tested. Keep the status honest.

## Limits

- `watch` polls. It misses changes faster than `--ms`, and it cannot tell who wrote a value. Who writes / reads / runs needs `grbresearch-dll`.
- `refs` object detection is a heuristic (walk back for a qword that points into an image whose first entry points at code).
- `chars` and the seed use GRB_vulkan.exe offsets; nothing is tested on GRB.exe.
- Addresses in saved scans and snapshots are only good for the same game run (pid is stored).

## Not done yet

- No pointer-path search to a static root (grbscan's `--ptrscan`); `refs` finds one level.
- No dump of the T target (`target.txt`) and its owner; read `target.txt` and use `inspect` meanwhile.
- No module-relative save of scans across game restarts.
- Not tried against the running game yet: the tests only read the tool's own process.
