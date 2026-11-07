# Code signatures in both builds

Checked offline against the files on disk (Steam build 25349072): read-only
pattern search over the executable sections, with a Python check first and `grbstatic sigs` as
the tool for re-checking. "Unique" means exactly one hit.

All 20 patterns below are **unique in both executables**. RVAs are module-relative, per build.

| Name (file) | Vulkan `GRB_vulkan-64ff3734` rva | DX11 `GRB-64ff3174` rva | Δ (Vk − DX) |
|---|---|---|---|
| FALL (airwalk.rs) | 0x353d0c0 | 0x351d2c0 | +0x1fe00 |
| IN_AIR (airwalk.rs) | 0x3539aef | 0x3519cef | +0x1fe00 |
| PATTERN credits (credits.rs) | 0x1a72155 | 0x1a52025 | +0x20130 |
| PATTERN god (godmode.rs) | 0x360a34e | 0x35ea50e | +0x1fe40 |
| SET (nvg.rs) | 0x214d79f | 0x212d53f | +0x20260 |
| LOAD (nvg.rs) | 0x11bb32b | 0x119a70b | +0x20c20 |
| SIGHT (unseen.rs) | 0x359e4bb | 0x357e67b | +0x1fe40 |
| FILL (unseen.rs) | 0x36344a2 | 0x3614662 | +0x1fe40 |
| CLAMP (unseen.rs) | 0x3634632 | 0x36147f2 | +0x1fe40 |
| POSE_FN (vkdraw.rs) | 0xacfc10 | 0xaafea0 | +0x1fd70 |
| RECOIL (weapon.rs) | 0x36ffe20 | 0x36e0620 | +0x1f800 |
| AMMO (weapon.rs) | 0x213a530 | 0x211a2d0 | +0x20260 |
| STAMINA (weapon.rs) | 0x36295e0 | 0x36097a0 | +0x1fe40 |
| DAMAGE (weapon.rs) | 0x3670c1a | 0x36511ca | +0x1fa50 |
| FIRE (weapon.rs) | 0x36b2380 | 0x3692c50 | +0x1f730 |
| SECOND (unseen.rs) | 0x363a6e0 | 0x361a8a0 | +0x1fe40 |
| SPREAD (weapon.rs) | 0x3650cd0 | 0x3630ea0 | +0x1fe30 |
| SHAKE (weapon.rs) | 0x37028e0 | 0x36e30e0 | +0x1f800 |
| SWAY (weapon.rs) | 0x37012c0 | 0x36e1ac0 | +0x1f800 |
| RESERVE (weapon.rs) | 0x216171c | 0x21414bc | +0x20260 |

The last five were added 4 Nov by `grbstatic sigs` (all 20 unique in both exes).

## What this proves
- **On-disk code = runtime code** at these spots: the Vulkan NVG `LOAD` hit + 7 = rva 0x11bb332,
  the same address the mod logged at runtime ("nvg loading hook at exe+0x11bb332"); `SET`'s
  patched byte at +0x10 = rva 0x214d7af, as the mod patches it; the god-mode site 0x360a34e
  matches the runtime file name `catch-360a34e.txt`. So static analysis of the files is valid
  for this code.
- **DX11 has the same code**, shifted by roughly 0x1f700–0x20c00 (not a constant: never convert
  by adding a delta; search the signature). The mod's non-drawing features could work on DX11;
  its drawing hooks are Vulkan-only and the Launchpad refuses DX11 today.

## Limits
- Static uniqueness is checked over executable, non-writable sections only; the protected RWX
  section is skipped.
- The mod checks its saved spots again at runtime (`Bin\Cache\Offsets.bin`), which is the real guard.
- Confidence: Confirmed for "pattern unique in this file" (direct check). The runtime
  equality is Confirmed for NVG LOAD/SET and the god site (three logged runtime addresses), and
  Likely for the rest (same method, not compared to a runtime log one by one).
