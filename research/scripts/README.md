# Scratch research scripts (read-only)

One-off Python helpers written during the research, Each was used once for the job in its header; none is polished or tested beyond that. All are read-only: they parse the exe files on disk, or read the running game's memory with `ReadProcessMemory` only. None writes to the game.

Run with `python -P script.py...` (Python 3.13). The ones that decode instructions need `pip install capstone`. Some have the exe path hard-coded at the top (`EXE = ...`): change it for another PC.

| Script | What it does | Arguments |
|---|---|---|
| `pe.py` | Minimal PE helper for `GRB_vulkan.exe`: sections, exception table, "which function holds this rva". Imported by the others. | hard-coded `EXE` path at the top |
| `pe_quick.py` | First look at an exe: header, sections, entropy per section, RTTI name count. | exe paths |
| `pe_dirs.py` | PE data directories, plus a sanity look at the exception table (.pdata) and RTTI. | exe paths |
| `sig_disk.py` | Finds the mod's code patterns in the on-disk exes and prints their RVAs, to compare with what the mod found at runtime. | exe paths |
| `globals_check.py` | Offline check of the 9 global patterns (`dll\src\globals.rs`): one hit per exe, rip target equals the known constant in Vulkan, sensible section in DX11. | none (paths inside) |
| `argsat.py` | For each call-site rva, prints the instructions just before it that set rcx/rdx/r8/r9, to see the arguments. | rvas (hex) |
| `insfind.py` | Finds instructions whose text matches a regex: scans for a byte needle, then decodes around each hit. | `<hex needle> <regex> [back]` |
| `vcall.py` | Finds functions that make a virtual call through one vtable slot offset, within an rva range. | `<slot offset hex> <lo rva> <hi rva>` |
| `enumrefs.py` | Finds code that uses a 32-bit constant as an immediate (default: the stimulus-type enum hash). | `[value hex]` |
| `cave.py` | Decodes a code cave's bytes with capstone, to check what was emitted. | edit the constants inside |
| `slots.py` | Runs `grbstatic` for a list of commands and prints the results (helper around the exe analyzer). | grbstatic commands |
| `classscan2.py` | Live, read-only: scans the game's memory for the player's 7 class levels stored as a group. | `<pid>`; writes `%TEMP%\aurora-class\scan.txt` |
| `nvgobj.py` | Live, read-only: reads the NVG display object and its overlay byte, to check the pointer path. | none (pid found inside) |

Also in `tools\`: `peek.py`: reads `candidates.bin` (address + float records saved by's finder), keeps the ones near the player's X coordinate, and prints the floats around each from the running game, read-only. One-off; superseded by `grbresearch` inspect/chain. Its folder path was renamed to `Aurora` on 4 Nov; the `candidates.bin` it expects is from 30 Oct and may not exist any more.

The real tools, with docs: `research\grbstatic\` (offline exe analyzer), `research\ghidra\ghidra.ps1` (Ghidra headless decompile), `research\grbresearch\` (external read-only inspector), `research\grbresearch-dll\` (in-game recorders).
