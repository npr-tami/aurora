# Shot path and hit trace

Read-only, offline. Only `grbstatic` and the Ghidra helper were run, on the exe file. The game was not started,
read or touched; no repo code changed; `aurora-dll` was not built.

Purpose: find where the player's shot gets its direction and where it turns into a hit on a character, for two
planned PvE-only features (NPC/drone targets only): Silent Aim (steer the player's shot to a chosen NPC) and Magic Bullets
(the player's shot hits the chosen NPC through walls).

**Build:** `GRB_vulkan-64ff3734`, Steam build 25349072, size 519,231,016, SHA-256 `ab6a37dbac9e9d4f…`
(same as `builds.md`). All addresses are **rva** in this build unless marked. DX11 not checked.

Confidence words: Confirmed / Likely / Candidate / Rejected. Nothing here is Confirmed: nothing was seen live.

## Short answer
- **Bullets are simulated projectiles, not instant ray hits.** Each shot is a 0x550-byte "shot object" that moves
  every frame; each frame its travelled segment is swept against the world, and then a precise ray is cast against
  each touched character's own hit shapes. (Likely)
- **Origin and direction enter at one function**: `0x3652430` (shot object, `float4* origin`, `float4* direction`).
  Called at fire time from `0x365af00`, and mid-flight by a re-aim function `0x3693250`. (Likely)
- **The game already has homing shots.** The per-frame update calls `0x36ad570` when `shot+0xF0` is set; it turns
  the shot's direction toward a target and re-aims through `0x3693250`. Best Silent Aim lead. (Likely)
- **Hit on a character**: `0x36af4b0` (per contact) → `0x3692300` (ray vs that object's own shapes) → `0x3690db0`
  (store hit) → `0x368f240` (send damage to the damage manager). Best Magic Bullets lead: `0x3692300` does not use
  the world, only the target's shapes. (Likely)
- **Drones:** no RTTI and no readable names for game classes (only Havok, UI, CRT). Whether drones use the NPC
  class (CHAR_COMPONENT) can't be told statically. Open; needs one live read of a drone. (Unknown)

## Commands run
```
G = code\target\release\grbstatic.exe E = C:\Program Files (x86)\Steam\steamapps\common\Ghost Recon Breakpoint\GRB_vulkan.exe
G --out %TEMP%\shottrace info E
G sigs E --pattern "dmg=83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8"
G func E 0x3670c1a; G dis E 0x3670c1a --func
G xrefs E <rva> for 0x3670990 0x365af00 0x3699770 0x4a590a8 0x3692300 0x36496b0 0x364f250 0x36af4b0
                     0x3692780 0x3690db0 0x36abef0 0x364a830 0x36907e0 0x368f240 0x3692a40 0x36ae4c0
                     0x49bfd70 0x4a4c5f0 0xc5b0c0 0x3691240 0x3652430 0x3693250 0x36a70e0 0x365b490 0x36b1c30
                     and the Havok wrappers 0xcd83c0 0xcef990 0xcf16a0 0xcf7110 0xcfdd40 0xd04180 0xd04d70 0xd05910 0xd0c820 0xcd0570
G xrefs E 0x4b2d610 / 0x4b2fb28 / 0x4b294e0 (hknpClosestHitCollector, hknpAllHitsCollector, hknpForwardClosestHitCollector vtables)
G vtable E 0x4a590a8 / 0x49bfd70 / 0x4a4c5f0
G --json rtti E      
G strings E <Bullet|Projectile|Ballistic|Hitscan|Raycast|RayCast|Drone|Behemoth|Azrael>
G makesig E 0x3652430 / 0x3691240 / 0x3692300
research\ghidra\ghidra.ps1 decompile -Program GRB_vulkan.exe -Rva... (files below)
```
(`shot-3670990.c`, `shot-365af00.c`, `shot-level2.c`, `shot-raywrappers.c`, `shot-3692300.c`, `shot-36af4b0.c`,
`shot-level4.c`, `shot-level5.c`, `shot-level6.c`, `shot-36ad570.c`).

## Q1. The "weapon damage" pattern and its callers
- Pattern `83 BF A8 00 00 00 06 …` → **OK, rva 0x3670c1a** (1 hit; same as the CE spot in `offsets.md` 2.6).
- Enclosing function: **`sub_3670990`** (0x3670990–0x3670e69, 0x4d9 bytes, one chunk). The site is at +0x28a.

What `0x3670990` does (Ghidra, `shot-3670990.c`): it **fills a new shot object from the weapon's stats**
(`param_1` = shot object, `param_2` = a weapon/ammo stats object read through its vtable slots 0x358–0x3c8).
- `damage = f(owner, stats->vf_0x378)` as float, then `cvttss2si` (the pattern's `F3 48 0F 2C E8`).
- `if (shot+0xA8 == 6 || stats->vf_0x398->byte_0x88) damage_b = damage >> 3` → writes `shot+0xAC` and `shot+0xB0`.
  So the unknown `[rdi+0xA8] == 6` is a **shot/ammo type** read from `stats->vf_0x3a8`; type 6 (or the flag) cuts one
  damage field to 1/8. (Candidate)
- Also sets speed-like `shot+0x370/0x378/0x37c` (`stats->vf_0x3b0` × 0.001, min 1e-6), range `shot+0x158` (min 20),
  `shot+0xc/0x10/0x8` (value, and value/3 capped at 60), shooter handle `shot+0x90` (from owner+0x160), `shot+0xD0`.
- Correction to `offsets.md` 2.6 ("runs right after a hit's damage is worked out"): statically this runs **when a shot
  object is created** (fire time), not at the hit. The player 2 Nov recording (the player's body on 22 of 399 hits) fits "per shot
  fired by anyone". (Likely; check live before changing the doc's wording.)

Caller chain (each has exactly one code caller):
| Level | rva | What it is (from decompile) | Conf. |
|---|---|---|---|
| 0 | `0x3670990` | fill shot from weapon stats (damage site) | Likely |
| 1 | `0x365af00` | **build a shot**: allocates 0x550 bytes, constructor `0x3643e70` (vtable `0x4a590a8`, no RTTI, 11 slots), calls `0x3670990`, scales damage by `req->vf_0x98`, builds the path points at `shot+0x160` (`0x3658840` straight / `0x3658be0` arcing with gravity 4.905 = g/2), then `0x3652430(shot, req->vf_0x8, req->vf_0x10)` | Likely |
| 2 | `0x3699770` | **fire handler**: `param_2` = fire request; checks `req->vf_0x58`, `0x286d110(global 0x463ebe00, 1, req->vf_0x68)`, builds the shot, registers it (`owner->vf_0x3e0`, list at `this+0x10`, callback at `this+0x30`) | Likely |
| 3 | table `0x4a593a8` slot 0 | no code caller; a listener pointer table (lea at `0x3644ef1`). Fire requests come in by a virtual call. | Candidate |

The fire request (`param_2` of `0x3699770` / `0x365af00`): slot `+0x8` = **origin** (float4), `+0x10` = **direction**
(float4; `0x3670810` turns it into a rotation and a pitch in degrees, clamped ±89.99). Others used: `+0x18 +0x20 +0x28
+0x30 +0x60 +0x68 +0x70 +0x98 +0xa0 +0xb0`. Class unknown (no RTTI). (Likely)

## Q2. Shot ray / hit trace
### RTTI and strings
- Game classes have **no RTTI**. The 1,626 RTTI classes are Havok (`hk*`, `hknp*`), the UI (`phoenix::`, `pwl::`,
  `graffiti_pnx::`), CryptoPP, CRT and test code. Game names are hashed (e.g. `0x89b2c8dd`, `0xfbb63e47`).
- Matches for the keywords are Havok or UI only: `hknpProjectileGun`, `hkdRaycastGun`, `maxBulletsInWorld`,
  `hknpClosestHitCollector`, `hknpAllHitsCollector`, `hkcdSimdTree::ProcessRayCastLeaves`, `pwl::HitTest` (UI hit test).
  No `Ballistic`, `Hitscan`, `Behemoth`, `Azrael` strings at all. `hknpProjectileGun` / `hkdRaycastGun` look like Havok
  demo classes, not the game's guns. (Rejected as the game's shot code)
- Havok ray-cast wrappers (game side, around 0xcd0000–0xd10000) found through the collector vtables, e.g.
  `0xcd83c0(world_or_shape, float4* from, float4* to, out)` = closest hit along from→to (`shot-raywrappers.c`).

### The shot object (0x550 bytes, vtable `0x4a590a8`)
| Field | Meaning | Conf. |
|---|---|---|
| `+0x90` | shooter handle (owner+0x160), used as "source" when damage is sent | Likely |
| `+0xAC`, `+0xB0` | damage (int); `+0xBC` = damage after falloff (`0x36699d0` × `0x3669a80`) | Likely |
| `+0xC8` | lifetime countdown (seconds; `-= dt` each frame) | Likely |
| `+0xF0` | homing target (compared to a targeting component's `+0x50`) | Likely |
| `+0x104` | homing turn rate | Candidate |
| `+0x120` | rotation from the fire direction; `+0x130` current unit direction (float4) | Likely |
| `+0x140` | origin (float4) | Likely |
| `+0x154` | distance travelled | Likely |
| `+0x160` | path points (float3 array, 12 bytes each), count `+0x176` | Likely |
| `+0x180` / `+0x190` | previous / current position (float4) for this frame's segment | Likely |
| `+0x378` | remaining "energy" (stops the sweep at ≤ 0; walls subtract from it in `0x36af4b0`) | Candidate |
| `+0x385`, `+0x391` | entry / exit hit lists (0xd0 per entry) | Candidate |
| `+0x4d8` | hits applied count | Candidate |
| `+0x53c` | pending per-character hit list (0x100 per entry) | Candidate |

### Origin + direction into the trace
- **`0x3652430(shot, float4* origin, float4* direction)`**: subtracts a world offset (global `[0x63f33c8]+0x28 → +0x188`)
  from the origin, transforms the path points (`0x36591c0`) into world space, copies them to `+0x16c`, and sets
  `+0x180/+0x190/+0x360/+0x4f0` (= first point), `+0x120/+0x130` (direction), `+0x140` (origin). Sig
  `4C 8B DC 53 56 41 54 41 57 48 81 EC 08` (13 bytes, unique). Callers: `0x365af00` (fire), `0x365b490` (another
  shot builder, caller `0x365b230`), `0x3693250` (re-aim). (Likely)
- **`0x3693250(shot, float4* new_direction, ushort n)`**: rebuilds the path for a new direction from the current
  position `shot+0x190`, then calls `0x3652430`. Called only by `0x36ad570`. (Likely)
- **`0x36ad570(shot, dt)` = homing step**: target position from `0x366b800` or from the shooter's targeting component
  (must match `shot+0xF0`); turns `+0x130` toward it by `dt × shot+0x104`; re-aims through `0x3693250`; stops homing
  (`+0x2a = 1`) when the target is behind. Called from the per-frame update `0x36a70e0` only when `shot+0xF0 != 0`.
  (Likely)

### Per-frame update and world sweep
- `0x36a70e0(shot, dt)` (reached by a `jmp` at `0x363fc68`, no exception entry there): lifetime, homing
  (`0x36ad570`), move (`0x36b1c30`), then `0x3691240`. (Likely)
- **`0x3691240(shot)`**: segment `+0x180 → +0x190`, unit direction into `+0x130`, distance into `+0x154`; skips if
  length < 0.001 or `+0x378 ≤ 0`. Sweeps forward and backward with `0xc5b0c0(shot+0x1b0 / +0x280, world, from, dir)`
  (a general cast, 27 callers game-wide), collects contacts (`0x36af350` → `0x3692a40` → `0x3692780`), merges them
  (`0x3676a70`), then **`0x36907e0`** walks the contacts. Sig `12 bytes at 0x3691240`, unique. (Likely)
- `0x36907e0(shot, contacts[0x90 each], dir, len)`: per contact calls `0x36af4b0`; return codes 0/2/3/4/5 (stop,
  next, …; meanings unknown); after the loop calls **`0x368f240(shot, target, hit, …)`** once for the best hit. (Likely)

### Trace result → hit on a character
- **`0x36af4b0(shot, contact, out)`**: `contact+0xC0` = physics body, `contact+0xC8` = entity, `contact+0xD0` = contact
  point. Skips the player's own shooter (distance < 5 m and same owner check `0x2868590`). Body type 0x19 or 0xE goes straight
  to the damage manager. For characters it builds origin = `shot+0x180` (or the contact point) and direction to the
  target, then calls `0x3692300`; on a hit `0x3690db0`. For non-characters (walls) it reduces `shot+0x378`
  (penetration) and records material. (Likely)
- **`0x3692300(target_component, out_hits, float4* origin, float4* dir)`**: for each shape part in the target's list
  (`+0x67`, count `+0x71`, skipping parts with `+0x98`/`+0x99` set), transforms origin and direction into that
  part's local frame and casts `origin ± 2·k·dir` with **`0xcd83c0`** against only that part's shape. Each hit gets a
  part/bone id (`0x3659f00`), a material (`0x366c200`), a 0x60-byte record; sorted (`0x3643140`, compare `0x369ef40`).
  Does not test the world: the walls were handled by the sweep. Sig 10 bytes at `0x3692300`, unique. (Likely)
- `0x3690db0(shot, hit, contact)`: stores hit point and normal into the entry list `+0x385` or exit list `+0x391`
  (by the side of the segment), sets `shot+0x520` and `+0x24`. (Likely)
- **`0x368f240(shot, target, hit, flags…)`**: builds a hit description (`0x3665d60`), then sends it to the damage
  manager (global `0x463ebe00`): `0x2887200` and `0x2887a90(manager, shooter [shot+0x90], target, info)`. Also
  called from `0x3698800` and `0x36a4f90`. This is the closest "a hit lands on a character" point. (Likely)

### How this maps to the two features (ideas only, nothing built)
- Silent Aim: (a) on the player's shots, set `shot+0xF0`/`+0x104` so the game's own homing steers it; or (b) change the
  direction passed into `0x3652430` from `0x365af00` (origin unchanged). (a) needs to know what `+0xF0` holds
  (a handle compared to `component+0x50`). Candidate.
- Magic Bullets: when the player's shot's sweep finds the chosen NPC anywhere along the segment, or by calling the
  `0x3692300` → `0x3690db0` → `0x368f240` path for that NPC directly. Walls only cost `+0x378`; keeping it above 0
  may already let the player's shot pass walls. Candidate.
- Both must check "the player's shot" (shooter `shot+0x90` = the player's) and "target is an NPC/drone class, never a player".

## Q3. Drones
- No RTTI or plain names for drones (`Drone` strings are effects/shaders/`deployDrone` keys; no `UAV`, `Robot`,
  `Behemoth`, `Azrael`).
- NPC class `0x49bfd70` (CHAR_COMPONENT): 20 slots, no RTTI, one constructor `0x3192570` (lea at `0x3194096`); slot 1
  returns a type hash `0x60BD1130`. Player class `0x4a4c5f0`: 136 slots, one constructor `0x358adf0`. Different
  classes. (Likely)
- Whether drones' character objects use `0x49bfd70` is **not visible statically**. One live read of a drone's
  character tag would answer it. (Unknown)

## Open questions
- Which of the player's weapons really go through this projectile path (all guns, or only some)? Live check.
- What `shot+0xF0` holds (handle vs pointer) and who normally sets it (homing launcher? Drones?).
- Return codes of `0x36af4b0`; meaning of `shot+0x378` (penetration energy?).
- The fire request class and its vtable (slots +0x8 origin, +0x10 direction).
- The DX11 build: none of these rvas were mapped (`grbstatic compare` not run).
- Drones vs CHAR_COMPONENT (live).

## Time and stop
About 70 minutes. Last finished step: the `0x36ad570` homing decompile.

## Live check
Read-only, `grbresearch.exe` only, same build. Full notes:.
- Shot objects (vtable `0x4a590a8`) are real per-bullet objects for the player's guns, soldiers' rifles and
  drone weapons. Shooter `[shot+0x90]` → entity: the player's body, the soldier, or a drone's main-6 part (0x1106). (Confirmed)
- Drones are not CHAR_COMPONENT: type word & 7 = 5 (players 1, NPCs 2), no NPC marker. (Confirmed for 3 drones)
- `shot+0xF0` = a 64-bit object ID looked up by `0x2655780` (static, Likely); 0 on all 171 live shots
  (the player's, soldiers', drones'). No homing shot seen. (Confirmed for those)
- The Damage Multiplier value showed up in a shot's `+0xAC` (21,600,000): the damage hook runs at shot
  build time, as this note said. (Confirmed, VHSD2)
- The player's guns on the shot path: VHSD2 (ammo 7, dmg 21, speed 950), TAC50 (ammo 9, dmg 133, speed 1100),
  PX4 (ammo 1, dmg 21, speed 500). (Confirmed)
