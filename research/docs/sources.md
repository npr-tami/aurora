# Outside sources

Checked (web search; some sites blocked the search). Trust: H high, M medium, L low.
Public Breakpoint reverse-engineering material is scarce: expect to find most things first-hand.

| Source | What | Trust | Use |
|---|---|---|---|
| AnvilToolkit (Kamzik123; Nexus: nexusmods.com/site/mods/455; wiki github.com/Kamzik123/AnvilToolkit-Resources) | Unpacks/repacks Anvil `.forge`/`.data` archives; lists Breakpoint as supported (not Wildlands). Closed source, needs.NET 5 desktop runtime | M | Game data side: entity/data records, skeletons (Bone ESP note used it). Not memory work |
| reshax.com topic 731 | Breakpoint model extraction via AnvilToolkit to Blender | L–M | Asset extraction only |
| GamePressure "Game-Manuals" Cheat Engine table (Dec 2023) | AOB scripts for Breakpoint | L–M | Signature ideas only; read the.CT as text, never run it |
| Nexus readme for an older CT | Needed anti-cheat disabled (pre-Sept-2023) | — | Off limits (anti-cheat), noted only |
| Steam discussions (Jan 2023, Sept 2023 patch) | User claims about protection; BattlEye removal patch sizes | L | Context only |
| pelite crate (`msrtti` example) | MSVC RTTI walking in Rust | H | Reference for grbstatic's RTTI code |
| iced-x86 crate | x86-64 decoder/formatter, MIT | H | Used by grbstatic |
| ClassDumper (GrandpaGameHacker), PyClassInformer | RTTI/vtable dumpers | M | Technique reference |
| Ghidra (NSA, Apache-2.0) | Decompiler, headless scripting, Version Tracking | H | Adopted: `research\ghidra\ghidra.ps1` |
| renderdoc.org docs | Vulkan/D3D11 capture via implicit layer | H (general) | Untested with Breakpoint; maybe for rendering questions |
| github.com/dataterminals/grb-modding-knowledgebase | Breakpoint data modding: `.forge`/`.data` formats, ATK, gameplay DB records, Python read tools. No licence: learn from it, don't copy its code or text | M–H for file formats (they mark static reads "Verified"), L for meanings ("Inferred") | See "Taken from the knowledgebase" below. Its forge editing / repacking workflow is off limits here |
| github.com/mshortt96/ForgeDuplicateHelper | C# helper for ATK repacks (duplicate files) | — | File modding only; not used |


## Taken from the knowledgebase
- **Build string**: `ChangeList:7793716;Version:E450_1.24;User:silex-buildfarm;Branch://tgt-streams/Y…`
  in GRB_vulkan.exe at rva 0x50c7fa0 (checked with `grbstatic strings`). A useful label,
  but not a build gate on its own: they think a Steam re-release kept the same ChangeList while
  the credits pattern changed in Oct 2026. Keep size + SHA-256 + signatures as the gate.
- **livepatch**: per their reading of the exe, the game deletes unknown files in
  `%LOCALAPPDATA%\My Games\Ghost Recon Breakpoint\livepatch` at start. Never put mod files there.
- **Enemy perception is several records** (their docs 14 and ai-db-records; fields read from
  game files, meanings inferred, nothing tried in game): sight, hearing (raw radii like
  10/15/25/150 m), a detector config, a perception config, and an AI "cheat" (omniscience)
  config; the game also ships "off" versions (`_NoDetection`, `NoPerception`, `NoCall`). The mod's
  inference, untested: Undetected's sight patch covers sight only, so close-range spotting
  (1–10 m) may be hearing. Candidate lead for the Detection investigation.
- **NPC health records** are templates (48 records, 408 B), not live health.
- **Item names**: their `id_census.py` maps a 64-bit Anvil ID to forge/type/name, read-only
  (needs Python, numpy and the game's own `oo2core_7_win64.dll`). The item id at +8 (0x4df39) is
  not one of those IDs. Lead: look in item objects for a 64-bit ID, then name it.
- **AnvilToolkit warning**: its unpack writes `Extracted\` and `Backups\` into the game folder.
  Found there on 4 Nov. The game's
  own files are unchanged.
- Nothing there on class levels/XP or per-player gear score.

Unverified either way: whether a specific DRM product is in the exe and whether a patch removed
it. A check (`builds.md`) shows a protected RWX section and plain main code; that is all that is needed, since nothing here touches the protection.
