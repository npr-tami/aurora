# Aurora features

What every feature of the mod does, how it works and what it touches.
Memory offsets are named by their role only; the numbers are in [offsets.md](offsets.md).
For how the pieces fit together see [architecture.md](../../docs/architecture.md). For the menu itself (keys, profiles, toasts) see [menu.md](menu.md).

## How a mod changes a game

The mod is a DLL loaded into the game's own process, so it can read and write the game's memory directly. It changes the game in four ways.
**Hold a value:** write the same bytes again and again, so the game can't change them back. God Mode writes `1` into the character's "immune" byte every 15 ms.
**Patch code:** change a few bytes of the game's machine code. No NVG Overlay turns the instruction "set the overlay flag to 1" into "set it to 0".
**Hook code:** overwrite the start of a game function with a jump to a small piece of the mod's own code (a "code cave"). The cave decides what to do, then runs the bytes it replaced and jumps back. No Recoil's cave asks "is this the player's gun?" and, if yes, skips the recoil function; for everyone else the game runs as normal. The ESP hooks the graphics driver's "show this frame" call the same way, to draw on top of the picture.
**Watch:** put a CPU hardware breakpoint on one spot and note who runs or touches it. God Mode waits for the damage check to run on the player and reads which object was hit.

## Reading this page

- **Units.** The game has two. World units are what memory holds. The game's HUD shows metres = world units × 1.5. Each feature says which one it shows.
- **On-screen order.** The list below follows `items_of` (`menu.rs`). On screen, `items` puts submenus first on most pages. Self keeps its own order since God Mode, Undetected, Infinite Stamina, then Movement and Weapons.
- **Code search.** Every feature that changes game code finds its spot by a byte pattern that must match exactly once (`memory::find_code`). If it matches zero or several times, the feature says "game patched?" and stays off for the rest of the session (its BAD / BROKEN flag).

## Overview

| Feature | Kind | Affects |
|---|---|---|
| God Mode | watch + value hold | the player only |
| Undetected | code patches + cave | every enemy NPC's sight (game AI only; not a bypass) |
| Fly | position hold | the player only |
| Safe Fly | uses God Mode | the player only |
| Stand While Flying | 2 code hooks, always on with Fly | the player only |
| Fly Through Everything | 2 code hooks, always on with Fly | the player only |
| Fly Water Fixes (Experiments) | after Fly off: hold and watch; Let Game Fall: fall hook slot 0 | the player only |
| Infinite Stamina | code hook | the player only |
| Gear Score page | memory writes | the player's items, drop level |
| Credits | code hook + watch | the player's own points |
| No Recoil | code hook | the player only |
| No Spread | code hook | the player only |
| No Scoped Sway | code hook | the player only |
| Infinite Mag | code hook | the player only |
| Infinite Reserve | call patch + cave | the player only |
| Damage Multiplier | code hook | shots fired by the player |
| Fire Rate | call patch + cave | the player only |
| Silent Aim | code hook (paused swap) + cave + worker | the player's shots; targets AI soldiers and drones only, never players (co-op included) |
| Teleport | position writes | the player only |
| ESP | Vulkan hook, read only | draws every NPC; changes nothing |
| Target (Pick, Yeet, Bring) | position hold | one picked NPC |
| No Screen Shake | code hook | the player only |
| No NVG Overlay | code patch | the player's screen |
| Notifications page | menu setting | menu only |

---

## Self

### God Mode

**What you see.** Self › God Mode, a switch.
Toasts: "God mode on", "God mode off". If the mod doesn't know the player's character yet: "Armed: take one hit and it turns on".

**How it works.** Value hold, found by a watch.
- Before the game damages something, it checks an "immune" byte on that character. The mod finds the check by its byte pattern (`damage_site`, `godmode.rs:147`).
- First it looks for the player's character from the player's body (`remembered`, `godmode.rs:156`, via `entities::my_character`). It only trusts it if the immune byte reads 0 or 1. Failing that, it uses the last character it held, if that object still has the same class tag and the same owner.
- If it still doesn't know, the menu arms a watch for 60 s (`arm_background`, `godmode.rs:65`). A hardware breakpoint on the damage check catches the next hit. The hit character is in one register (rdi).
- `on` (`godmode.rs:179`) holds the immune byte at 1 with `freeze::set_guarded("god", …)`. The guard is the object's class tag: if those bytes change, the object is gone, and the hold stops by itself.
- At inject, `learn_background` (`godmode.rs:88`) waits up to 10 minutes for the player's body, waits 5 s more, then quietly catches the first hit without turning god mode on. A later "on" is then instant.
- `off` (`godmode.rs:216`) cancels a waiting arm, stops the hold, and writes 0 back only if the hold was still running, the byte is readable and the owner check passes. Once the hold has dropped, the memory may belong to something else, so it is left alone.

**Safety checks.**
- Owner check (`mine`, `godmode.rs:129`): a caught character whose body isn't the player's is refused and logged "is not the player's". Before 4 Nov, a hit the player dealt to an NPC could make that NPC immune.
- When the guard drops the hold (world change), `held` (`godmode.rs:46`) logs "god mode off: entity … went away (world change)" and the switch shows off.
- The watchdog drops all holds, this one too, if the game hangs for 6 s.

**Who it affects.** The player's character only.

**Limits and known issues.**
- Injuries are not cleared (God mode hunt note).
- After a world change the hold stops. Turn it on again.
- 2 Nov: a crash came from `my_character` matching a thread stack. Fixed in 20416c6.
- 4 Nov: the game crashed about 12 s after a world change; a gear score write and a god hold drop were logged just before. Cause unknown.

### Undetected

> **What this is, and what it is not.** "Undetected" is only the menu label. The feature is about the game's enemy NPC AI: whether its computer-controlled soldiers and drones *see* the player and fill their alert meter, like a difficulty change where enemies go blind. It is **not** a bypass, **not** anti-cheat evasion, and **not** hiding the mod or the player from anything. It touches no anti-cheat, network, account or logging code. In code it is `unseen.rs`.

**What you see.** Self › Undetected, a switch. Toasts: "Undetected on", "Undetected off".

**How it works.** Four code changes, switched together by `set` (`unseen.rs:225`):
1. **Sight range** (`unseen.rs:157`): a code cave where the game loads a sight-range factor. The cave multiplies it by 0 and jumps back.
2. **Meter fill** (`prepare_fill`, `unseen.rs:62`): a conditional jump (`jae`) becomes an always-jump (`jmp`).
3. **Meter clamp** (`prepare_clamp`, `unseen.rs:89`): a `maxss` becomes a `movaps`.
4. **Second meter** (`prepare_second`, `unseen.rs:112`): a wrapper that zeroes a second detection meter.

Each change is one atomic 8-byte swap (`memory::swap_code_u64`). The swap checks the old bytes first and refuses if they aren't what it expects.
If one change fails, the ones switched on in that same call are put back. Off is always allowed.

**Who it affects.** Every NPC's sight. The patch has no owner check. The code comment says "This is for every NPC's eyes". Research (Detection investigation) says the patched function works out how visible the player is, and every enemy sensor uses that result. Whether NPCs also stop seeing other players, or each other: unknown. Co-op effect: unknown.

**Limits and known issues.**
- Works at range. NPCs still detect the player at 1–10 m.
- The two meter patches were tested and did not stop detection. The player's awareness object only receives the result; it doesn't make it (Detection investigation).
- Likely cause of close detection: a close circle in the NPC vision shape that ignores visibility. Not live-tested.

### Movement › Fly

**What you see.** Self › Movement › Fly, a page with four rows:
- **Fly**, a switch. Help: "W A S D move, {up} up, {down} down, {fast} faster." With default keys that reads: Shift up, Ctrl down, Space faster. Toasts "Fly on", "Fly off".
- **Fly Speed**, a multiplier shown as "1.00" (no m/s), as Stand's Levitation. Fly Speed 1.00 = 8 m/s (the old default). 0.01 to 10000.00, Left / Right step 0.10, then round to the other end. Default 1.00. Command `flyspeed`.
- **Sprint**, its own speed while Space (the fast key) is held: Sprint x 8 m/s, so Sprint 4.00 is 32 m/s whatever Fly Speed is. Same range and steps. Default 4.00. Help: "How fast you go while you hold [Space]." Command `flysprint`.
- **Hop Up**, a switch. Default on. Command `flyhopup`.
- **Hop Height**, in the HUD's metres (world units x 1.5), shown as "3.75" (the old 2.5 units). 0.50 to 15.00, Left / Right step 0.10, then round to the other end; typed values too. Command `flyhopheight`. A profile keeps both.
- **Safe Fly**, a switch. Default on. Help: "Turns on God Mode while you fly, until you land."
- Stand While Flying has no row since it is always on while Fly is on (see below).

#### Fly

**How it works.** Position hold.
- `body_addr` (`fly.rs:66`) finds the player's body position through a body root pointer in the exe ([offsets.md](offsets.md)).
- `set` (`fly.rs:95`) starts a worker (`fly.rs:275`) that writes the position every 4 ms, so gravity and the game's own movement can't move the player.
- Fly Through Everything: every frame the game sets the player's body's place through one setter, after its collision and ground snap. That fought Fly's writes: the player got stuck on slopes and objects, slid, and seemed pulled back. One hook on the setter: for the player's body only, while Fly is on, it copies the player's current place into the game's new one, so the game keeps the player where Fly has them. Turning still comes from the game. In with Fly, out with it; the player is held once Fly's 100 ms fall check is over. On a respawn or a world change the game clears the player's body first, and another body could get its old address: Fly lets go at the first tick it can't write the player's body and holds the player again once it can. Found live with the research DLL ([offsets.md](offsets.md) 3.19). Tested in game: no more getting stuck on slopes and objects; live check: the game's own write of the player's place equalled where Fly holds the player in 55 of 55 frames. Other players: the game still runs its own setter for the player, so it hears where the player is; how the player looks to them is not checked (needs a second player).
- The game's own copy of the player's place: each frame the game builds the player's place from a physics object (its +0x50: three floats from the world's origin). While the player was held the game left that copy where Fly came on, and the first frame after Fly off put the player back there. A second hook, where the game reads those floats (exe+0xc73651 in that build), notes the object for the player's body, and Fly's worker writes the copy each tick while the player is held (`noclip::keep_copy`, re-reading the way from the player's body first). Checked from outside the game first (a script kept the copy updated every 5 ms): the snap on land went away. In water the game still moves its copy itself every frame, and Fly off can snap the player back into the water: see [Fly Water Fixes](#fly-water-fixes).
- Movement follows the camera: `facing` (`fly.rs`) reads the camera's right direction; forward on the map is a quarter turn of it (`heading`: (-ry, rx)). The right direction stays level at any pitch, so looking far down or up no longer flies the player's north / east. The forward direction is only the backup; map north / east only when the camera can't be read.
- Shift, Ctrl and Alt count only on the left side of the keyboard (`left_only`, `fly.rs:46`); the right ones stay with the game.
- The fast key uses Sprint in place of Fly Speed. The hop: with Hop Up on, the first press of the up key lifts the player's Hop Height at once, so the game's ground check lets go of the player. Only from the ground: Fly never looks at the ground, so "on the ground" is "still on the spot where Fly found the player's standing" (`on_ground`, `hop_now`, `fly.rs`). In the first 100 ms Fly adds up how far the game still moves the player's height, up or down (`game_moved`, `in_air`): over 0.05 units in all means the player was falling or jumping and gets no hop. A sum, so the frame rate does not matter. Any move (W A S D, up, down, a teleport) is flying: no hop until Fly goes off and on again. With Hop Up off, up from the ground still lifts the player at the normal speed (Fly raises its own spot and writes it every 4 ms); the player may stick or shake for a moment until the game's ground check lets go
- One step is at most 50 units (75 m): a huge Sprint stays very fast (3,000 to 12,000 units a second) but can't throw the player off the map in one step. The log says once per Fly on when it kicks in. Normal speeds never reach it: with one key only above 500 units a second, with a diagonal and up together from about 289.
- While flying, the game doesn't get any of Fly's keys (`dinput::set_fly_keys`, `blocked`, `dinput.rs`): forward, back, left, right, up, down and sprint. So Ctrl doesn't crouch, and the player's body stands still instead of walking or sprinting on the spot. Presses are kept from the game, releases still pass, so no key stays stuck.
- Fly stops by itself when the player's body object changes (a load) or after more than 100 failed writes.
- A teleport while flying moves Fly's target spot too, so Fly doesn't pull the player back.
- The watchdog turns Fly off when the game hangs for 1 s.

**Speed and Sprint rows.** Multipliers, kept to two decimals in 0.01–10000 (`fly::clamp`). A profile keeps them; each inject starts at 1.00 and 4.00. Profiles are written as "Tree Compatibility Version: 3" since (version 2's Sprint becomes Speed x Sprint, rounded to 0.01; no Sprint line and Speed not 1 gets Speed x 4, so the player sprints as fast as before). A version 1 profile's Fly lines are read in the new units (`upgrade_profile`, `menu.rs`): Speed s m/s becomes s / 8, Sprint n (n x 4 m/s) becomes n x 4 / (new Speed x 8), rounded down so never faster. No Sprint line: Speed x 4, as for version 2 (4.00 when Speed is 1). The old settings.json `fly_speed` is carried the same way.

**Who it affects.** The player only.

**Limits and known issues.**
- Erewhon (the hub) has a placeholder body; Fly there is a known gap.
- Turning Fly off in the air: the player falls.

#### Safe Fly

**How it works.** Uses God Mode.
- When Fly turns on, `safe_start` (`fly.rs:181`) turns god mode on, but only if the character is already known (`godmode::on_if_known`). It never arms a wait. It notes that Fly did it.
- If the player switches god mode the player, `forget_god` (`fly.rs:177`) hands it back to the player and Fly leaves it alone.
- If Fly's god hold dropped in a world change, the next Fly start turns it on again.
- When Fly turns off, `safe_end` (`fly.rs:200`) waits in a thread until the player's height stays still for 1 s (at most 90 s), then turns god off. Only if Fly turned it on. On unload it turns off at once.

**Who it affects.** The player only.

**Limits.** No god mode if it hasn't learned the character yet (log: "god mode not learned yet (take one hit first)"). The switch shows no toast.

#### Stand While Flying

**How it works.** Two code hooks (`airwalk.rs`). See also [airwalk.rs](#airwalkrs). There is no switch: the hooks go in when Fly turns on and come out when it turns off or stops by itself (`follow_fly`, `fly.rs`). If they can't go in, Fly still flies and the log says why. Unload still takes them out (`fly::shutdown`, then `airwalk::shutdown`).
- **Fall hook:** returns at once when the fall object belongs to the player's state manager.
- **In-air hook:** when the game stores "in the air = true" on the player's blackboard, the cave stores false instead.
- `the player` (`airwalk.rs:156`) gives zeros unless Fly is on, so both hooks do nothing when not flying.

No toasts since (no switch). The old command `standwhileflying` says "Stand While Flying is always on while Fly is on".

night the fall hook counts the falls it keeps from the player; the count for each flight is in Fly off's summary line (see [Fly Water Fixes](#fly-water-fixes)). Let Game Fall (Experiments) lets the player's falls through while the player flies; the in-air hook stays.

**Who it affects.** The player only. Both caves compare against the player's own character and state manager.

**Limits.** Turning it on mid-fall doesn't stand the player up. Turning Fly off in the air: the player falls.

#### Fly Water Fixes

**Why.** After the player touched water while flying, Fly off could snap the player back: down to the water at once, or pulled back to the shore at the same height for 0.7 s before the player fell. On land the snap is gone since the game's copy of the player's place is kept on the player (Fly Through Everything). The player's wish: "Implement multiple fixes or something if you want and let me experiment".

**The switches**:
- **Wait For Calm** (`flywaitforcalm`): at Fly off, only when the game moved the player's copy in the last 0.5 s (water, the ground), Stand While Flying goes first and Fly Through keeps holding the player, body and copy, until the game has made no move but a plain fall for 0.15 s, at most 1.5 s. Then it lets go. Any other Fly off lets go at once, as before.
- **Snap Guard** (`flysnapguard`): for 2 s after Fly lets go, a throw (one step of over 1 unit, much bigger than the step before) or a pull (in the first second, over 1 unit sideways from the spot at over 8 units a second) puts the player back (where the player was before the throw, or the spot for a pull) and holds the player's until calm, at most 3 times.
- **Let Game Fall** (`flyletgamefall`): while the player flies, Stand While Flying no longer keeps the player's falls from the player. The in-air hook stays, so aiming in the air should stay. The idea (read offline, not proven): the fall function sets the player's movement mode, and kept from the player, swimming may stay after the player flew out of water.

**What it writes.** At a Fly off where the game was moving the player, or with Wait For Calm or Snap Guard on, one line in dll.log ("fly off after: ..."): how many falls Stand While Flying kept from the player this flight, how often the game moved its copy, what the wait or the guard did, and where the player was 2 s later. When the game was moving the player (or threw the player), the flight's last second and the 2 s after go to `flytrace.log` next to dll.log, a line a tick (it stops growing at 10 MB).

**Order.** A teleport, a second Fly off, the freeze guard and the unload end the after phase at once (it lets go of the player); Fly on ends it too and holds the player as usual. God mode's landing check (Safe Fly) starts when the after phase ends.

**Who it affects.** The player only: the player's body and the player's copy; the player's state manager for Let Game Fall.

### Infinite Stamina

**What you see.** Self › Infinite Stamina, a switch. Toasts "Infinite stamina on", "Infinite stamina off".

**How it works.** Code hook (`stamina_code`, `weapon.rs:129`). When the stamina code runs for the player's body, the cave sets the game's "free stamina" byte. On off, `set` (`weapon.rs:351`) reads that byte again and clears it.

**Who it affects.** The player only (owner check: the object's body equals the player's).

### Progress › Editor › Gear Score

**What you see.** Game › Progress › Editor › Gear Score:
- **Dropped Item Gear Score**, 0–1000. 0 = the game's own.
- **Total Gear Score**.
- Header **Weapons**: Primary, Secondary, Pistol.
- Header **Gear**: Head, Hands, Vest, Pants, Shoes.
- Header **Everything You Own**: Set All Owned Items, 0–1000.
- While Aurora › Settings › **Enable Experimental Features** is on, every number here goes up to 1,000,000 and shows the player's note: "Recommended max is 600. If you set an absurd number, use at your own risk." Off, the rows say 0–1000 and nothing more. Turning it off brings Dropped Item Gear Score back to your last number up to 1000; if you had none, a drop score over 1000 left in the game goes to 1000 once you are in the world. Items already set keep their score. Profiles keep only numbers up to 1000.

Toasts: "Dropped item gear score set to N" (or "… is the game's own" at 0), "{slot} set to N", "Everything you have on set to N", "Setting all owned items to N...".

**How it works.**
- **Dropped Item Gear Score.** A value write. `gear_addr` (`menu.rs:980`) points at the drop level in the level root ([offsets.md](offsets.md)). `gear_tick` (`menu.rs:1003`) runs every 100 ms in the world and writes the value again whenever it differs (the value lives in memory; a profile keeps it). It moved into the DLL because the launcher's hold stopped while the launcher was minimised. Every inject starts with the hold off (a new DLL starts that way). Once after every inject and each time Enable Experimental Features goes off, in the world with the hold and the switch off, a drop score over 1000 left in the game goes to 1000 (`gear_cap_check`, `menu.rs:1032`).
- **Slots.** `equipped` (`gearscore.rs:83`) reads the slot table; each item's score sits at a score field ([offsets.md](offsets.md)). `set_slot` (`gearscore.rs:136`) reads the slot again right before writing, so a weapon swap in between gets the new item.
- **Total Gear Score.** Shows the weighted total (`total`, `gearscore.rs:116`). Weights: Primary 25, Secondary 25, Pistol 10, Head 10, Hands 5, Vest 10, Pants 10, Shoes 5. With an empty slot it shows the game's own number. Setting it sets every worn item (`set_equipped`, `gearscore.rs:143`).
- **Set All Owned Items.** `start_set_owned` (`gearscore.rs:188`) runs `set_owned` (`gearscore.rs:214`) on a thread. It scans the writable heap (`heap`, `gearscore.rs:280`) for objects with one of the two item class tags and a score of 2 or more. Each item is checked again right before its write.

**Safety checks.** Set All Owned Items runs with other players near too; it logs how many were near. Values are capped at 1000 (`gearscore::MAX`), or 1,000,000 (`gearscore::HIGH_MAX`) while Enable Experimental Features is on (`gear_max` in menu.rs). Set All Owned Items checks the cap again before each item it writes (`gearscore::cap`).

**Who it affects.** The player's own items. The drop level affects drops in the player's game; in co-op: unknown.

**Limits.** The drop hold is off after every inject until set again.

- Single-item write proved 3 Nov (from outside the game): it survives fast travel and relog. Slots and formula verified 3 Nov.
- The Gear Score page in the menu: no explicit test recorded.
- The DLL-held drop gear score: not tested in game.
- Set All Owned Items: no test recorded.

### Progress › Editor › Credits

**What you see.** Game › Progress › Editor › Credits:
- **Credits**, the balance, 0–2147483647.
- **Pick Other Points**, a page.

A risk page comes first: "Credits live on Ubisoft's servers…". Menu › Settings › Warnings decides when it is skipped.

Toasts: "Credits set to N". "Open a shop now" while the mod hasn't seen the credits object yet. "Credits picked". "Picked; Credits changes that amount now".

**How it works.** Code hook, plus a watch for Pick Other Points.
- At inject, `install` (`credits.rs:95`) puts a cave at the start of the game's points-read function. It only notes the object (rcx) in a 16-slot ring when the caller is the player's credits code. Nothing in the game changes.
- `look` (`credits.rs:149`) finds the credits object from that ring.
- The value is stored scrambled with a key. `key_from_bytes`, `key_of` and `balance_of` (`credits.rs:182–208`) decode it.
- `set` (`credits.rs:363`) writes the new value scrambled, reads it back, and puts the old value back if they don't match.
- If several candidates turn up, a pick page appears.
- **Pick Other Points.** `start_pick_other` (`credits.rs:255`) runs `catch` (`credits.rs:278`): a 20 s hardware watch on the read function, keyed by object, up to 5 passes. It ends 2 s after the shop reads. `others` lists every amount under 100 million, 0 too, highest first. Picking one (`pick_own`, `credits.rs:357`) makes the Credits row change that amount.

**Who it affects.** The player's own points. They live on Ubisoft's servers.

**Limits and known issues.**
- Ghost Coins must never be picked. The list can't tell which value is which.
- 3 Nov: the game crashed joining a raid just after Credits picked 2 objects. Cause unknown.

- Decode and write proved live 1 Nov, in the old window (Credits and keys note).
- Shop hook: the log shows the game ran fine; a test not recorded.
- Pick Other Points' fast list: confirmed by the player 3 Nov.
- The in-game Credits row: "Not tested live".

---

## Self › Weapons

Weapon is no tab any more since its rows are Self › Weapons, same rows, same order, same commands. Hotkeys, saved commands and profile lines on "Weapon › …" follow. Below, "Weapon ›" means Self › Weapons ›.

All seven are code hooks built by `weapon.rs`. Shared parts:
- `prepare` (`weapon.rs:254`) writes every cave into one 1 KB block near the exe (`memory::alloc_code_near`).
- No Recoil, No Spread, Infinite Mag, No Screen Shake, No Scoped Sway, Infinite Stamina and Damage Multiplier jump to their cave from the start of the game function.
- Infinite Reserve and Fire Rate only change the 4-byte target of one call.
- `refresh` (`weapon.rs:334`) writes the player's body and network part into the caves. The watchdog calls `weapon::tick` (`weapon.rs:479`) every second; while any hook is on, it refreshes them.
- Each cave checks the owner first. If it isn't the player, the game's own code runs unchanged.

### No Recoil

**What you see.** Weapon › No Recoil. Toasts "No recoil on", "No recoil off".
**How it works.** `skip_code` (`weapon.rs:109`): when the object's body is the player's, skip the recoil function.
**Who it affects.** The player only.
**Limits.** The first version didn't work (wrong address); fixed in 2c35fee.

### No Spread

**What you see.** Weapon › No Spread. Toasts "No spread on", "No spread off".
**How it works.** Code hook. Acts when the body in rax is the player's.
**Who it affects.** The player only.

### No Scoped Sway

**What you see.** Weapon › No Scoped Sway. Toasts "No scoped sway on", "No scoped sway off".
**How it works.** `skip_code`: when the object's body is the player's, skip the sway function.
**Who it affects.** The player only.

### Infinite Mag

**What you see.** Weapon › Infinite Mag. Toasts "Infinite mag on", "Infinite mag off".
**How it works.** Code hook on the game's ammo question. When the gun item's owner is the player's network part, the cave answers "yes" (al = 1).
**Who it affects.** The player's guns only.

### Infinite Reserve

**What you see.** Weapon › Infinite Reserve. Toasts "Infinite reserve on", "Infinite reserve off".
**How it works.** Call patch plus cave (`reserve_code`, `weapon.rs:167`). After each reload of the player's gun (gun owner = the player's network part), the reserve is set to 500.
**Who it affects.** The player's guns only.

### Damage Multiplier

**What you see.** Self › Weapons › Damage Multiplier, 0.00–1000000.00, shown "x2.50". Left / Right step 1.00: Off, x1.00, x2.00 ... up to the top, then Off again; Left from Off is the top. Typed values keep two decimals (x2.37 steps to x3.37). 0 = off. Fire Rate steps the same way. above 100.
**How it works.** Code hook (`damage_code`, `weapon.rs:145`; set at `weapon.rs:407`). When the shooter (r14) is the player's body, the damage (xmm0) is multiplied by the factor.
**Who it affects.** Shots fired by the player. It changes the damage to whatever the player hits.

### Fire Rate

**What you see.** Self › Weapons › Fire Rate, 0.00–1000000.00, as Damage Multiplier. 0 = off. Guard: the wait between shots never goes under 2 ms (`FIRE_MIN_S`, a `maxss` in the cave), so a huge value can't stall the gun; past a few hundred times, every gun is at that floor and a bigger value changes nothing. above 20.
**How it works.** Call patch plus cave (`fire_code`, `weapon.rs:199`; set at `weapon.rs:459`). For the player's gun (gun handle → item → owner = the player's network part), the wait between shots is multiplied by 1 ÷ the value.
**Who it affects.** The player's guns only.
**Limits.** Likely capped at about one shot per frame.

### Silent Aim

**Where.** Aurora › Experiments › Silent Aim, shown while Aurora › Settings › Enable Experimental Features is on (it was in Self › Weapons until then). Magic Bullets, No Range Loss and Instant Hit are in the same list. Its settings are not kept in profiles: they start at their defaults at every inject and profile load.
**What you see.** Self › Weapons › Silent Aim (switch), Silent Aim FOV (1–90 degrees, 10 at inject, shown "10 deg"), Silent Aim Range (10–1000 m as the player's HUD counts them, 300 at inject). Toasts "Silent aim on", "Silent aim off". Commands `silentaim`, `silentaimfov`, `silentaimrange`. Bindable to hotkeys like the other rows; not kept in profiles since (see Where). Off at every inject.
**6 Nov 2026 changes.** Self › Weapons › Silent Aim is a list: Silent Aim, Silent Aim FOV, Silent Aim Range, Show FOV Circle, Priority. FOV is now the cone's full width, 20 at inject (same cone as the old 10); a white circle round the crosshair shows it (`vkdraw::circle_px`: tan(FOV/2) / tan(camera FOV/2) × half the picture height, live camera FOV so it grows when scoped), and the picker uses exactly that cone (unit test `the_circle_is_the_cone`). Priority: Crosshair, Nearest or Farthest, always inside the circle and range. Animals are left out: a "soldier" must have a readable side that is not friendly (animals have no soldier character; read in code, not proven live; the log line `silent aim target: ... type... side...` shows each pick). Aim point: the head offset from the ESP's last bones added to the body's live position (was the head itself, up to 250 ms old). Drop: the aim goes up by fall = k·d² (d = distance to the point), k = g/2 / speed² from physics (g/2 = 4.905 world units), with the speed of the player's last shot (`shot+0xC`; 950, the VHSD2's, until one was seen). It works from the first shot. Each steered shot is still watched as it flies (`shot+0x140` origin, `+0x190` position) and its measured fall is logged, but it no longer changes the aim. Every steered shot logs `silent aim shot N: origin, camera, game dir, ours, speed, range, drop k, target, how far ours passes above and across it` (30 lines per 10 s at most).
**How it works.** `silentaim.rs`.
- The cave sits at the start of the function every bullet's origin and direction go through (`0x3652430(shot, float4* origin, float4* direction)` in build 25349072; `research\docs\shot-trace.md`). For a shot whose shooter (`[shot+0x90]` → entity) is the player's body (`[BODY_ROOT]`, read each call), it calls the function with the direction `normalize(target − origin)` from a frame of its own. The game's own direction is never written to. Anyone else's shot, or no target, runs the game's code unchanged.
- A worker (every 25 ms; the world list every 100 ms) picks the target: of the AI soldiers (type & 7 = 2, not dead, side not friendly) and drones (type & 7 = 5, not dead) within the range, the one nearest the crosshair inside the FOV angle. Aim point: the head when the ESP draws that body with bones (`vkdraw::head_of`, a frame at most 250 ms old), else the chest (1.3 units over the body position); a drone's middle.
- **No player gate:** it used to refuse to switch on, and switch itself off, with any other player within 1000 world units. Gone: co-op PvE with friends is fine. Players (type & 7 = 1) are still never candidates: left out of the world-list pass, and a body re-read as a player is never picked (unit test `coop_friend_on_the_crosshair_is_never_picked`). Not for PvP (Ghost War); the mod cannot tell a PvP match apart.
**Who it affects.** The player's shots only; the targets are AI soldiers and drones. Never a player: players are never picked. A co-op partner's shots are the game's own.
**Limits.** Bullet drop is raised from physics (speed of the player's last shot), not measured (and not at all while No Bullet Drop is on); no lead for a moving target; walls are not checked (a target behind cover takes the shot into the cover); a friendly NPC is skipped only when its side reads 0. The shot's path points are made before this call and turned into the world with this direction: likely, not seen live. DX11 build: the pattern was not checked there (no match = the switch says so and stays off).

### No Bullet Drop

**Where.** Aurora › Experiments › Silent Aim › No Bullet Drop (switch, off at inject; command `nobulletdrop`). Not kept in profiles, like everything in Experiments. Toasts "No bullet drop on" / "No bullet drop off".
**What it does.** All the player's shots fly straight along the aim line (pitch kept), with no fall over distance. Silent Aim on or off: with it on, steered shots are no longer aimed high for drop (`Drop::raise` stands down), so they go where the aim point is.
**How it works.** `silentaim.rs`, `flatten_code`. When the game builds a shot (`0x365af00`), the weapon's ballistics profile picks a path builder by a mode number: mode 2 builds an arc (`0x3658be0`: for each point, along = L·t, drop = v·sin(a)·τ − 4.905·τ², side = spread; a is a launch angle that is 0 for guns), anything else a straight line (`0x3658840`). The points are local `float3`s (horizontal distance along, height, side; 12 bytes) at `shot+0x160` (pointer) with their count at `+0x16a`; the height holds the aim's pitch (x·tan P) and the gravity curve. The shot build (`0x3652430`, where Silent Aim's cave sits) then turns them into world points (`+0x16c`, count `+0x176`) by the yaw, and the per-frame mover (`0x36b1c30`) only slides along those points: it adds no gravity of its own. So the drop is nothing but the curve in the local heights. The cave, before it calls the game's function, rewrites every local point's height to the first point's plus its distance along times the direction's rise over run (`dz / sqrt(dx² + dy²)`, from the direction the call gets) when the feature word has bit 8 (`FEAT_NODROP`): about 110 bytes of SSE (`flatten_code`), writing only inside the player's own shot object, in a list the game sized. Nothing is done with fewer than two points, no pointer, or a direction straight up or down. (A first try set every height to the first point's: shots then flew level wherever the aim pointed, which showed the height holds the pitch.)
**Who it affects.** The player's shots only (the cave's shooter check). Everyone else's shots are the game's own. It aims at nobody, so it has no player guard.
**The hook.** It rides Silent Aim's jump at the shot-build entry. With Silent Aim off, switching No Bullet Drop on puts the jump in by itself (the paused swap, as Silent Aim does) and writes the feature word with its bit alone; Silent Aim off then leaves the jump in while No Bullet Drop wants it; No Bullet Drop off takes it out unless Silent Aim is on. Unload takes it out with Silent Aim's.
**Limits.** Built from the decompiled path builders and mover (research\docs\shot-trace.md). Ammo with a launch angle (launchers) flies along the aim line too. Co-op: whether a host simulates the shot with drop is unknown. The `silent aim track` log line shows "fell" for each watched shot; with the switch on it should read about 0.

---

## World

### Teleport

**What you see.** World › Teleport:
- **Where Am I**: the player's position in world units, "x, y, z".
- **Go To {spot}** for each saved spot.
- **Save This Spot**.
- **Undo Last Teleport**.
- Header **Coordinates**: X, Y, Z, then **Go To Coordinates**. Where Am I shows two decimals.

Toasts: "Teleported to {name}", "Saved as spot N", "Back where the last teleport started".
The command box also takes `tp x y z` (`menu.rs:2690`).

Spots are text files, `%APPDATA%\Aurora\Places\<name>.txt`, with `X:`, `Y:` and `Z:` lines (`spots`, `menu.rs:624`; add, remove, rename at `menu.rs:699–723`). The typed coordinates are profile rows like any other; each inject starts at 0, 0, 0.

**How it works.** Position writes (`tp.rs`).
- `to` (`tp.rs:26`) refuses a target that isn't a finite number or is beyond 100000.
- It calls `fly::jump` first, so a running Fly doesn't pull the player back.
- It writes the position 30 times, 10 ms apart, waits 150 ms, and checks the player landed within 5 units sideways. Up to 3 tries.
- It stops writing if the body object changes, because that means a load started.
- It counts jumps, so the ESP waits 10 s before rescanning.
- It remembers the start point. `undo` (`tp.rs:76`) goes back there. Undo twice and the player is back where the undo started.

**Who it affects.** The player only.

**Limits.** Undo remembers one teleport only.

- Proved live 30–31 Oct (Step 2); saved spots work.
- 31 random teleports, no GPU reset.
- The Coordinates rows and Places files: no explicit test recorded.
- Teleport while flying and stop-on-load: not tested in game.

### ESP

**What you see.** World › ESP:
- **ESP**, a switch.
- **Range**, 25–1500 m (game metres).
- **Targets**: Hostile, Friendly, Players, Drones, Hide Dead.
- **Drawing**: Style (Box or Bones), Lines (0–6 px), Black Edge, Distance, and a Colours page with Hostile, Friendly, Players and Drones, each with a Hex Code row.

**How it works.** Reads the game's memory and draws on the frame. It changes nothing in the game.
- **Drawing.** The code is in `dll/src/vkdraw/`. On Vulkan, `vulkan.rs` hooks three calls through `detour`: present (`hk_present`), acquire-next-image, and create-swapchain (`hk_create`). The create hook adds the "transfer destination" flag, so the mod may copy into the game's images. `nudge` gets the game to remake its swapchain with that flag. `draw` only draws into swapchains that have the flag. Every line is a copy of a 1×1 coloured image, so no shaders are needed. DirectX 11: see [dx11.md](dx11.md).
- The `Hook:` line in the Launchpad's Settings.txt (`%LOCALAPPDATA%\Borealis\Aurora.Launchpad`) picks where to hook: Auto, Driver or Off.
- **Finding NPCs.** `scan_loop` (`mod.rs`), using `entities.rs`. It stays quiet 10 s after a teleport or a jump.
  - (instant ESP): the game's own list of every entity in the player's world, read every 200 ms (`WorldList`). A new body shows within about a quarter of a second.
  - A body's marker comes from its own components (the game's link), checked by class and back pointer.
  - The old marker scan stays: every 5 s while a character has no marker from the list (its marker is taken then), and a full one every 30 s as a check.
  - When the list can't be used (no world, looks wrong, empty, no characters): the old scan as before, every second, a full one every 10 s.
  - `WORLD_LIST = false` in `entities.rs` turns the list off.
- **Each frame,** `boxes` (`mod.rs`) checks every NPC's class tag and body again, projects it to the screen, and:
  - skips the dead if Hide Dead is on;
  - drops anyone beyond Range (Range ÷ 1.5 → world units, 10–1000);
  - filters by side (`side_of`, `mod.rs`): side 3 is hostile, side 0 friendly;
  - draws other NPCs always, in the NPC colour;
  - draws the picked target in white;
  - shows distance labels in game metres (world × 1.5).
- **Bones, P path first**. Bones come from each body's own current pose. The scan finds the body's animation owner (`ppath::anim_owner`). Each frame `ppath::skeleton` reads P = [owner+0x200] again (it swaps between 3 pose objects), checks it is this body's (owner+0x10, within 0.5 units), gets the 18-bone layout from a cache per skeleton [P+0x240] and reads [P+0x248]. Same stretch check, last good frame and `place` as below. Level 0 for 10 frames skips it. Any failure: the old route below. `P_PATH` (`mod.rs`) turns it off.
  - By name first. The skeleton's bone table [S+0x70] has, per bone, its parent and the CRC32 of its name. The 18 bones are looked up by name once per skeleton, each checked against the bone that must be above it (at most 4 steps up). The head point is the top of the head: 0.14 up the Head bone, in its own frame. A missing name or a table that looks wrong: by lengths (`rig_layout`) as before.
  - Why (read live, 8 bodies in a hub): `rig_layout`'s head point is whichever head bone sits highest. On 1 of 6 bodies that was the top of the head; on 5 the back of the skull, the forehead or a helmet bone, 9 to 14 cm from the top, and on 2 it changed between reads. So the neck line and the ring sat at a different spot from head to head. On two level-3 players the game also left 268 bones behind, the top bone among them (12 to 25 cm off); the 18 named bones and the Head kept their place on all 8. `rig_layout` failed on 2 of 3 level-3 bodies, and it fails when the pelvis is under 0.2 up, as when lying down (read in code).
  - The heartbeat counts `laid out by name` and `by lengths`.
- **Bones.** `harvest_poses` (`mod.rs`) reads, every 2 s, the pose objects that [posehook.rs](#posehookrs) collected. `rig_layout` and `rig_skeleton` (`mod.rs`) match a pose to an NPC within 0.5 units, take the latest one that moved, and skip torn frames by checking limb lengths. `place` (`mod.rs`) does a pelvis sanity check. An NPC without a matching full pose stays a box.
- **Head ring**. Every skeleton gets an outline circle around its head: same colour, same Lines thickness, same black edge. Box style is unchanged; no new row. `head_ring` and `Ring::draw` (`mod.rs`):
  - Centre on the P path: the middle of the head in the Head bone's own frame, 0.07 up (eye height) and 0.02 forward, so it turns with the head. Not a number, over 0.4 units from the chest, no Head by name, or the old route: placed as below. The heartbeat counts `ring from Head`.
  - Otherwise: `HEAD_RING_OFFSET` (−0.08 units) from bone 15 along the chest → head line, so it tilts with the head. On a full rig bone 15 is the top of the face block (laid out by name: the top of the head), so the centre goes back towards the chest.
  - Radius: `HEAD_RING_RADIUS`. The centre and a point one radius away along the camera's up are projected, so the ring shrinks with distance like the bones.
  - The chest → head line stops at the ring's edge; when the chest is inside the ring on screen it is left out.
  - Under 2.5 px radius it is one square dot. Otherwise 8 to 24 sides (more on bigger rings), drawn with the same `line` as the bones: about 2.35 × radius + sides rectangles per colour (10 px: about 36, 50 px: about 141).
  - Each side runs half a pixel past both corners, so 1 px rings have no holes where the sides meet.
  - No ring when the head is behind the camera, off screen, taller than the screen, or not finite numbers.
  - Neither backend caps rectangles (one blit region or one ClearView rectangle each). So rings have their own budget: 8192 rectangles a frame, both colours. Past it the rest of the frame's skeletons (nearest first) get no ring; bones are never dropped.
- **Drones**. The same world-list pass keeps them (`WorldList::drones`, `entities.rs`):
  - A drone is main type 5 in the type word (+0x108 & 7), any kind (bits 10–13) but the world's own, 1 (`drone_type`). Read live next to a ground drone the player watched drive, then shot: 0x1115 (53 components, a 3.2 × 2.9 × 1.3 box). Flying drones: 0x110d (21–25 components, a box about 1 × 1 × 1 near, none far away); two the player saw were both that. Left out: its 3-component shadow 0x515 (kind 1, same spot) and the 3 parts on it (main 6, 0x1106, 1.3 up). Players' drones may show too. Planted turrets are left out too: they are main 7 (0x1117, 9 components, dead 0x5117) with 4 main-6 parts at the same spot.
  - Each pass: re-read like the characters (handle, world, own slot and handle, still main 5), the nearest 32 within reach, live ones first (shot ones stay listed and must not push a live one out), handed to the drawing. The kind check runs both ways: a handle that was something else at first (type 0, or a freed prop's memory reused) is looked at again once it is a drone.
  - Each frame `entities::drone_at` reads each again, then `draw_drones` (`mod.rs`) draws a diamond (`diamond`, `Diamond::draw`) in the Drones colour, with the black edge and the distance under it, after the characters. Middle and size from the entity's box (+0xDC, f32 min x y z then max) when each side is 0.1–40 units: the middle at the box's mid height, the size (middle to a corner) a sixth of its three sides, 0.4–8 units; else its position and 0.9. On screen at least 5 px, none bigger than half the screen.
  - Shot down: the game keeps it listed with the dead bit (0x4000; the player shot one, 0x1115 → 0x5115). Hide Dead hides it.
  - Own budget: 8192 rectangles a frame. The heartbeat says `drones drawn N (listed M)`.
  - After a jump (teleport or fast travel) or a scan crash the list is dropped with the characters' (`forget_seen`), so no diamond draws while the game loads.
  - The last three: review finds, fixed.
- At every inject the ESP switch starts off (`start`, `menu.rs:1954`). Style, colours and targets start at their defaults too; the Load On Inject profile puts the saved settings back.

**Who it affects.** Draws every NPC (and other players with Players on). Changes nothing in the game.

**Limits and known issues.**
- Most far NPCs stay boxes. NPCs 60 m and further have no full pose.
- The dead bit, sides and markers rest on small samples (ESP reference).
- The NPC colour and the target colour can't be set in the menu. NPCs of other sides can't be filtered out.
- A ghost-entry fix is built, not tested.

- Boxes line up: proved live. Scope ESP confirmed 2 Nov.
- GPU fix proved live: the game remade its swapchain with the flag; 0 unsafe frames.
- Bones proved live on close NPCs 2 Nov, with the earlier hardware-watch pose catch.
- The code-hook version (4cec053) was "Untested" when built 2 Nov. Later the player confirmed the Box/Bones switch.
- Head ring:.; where bone 15 sits on the head is read from code, not seen.
- Drones: the rule is read live on one ground drone (alive and driving, then shot: the dead bit) and matches the game's own "main 5" check; flying drones are 0x110d. The drawing: unit tests, then the player's try with build 14: the heartbeat said `drones drawn 312 (listed 2)`, 0 unsafe frames.
- Bones by name and the ring's centre from the Head bone: unit tests, with numbers read live from 8 bodies (read only, nothing drawn).

### Target

**What you see.** World › Target:
- **Pick Target**. Help: "Look at an NPC, then press Enter."
- **Yeet Target**, dimmed until a target is picked.
- **Bring Target**, dimmed until a target is picked.

Toasts: "Target: {distance} m away", "Yeeted", "Brought over".

**How it works.** Position hold on one NPC (`entities.rs`).
- `aim` (`entities.rs:367`) picks the NPC closest to where the camera looks, within 12°, among NPCs within 600 world units. Only NPCs. The pick is saved in `%APPDATA%\Aurora\Dumper\target.txt`.
- `yeet_target` (`entities.rs:415`) moves it 50 units up.
- `bring_target` (`entities.rs:420`) moves it 2 units in front of the player, 0.2 up.
- Both use `move_body` (`entities.rs:297`): a guarded hold, one at a time, kept for 2 s.

**Who it affects.** The one picked NPC.

**Limits.** The distance toast is in world units, though it says "m".

---

## Game

### No Screen Shake

**What you see.** Game › Disables › Screen Shake. Toasts "No screen shake on", "No screen shake off".
**How it works.** `skip_code` (`weapon.rs:109`): when the object's body is the player's, skip the shake function.
**Who it affects.** The player's camera only.

### No NVG Overlay

**What you see.** Game › Rendering › No NVG Overlay. Help: "Works from the next time NVG turns on." Toasts "No NVG overlay on", "No NVG overlay off".

**How it works.** `nvg.rs`.
1. **Code patch** (`prepare`, `nvg.rs:60`): the instruction that sets the overlay flag to 1 now sets it to 0. One byte changes.
2. **Code cave**: at the spot where a loading teleport sets the flag, the cave writes 0 instead.

`set` (`nvg.rs:135`) puts the first back if the second fails. There's no owner check: it is a screen effect in the player's own game.

**Who it affects.** The player's screen.

**Limits.** Thermal vision is not covered. Already-on NVG keeps its overlay until it turns on again.

Since 4 Nov Screen Shake sits in Game › Disables. NVG Overlay moved on to Game › Rendering; hotkeys, saved commands and profile lines on its old addresses follow it. NVG In Erewhon stays in Disables (a hub restriction).

### NVG In Erewhon

**What you see.** Game › Disables › NVG In Erewhon. Toasts "NVG in Erewhon on", "NVG in Erewhon off".
**How it works.** `nvgzone.rs`: two code hooks in the character's action tree, for the player's character only. In the safe zone the goggles go on the player's face like outside (others see them).
**Who it affects.** The player's own character.

---

## Menu

The menu's own pages are covered in [menu.md](menu.md). In short:
- **Tabs**: Self, World, Game, Aurora. Weapon became Self › Weapons; Game is Disables, Rendering, Progress.
- **Lists on top** of every page, except where the player set the order: Profiles, Credits, ESP, Find Command and the "O" pages. Self (Movement, Weapons, God Mode, Infinite Stamina, Undetected) and Notifications changed.
- **Settings**: Appearance (Position, Colours, Tabs, Address Bar, Notifications, then Max Visible Commands, List Width, List Height, Spacer Size), Notifications (Suppress Generic Responses), Warnings.
- **Appearance size rows**. List Width (`listwidth`, 0–32767, default 450) and List Height (`listheight`, one row's height, default 32): 450 / 32 is exactly the menu as it was at Menu Size 85%. They scale everything: text, tabs, address bar, help boxes, the command box and where the mouse clicks. Max Visible Commands (`menuheight`, 3–255, default 10): rows before the list scrolls, its own thing. Spacer Size (`spacersize`, 0–50, default 3): the gap between the menu's parts. Old Menu Size lines become List Width / Height that look the same, and scale the notifications' Size and Text Size, which used to follow it.
- **Appearance › Position**: X and Y in screen pixels (commands `menux`, `menuy`, -32768 to 32767, step 1, typed values too), and Move With Mouse. Evening: X is the menu's right edge, the wall; Y the address bar's top. The list and the address bar end on the wall, the tabs sit left of the list, help boxes under the tabs. A wider list, tabs or bar grows to the left only; what would go off the screen's left edge is cut there, the wall never moves. Not set: the right edge of a 16:9 screen, where the list ended before. Old X values (the address bar's left edge, profile version 3) get the bar-to-wall width added once, so the player's menu stays put.
- **Mouse**: only with Aurora > Settings > Mouse Support on. Then, while the menu is open and the game in front, the game gets no mouse at all; a pointer shows. The row under the pointer is picked. Left click: Enter on a row, or switch tab. Right click: Back, anywhere. The wheel only scrolls a long list. Menu closed or game behind: the mouse is the game's again. Keys work as before.
- **Notifications**, split like Stand's. Settings › Notifications holds only Suppress Generic Responses › Command Box and Hotkey (the Menu switch went: doing something by hand in the menu never shows a plain "on" / "off" / "set to"; errors, warnings and lines like "Profile saved" still show, except that Command Box on hides everything after the command box, errors such as "No command called x" too). Appearance › Notifications holds how they look: Custom Position (X / Y pixels, the top-left), Timing › Time, Position (Bottom Left, Bottom Right, Top Left, Top Right, Custom), Width (the widest before the text wraps), Size (the box), Text Size, Border Colour, Background Colour, Flash Notification (a test one). Hotkeys, saved commands and profile lines on the old addresses follow. Invert Flow, Padding and Flash Colour went; old lines are ignored quietly. A notification fits its text snugly. Several show at once, stacked, as many as fit. A live preview shows while these pages are open. Commands are `notify...` now (`toastposition` / `toastcolour` before).
- **Warnings**: Automatically Proceed On Warnings › Command Box ("Includes the command box.", also `command.request`) and Hotkey: no "are you sure" page when the action comes from there. Force Me To Read Warnings (on): Yes shows 2 s after the question (the credits page counts as read then). Off: no warning page at all; Unload, Credits, Experiments and a profile's Delete go ahead. Warning text can never be picked, highlighted or hovered. Skip Already-Accepted Warnings (on): a warning answered Yes once since inject is not asked again. From a hotkey or the command box the question now opens the menu, so it is seen. An old Disable Warnings On becomes both Automatically Proceed switches and Skip Already-Accepted on.
- **findc**: `findc esp` in the command box opens Aurora › Find Command: esp, every row whose shown name has the word in it, working as where it lives; the help line shows where it lives. A list that matches is listed without its rows.
- **save<name> / load<name>**: `savemain` saves profile Main, `loadmain` loads it (any case, spaces in the name left out). Another command's name wins (`savethisspot`). An unknown name says "No profile called...".
- **Profiles**, with Load On Inject. A switch that needs the world waits with a ring until the player's character loads ("Enables once your character loads.", `needs_world`, `menu.rs:3895`).
- **Hotkeys**, from `Hotkeys.txt` (`row_hotkeys`, `menu.rs:3119`). Default keys: menu F4, command box Y, Fly keys as above.
- **Experiments**: untested switches. Shown again, behind a warning page: "Experiments are untested. They can crash your game or cause unwanted behaviour." then Yes. Its row's help: "Untested. Can crash your game or cause unwanted behaviour." Today: Ignore only me (untested), and the three [Fly Water Fixes](#fly-water-fixes) under their own title: Wait For Calm, Snap Guard, Let Game Fall, all untested and off by default. Any page can be locked by adding it to `HIDDEN` (`menu.rs`): its row grays out, the cursor skips it, nothing opens it.
- **Commands**: Apply Default State is `default`, Unload is `unload`; Commands With Non-Default State and Move With Mouse have none. The old names still work quietly.
- **Help boxes**: one box can hold several lines. Fly's is four ("W A S D move / L Shift up / L Ctrl down / Space sprint", the keys as bound); Pick Other Points' three; long help is split into short lines. Hotkey rows say "Click to change."; Pick Target has no help.

- **Saved Commands**, and **Commands With Non-Default State**.
- **Apply Default State** (`default_all`). Defaults (`default_of`): Safe Fly on, Fly Speed 1.00 and Sprint 4.00, Hop Up on and Hop Height 3.75, ESP Hostile, Friendly, Players and Hide Dead on.
- **Value rows**: Left / Right keep each row's step and stop at its ends; one more step at an end goes round to the other end (`stepped`, `limits` in `menu.rs`). The "O" menu of a value row has **Set To Min Value** and **Set To Max Value** (Credits read their warning first).
- **Save State / Load State**, in the "O" menu of every row with a state, and of a list (all rows inside): writes only that row's line into the active profile (last loaded, saved or created; else the Load On Inject one), or sets it from there. No line: a toast says so. Experiments rows: Save State writes nothing ("Experiments are never saved") and Load State puts them at their default.
- **Unload Aurora**: the mod unloads itself (`unload_now` -> `server::unload_self`): it sends `Bye` to its own pipe, shuts down, and frees its own DLL. The Launchpad never unloads.

---

## Background systems

### freeze.rs

The value-hold engine.
- One worker thread rewrites every hold every 15 ms (`worker`, `freeze.rs:87`).
- `set` (`freeze.rs:44`) starts, replaces or (with no bytes) stops a hold.
- `set_guarded` (`freeze.rs:49`) adds a guard: if the guard bytes change, the object was freed or replaced, and the hold stops. A failed write stops it too. Log: "freeze {id} dropped: address went away".
- `running` (`freeze.rs:69`) lets God Mode notice its hold dropped.
- `clear` (`freeze.rs:78`) drops every hold. The watchdog uses it after a 6 s hang.

Used by God Mode ("god") and Target ("move-<body>").

### hwbp.rs

Hardware watchpoints.
- Sets debug register DR0 on every other game thread (`set_on_thread`, `hwbp.rs:233`: suspend, get context, set, resume).
- A named mutex, `Local\aurora-hwbp-<pid>`, is shared with the research DLL, which uses DR1.
- A vectored exception handler, first in the chain (`handler`, `hwbp.rs:147`), checks that DR6 says it was the mod's breakpoint. For "run" catches it sets the resume flag so the instruction can go on. It records the instruction address, the registers, and the return address on the stack. It keeps one hit per spot and key register, at most 256.
- `find_writers_until` (`hwbp.rs:289`): only one watch at a time; a new watch cancels the old one. Clearing happens twice, with a stopping flag, after a thread put the watch back on itself while it was being cleared.
- `cancel` (`hwbp.rs:132`), `shutdown` (`hwbp.rs:138`).

Used by God Mode (learn and arm), Credits and Pick Other Points.

### watchdog.rs

A worker that runs every second (`worker`, `watchdog.rs:92`).
- Calls `weapon::tick` and `airwalk::refresh`.
- Asks Windows whether the game's biggest window is hung (`IsHungAppWindow`).
- Hung for 1 s: writes a report, turns Fly off, cancels any hardware watch.
- Hung for 6 s: drops every value hold (`freeze::clear`), God Mode's too.

### entities.rs

Finds characters in memory: the game's world list first, the marker scan as the fallback.
- `WorldList` reads the world list. The player's world is the player's body's +0x18; the list is at +0x3F0 (handles), count +0x3F8, room +0x3FC. Checks: the world's +0x3E8 points back at it; each entity's own slot (+0x144), handle (+0x160) and world.
  - A slot that holds the same handle as last pass is not read again (except the last 64 and a rolling window of 256). Characters (type +0x108 & 7: 1 player, 2 NPC) and drones are read every pass.
  - Marker: a component's +0x28 is a container; its index table (in the exe) gives the slot, 0x67 for NPCs, 0x28 for players.
- `all_blocks` (`entities.rs:98`) lists private, writable memory blocks up to 512 KB. It skips thread stacks (guard pages) and GPU memory (no-cache or write-combine).
- `marker_class` (`entities.rs:140`) tells a character object by its class tag.
- `owner_check` (`entities.rs:155`) compares an object's body with the player's.
- `my_character` (`entities.rs:164`) finds the player's character from the player's body.
- `near` (`entities.rs:194`) keeps known objects and adds new blocks; a full scan every 10 s; a 1 ms pause every 64 MB.
- `move_body`, `aim`, `target`, `yeet_target`, `bring_target`: see [Target](#target).

### airwalk.rs

The two hooks behind [Stand While Flying](#stand-while-flying).
- `refresh` (`airwalk.rs:171`) writes the player's ids into the cave slots every second (from the watchdog) and at every Fly switch.
- `the player` (`airwalk.rs:156`) returns zeros unless Fly is on.
- `set` (`airwalk.rs:192`) puts the first hook back if the second swap fails.

### posehook.rs

Collects pose objects for Bones ESP.
- `prepare` (`posehook.rs:36`) builds a cave at the start of the game's pose function. It stores the pose object (r8) in a ring of 8192 slots. A locked add (`lock xadd`) gives each thread its own slot. Then it runs the instruction it replaced and jumps back.
- `set` (`posehook.rs:76`), `off` (`posehook.rs:106`), `objects` (`posehook.rs:116`).
- On only while the ESP draws Bones.
- It replaced a hardware watch that crashed the game during busy pose catches.

### Safety helpers (memory.rs)

- `read` (`memory.rs:81`) and `write` (`memory.rs:103`) go through ReadProcessMemory and WriteProcessMemory on the game's own process. Touching freed memory then fails with an error instead of crashing the game.
- `readable` (`memory.rs:63`) asks Windows (VirtualQuery) before touching an address.
- `swap_code_u64` (`memory.rs:128`) changes code as one aligned 8-byte atomic swap. It checks the old bytes first, puts them back and refuses if they differ ("left alone"). Since 4 Nov only one swap runs at a time.
- `alloc_code_near` (`memory.rs:157`) gets cave memory within 2 GB of the exe, so a 5-byte jump reaches it. It is never freed.
- `scan_module` (`memory.rs:174`) searches region by region, so the copy protection's unreadable holes don't stop it.
- `find_module` (`memory.rs:50`) caches the exe's address.

### Start and stop (lib.rs, server.rs)

- At load, a thread warms up Undetected, the weapon caves, No NVG Overlay, Stand While Flying, Fly Through Everything and Credits (finds their code). Then God Mode starts learning (up to 10 minutes), the watchdog starts, and the named-pipe server starts.
- On "Bye" (sent only by the mod's own unloader, from menu › Unload or a test script's `unload.request`), `server.rs` shuts down in order: menu, ESP, watchdog, Fly, Undetected, weapon, NVG, Stand While Flying, Fly Through Everything, Credits, Gear Score, God Mode off, holds. Then the start thread frees the DLL with `FreeLibraryAndExitThread`.
- The Launchpad only pings the mod after inject. When it closes, the mod keeps everything.

---
