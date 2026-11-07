# The two executables

Read-only checks of the files on disk. Steam build 25349072
(`steamapps\appmanifest_2231380.acf`), files dated.

| | Vulkan | DX11 |
|---|---|---|
| File | `GRB_vulkan.exe` | `GRB.exe` |
| Build id (ours) | `GRB_vulkan-64ff3734` | `GRB-64ff3174` |
| Size | 519,231,016 | 536,007,208 |
| SHA-256 (first 16 hex) | ab6a37dbac9e9d4f | 51569e67923b7f00 |
| PE timestamp | 0x64ff3734 | 0x64ff3174 |
| Image base | 0x140000000 | 0x140000000 |
| Graphics import | vulkan-1.dll | d3d11.dll |
| Exception-table functions | 375,670 | 374,915 |
| RTTI complete object locators | 1,626 | 1,626 |
| `.?AV` class names in file | 1,729 | 1,729 |

The PE timestamp has stayed 0x64ff3734 since Sept 2023 while Steam updated the files (Oct 2026),
so a timestamp alone doesn't identify a build: also use size + SHA-256 + Steam buildid.

## Layout (both builds look alike)
- **Section names are shuffled** and mean nothing (the main code is called `.sbss` in Vulkan and
  `.rsrc` in DX11). Use flags and data directories.
- Main game code: the first section, rva 0x1000, ~70 MB, flags code|exec|read, entropy ≈ 6.5
  (normal compiled code). This is the code the mod's signatures hit.
- A ~390 MB section with read|write|exec flags and entropy ≈ 6.9: the **protection layer**. The
  tools skip it; nothing here analyses, decrypts or touches it.
- Data directories work: exception table, relocations, imports, TLS, load config all parse.

## What is available statically (no game running)
| Thing | Status | Evidence |
|---|---|---|
| Function bounds (exception table) | Confirmed | 375k entries, sorted, all plausible; Ghidra decompile of rva 0x11bb000 used it |
| Plain code = runtime code | Confirmed at 3 spots, Likely for the 15 signatures | `signatures.md` |
| RTTI class names + vtables | Confirmed present (1,626 COLs, self-check) | `grbstatic rtti` lists them |
| Decompiled functions | Works | `research\ghidra\ghidra.ps1 decompile`, 5 s per call after a 47 s import |

## DX11 vs Vulkan
- Same game code, shifted: the 15 signatures hit once in each, RVAs differ by 0x1f700–0x20c00
  (not constant). Map addresses between builds by signature (`grbstatic makesig --other`,
  `grbstatic compare`), never by adding a delta.
- Different: the graphics backend (imports, the mod's Vulkan drawing hooks, `POSE_FN` is in shared
  code though). The mod only supports Vulkan today.
- RTTI counts are equal; vtable RVAs differ per build.

## Open questions
- Is the protection layer involved at runtime in any code the mod patches? (No sign so far: all
  patched spots are in the plain code section.)
- Does a future Steam update change the timestamp? Check `grbstatic info` after every update.
