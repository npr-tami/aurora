# Enemy NPC alert system: offline map

Gameplay AI only: how the game's enemy NPCs build their orange alert meter. For the menu switch
"Undetected" (unseen.rs). Found offline in GRB_vulkan.exe (Steam build 25349072, build id
`GRB_vulkan-64ff3734`) with grbstatic + Ghidra; nothing run in the game. All rvas are Vulkan.
Confidence: what the code does is **strong** (read in the decompile); what each part means in
play is a **candidate** until tested in game.

## The pieces
| What | Where | Notes |
|---|---|---|
| Awareness class | pointer table rva 0x4a52e20; slot 1 returns class ID 0xFF34644B | Whose object it is stays open: one per enemy, or one owned by the player's character (`[obj+8]`); offsets.md 2.11 has both readings. Another class, table 0x4a4ce88 (ID 0x94D06CD5), uses +0x268…+0x273 for zone tracking instead: not the same thing |
| Awareness update | sub_362b9c0 (slot 17) | Every tick: flush the stimulus queue, maybe add penalty 0x10, apply penalties, spotting resets, then the two meter functions |
| Stimulus queue | aware+0x150 (array of 0x40-byte records), count u16 at +0x15a | Record: +0x00 source handle, +0x10 and +0x20 two positions, +0x30 penalty type, +0x34 delay (counts down) |
| Queue flush | sub_35bfb10 (debug text "Flush %s") | When a record's delay runs out: AddPenalty(type, distance between its two positions) |
| **AddPenalty** | sub_361f530 (texts "START Storage", "ADD %2.0f Penalty for %s", "IGNORE …") | The single entry for every alert source. Reads curves from the NPC's config (`[[aware+0x208]+0x100]`): +0x38 + type·8 → +0x264 (orange meter), +0x740 + type·8 → +0x260 (second meter; type 0x10 = 100), +0x3c0 + type·8 → +0x268. Sets +0x26c = 1 (pending). Penalty type names come from an enum (hash 0xADEB13F6) whose value names are not in the exe |
| Apply penalties | sub_360d5b0 (texts "ADD Penalty (%2.0f)", "FINAL VALUE up to from %2.0f to %2.0f", "RESET Storage") | Meter targets: +0x250 = (+0x250 or +0x22c) + +0x264, +0x24c = … + +0x260, both clamped 0..100; then clears the pending values |
| Orange meter | +0x22c, target +0x250 | Filled by sub_3634460 (the mod's FILL / CLAMP patches) |
| Second meter | +0x228, target +0x24c | sub_363a6e0 (the mod's SECOND wrapper) |
| Spotting resets | sub_3634bc0, sub_3633f80 (texts "Reset Spotting - TIME / RANGE", "- Grass - TIME / HEIGHT / RANGE") | Times and radii from the NPC config (+0xafc…+0xb64); a grass variant with a 3 m height check |
| Other AddPenalty callers | sub_35fc4b0 (event handler: penalty 0x3f at distance 0 when +0x26f is set); the update itself (penalty 0x10, 80) | Only three callers in the whole exe |
| Sight function | sub_359e410 = slot 28 of table 0x4a535d0, class ID 0xE2A6EEBC | A different class (the player's visibility, per round 2 of the Detection investigation). It feeds this system from outside |

## What this means for "Undetected" (candidate, untested)
- Hearing is not a separate meter. Every alert source, whatever it is, ends in AddPenalty.
  Close-range spotting with sight at 0 is most likely a queued stimulus (sound or proximity)
  turned into a penalty by distance.
- So one hook at AddPenalty's start (or at the queue flush, which knows the stimulus source)
  could replace today's three meter patches. If the object is the player's (reading 2), the hook
  touches only the player's alert state; if it's per enemy, the flush route can check the source.
- Same caveat as today: it changes how every enemy NPC reacts, so it stays off until the player
  turns it on.

## Round 2: who fills the queue, and the enemy side
The awareness queue is the *end* of the chain, not the start. The full path, noise first:

| Step | Where | What it does |
|---|---|---|
| 1. Noise maker | sub_368e680 (weapon fire; callers sub_368b800, sub_369c740), sub_2890490 (explosion damage; caller sub_27d9370) | Build a noise event from a noise-data object (the weapon's / explosive's): radius = `obj+0x74`, type = `func 0x20cb3c0(obj)`, loudness = `obj+0x58` (weapon) or the scaled blast radius (explosion) |
| 2. Post noise event | sub_22411d0 | 0x60-byte event (event type at rva 0x63e15d0): +0x18 source handle, +0x20 second handle, +0x30 vec, +0x40 position, +0x50 loudness, +0x54 radius, +0x58 type, +0x5c extra. Only those 2 callers |
| 3. Noise handler | sub_2458cd0 (subscribed in sub_2444340 with 16 other AI event handlers) | Skips loudness 0 and "no listener within radius" (sub_10ed380 on the manager's grid); type 0x14 → sensor path (sub_2486f80), else → broadcast |
| 4. Broadcast | sub_240ea50 (also used by other handlers) | For every AI agent in the manager's list (`[mgr+0x3a8]`, count u16 `+0x3b2`) except the source: type filter sub_2449d90, distance < radius (radius < 0 = everywhere), side checks, then step 5 |
| 5. **Post to an enemy's inbox** | **sub_29cb140** (sets `+0x2c1` bit 1) | The one entry for everything an enemy perceives: needs `+0x2c0` bit 0x10 (perception on); merges same source + type; appends a 0x60-byte record to `npc+0x1b8` (count u16 `+0x1c2`, lock `+0x2a0`). 29 call sites in 24 functions |
| 6. Inbox drain | sub_29ef840 | Copies the inbox, calls slot 29 (`+0xe8`, OnStimulus) per record |
| 7. OnStimulus | sub_2a02f50 (overrides: sub_2fb0680, sub_31ba780, sub_31ba850, sub_3316360, sub_35ca1d0) | Accept check slot 28 (sub_29ce990: per-type ignore flags in `+0x2c0`, faction check), memory, then QueueWarning when the source may be warned about |
| 8. QueueWarning | sub_28a80a0 → manager `[rva 0x63ebf08]` | Queues (enemy, source, type); flushed later as event (0x19, 0) to the source |
| 9. The player's awareness | sub_35952e0 (event (0x19,0) handler) → queue `+0x150` → flush sub_35bfb10 → AddPenalty | The record's +0x00 is the **warning enemy**, pos A the enemy, pos B the player. So this queue only mirrors what enemies already decided |

Enemy inbox record (0x60 bytes): +0x00 source handle (pointer to a handle; entity = `[[rec]]`
when bit 31 of `[[rec]+0xc]` is set), +0x08 type (u32), +0x0c/+0x0d flags, +0x18 u32 (10 =
skip; faction-like), +0x20 and +0x30 positions, +0x50/+0x54 distances (filled by OnStimulus).
Built by sub_2a092d0 / sub_2a092f0 (record, type, position, source).

Other senders seen (type: function): 0x0e: sub_2462670 (radius from config `+0xf54`, default
10 m; event has position + direction); 0x31: sub_245a460 (periodic list, radius config
`+0xf64`); 0x0c: sub_2461f70 (two entities); 0x4c: sub_2486d50 (enemies within 30 m);
0x4d/0x4e/0x50/0x51: sub_245eb70 (150 s cooldowns); 0x1c: sub_24eb720; 0x1e: sub_2a27910;
0x16: sub_3087dd0, sub_31875e0; 0x1f: sub_309d8a0, sub_309daf0; 0x43, 0x0b: sub_31875e0;
0x56: sub_31dbb90; 0x0d: sub_32d1e10. What each type means in play: **candidate** until a live
recording. Sight and the close-range sense: round 3 below.

### What this changes for "Undetected" (candidate, untested)
- A "the player only" check fits at step 5 (sub_29cb140): the record's +0x00 is whoever made the
  stimulus, so skipping records whose source is the player's body leaves other stimuli alone. Skipping
  is a clean early return (no queue index to keep moving, unlike the drain). Narrower option:
  step 4 (noise only).
- The awareness flush (step 9) is the wrong place: its source is the enemy, not the player, and
  holding the player's meters at 0 already failed to stop detection (Detection investigation).
- Blocking every stimulus from the player's may also stop reactions to being shot or seeing the player; which
  types to drop is the player's call after the live recording.

### Naming the types
Built 4 Nov: `research\grbresearch-dll\requests\noise.request` (regs recorder with reads, 300 s, ring
16384). At sub_29cb140's start it keeps the caller, the source handle and the type; at
sub_22411d0's start the caller, both handles, loudness, extra, radius and type. Then
`python research\scripts\noise_report.py regs-rows.csv` names each caller from this map and shows a
per-second timeline of the player's stimuli and every noise event. Both patterns are unique in both
exes (grbstatic sigs); the request parses and its moved bytes pass the decoder (unit test);
the recorder code ran in a test process. Not run in the game yet.

## Round 3: sight and the close-range sense also use the inbox
Most senders don't call the inbox directly. They call a generic helper, **sub_2a0aca0**(npc,
type, position, source entity), which builds the record (sub_2a092f0) and calls the inbox. 25
call sites. Prologue `push rdi; sub rsp,0xB0`, so at the inbox's first byte `[rsp+0xC0]` is the
return address into the helper's caller.

**Sight** (read in the disassembly; meaning candidate):
- Sensor results are dispatched by sub_2a2d380 (per sensor kind `[query+0x98]`, enabled per kind
  at `npc_sensor+0x74+kind`; hits at `+0xcc`, count u16 `+0xd6`, 0x20 bytes each: position +
  entity handle). Kinds 0, 1, 9 → **sub_2a59050** (sight). Kind 3 → type 0x0a (target must have
  component 0x387e8d67), 5 → 0x34, 6 → 0x3c, 8 → 0x45, 4 → its own virtual (slot +0xb8).
- sub_2a59050: two checks (slots +0xe8, +0xf0). Seen at once → type **0x10** (or **0x59** when
  sub_2abc880(entity) is true) via the wrapper **sub_29c7d60** → helper → inbox. Otherwise it keeps
  a per-target memory (sub_29f6d50 / sub_29dd820, keyed by `entity+0x160`): above a threshold →
  sub_2a568f0 (0x10 / 0x59 again); below → sub_2a5e3c0 (types **0x54, 0x11, 0x04, 0x03**, a
  build-up). Sub_2a56950 → 0x12.
- The record's source is the seen entity. So a "the player only" skip at the inbox also covers sight.

**Close-range sense** sub_31af140 (one of the NoPerception-guarded functions from round 2):
- Every tick, for each entity in a global list (`[rva 0x63ebef8]`, likely the players) that
  passes sub_2abd360: radius from the sense object's own floats (`+0x7c…+0x94`) by two states of
  that entity (sub_29f5160 = 1/2/3, sub_29ffda0 = 0..3; candidate: stance × movement), floored
  at `+0x78`; scaled by two
  curves (`+0x98`, `+0xa0`: clamped 0..1, candidate light / camouflage) and a per-surface table
  (`+0x70`, index up to 0x3b); then the NPC's own virtual (slot +0xd8) adjusts it.
- Distance² < radius² → helper type **0x38**, source = that entity.
- **Strong candidate for the 1–10 m detection** with sight at 0: no vision cone, radius from
  stance and speed. Live recording will show it.

All helper call sites (type in `edx`):

| Caller | Type | Caller | Type |
|---|---|---|---|
| sub_22dea20 | 0x28 | sub_31ad900 | variable |
| sub_27299f0 | 0x62 | sub_31af140 (close sense) | 0x38 |
| sub_29c7d60 (sight wrapper) | its argument | sub_31f5820, sub_31f6520 | 0x46 |
| sub_2a2d380 (sensor dispatch) | 0x0a, 0x34, 0x3c, 0x45 | sub_321f660 | 0x5a |
| sub_2a44e60 | 0x30 | sub_321f960 | 0x1a |
| sub_2a58e90 (two sites, random timer) | 0x18 | sub_32820e0 | 0x30 |
| sub_2ad8210, sub_2ad84b0 | variable | sub_333f290 | variable |
| sub_308b9a0 | 0x43, 0x0b | sub_362f0b0 | 0x2a |
| sub_30ed3c0 | 0x54 | sub_317d240 | variable |

Sight wrapper sub_29c7d60 callers: sub_2a59050 (0x10/0x59, two sites), sub_2a568f0 (tail jump,
0x10/0x59), sub_2a5e3c0 (0x54/0x11/0x04, one tail jump with 0x03), sub_2a56950 (0x12).
Broadcast sub_240ea50 callers: noise handler sub_2458cd0, sub_24590a0, sub_245eb70, sub_2461f70,
sub_240ed80, sub_2fbcf60 (and one tail jump at rva 0x240ed77).

Stack offsets at the inbox's first byte, to name the sender behind a wrapper (stack reads only):
`[rsp+0xC0]` when the direct caller is the helper; `[rsp+0xF0]` when, in turn, the helper's
caller is sub_29c7d60 (`push rdi; sub rsp,0x20`); `[rsp+0x100]` when the direct caller is the
broadcast (`push rbx; sub rsp,0xF0`).

### Draft switch: `dll\src\inbox.rs`
Menu > Experiments > "Ignore only me (untested)". At the inbox's first byte: if perception is on, the
type is in its mask (all types for now) and the record's source entity equals the player's body
(`[BODY_ROOT]`), it returns with ZF=1 so the game's own `je` drops the record. Ran in a test
process (unit test with a stand-in function); never in the game. Waits on the noise recording
for two answers: is the source entity the player's body pointer, and which types to drop.

## Not found yet
- The penalty / stimulus type names (enum values aren't stored as text; recording above).
- Which of the inbox types fire for the player at 1–10 m (0x38 is the lead; needs the recording).
