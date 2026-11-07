# Notes

| | |
|---|---|
| [builds.md](builds.md) | The two executables: identity, layout, protection, RTTI, DX11 vs Vulkan |
| [signatures.md](signatures.md) | Every code pattern the mod uses, checked in both builds |
| [perception.md](perception.md) | How enemy NPCs see the player |
| [shot-trace.md](shot-trace.md) | How a shot is built and fired |
| [sources.md](sources.md) | Outside resources and how far to trust them |
| [../../dll/docs/offsets.md](../../dll/docs/offsets.md) | The mod's offsets, globals and patches |

## Conventions

- Every address says its kind: `rva` (module-relative, per build), `va` (image base 0x140000000 + rva), `base+rva` (runtime), `obj+0x..` (field offset), `[..]` chains, signatures.
- Builds are named `<exe stem>-<PE timestamp>`, e.g. `GRB_vulkan-64ff3734`. Vulkan RVAs do not apply to DX11.
- Confidence: Confirmed, Likely, Candidate, Rejected.
- The protected section of the executable is skipped, never analysed.
