# grbstatic

An offline analyzer for Ghost Recon Breakpoint's exes on disk (`GRB_vulkan.exe`, `GRB.exe`). It answers "where is this code, who calls it, what class is this, and where is the same thing in the other build" without starting the game.

**Read-only.** It maps the exe file read-only (`GENERIC_READ`, `PAGE_READONLY`). It never opens a process, never writes into the game folder (an `--out` there is refused), and never changes a file it reads. Writable + executable sections (the game's protection layer, about 390 MB) are listed as `protected (skipped)` and are never decoded, searched or analysed.

## Build

```
cargo build -p grbstatic --release
cargo test -p grbstatic
```

The exe is `target\release\grbstatic.exe`. The unit tests use small hand-made byte buffers and never touch `%APPDATA%`.

## Addresses

Every address says what it is:

| Form | Meaning |
|---|---|
| `rva 0x11bb332` | module-relative (what the mod logs as `exe+0x11bb332`) |
| `va 0x1411bb332` | 0x140000000 + rva: the on-disk preferred address. NOT a runtime address: ASLR moves the game. |
| `file 0x...` | offset in the file |
| `sub_11bb000` | a function start (from the exception table); the number is its rva |

Inputs are hex rvas: `11bb332`, `0x11bb332` or `exe+0x11bb332`. Absolute addresses are refused.

Tables label their columns (`vtable rva`, `COL rva`).

## Output files

Every command prints aligned text. With `--json` it also writes its result as JSON; `info` and `rtti` always write.

- Default base folder: `%APPDATA%\Aurora\Dumper\research\static\`, used only when `Aurora` already exists (the tool never makes `Aurora`; otherwise pass `--out`).
- Per-exe files go to `<base>\<build id>\` (build id = exe name + PE timestamp, e.g. `GRB_vulkan-64ff3734`): `build.json`, `rtti.json`, `rtti.txt`, `func-<rva>.json`, `dis-<rva>.json`, `xrefs-<rva>.json`, `makesig-<rva>.json`, `vtable-<name>.json`, `strings-<text>.json`.
- Two-exe files go to `<base>\`: `sigs.json`, `compare-<A>-<B>.json`.
- Tests: `--out %TEMP%\grbstatic-out`.

Exit codes: 0 ok, 1 error (or a sig not OK), 2 bad arguments or no output folder.

## Commands

| Command | What it does |
|---|---|
| `info <exe>` | Size, SHA-256, PE timestamp, checksum, image base, entry, flavour (Vulkan / DX11 from the imports), Steam build id (from `steamapps\appmanifest_2231380.acf` above the exe), sections (flags, entropy of the first 4 MB, role), data directories, exception entries, RTTI COL count, relocations. Writes `build.json`. |
| `sigs <exe>... [--src DIR] [--pattern NAME=HEX]...` | Every `const NAME: &str = "<hex>";` in `dll\src\*.rs` (named `file.NAME`, e.g. `nvg.LOAD`), searched in each exe's code sections: `OK rva 0x...` (1 hit), `MISSING`, `AMBIGUOUS(n)`. One column per exe. Exit 1 if any is not OK. `--src` picks another folder. |
| `func <exe> <rva>` | The function holding rva: start, every chunk (chained unwind entries followed to the primary), size, unwind info rva. |
| `dis <exe> <rva> [--count N] [--func]` | Intel-syntax disassembly (default 20 instructions). `--func` shows the whole function, chunk by chunk, with `=>` on the asked rva. Notes name what a target holds: a string, `vtable Class`, an RTTI COL or type descriptor, an import, a pointer to code. |
| `xrefs <exe> <rva> [--limit N]` | Code that references rva (`call`, `jmp`, `jcc`, `call-ptr`, `jmp-ptr`, `lea`, `read`, `write`, `read+write`) with its function, and 8-byte data pointers to it. A data pointer inside a vtable says which class and slot; without RTTI it names the pointer table that holds it (its start is the nearest spot code `lea`s). |
| `rtti <exe> [--filter TEXT]` | All RTTI classes: name (`Bar::Foo`, templates and function pointers included; raw name kept), vtable rva, slot count, COL rva, type descriptor rva, base classes. Writes `rtti.txt` and `rtti.json` (all classes); `--filter` only narrows the screen list. |
| `vtable <exe> <class name or vtable rva>` | The slots: index, where it sits, the function, its first instruction. A pointer table without RTTI ends where the next one starts (the next spot code `lea`s). |
| `makesig <exe> <rva> [--other EXE2]` | The shortest pattern starting at the instruction at rva that matches once in the code sections. Wildcards: rel32 branch offsets, RIP-relative displacements, relocated bytes, and 4-byte values that look like an rva (table addresses). If nothing within 96 bytes is unique, it starts up to 64 bytes earlier and says where the rva sits in the pattern. `--other` searches the other exe and grows the pattern while that one has several hits. |
| `strings <exe> <text> [--xrefs]` | ASCII and UTF-16LE hits (rva, section, the whole string around it). `--xrefs`: who uses each string (first 50 hits). |
| `compare <exeA> <exeB> <rva>...` | Maps each rva of A into B. `unique signature` (high; medium when the pattern runs past the function end and leans on the neighbour), then `same vtable slot` (medium: same RTTI class and slot in B), else `not mapped`. |

## Examples

```
grbstatic --out %TEMP%\grbstatic-out info "C:\...\GRB_vulkan.exe"
grbstatic sigs "C:\...\GRB_vulkan.exe" "C:\...\GRB.exe"
grbstatic dis "C:\...\GRB_vulkan.exe" exe+0x11bb332 --func
grbstatic makesig "C:\...\GRB_vulkan.exe" 0x11bb332 --other "C:\...\GRB.exe"
grbstatic xrefs "C:\...\GRB_vulkan.exe" 0x360a130
grbstatic --out %TEMP%\grbstatic-out rtti "C:\...\GRB_vulkan.exe" --filter phoenix::
```

## How it works

- **Sections:** the protection shuffles section names, so roles come from flags only: executable and not writable = code; writable and executable = protected (skipped); the rest = data.
- **Functions:** the exception directory (about 375,000 entries). A chunk whose unwind info has `UNW_FLAG_CHAININFO` holds its parent's entry after its unwind codes; parents are followed to the primary entry, whose start is the function start. Leaf functions (no stack use) have no entry.
- **Xrefs:** one pass over the code finds every 4 bytes that, as a rel32 / disp32, would land on the target; each candidate is confirmed by decoding its whole function chunk from the start (real instruction boundaries). Code outside every entry is decoded from the end of the entry before it.
- **RTTI:** a COL is a 4-aligned spot with signature 1 whose self rva (+20) is its own rva. Its vtable sits after an 8-byte pointer to it in a data section. Most of the game's classes have no RTTI (about 1,600 do).

## Limits

- The protected section is never read: code or tables inside it are not covered, and vtable slots pointing there are listed but not decoded.
- Leaf functions have no exception entry: `func` and `dis --func` cannot bound them; `xrefs` still finds their references, marked `no entry`.
- Data pointers are searched 8-aligned in non-executable sections only.
- The shortest unique pattern can be very short (4 bytes). It is unique in this build; `compare` and `--other` check the other build.
- Demangling covers names, templates, pointers, references, function and member-function pointers; anything else falls back to a plain split (the raw name is always kept).
