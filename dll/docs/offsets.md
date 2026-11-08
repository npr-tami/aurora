# Offsets, patterns and code patches

This file lists every game-specific address, field offset and byte pattern Aurora uses, plus every change it makes to the game's code. For each one it says what it means, how sure it is.

The game is Ghost Recon Breakpoint, Vulkan build (`GRB_vulkan.exe`). Every `exe+0x...` number belongs to build 25349072. Patterns are searched at runtime, so they may survive an update.

**Status**
- **Confirmed**: proved or tested live in the game.
- **Experimental**: it is in the code, but it was not proved, or it only partly works.
- **Unknown**: the code depends on it (usually as bytes inside a pattern), but nobody knows what it means.

**Sources**
- `file.rs:line` means `dll\src\file.rs`, and `core/file.rs` means `shared\aurora-core\src\file.rs`.
- "CE tables" means community Cheat Engine tables.

---

## 1. The basics

### Static offset

A static offset is a fixed spot inside the game's exe. Windows loads the exe at a different base address each run, so the mod adds the base: `address = exe base + offset`. Most static offsets here hold a pointer to an object.

Example, the player's position (fly.rs:13-15, 66-75):

```
exe base + 0x63EBEB0 a fixed spot in the exe (the "body global")
read 8 bytes there -> the address of the player's body object (it changes on death and loading)
that address + 0x20 -> x, y, z as three f64 numbers
```

The `+0x20` is a **field offset**: a spot inside an object, counted from the object's start.

**Why updates break static offsets.** Each game build is compiled and laid out again, so the global can land at a different offset. After an update `exe+0x63EBEB0` may point at something else, and the code cannot tell. The only protection is a sanity check. For example, fly treats a pointer under 0x10000 or an unreadable position as "no body" (fly.rs:66-75). Field offsets can move too when an object's layout changes. Weapon.rs:27-28 records the community table's warning that the player's network part (+0x190) moves with game updates.

### Pattern

A pattern is a short run of the game's machine code, written as hex bytes. `??` means "any byte here". It stands for numbers that change between builds, such as call targets and stack sizes. The mod searches the exe for the pattern at runtime, so only the code's shape has to stay the same, not its place.

Example, the damage check (godmode.rs:12):

```
48 8B ?? 48 8B ?? FF 90 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? ?? ?? 44 38 A7
```

How the search works (`memory::find_code`, memory.rs:196-257):
- It scans the exe one memory region at a time. The protector leaves unreadable holes, so the exe is never read as one block.
- It needs **exactly one hit**. Zero hits or two give the error `NAME: pattern found N times, want 1 (game updated?)`, and nothing is patched.
- A hit is saved in `%APPDATA%\Aurora\Bin\Cache\Offsets.bin` (`paths::offsets_cache`) under the search's name, together with the game's build id. The build id is the exe's link time plus its size (memory.rs:172-178).
- On the next inject the saved spot is checked against the pattern again and the scan is skipped. A new build id starts an empty list. This cache was built and is not tested in game yet.
- The log says where each search landed in this build: `NAME: found at exe+0x... (saved)`.

Search names in use: `god damage check`, `credits shop read`, `airwalk fall`, `airwalk in-air`, `nvg switch-on`, `nvg loading`, `unseen sight`, `unseen meter fill`, `unseen meter clamp`, `unseen second meter`, `weapon recoil`, `weapon accuracy`, `weapon ammo`, `weapon shake`, `weapon sway`, `weapon stamina`, `weapon damage`, `weapon reserve`, `weapon fire rate`, `pose function`, `lod table`.

**When a pattern does not match.** The mod never changes game code in that case:

| Feature | What happens | Source |
|---|---|---|
| God Mode | It still turns on when the player's character object is found through the player's body, which needs no pattern. Only the "wait for a hit" path fails, with the error. | godmode.rs:179-193 |
| Credits | The shop hook is not put in (logged). Finding credits by watching fails with the error. | credits.rs:86-124, 272-274 |
| The 9 weapon switches | All nine are prepared together. One failed search marks **all nine** broken for the session. | weapon.rs:249-260, 491-512 |
| NPC perception ("Undetected") | Marked bad for the session. Switching on returns the error. | unseen.rs:188-207 |
| No NVG Overlay, Stand While Flying | Marked broken for the session. | nvg.rs:95, airwalk.rs:127 |
| Bone ESP full-body poses | The pose catch stays off. ESP draws the bone object's bones or boxes. | vkdraw.rs:1708-1728 |

### How the mod checks before it reads or writes

1. **Readable first.** `memory::readable` asks Windows (VirtualQuery) whether every page is committed and not marked no-access or guard (memory.rs:62-78).
2. **Reads and writes go through the kernel.** The mod uses ReadProcessMemory and WriteProcessMemory on its own process. If memory is freed between the check and the copy, the call fails instead of crashing the game. A plain copy crashed the game (memory.rs:81-99). Writes also refuse pages that are not writable (memory.rs:103-124).
3. **Guards.** An object's first 8 bytes are its class tag. A held value (a "freeze") remembers the tag it saw. Every 15 ms the freeze worker reads the tag again and drops the hold if it changed (freeze.rs:49-61, 87-111). Example: God Mode holds `char+0x378 = 1` with the guard (character, its tag). After a world change the tag changes, so the hold stops instead of writing into freed memory (godmode.rs:183, 205-207).
4. **Owner check.** An object counts as the player's only when its +8 is the player's body (entities.rs:152-159). Example: God Mode refuses a caught hit on someone else (godmode.rs:194-199).
5. **Read again right before writing.**
   - Gear score reads the slot again just before the write (gearscore.rs:115-120).
   - "Set all owned" checks each item's class and old score again just before writing it (gearscore.rs:211-213).
   - Credits reads the old value, writes, reads it back, and puts the old value back if the check fails (credits.rs:353-373).
6. **Code changes** check the original bytes first. Each change swaps one aligned 8-byte word in a single step. If the word is not what the mod expects, the mod puts back what it found and logs "left alone" (memory.rs:126-153; section 3).

---

## 2. Reference tables

Columns: **Where** is `exe+0x...` (static), `pattern "name"` (searched at runtime), or `obj+0x...` (a field of an object). **Use** says whether the mod reads it, writes it, hooks it or only compares with it.

### 2.1 Player body and position

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Body global | `exe+0x63EBEB0` (static) | u64 pointer | Points at the player's physics body ("body"). It survives death by pointing at the new body. One game function writes it (exe+0x28c1090) and zeroes it first, so a reader can see 0. | Read by fly, teleport, the NPC scan, ESP and the gear hold. Under 0x10000 means no body. | **Confirmed** | fly.rs:13-14, 66-75; vkdraw.rs:40; Step 2 (found by shaped scan, bisect and pointer scan); ESP reference |
| Position | `body+0x20` | 3 x f64 (x, y, z) | The player's position in the world, or an NPC's position in an NPC's body. The game's controller pulls it back, so moves must be held. | Read for distances, aim and ESP. Written by fly (every 4 ms), teleport (held for 30 writes), and Bring and Yeet (held, guarded). | **Confirmed** | fly.rs:15, 286-292; entities.rs:28, 295-317; tp.rs:17; ESP reference |
| Transform setter | pattern "fly through setter" `48 89 5C 24 08 48 89 6C 24 10 48 89 74 24 18 48 89 7C 24 20 41 56 48 83 EC 20 41 0F B6 E8 C6 81 76 01 00 00 00 48 8B F9 E8` | code (function start) | The body's vtable slot +0x98: sets a body's place and turn (copies 0x50 bytes from rdx to body+0x20 through exe+0x293f80), then tells the rest of the game (exe+0x119eb80; sets bit 0x20000 at body+0xB0). Exe+0x11db540 in that build. For the player's it runs every frame from exe+0xc73721 -> exe+0x293d90 -> exe+0x293850 (a vcall), with the new place on the stack (0x4a8 above rsp). The only writer of the player's z in a 3 s watch. | Hooked (patch 3.19). | **Experimental** | noclip.rs; found live (research DLL write watch, then regs spots up the calls) |
| Physics copy read | pattern "fly through copy" `48 8B F9 0F 57 D2 48 8B 89 40 04 00 00 48 8B DA 0F 10 59 50 0F 10 41 20 0F 10 49 30`, hooked at +0x10 | code | In the per-frame update (exe+0xc73620 in that build): `this+0x440` is the player's physics object, `this+0x10` the player's body. The object's +0x20..+0x5F look like a transform; +0x50 is the player's place as 3 floats from the world origin `[[object+8]+0xa40]+0x30` (3 doubles, all three added, exe+0xd00e10). The game builds the player's place from it each frame. In water the game moves it itself every frame. | Hooked (patch 3.20); Fly writes object+0x50 while the player is held. | **Experimental** | noclip.rs; found (research DLL regs spots, offline read) |
| Body class tag | `body+0x0` | u64 | The body object's first word. It changes when the memory is freed or reused. | Guard for a held move (move_body). The player's tag is also read. | **Experimental** | entities.rs:43-54, 309-311 |
| Body facing | `body+0x40` | 3 rows x 4 f32 | A turn about the up axis. | Read to place bone poses in the world. | **Experimental** | vkdraw.rs:1472-1473, 1797-1821; ESP reference ("data only": right on 3-4 NPCs; yaw-only vs full turn untested) |
| Dead bit | `body+0x108`, bit `0x4000` | u32 flags | Set once an NPC is dead (the player's saves: 4 kills flipped it, and no living NPC had it). | Read: ESP can hide dead NPCs. | **Experimental** | vkdraw.rs:48-51, 1351; ESP reference ("data only"; takedowns, revives and despawns unchecked) |

### 2.2 Character object

Every NPC and player has a character object. Its first word is its class and its +8 is its body. God Mode's "entity" is the player's character object.

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| NPC character class | `exe+0x49BFD70` (static, compared as a value) | address | The class of the per-NPC character object. One per NPC. Players have none. | NPC scan: a heap word equal to this, followed by a body pointer, is one NPC. | **Confirmed** | entities.rs:133-135, 246-262; Entities |
| Player character class | `exe+0x4A4C5F0` (static, compared) | address | The same kind of object for players, the player and others. | Finds the player's character (`my_character`). Other players are shown in ESP. | **Confirmed** | entities.rs:136-138, 161-191; ESP reference (God row: live + code) |
| Class tag | `char+0x0` | u64 | Equals one of the two class addresses above. | Guard for God Mode's hold. ESP's ghost-box check needs the tag and +8 unchanged. God Off checks it before writing 0. | **Confirmed** | godmode.rs:183, 207, 216-240; vkdraw.rs:1331-1343 |
| Body pointer | `char+0x8` | u64 pointer | The body this character drives. | Owner check (the player's only when it equals the player's body), the NPC list, and the weapon hooks' "is this the player". | **Confirmed** | entities.rs:152-159; weapon.rs:329-339; ESP reference ("NPC marker +8 -> body: live") |
| Immune byte | `char+0x378` | u8, 0 or 1 | While it is 1, damage to this character is skipped. Any other value means "not a character". | God Mode writes 1 and holds it (every 15 ms, guarded). Off writes 0 only if the tag is unchanged and it is the player's. | **Confirmed** | godmode.rs:13-14, 156-167, 179-240; God mode hunt; ESP reference |
| Network part | `char+0x190` | u64 pointer | The player "network part" (the CE table's name). A gun item's +0x88 points at its owner's one. | Copied into the weapon code block every second. Infinite Mag, Infinite Reserve and Fire Rate compare against it. | **Confirmed** (indirectly) | weapon.rs:27-29, 329-339; Reserve ammo; Fire rate. ESP reference still calls it untested. |
| State manager | `char+0x310` | u64 pointer | The player's character's state manager. The fall object's +0x38 points at it. | Written into the fall hook's slot while Stand While Flying and Fly are on. | **Confirmed** | airwalk.rs:5-6, 27-28, 151-162; Noclip combat and early inject |
| Side | `char+0x60 -> +0x160 -> +0x104` | i32, 0 to 63 | 3 = enemy, 0 = friendly (the player's saves plus 42 NPCs in Erewhon). | Read for ESP colours. | **Experimental** | vkdraw.rs:2019-2027; ESP reference ("data only", 55 NPCs) |
| Bone object path | `char+0x60 -> +0x230` (ESP also searches two levels down) | u64 pointer | The bone object (2.12). | Read by ESP bones. | **Experimental** | vkdraw.rs:1466-1467, 1519-1553; ESP reference (only about 2% of body-frames have it) |

**Character state while flying (Stand While Flying)**

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Fall function | pattern "airwalk fall" `48 89 5C 24 18 55 48 8D AC 24 60 FF FF FF 48 81 EC A0 01 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 90 00 00 00 48 8B D9 E8` | code (function start) | Runs on a character's fall object when it starts to fall. It asks the state manager for state 0x2D (falling). Exe+0x353d0c0 in that build. Read offline night: it is a part of the behaviour tree; on its main path it also sets the movement mode (4 or 5, vcall +0x510 on the component found by hash 0xDF715532) and blackboard value 0xF2. | Hooked (patch 3.15). | **Confirmed** | airwalk.rs:4-6, 18-19; Noclip combat and early inject |
| Fall object's manager | `fallobj+0x38` | u64 pointer | The character's state manager. | Compared with the player's in the hook. | **Confirmed** | airwalk.rs:5-6, 25-26 |
| In-air part | pattern "airwalk in-air" `48 8B F9 C6 44 24 20 01 48 8B 49 08 4C 8D 44 24 20 BA F1 00 00 00 48 8B 49 38 E8`, hooked at +3 | code | Part of the behaviour tree. When the player leaves the ground it stores true into blackboard value 0xF1, and while that is true there is no aiming and no shooting. The part is at exe+0x3539ac0 in that build. | Hooked (patch 3.16). | **Confirmed** | airwalk.rs:7-12, 20-24; Noclip combat and early inject |
| Tree owner, character | `part+0x8 -> owner+0xA8` | u64 -> u64 | The tree's owner, then its character. | Compared with the player's character in the hook. | **Confirmed** | airwalk.rs:10-11, 100-105 |

### 2.3 Camera

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Camera global | `exe+0x62F24B0` (static) | u64 pointer | The camera object. One camera function writes the fields below together. The code calls it "player root"; the ESP reference says it is the camera. | Read every frame by ESP, and by aim and fly. | **Confirmed** | entities.rs:319-320; fly.rs:246-247; vkdraw.rs:35-39; ESP reference (live + code) |
| Camera position | `cam+0x1C0` | 3 x f64 | Where the camera is. | ESP projection. | **Confirmed** | vkdraw.rs:35-36, 41 |
| Right | `cam+0x1E0` | 3 x f32, unit length | The camera's right direction. | Fly's sideways move; ESP. | **Confirmed** | fly.rs:248, 250-261; vkdraw.rs:42; Fly and hotkeys |
| Forward | `cam+0x1F0` | 3 x f32, unit length | The camera's forward direction. | Aim (Target > Pick, within 12 degrees, length checked to be 0.5-1.5), fly direction, ESP. | **Confirmed** | entities.rs:321, 334-345, 366-372; fly.rs:249; vkdraw.rs:43; Fly and hotkeys |
| Up | `cam+0x200` | 3 x f32 | The camera's up direction. | ESP. | **Confirmed** | vkdraw.rs:44 |
| Vertical field of view | `cam+0x1B0` | f32, radians | The code says "a guess that fits: 1.13 = 65 degrees". The ESP reference rates it live + code. | ESP. A value outside 0.01-2.5 falls back to 65 degrees. | **Confirmed** | vkdraw.rs:36-37, 45, 1294-1295; ESP reference |
| HUD distance scale | constant 1.5 | f64 | The game's HUD distances are 1.5 x world units. | ESP distance labels. | **Experimental** | vkdraw.rs:52-54; ESP reference ("3 of 4 pairs give 1.47-1.48") |

### 2.4 Entities and the NPC list

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Blocks that hold characters | private read-write memory, blocks of 512 KB or less; thread stacks and graphics memory skipped | memory regions | NPC character objects sit in small blocks, 64-256 KB. | Read in 8 MB chunks through the kernel. A full rescan every 10 s; otherwise only the blocks that held bodies last time. | **Confirmed** | entities.rs:14-17, 67-76, 97-131; Entities; ESP reference (some hits sit right at the 512 KB cap) |
| Character match | a word equal to a class address (2.2), with the next word a body | 2 x u64 | One NPC or player character. | Builds the NPC list. The body must be at least 0x10000, not the player's, and its position finite and under 1e5. | **Confirmed** | entities.rs:56-64, 194-293, 246-262; Entities |

### 2.5 Damage and immunity (God Mode)

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Damage check | pattern "god damage check" `48 8B ?? 48 8B ?? FF 90 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? ?? ?? 44 38 A7` | code | The check the game runs before it damages a character. The virtual call in it reads `char+0x378` (ESP reference: vf+0xa8 = `movzx eax,[rcx+0x378]`). Taken from the Recon Cheese table; the CE tables list God at exe+360a34e. | **Never patched.** A hardware execute watch on its first byte catches the registers, and stops at the first hit. It runs at inject in the background and when God Mode has to wait for a hit. | **Confirmed** | godmode.rs:1-4, 11-12, 88-126, 147-152, 189-193; God mode hunt; CE tables reference |
| Damaged character | register `rdi` (index 5) at the damage check | u64 | The character being damaged. That can be anyone, including an NPC the player hits. | Taken from the caught registers and accepted only if the owner check says it is the player's. | **Confirmed** | godmode.rs:15-16, 194-199; God mode hunt. The owner check itself is not tested in game. |

The watch itself: debug register DR0 on every game thread, with length 0 meaning "on execute". A handler records 16 registers and the return address, then everything is cleared (hwbp.rs:233-272, 276-395). The research tool uses DR1 and shares a lock with it (hwbp.rs:4-5, 48).

### 2.6 Weapon

The functions below are taken from community CE tables (Sunbeam's, weapon.rs:3) and checked against the exe. All of them are hooked through one shared code block (patches 3.8-3.14).

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Recoil function | pattern "weapon recoil" `48 8B C4 55 41 55 48 81 EC ?? ?? ?? ?? 49 83 78 ?? 00 4D` | code, function start (first 6 bytes are `mov rax, rsp; push rbp; push r13`) | Kicks a character's aim up after a shot. The character is at `[rcx+8]`. CE table spot exe+36ffe20. | Skipped for the player (patch 3.8). | **Experimental** | weapon.rs:20-21; CE tables reference; CE table features |
| Spread function | pattern "weapon accuracy" `40 55 56 41 56 48 8D AC 24 ?? ?? FF FF 48 81 EC ?? ?? ?? ?? 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 ?? ?? ?? ?? 80 7A` | code, function start (5 bytes: `push rbp; push rsi; push r14`) | Spreads a character's shots. The character is in `rax`. CE spot exe+3650cd0. | Skipped for the player. | **Experimental** | weapon.rs:22-23; CE tables reference |
| Shake function | pattern "weapon shake" `48 8B C4 41 54 41 55 48 81 EC ?? ?? ?? ?? 4D 8B 60 ?? 4C 8B E9 4D 85 E4 0F 84` | code, function start (7 bytes) | Screen shake. The character is at `[rcx+8]`. CE spot exe+37028e0. | Skipped for the player. | **Experimental** | weapon.rs:32-33; CE tables reference |
| Scoped sway function | pattern "weapon sway" `48 89 5C 24 ?? 57 48 83 EC ?? 49 83 78 ?? 00 49 8B F8 48 8B D9 0F 84 ?? ?? ?? ?? 49 83 78 ?? 00 0F 84 ?? ?? ?? ?? 48` | code, function start (5 bytes) | Sway while aiming down the scope. The character is at `[rcx+8]`. CE spot exe+37012c0. | Skipped for the player. | **Experimental** | weapon.rs:34-35; CE tables reference |
| Stamina check | pattern "weapon stamina" `80 BF F1 01 00 00 00 75 ?? F3 0F 10 8F ?? ?? ?? ?? 0F 57 C0 0F 2F C8 76` | code, a 7-byte `cmp byte [rdi+0x1F1], 0` | In the stamina update. `rdi` is a stamina part. CE spot exe+36295e0. | Hooked (patch 3.13). | **Confirmed** | weapon.rs:36-38; CE tables reference ("the player confirmed stamina") |
| Stamina part's owner | `part+0x8` | u64 pointer | The body the stamina part belongs to. | Compared with the player's body. | **Confirmed** | weapon.rs:36-37, 130 |
| Free-stamina byte | `part+0x1F1` | u8 | While set, the game does not use stamina. | Set to 1 for the player while on; cleared on Off after a readability check. | **Confirmed** | weapon.rs:36-38, 131, 360-367 |
| Damage step | pattern "weapon damage" `83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8` | code, a 7-byte `cmp dword [rdi+0xA8], 6` | Runs **when a shot object is filled from the gun's stats** (`0x3670990`, at fire time), not at the hit. `xmm0` is the damage and `r14` is the shooter's body. CE spot exe+3670c1a. | Hooked: the player's hits are multiplied (patch 3.14). | **Experimental** | weapon.rs:39-42, 140-152, 402-411; Checklist 3 Nov (Damage Multiplier unchecked) |
| `[rdi+0xA8] == 6` | inside the damage pattern | i32 | The shot's ammo type (`rdi` is the shot object); type 6 cuts one damage field to 1/8. Live: VHSD2 7, TAC50 9, PX4 1. | Only matched as pattern bytes. | **Experimental** | weapon.rs:42; re\shot-trace.md Q1 |
| Shot build entry | pattern "silent aim shot build" `4C 8B DC 53 56 41 54 41 57 48 81 EC 08` (13 bytes, unique) | code, function start: `mov r11, rsp; push rbx; push rsi; push r12; push r15; sub rsp, 0x108` | `0x3652430(shot, float4* origin, float4* direction)`: every shot's origin and direction go in here (fire time from `0x365af00` and `0x365b490`, re-aim from `0x3693250`). The player's VHSD2, TAC50 and PX4 all build shot objects. Exe+0x3652430 in that build. Saved in `Bin\Cache\Offsets.bin` like every pattern. | Hooked for the player's shots: Silent Aim (patch 3.21). | **Experimental** (path live, hook not tested) | silentaim.rs; re\shot-trace.md |
| Shot object | 0x550 bytes, vtable `exe+0x4A590A8` | object | One per bullet. `+0x90` shooter handle (entity when bit 31 of its `+0xC` is set), `+0xA8` ammo type, `+0xAC`/`+0xB0` damage, `+0xC` speed, `+0x130` unit direction, `+0x140` origin, `+0xF0` homing target ID (0 on every live shot seen). | Silent Aim reads `+0x90` in its cave. With Magic Bullets, No Range Loss or Instant Hit on, the cave also writes `+0x378` (Magic Bullets, steered shots only), `+0x158` (No Range Loss) and `+0xC` and `+0x8` (Instant Hit) after the game filled the shot (silentaim.rs `feature_writes`). | **Confirmed** (fields seen live) | re\ |
| Entity type word | `entity+0x108` | u32 | `& 7`: 1 player (the player's 0xD09, others 0xE09), 2 NPC, 5 drone (0x1115 ground, 0x110D flying), 6 drone/turret part, 7 planted turret. Bit 0x4000 dead. | ESP and Silent Aim: 2 and 5 can be targets, 1 never. | **Confirmed** | entities.rs; re\ Q1 |
| Fire countdown | pattern "weapon fire rate" `40 53 48 83 EC 20 48 8B D9 48 8B 49 10 48 8B 49 48 E8 ?? ?? ?? ?? 48 8B 43 10 F3 0F 58 40 38 F3 0F 11 43 18`; the call is at +17 | code | Sets the shot countdown. The gun is `[[rcx+0x10]+0x48]`. The call returns the wait in seconds (60 / RPM) in `xmm0`. Then the countdown at `+0x18` = wait + `[[rbx+0x10]+0x38]`. Exe+0x36b2380 in that build. | Only the call's 4-byte target is changed (patch 3.12). | **Confirmed** | weapon.rs:51-57, 194-217, 454-462; Fire rate |
| Gun's handle | `gun+0xC8` | u64 pointer | A handle. Its first word is the gun item while the handle's i32 at +0xC is below 0 (the game reads it the same way). | Followed in the fire-rate code. | **Confirmed** | weapon.rs:189-193, 203-208 |
| Item's owner network part | `item+0x88` | u64 pointer | The owner's network part (compare `char+0x190`). | Compared in Infinite Mag, Infinite Reserve and Fire Rate. | **Confirmed** | weapon.rs:30-31; Reserve ammo ("db: off.weapon_item_owner_net, verified") |

### 2.7 Ammo

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| "May fire without a round" | pattern "weapon ammo" `48 89 5C 24 ?? 57 48 83 EC ?? 8B DA 48 8B F9 E8 ?? ?? ?? ?? 83 78 ?? 02 0F 84` | code, function start (5 bytes) | The gun is `rcx`. Returning 1 means the shot uses no round. CE spot exe+213a530. | For the player's guns it returns 1 at once: Infinite Mag (patch 3.10). | **Experimental** | weapon.rs:24-26; CE tables reference |
| Reload, mag load call | pattern "weapon reserve" `0F B7 F3 66 45 85 F6 74 09 66 44 3B F3 66 41 0F 46 F6 0F B7 D6 44 8B C7 48 8B CD E8 ?? ?? ?? ?? 66 2B DE 48 8D 4C 24 20 66 03 C3 0F B7 D0`; the call is at +27 | code | In the reload, `bx` is the reserve, `si` the rounds taken from it and `rbp` the gun item (class exe+0x4830490). The call loads the mag and returns `ax`. The new reserve is reserve - si + ax. Exe+0x216171c in that build. | Only the call's 4-byte target is changed: Infinite Reserve (patch 3.11). | **Confirmed** | weapon.rs:43-50, 154-187; Reserve ammo |

### 2.8 Gear score

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Slot table | `exe+0x63EB5D0` (static) | u64 pointer | The player's equipped slots. | Read. | **Confirmed** | gearscore.rs:17-20, 65-82; Owned gear score ("verified"); CE tables reference ("store equipment global") |
| Slot records | `slots+0x328` | u64 pointer | An array of records, 0x20 bytes each. In a record, +0 is the slot and +8 is a handle. | Read (at most 32). | **Confirmed** | gearscore.rs:17-19, 69-75; Owned gear score |
| Record count | `slots+0x332` | u16 | How many records there are. | Read. | **Confirmed** | gearscore.rs:17, 69-70 |
| Slot id | `slot+0x8` | u32 | `0x4001E65F` Primary, `E660` Secondary, `E661` Pistol, `E662` Head, `E664` Hands, `E663` Vest, `E665` Pants, `E666` Shoes. | Tells which row a record is. | **Confirmed** | gearscore.rs:31-32 (matched live by the player's swaps); Owned gear score |
| Slot weight | `slot+0x3C` | f32 | Weights 25, 25, 10 (weapons) and 10, 5, 10, 10, 5 (gear), in the order above. | **Not read**: the weights are written into the code. | **Confirmed** | gearscore.rs:33-34; Owned gear score |
| Handle to item | `handle+0xC` (i32) and `[handle]` | i32, u64 | While the i32 at +0xC is below 0, the handle's first word is the item (the game's own lookup is exe+0x258db40). | Read. | **Confirmed** | gearscore.rs:17-19, 72-75; Owned gear score |
| Weapon item class | `exe+0x4894BD0` (static, compared) | address | The class of weapon items. | `is_item` check, and the owned-items scan. | **Confirmed** | gearscore.rs:23, 60-62, 221-248; Owned gear score |
| Gear item class | `exe+0x47B5130` (static, compared) | address | The class of gear items. | The same. | **Confirmed** | gearscore.rs:24; Owned gear score (scan found 762 weapon and 26 gear objects) |
| Item score | `item+0x7C` | i32 | The item's gear score. The stash, the slots and the player's gear level all read it here. It stays after fast travel and relog. | Read (0 to 100000). Written per slot, or for every owned item, capped at 1000. | **Confirmed** | gearscore.rs:1-4, 25-27, 84-86, 115-120, 194-217; Owned gear score |
| Gear level shown | `[exe+0x63A0D20]+0x48C` | i32 | The player's gear level as the game shows it. The game works it out again only on a swap (exe+0x2733ee0). | Read when a slot is empty. | **Experimental** | gearscore.rs:21-22, 93-113 |
| Drop gear score | `[exe+0x63A0D20]+0x490` | u32 | The gear score given to new drops. Its gear level is the i32 before it. | Held 10 times a second while set. It is written back when the game changes it. | **Experimental** | menu.rs:534-575; CE tables reference ("confirmed by two tables"; "Nothing here was tried live yet") |

### 2.9 Credits

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Credits read function | pattern "credits shop read" `48 8B 01 33 D2 4C 8B C1`; the function starts 5 bytes before the hit, with `48 89 5C 24 08` | code | The one function the game reads scrambled numbers through (credits, and ammo too per the Fire rate note). Exe+0x1a72150 in that build. The table's full pattern stopped matching after the Oct 2026 patch; these core bytes match once. | (a) A hook at the function start (patch 3.1). (b) A hardware execute watch on the hit, keyed by `rcx`, for "Find credits" and "Pick Other Points". | **Confirmed** (read and decode); the hook is **Experimental** (patch 3.1) | credits.rs:18-22, 94-124, 272-320; Credits and keys |
| Credits object | register `rcx` (index 2) in the read function | u64 | The object being read. | Kept in the hook's ring, or taken from the watch's registers. | **Confirmed** | credits.rs:30, 64-84, 306-312 |
| The player's credits caller | `exe+0x273990E` (static, a return address) | address | The player's own credits are read from this caller. The CE table's caller (exe+2B7FFC7) is older. | Compared with the return address, in the hook and in the watch. | **Confirmed** | credits.rs:33, 287-311; Credits and keys; CE tables reference |
| Key pointers | `obj+0x0`, `+0x8`, `+0x10`, `+0x18` | 4 x u64 pointers | One byte behind each pointer builds the key: bit n of byte k goes to bit 4n+k (community table v1.5). | Read; each pointer must be in 0x10000-0x7FFFFFFF0000. | **Confirmed** | credits.rs:9-11, 176-200; Credits and keys |
| Stored balance | `obj+0x20` | u32 | balance = stored XOR key. | Read and written (value XOR key), capped at 0x7FFFFFFF. The value is read back, and the old one is put back on a mismatch. Lists show only objects under 100,000,000. Pick Other Points lists every value under that, 0 too, highest first. | **Confirmed** | credits.rs:31, 35, 157, 202-206, 326, 338-343, 353-373; Credits and keys (balance 992,030,636 matched; +1 showed in the shop) |

### 2.10 NVG display

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| NVG switch-on store | pattern "nvg switch-on" `83 F8 01 75 0C E8 ?? ?? ?? ?? C6 87 99 02 00 00 01 E8`; the 1 is at +16 | code | `mov byte [rdi+0x299], 1`, run when NVG turns on. It is at exe+0x214d7a9 in that build, and the patched byte is exe+0x214d7af. | Patched (3.2). | **Confirmed** | nvg.rs:1-5, 15-17, 55-66; NVG |
| Overlay flag | `display+0x299` | u8 | 1 shows the green NVG screen. With 0 and NVG on, the view is normal and the goggles stay down. | Changed only through the code patches. | **Confirmed** | nvg.rs:1-5; NVG |
| Loading-screen store | pattern "nvg loading" `41 88 85 0C 12 00 00 45 88 BD 99 02 00 00 48 83 C4 38`; the store is at +7 | code | `mov [r13+0x299], r15b`: a loading-screen teleport sets the display up again. `r13` = the display (seen live). Exe+0x11bb332 in that build. | Hooked (3.3). | **Experimental** | nvg.rs:7-9, 18-21, 68-85; NVG (not proven live); Aurora review 4 Nov (item 7) |
| `[r13+0x120C]` | inside the loading pattern | u8 | Unknown. | Only matched as pattern bytes. | **Unknown** | nvg.rs:18-19 |

### 2.11 NPC perception (menu name "Undetected")

This group is only about the game's enemy NPC AI: whether its soldiers and drones see the player and fill their orange alert meter. It is a single-player difficulty switch. It is not a bypass, not anti-cheat related, and it touches no network, account or logging code.

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| Sight check load | pattern "unseen sight" `F3 44 0F 10 79 3C 48 85 D2 74 17 80 BA D0 00 00 00 00` (from Spartan Mod v0.37) | code (6-byte `movss xmm15, [rcx+0x3c]`) | Loads a sight factor that scales what is seen. Exe+0x359e4bb in that build. | Hooked: the factor is multiplied by 0 (3.4). | **Confirmed** (works at range, **not** at 1-10 m) | unseen.rs:9-13, 32-36, 150-176; Detection investigation (tested live) |
| Sight factor | `[rcx+0x3C]` at the sight check | f32 | The sight factor above. The code says "every NPC's eyes"; round 2 of the note says this function is the player's and writes the player's visibility (0 to 1) that enemy sensors scale by. | Not read; changed only in registers by the hook. | **Confirmed** (the effect); owner disputed | unseen.rs:10-13; Detection investigation |
| `[rdx+0xD0]` byte | inside the sight pattern | u8 | Unknown. | Only matched as pattern bytes. | **Unknown** | unseen.rs:33 |
| Alert meter | `aware+0x22C` | f32, 0 to 100 | The orange meter. Its rise and fall matched the on-screen meter. The code says each NPC's awareness object; the note says one object, owned by the player's character (`[obj+8]` = the player's body). | Changed only through patches 3.5 and 3.6. | **Confirmed** | unseen.rs:15-17; Detection investigation |
| Meter target | `aware+0x250` | f32 | The target of the meter (100 while noticing, then 0). | Matched as pattern bytes (`comiss xmm0, [rcx+0x250]`). | **Confirmed** | unseen.rs:37-39; Detection investigation ("Confirmed" list) |
| Meter fill | pattern "unseen meter fill" `F3 0F 10 81 2C 02 00 00 0F 2F 81 50 02 00 00 0F 83 ?? ?? ?? ?? 48 8B 81 08 02 00 00`; `jae` at +15 | code | The `jae` past the fill, in exe+0x3634460. | Patched (3.5). | **Experimental** (meter held at 0, but detection did not stop) | unseen.rs:15-18, 37-39, 69-89; Detection investigation |
| Meter clamp | pattern "unseen meter clamp" `F3 0F 10 83 2C 02 00 00 F3 41 0F 5F C0 F3 0F 5D 05 ?? ?? ?? ?? F3 0F 11 83 2C 02 00 00`; `maxss` at +8 | code | The 0-100 clamp. Xmm8 is 0 there. Exe+0x363463a in that build. | Patched (3.6). | **Experimental** (same) | unseen.rs:18-21, 50-52, 91-106; Detection investigation |
| Second meter | `aware+0x228` | f32, 0 to 100 | A twin meter. It filled in under 2 s when +0x22C was held at 0. | Set to 0 after each run of its function (3.7). | **Confirmed** (it exists and fills); holding it did not stop detection | unseen.rs:23-26; Detection investigation |
| Second meter function | pattern "unseen second meter" `40 53 48 81 EC A0 00 00 00 0F 29 B4 24 90 00 00 00 44 0F 29 44 24 70 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 44 24 60 48 8B 81 08 02 00 00 0F 28 F1 44 8B 81 20 02 00 00` | code, function start | Updates +0x228. Exe+0x363a6e0 in that build. | Hooked (3.7). | **Experimental** | unseen.rs:23-26, 54-55, 108-148 |
| `aware+0x208` | inside the fill and second-meter patterns | u64 | Unknown. | Only matched as pattern bytes. | **Unknown** | unseen.rs:39, 55 |
| `aware+0x220` | inside the second-meter pattern | u32 | Unknown. | Only matched as pattern bytes. | **Unknown** | unseen.rs:55 |

### 2.12 Rendering (Vulkan hooks, bones, poses)

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| vkQueuePresentKHR | found at runtime: a throwaway Vulkan instance and device, then vkGetDeviceProcAddr | function | The game's "show this frame" call. In Auto mode this is the first function in the chain: the Steam overlay layer if it is on, else the GPU driver. | Detoured (retour). ESP and the menu are copied into the frame just before it goes out. | **Confirmed** | vkdraw.rs:1-4, 451-541; Stand menu 2 Nov (menu proved live) |
| vkAcquireNextImageKHR | same | function | Gets the next screen image. | Detoured. | **Confirmed** | vkdraw.rs:506-508, 518-541 |
| vkCreateSwapchainKHR | same | function | The game makes its screen images here. The game's own usage is TRANSFER_SRC and COLOR_ATTACHMENT. | Detoured: it adds TRANSFER_DST, so the mod may copy into them. Only such swapchains are drawn into. | **Experimental** | vkdraw.rs:181-184, 562-617; Noclip combat and early inject ("built, not tested") |
| Driver hook mode | `Hook: Driver` in the Launchpad's Settings.txt; the probe briefly sets `VK_LOADER_LAYERS_DISABLE=~implicit~` and `DISABLE_VK_LAYER_VALVE_steam_overlay_1=1` | setting | Hooks the driver's own functions, past every layer. | Read once at inject. | **Experimental** | vkdraw.rs:248-274, 421-447; Checklist 3 Nov (unchecked) |
| Bone object | `bones+0x10` = body; `bones+0x308` = list | u64; 44 x 0x30 | 44 bone poses, each position x y z w, rotation quaternion, scale. They are in body space: x right, y forward, z up, metres from the feet. About 20 are in use. | ESP reads the first 18 and joins 15 pairs. | **Confirmed** (on close NPCs) | vkdraw.rs:1465-1477, 1519-1586; Bone ESP ("Proved live"); ESP reference (found on only about 2% of body-frames) |
| Bone index map | indexes into the 44-list: pelvis 0, spine 9, chest 10, head 15, shoulders 13 14, elbows 7 8, hands 3 4, hips 16 17, knees 5 6, ankles 1 2, toes 11 12 | list | Taken from one NPC's pose. | Used to draw lines. | **Confirmed** (one NPC) | vkdraw.rs:1474-1477; Bone ESP; ESP reference ("The bone map came from one NPC") |
| Pose function | pattern "pose function" `48 89 5C 24 08 57 48 83 EC 30 49 8B 80 40 02 00 00 49 8B B8 48 02 00 00 4D 8B 81 48 02 00 00` | code, function start | Writes full-body poses. `r8` is the pose object. Exe+0xacfc10 in that build. Found by a hardware watch on a pose list. | Hooked (3.17). | **Confirmed** (the function and `r8`) | vkdraw.rs:1588-1598, 1708-1728; Bone ESP; ESP reference (code level) |
| Pose object position | `pose+0x0` | 3 x f64 | Its world position, the same as its body's (to 3 cm once; matched within 0.5). | Matches pose objects to bodies. | **Experimental** | vkdraw.rs:1591-1602; ESP reference ("Pose -> body matching... within 0.5") |
| Pose list | `pose+0x248` | u64 pointer to entries of 0x20 | One entry per rig bone (431 and 456 on the NPCs checked), each position x y z w and rotation, in body space. | Read (640 bytes at most per rig). The bone order is worked out from bone lengths. | **Confirmed** (the list); the length-based layout is **Experimental** | vkdraw.rs:1591-1605, 1660-1698; Bone ESP; ESP reference |
| Animation level table | pattern "lod table" (`LOD_TABLE` in vkdraw/bonetrace.rs, 87 bytes, ends `45 85 C0`) | code in the scene init; tables of 6 x f32 | The game's animation level cut-offs (A) and their squares (B), plus the config count and values it fills them from (value i goes to level 5-i). Vulkan: code rva 0x11752a3, count exe+0x62f6cb8, A exe+0x5353a80, B exe+0x5355418. DX11: code rva 0x11547c3, count exe+0x5a6a1f8, A exe+0x5335798, B exe+0x5337380. One hit in each exe. | Read only, for the bone trace log line. The addresses come from the code's rip offsets; four more stores must land at A+0x10, A+0x14, B+0x10, B+0x14. | **Experimental** (pattern and addresses checked offline, not seen live yet) | vkdraw/bonetrace.rs |

### 2.13 Input (DirectInput 8)

These are the Windows DirectInput layout, not the game's own data. They do not change with game builds.

| Name | Where | Size/type | What it is | How the mod uses it | Status | Source |
|---|---|---|---|---|---|---|
| DirectInput8Create | export of `dinput8.dll` | function | Makes a throwaway DirectInput object (IID `BF798031-...` or `BF798030-...`) to read the method tables. | Called once at install. | **Confirmed** | dinput.rs:130-133, 253-266 |
| CreateDevice | IDirectInput8 table slot 3 | function | Makes a throwaway keyboard device (`GUID_SysKeyboard`). | Called once. | **Confirmed** | dinput.rs:132, 269 |
| GetDeviceState | device table slot 9 | function | Keyboard state is 256 bytes (one per key). Mouse state is 16 or 20 bytes. | Detoured: keys bound to the mod, the menu keys while it is open, and Fly's down key while flying are removed. The mouse reads as still while the command box is open, and while the menu is open with the game in front. | **Confirmed** | dinput.rs:188-210, 274; Stand menu 2 Nov |
| GetDeviceData | device table slot 10 | function | Buffered key and mouse events. Mouse buttons are offsets 12 to 19. | Detoured: the same keys are dropped. Mouse presses are dropped while typing, and mouse moves, presses and wheel while the menu has the mouse; releases pass. | **Confirmed** | dinput.rs:212-237, 274; Stand menu 2 Nov |
| GetCapabilities | device table slot 3 | function | Device type: `0x13` is a keyboard. | Tells keyboards from mice. | **Confirmed** | dinput.rs:133, 171-179, 274 |
| Release | slot 2 of both tables | function | Frees the throwaway objects. | Called after reading the tables. | **Confirmed** | dinput.rs:275-278 |
| Menu keys | DirectInput key codes `1C 9C 0E C8 D0 CB CD 36 9D 48 50 4B 4D 4C 52 18 D3` | u8 list | Enter (both), Backspace, the arrows, Right Shift, Right Ctrl, number pad 8 2 4 6 5 0, O and Delete. | Kept from the game while the menu is open. | **Confirmed** | dinput.rs:26-29, 113-128 |
| Fly down key | key code of the fly "down" key (default Ctrl) | u8 | It is also the game's crouch. | Kept from the game while Fly is on. | **Confirmed** | dinput.rs:84-94, 113-124; Noclip combat and early inject ("works") |

---

## 3. Code patches

### Rules every patch follows

- **One word, one step.** A patch only changes bytes inside one aligned 8-byte code word. The word is swapped with a single atomic store, so no game thread can run a half-written instruction. Before the store the page is made writable, and afterwards its protection is restored and the instruction cache flushed. Only one swap runs at a time (memory.rs:126-153).
- **A jump must fit.** A 5-byte jump (`E9` + 4-byte offset) must not cross a word boundary, so the hook site must be at most 3 bytes into its word. Otherwise the patch is refused (for example airwalk.rs:59-61, nvg.rs:72-74).
- **Checked first.** The original bytes are read and compared before anything is built (for example credits.rs:101, nvg.rs:61 and 69, airwalk.rs:86, posehook.rs:38, unseen.rs:79).
- **The mod's own code** goes into a block it allocates within 2 GB below the exe, so a 4-byte jump reaches it. Blocks are **never freed**, because a game thread may still be inside one after the jump is gone (memory.rs:155-170).
- **Undo.** Off swaps the original word back. If the word no longer holds the mod's value, the mod puts back whatever it found and logs "left alone" (weapon.rs:310-325, airwalk.rs:176-185, nvg.rs:119-128, unseen.rs:240-262, posehook.rs:76-103, credits.rs:127-139).
- **Prepared early, all off.** At inject a warm-up thread finds and builds the patches for NPC perception, weapon, NVG, Stand While Flying and credits (lib.rs:111-118). All are off except the credits hook, so switching on is instant.
- **Unload** turns everything off and puts back every word (server.rs:142-157).

`rel32` below means the 4-byte jump or call offset, which is worked out at runtime.

### 3.1 Credits shop hook (Experimental)

- **Where:** the credits read function's start, which is the "credits shop read" hit minus 5 (exe+0x1a72150 in that build).
- **Original:** `48 89 5C 24 08` (`mov [rsp+8], rbx`). It is checked (credits.rs:101).
- **New:** `E9 rel32`, a jump to a 0x200-byte block. The code sits at +0x100, the player's caller's address at +0, a counter at +8, and a 16-entry ring at +0x10.
- **The mod's code:**
  1. `push rax; push rdx`
  2. Compare the return address on the stack (`[rsp+0x10]`) with exe+0x273990E.
  3. If equal: store `rcx` (the credits object) in the ring and add 1 to the counter.
  4. `pop rdx; pop rax`
  5. Run the original 5 bytes, then `jmp` to function+5.
- **Meaning:** it notes each object the player's own credits caller reads, so a shop opening is seen by itself. The game's numbers are not changed.
- **Undo:** put in once at inject and left on all session. Unload swaps the word back, or leaves it alone if it changed (credits.rs:94-139).
- **Status:** Experimental. "Game ran fine", but nothing tried by the player yet (Menu rebuild and profiles); Checklist 3 Nov is unchecked.

### 3.2 No NVG Overlay: switch-on (Confirmed)

- **Where:** "nvg switch-on" hit + 16, the immediate byte of `C6 87 99 02 00 00 01` (`mov byte [rdi+0x299], 1`). That is exe+0x214d7af in that build.
- **Original:** `01`. It is checked (nvg.rs:61).
- **New:** `00`. Only that byte of the word differs.
- **Meaning:** when NVG turns on, the overlay flag is set to 0 instead of 1. The goggles go down, but there is no green screen. It works from the next time NVG turns on.
- **Undo:** the word is swapped back on Off and at unload (nvg.rs:119-149).

### 3.3 No NVG Overlay: loading screen (Experimental)

- **Where:** "nvg loading" hit + 7 (exe+0x11bb332 in that build).
- **Original:** `45 88 BD 99 02 00 00` (`mov [r13+0x299], r15b`). It is checked (nvg.rs:69).
- **New:** the first 5 bytes become `E9 rel32`. The last 2 original bytes stay but never run.
- **The mod's code (0x40 block):** `41 C6 85 99 02 00 00 00` (`mov byte [r13+0x299], 0`), then `E9 rel32` back to hit + 14.
- **Meaning:** after a loading-screen teleport the overlay flag is stored as 0.
- **Undo:** it is switched together with 3.2.
- **Status:** Experimental. Built 4 Nov, not proven live.

### 3.4 NPC perception: sight (Confirmed at range)

- **Where:** the "unseen sight" hit (exe+0x359e4bb in that build).
- **Original:** `F3 44 0F 10 79 3C` (`movss xmm15, [rcx+0x3c]`).
- **New:** `E9 rel32` over the first 5 bytes. The 6th byte never runs.
- **The mod's code (64 bytes):**
  - `F3 44 0F 10 79 3C` (the same load)
  - `F3 44 0F 59 3D 05 00 00 00` (`mulss xmm15, [rip+5]`)
  - `E9 rel32` back to hit + 6
  - `00 00 00 00` (the factor, 0.0)
- **Meaning:** the sight factor becomes 0, so NPCs do not spot the player at range. It does not help at 1-10 m (Detection investigation).
- **Undo:** the 4 NPC-perception patches (3.4-3.7) go on and off together. If one fails while switching on, the ones already switched are rolled back (unseen.rs:213-262).

### 3.5 NPC perception: meter fill (Experimental)

- **Where:** "unseen meter fill" hit + 15.
- **Original:** `0F 83 rel32` (`jae`). It is checked (unseen.rs:79).
- **New:** `90 E9 rel32` (`nop; jmp`). The end is the same, so the offset is the same.
- **Meaning:** the fill is always skipped, so the +0x22C meter never rises. Draining and the clamp still run.
- **Status:** Experimental. The meter stays at 0, but the player was still spotted (Detection investigation).

### 3.6 NPC perception: meter clamp (Experimental)

- **Where:** "unseen meter clamp" hit + 8.
- **Original:** `F3 41 0F 5F C0` (`maxss xmm0, xmm8`). These bytes are part of the pattern, so they are checked by the search.
- **New:** `41 0F 28 C0 90` (`movaps xmm0, xmm8; nop`).
- **Meaning:** xmm8 is 0 there, so every update leaves the meter at 0.
- **Status:** Experimental, same result as 3.5.

### 3.7 NPC perception: second meter (Experimental)

- **Where:** the "unseen second meter" hit, the function start.
- **Original:** the first 5 bytes, `40 53 48 81 EC` (the start of `push rbx; sub rsp, 0xa0`).
- **New:** `E9 rel32`, a jump to a wrapper.
- **Wrapper:**
  - `53` push rbx
  - `48 83 EC 20` sub rsp, 0x20
  - `48 8B D9` mov rbx, rcx
  - `E8 rel32` call tramp
  - `C7 83 28 02 00 00 00 00 00 00` mov dword [rbx+0x228], 0
  - `48 83 C4 20` add rsp, 0x20
  - `5B` pop rbx
  - `C3` ret
- **Tramp:** the function's first 9 bytes, read live (by the pattern, `40 53 48 81 EC A0 00 00 00`), then `E9` to function + 9.
- **Meaning:** the game's function runs as usual, then the second meter is set to 0.
- **Status:** Experimental. The player was still spotted (Detection investigation).

### 3.8-3.14 Weapon switches

All nine weapon patches share one 0x400-byte block (weapon.rs:58-70):
- **Code:** 0x000 recoil, 0x060 spread, 0x0C0 ammo, 0x120 shake, 0x180 sway, 0x1E0 stamina, 0x280 damage, 0x300 reserve, 0x380 fire rate.
- **Data:**
  - 0x260 the player's body
  - 0x268 the player's network part
  - 0x270 the player's stamina part (noted by the stamina code)
  - 0x278 damage factor (f32)
  - 0x27C fire-rate factor (f32, 1/x)
- **Refresh:** the player's body and network part are read from the player's character every second while any switch is on, and at each switch-on (weapon.rs:329-343, 474-480; watchdog.rs:97-103).
- **Zero means nobody:** a zero there makes every hook leave everyone alone.

**How the function-start patches work (3.8-3.10).** The function's first bytes become `E9 rel32` into the block. The block's code runs this order (weapon.rs:100-121):
1. `push`
2. Load the player's value from the slot. If it is 0, go to step 6.
3. Compare it with this call's character or owner.
4. If it is the player's: `pop`, give the answer, `ret`. The game function is skipped.
5. (Otherwise:)
6. `pop`, run the original first bytes, `jmp` back.

| # | Switch | Where | Original bytes (run again in the block) | "Is it the player" test | For the player | Status |
|---|---|---|---|---|---|---|
| 3.8 | No Recoil | "weapon recoil" start | 6 bytes `48 8B C4 55 41 55` | `cmp [rcx+8], rax` (body) | return at once | **Experimental** |
| 3.8 | No Spread | "weapon accuracy" start | 5 bytes `40 55 56 41 56` | `cmp rax, rdx` (rdx = the player's body) | return at once | **Experimental** |
| 3.9 | No Screen Shake | "weapon shake" start | 7 bytes `48 8B C4 41 54 41 55` | `cmp [rcx+8], rax` | return at once | **Experimental** |
| 3.9 | No Scoped Sway | "weapon sway" start | 5 bytes `48 89 5C 24 ??` (read live) | `cmp [rcx+8], rax` | return at once | **Experimental** |
| 3.10 | Infinite Mag | "weapon ammo" start | 5 bytes `48 89 5C 24 ??` (read live) | `cmp [rcx+0x88], rax` (network part) | `mov al, 1; ret` (no round used) | **Experimental** |

**3.11 Infinite Reserve (Confirmed).**
- **Where:** only the call's 4-byte offset at "weapon reserve" hit + 28 changes. It sits alone in its word (exe+0x2161738 in that build). A 5-byte jump would not fit any word there (weapon.rs:300-302).
- **Original:** rel32 to the mag-load function. The call byte `E8` and its target inside the exe are checked (weapon.rs:240-247).
- **New:** rel32 to the block's reserve code.
- **The block's code (weapon.rs:162-187):**
  1. If the player's network part is set and `[rbp+0x88]` equals it, call the game function. It gets its own frame, with stack slots 5 to 7 copied.
  2. Then set `ax = 500 - bx + si`, so the game's `sub bx, si; add ax, bx` gives a reserve of 500. Then `ret`.
  3. Anyone else jumps straight to the game function, unchanged.

**3.12 Fire Rate (Confirmed).**
- **Where:** only the call's 4-byte offset at "weapon fire rate" hit + 18 changes. Its word is exe+0x36b2390 in that build (weapon.rs:303-305).
- **Original:** rel32 to the wait function (60 / RPM). It is checked like 3.11.
- **New:** rel32 to the block's fire code.
- **The block's code (weapon.rs:194-217):**
  1. Call the game function.
  2. Follow the gun's handle (`+0xC8`; the i32 at handle+0xC must be below 0) to the item.
  3. If `item+0x88` is the player's network part, `mulss xmm0` by the factor. The menu writes 1/x, so x times faster. Then return.

**3.13 Infinite Stamina (Confirmed).**
- **Where:** the "weapon stamina" hit.
- **Original:** 7 bytes `80 BF F1 01 00 00 00` (`cmp byte [rdi+0x1F1], 0`).
- **New:** `E9 rel32`. Bytes 6-7 never run.
- **The block's code:**
  1. If `[rdi+8]` is the player's body, note `rdi` at block+0x270 and `mov byte [rdi+0x1F1], 1`.
  2. Run the original compare (it now sees 1), then `jmp` back to hit + 7.
- **Meaning:** the player's stamina is not used.
- **Undo:** Off swaps the word back, then clears +0x1F1 in the noted part after a readability check (weapon.rs:360-367).

**3.14 Damage Multiplier (Experimental).**
- **Where:** the "weapon damage" hit.
- **Original:** 7 bytes `83 BF A8 00 00 00 06` (`cmp dword [rdi+0xA8], 6`).
- **New:** `E9 rel32`.
- **The block's code:** if `r14` is the player's body, `mulss xmm0, [factor]`. Then the original compare, then `jmp` back to hit + 7.
- **Meaning:** the player's hits do factor x the damage. The menu writes the factor; 0 turns the switch off (weapon.rs:402-411).

**Undo (all nine):** each switch swaps its own word back. Unload turns all of them off (weapon.rs:515).

### 3.15 Stand While Flying: fall (Confirmed)

- **Where:** the "airwalk fall" hit, the function start (exe+0x353d0c0 in that build).
- **Original:** `48 89 5C 24 18` (`mov [rsp+0x18], rbx`, read live and copied).
- **New:** `E9 rel32`, to a 0x80 block. Its slot at +0x40 holds the player's state manager, or 0.
- **The mod's code:**
  1. `push rax; mov rax, [slot]; test rax, rax; jz go`
  2. `cmp [rcx+0x38], rax; jne go`
  3. `inc qword [count]`, `pop rax; ret` (the player's fall never starts)
  4. `go:` `pop rax`, the original 5 bytes, `jmp` to function + 5.
- **Meaning:** the player's character never enters the falling state while flying. The count says how many falls were kept from the player (Fly off's summary line). Let Game Fall (Experiments, untested) keeps the slot at 0 while the player flies.
- **Status:** Confirmed. The player's test showed no falling pose (Noclip combat and early inject).

### 3.16 Stand While Flying: in the air (Confirmed)

- **Where:** "airwalk in-air" hit + 3.
- **Original:** `C6 44 24 20 01` (`mov byte [rsp+0x20], 1`, the value about to be stored into 0xF1). It is checked (airwalk.rs:86).
- **New:** `E9 rel32`, to a 0x80 block. Its slot at +0x40 holds the player's character, or 0.
- **The mod's code:**
  1. `push rax; mov rax, [rdi+8]`; if 0, go to "other".
  2. `mov rax, [rax+0xA8]`; if 0, go to "other".
  3. `cmp rax, [slot]; jne other`
  4. `pop rax; mov byte [rsp+0x20], 0; jmp` back.
  5. `other:` `pop rax; mov byte [rsp+0x20], 1; jmp` back (hit + 8).
- **Meaning:** for the player the "in the air" value is stored as false every time, so the player can aim and shoot.
- **Status:** Confirmed.
- **Undo:** 3.15 and 3.16 switch together. On Off both slots are zeroed before the jumps are removed. If the second swap fails, the first is undone (airwalk.rs:187-214). The slots are refreshed every second and at each Fly switch; they are 0 while Fly is off (airwalk.rs:166-174; watchdog.rs:103).

### 3.19 Fly Through Everything (Experimental)

- **Where:** the "fly through setter" hit, the function start (exe+0x11db540 in that build).
- **Original:** `48 89 5C 24 08` (`mov [rsp+0x8], rbx`, checked before the hook is made).
- **New:** `E9 rel32`, to a 0x80 block. Its slot at +0x40 holds the player's body, or 0.
- **The mod's code:**
  1. `cmp rcx, [slot]; jne game`
  2. `mov rax, rdx; sub rax, rsp; cmp rax, 0x10000; jae game` (the new place must be a caller's copy just above rsp)
  3. `mov rax, [rcx+0x20]; mov [rdx], rax`, the same from +0x28 to [rdx+0x8] and from +0x30 to [rdx+0x10]
  4. `game:` the original 5 bytes, `jmp` to function + 5.
- **Meaning:** while Fly is on the game keeps the player where Fly has them: its collision and ground snap no longer move the player. Turning, and the rest of the setter, as before.
- **Status:** Experimental. Found live. Tested in game: no more getting stuck on slopes and objects; live check: the game's own write of the player's place equalled where Fly holds the player in 55 of 55 frames. Still open: sometimes a snap to a nearby spot when Fly turns off; how other players see the player's fly is not checked (needs a second player).
- **Undo:** in with Fly, out with it (fly.rs `follow_fly`). The slot is 0 while the jump goes in or out; Fly's worker fills it once its 100 ms fall check is over (noclip.rs `hold`), and empties it at the first tick it can't write the player's body until it can again. One instruction is moved, so a single code-word swap is enough (no pause).

### 3.20 Fly Through Everything: the game's copy of the player's place (Experimental)

- **Where:** the "fly through copy" hit + 0x10 (exe+0xc73651 in that build), in the per-frame update exe+0xc73620, after `mov rdi, rcx; xorps xmm2, xmm2; mov rcx, [rcx+0x440]; mov rbx, rdx`.
- **Original:** `0F 10 59 50 0F 10 41 20` (`movups xmm3, [rcx+0x50]; movups xmm0, [rcx+0x20]`), checked before the hook is made.
- **New:** `E9 rel32`, to a 0x80 block: +0x40 the player's body (or 0), +0x48 the object, +0x50 `this`, +0x58 a read count.
- **The mod's code:** `mov rax, [slot]; test rax, rax; jz game; cmp [rdi+0x10], rax; jne game; mov [obj], rcx; mov [this], rdi; inc qword [count]; game:` the two instructions, `jmp` back after them. Only rax and the flags change; both are free there.
- **Meaning:** nothing in the game changes; the object for the player's body is noted. Fly's worker writes the player's place into object+0x50 each tick while the player is held, after reading the way from the player's body again (`this+0x10`, `this+0x440`) and only if the game read it in the last 0.25 s; at a hold's first look the copy must be within 5 units of the player.
- **Status:** Experimental. Proven from outside the game (a script, every 5 ms): the snap on land after Fly off went away . Not tested in the mod yet. Water: the game moves the copy itself every frame (about 44 a second); see Fly Water Fixes in features.md.
- **Undo:** in and out with 3.19 (fly through's `set`); its slot is 0 while the jump goes in or out. Fly Water Fixes put both back in for a moment after Fly off (Snap Guard), and take them out again. The jump covers two instructions, so it goes in with the game's threads stopped and none inside its bytes (`airwalk::swap_paused`); out needs no pause.

### 3.17 Bone ESP: pose catch (Experimental)

- **Where:** the "pose function" hit, the function start (exe+0xacfc10 in that build).
- **Original:** `48 89 5C 24 08` (`mov [rsp+8], rbx`). It is checked (posehook.rs:38).
- **New:** `E9 rel32`, to a block with an index at +0x40 and a ring of 8192 x 8 bytes at +0x80.
- **The mod's code:**
  - `B8 01 00 00 00` mov eax, 1
  - `F0 0F C1 05 rel32` lock xadd [index], eax
  - `25 FF 1F 00 00` and eax, 8191
  - `4C 8D 1D rel32` lea r11, [ring]
  - `4D 89 04 C3` mov [r11+rax*8], r8
  - the original 5 bytes, then `jmp` to function + 5
- **Meaning:** each pose object the game hands over (`r8`) is noted in a ring. Nothing in the game changes. It replaced a hardware watch that crashed the game now and then (posehook.rs:1-5).
- **On and off:** ESP collects the noted objects every 2 s while Bones is on. The patch is off when Bones is off and at unload (vkdraw.rs:1996-2001, 351).

### 3.21 Silent Aim: shot build (Experimental, not tested in game)

- **Where:** the "silent aim shot build" hit (2.6), the function start.
- **Original:** `4C 8B DC 53 56` (`mov r11, rsp; push rbx; push rsi`): three instructions, checked (silentaim.rs `MOVED`).
- **New:** `E9 rel32` to its own 0x300-byte block. It covers three instructions, so it goes in with `swap_code_u64_paused` (no thread inside +1..+5); out with the plain swap.
- **Block:** code at 0x000 (must end before 0x280); data 0x280 target (x y z 0, f32), 0x290 mask (clears w), 0x2A0 on (u32), 0x2A4 least squared distance (1.0). After them, for the log: 0x2A8 last steered shot, 0x2B0 its origin, 0x2C0 the game's direction, 0x2D0 ours, 0x2E0 count of steered shots, 0x2E4 feature word (bits: 1 Magic Bullets, 2 No Range Loss, 4 Instant Hit, 8 No Bullet Drop), 0x2E8 count of all the player's shots.
- **The mod's code:** on and the feature word both 0, no origin or direction pointer, no shooter, or shooter not `[BODY_ROOT]` → GO. Else the shot, its origin and the game's direction are noted for the log, then `sub rsp, 0x68` and stack slots 5-7 are copied. If on is set: `xmm0 = (target - [rdx]) & mask`; length squared ≤ 1 or not a number → no steering (call with the game's direction); else `xmm0 /= length`, direction at `[rsp+0x40]`, `lea r8, [rsp+0x40]`. Then, with bit 8 of the feature word set, the gravity curve is taken out of the shot's local path points (`silentaim.rs` `flatten_code`: `mov rax, [rcx+0x160]`, `movzx r10d, word [rcx+0x16a]`, at least 2 points, k = `[r8+8] / sqrt([r8]² + [r8+4]²)` from the direction, then every point's height `[point+4]` becomes the first's + (`[point]` − the first's `[point]`) × k; 12 bytes per point). Then `call GO`. After the game's function returns, the feature writes go into the shot (see Meaning), then `add rsp, 0x68; ret`. **GO:** the three moved instructions, `jmp` function + 5. The checks use near jumps (the way to GO is too long for short ones; the unit test caught it).
- **Meaning:** the player's shot is built with the direction to the target; the game's direction word is never written. After the build the cave writes f32 values into the player's shot, only for the features that are on: Magic Bullets `+0x378` = 1.0e6 (all the player's shots), No Range Loss `+0x158` = 10000, Instant Hit `+0xC` = 100000 and `+0x8` = 100000 / 3. No Bullet Drop writes before the call instead (above): the local points at `+0x160` (count `+0x16a`) are built by the game before this function as (horizontal distance, height, side), the height holding the aim's pitch and the gravity curve (`0x3658be0`, g/2 = 4.905); this function turns them by the yaw into the world points at `+0x16c`, and the mover `0x36b1c30` only follows those, so heights on the pitch line are a straight flight. The worker writes the target, the on word and the feature word every 25 ms (on 0 with no target); with Silent Aim off and No Bullet Drop on there is no worker and the feature word holds bit 8 alone.
- **Undo:** on = 0, then the word swapped back. The jump stays in while any of Silent Aim, Magic Bullets or No Bullet Drop is on. Unload does it before the weapon hooks (server.rs). The block is never freed.

### 3.22 Magic Bullets: contact verdict call (Experimental)

- **Where:** in the shot's contact walk (`0x36907e0` in build 25349072), the `call` to the per-contact function (`0x36af4b0`), at the hit of pattern "magic bullets contact verdict" `F3 0F 11 8D F8 00 00 00 E8 ?? ?? ?? ?? 8B C8 85 C0 74 ?? 83 E9 02 0F 84 ?? ?? ?? ?? 83 E9 01 74` plus 8 (exe+0x3690a03).
- **Original:** `E8 rel32` (the call). Only the rel32 changes.
- **New:** `E8 rel32` to a 0x90-byte block of its own, swapped with `swap_code_u64_paused` (no thread inside the call's bytes). Out with the plain swap.
- **Block:** code at 0x00 (0x7B bytes), 0x80 on (u32), 0x84 count of contacts skipped (u32). The cave only acts on shots carrying the bullet mark (`+0x378` = 1.0e6, written by the shot-build cave), so grenades and other gadgets still stop on the ground.
- **The mod's code:** on 0, no shooter handle, shooter not `[BODY_ROOT]` → `jmp` into the game's function (it returns to the game). Else `sub rsp, 0x28; call` the game's function; `add rsp, 0x28`; eax 2 → `ret`; else count up, eax = 1; `ret`.
- **Meaning:** the function's verdict on one contact: 0 passed through (the walk records it and goes on), 1 and 2 skipped (2 = a character, hit applied inside), 3 (no owner, or a surface that can't be penetrated: the walk clears its flag byte 1 and `shot+0x518`) and 4 (a body of class `0xc8ad60`: the walk stops) end the shot, 5 the shot's ignore list. For the player's shots every verdict but 2 becomes 1 (skipped: nothing recorded). A 0 is not enough: the walk records a passed contact as the shot's impact (`shot+0x3a0`, `shot+0x28` = 1), which ends it.
- **Undo:** on = 0, then the word swapped back (Magic Bullets off, unload). The block is never freed.

### 3.23 Instant Hit: mover call (Experimental)

- **Where:** in the per-frame shot function (`0x36a70e0` in build 25349072), the `call` to the mover (`0x36b1c30`, shot in rcx, frame time in xmm1), at the hit of pattern "instant hit mover call" `74 0B 0F 28 CE 48 8B CF E8 ?? ?? ?? ?? 0F 28 CE 48 8B CF E8 ?? ?? ?? ?? 85 C0 48 8B CF 0F 95 C0 88 47 22` plus 19 (exe+0x36a7238).
- **Original:** `E8 rel32` (the call). Only the rel32 changes.
- **New:** `E8 rel32` to a 0x50-byte block of its own, swapped with `swap_code_u64_paused`. Out with the plain swap.
- **Block:** code at 0x00 (0x35 bytes), 0x40 on (u32), 0x44 count of shot frames sped up (u32).
- **The mod's code:** on 0, or `[shot+0xC]` is not Instant Hit's mark (100000, written by the shot-build cave into the player's bullets) → `jmp` into the mover. Else `[shot+0x10]` = 1.0e8, byte `[shot+0x374]` = 1, count up, `jmp` into the mover.
- **Meaning:** the mover moves a shot by `[shot+0x10]` × frame time along its world points (`+0x16c`, count `+0x176`), not by `+0xC` (it only copies `+0xC` / 3 into `+0x8`). At each point it reaches it drops the distance left over unless the byte `+0x374` is set (`cmp byte [rbx+0x374], 0` at exe+0x36b1ee4), so a shot covers about one point per frame whatever its speed. With both set the whole path is walked in the shot's first frame and the sweep covers it at once. Writing only `+0xC` (and `+0x8`) changed nothing in game (shots still flew at about 850 units/s).
- **Undo:** on = 0, then the word swapped back (Instant Hit off, unload). The block is never freed.

### 3.18 Hooks outside the game's exe (Confirmed)

- **Vulkan:** vkQueuePresentKHR, vkAcquireNextImageKHR and vkCreateSwapchainKHR are detoured with the `retour` library. They live in the Steam overlay layer or the GPU driver, not in the exe. Retour writes their bytes and keeps a trampoline; the code does not list them. Undo: `vkdraw::shutdown` disables each detour (vkdraw.rs:341-356, 518-541).
- **DirectInput:** GetDeviceState and GetDeviceData are detoured in `dinput8.dll` the same way. Undo: `dinput::shutdown` (dinput.rs:299-347).

### Not code patches

- **Hardware watch** (God Mode, credits): debug registers only. No bytes are changed (hwbp.rs).
- **Held values** (freezes): data writes repeated every 15 ms. They cover the immune byte (God Mode), body positions (Bring, Yeet, teleport) and the drop gear score (10 times a second, menu.rs:551-575).

---

## 4. Re-finding things after a game update

What to expect, from the code:
- Patterns usually survive, because the cache in `Offsets.bin` searches again by itself on a new build. If a pattern now matches 0 or 2+ times, the log says `NAME: pattern found N times, want 1 (game updated?)`, and that feature stays off for the session (section 1).
- Static offsets are **not** checked against anything. After an update these may point at the wrong thing:
  - body `exe+0x63EBEB0`
  - camera `exe+0x62F24B0`
  - NPC class `exe+0x49BFD70`
  - player class `exe+0x4A4C5F0`
  - credits caller `exe+0x273990E`
  - slot table `exe+0x63EB5D0`
  - level root `exe+0x63A0D20`
  - item classes `exe+0x4894BD0` and `exe+0x47B5130`
- Signs that a static offset moved: no body found, no NPCs, ESP boxes in the wrong place, or credits never found. These must be found again by hand and the code changed.
- Field offsets can move too. The CE table warns about `char+0x190` (weapon.rs:27-28). A wrong one only makes the ammo check never match.
- The dll.log lines `found at exe+0x...` and `... ready: exe+0x...` give each pattern's spot in the build being run. These help when comparing builds.

How each one was found, so it can be found again (from the notes):
- **Body global:** the old grbscan tool was used. Its shaped scan listed copies of the player's position with a pointer into the exe 0x20 before them; a bisect (nudge half, halve on the player's answer) narrowed them; a reverse pointer scan found the static root (Step 2). Grbscan was removed. The research tool's `scan value`, `scan ptr`, `refs` and `snap`/`diff` commands do this job now (Research tool).
- **Camera:** the camera global also mirrors the player's position at `+0x220` (Step 2; ESP reference). It can be found from a known position the same way.
- **NPC class:** follow a walking NPC; one character object per NPC, its +8 the body (Entities). A wrong marker (exe+0x4a6bed0) was rejected before.
- **Player class and immune byte:** an execute watch on the damage check, taking one hit, gives the character in `rdi`. Its +0x378 is the immune byte (God mode hunt).
- **Credits caller:** watch the credits read function while a shop opens and look at the return addresses. The mod's own "Find credits" does this and lists objects with the caller first (credits.rs:272-320; Credits and keys). The CE table's caller (exe+2B7FFC7) was already out of date.
- **Weapon patterns:** they come from community CE tables. Check each table spot against the exe (CE tables reference). The fire-rate and reserve calls were found live with the research tool (Fire rate; Reserve ammo).
- **Gear score:** scan memory for the two item classes. The slot table is the CE tables' "store equipment global". The gear level formula and slot ids came from the player's swaps (Owned gear score).
- **NVG:** the display object was found by recording `rdi` at the switch-on code (NVG note: display class exe+0x46C72B0, path `[[[exe+0x63EBEF8]+0x2b8]+0x90]` in that build). That path is not used by the code.
- **NPC perception:** the sight check came from the Spartan Mod table. The meters were found with an awareness hook and live walk-ins (Detection investigation).
- **Stand While Flying:** the fall state 0x2D first, then the in-air blackboard value 0xF1, each tested live (Noclip combat and early inject).
- **Bone ESP poses:** a hardware watch on a pose list caught the function that writes it (Bone ESP).
- **Early inject caution:** the exe has protector-style sections. Right after the game starts, code may not be unpacked yet, so patterns can fail or match stubs (Noclip combat and early inject).
