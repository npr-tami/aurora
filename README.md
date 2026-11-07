# Aurora

A mod menu for Ghost Recon Breakpoint (PC, Vulkan and DirectX 11), plus the offsets, research tools and notes behind it.

Aurora is made for PvE. It does not touch anti-cheat, network or account code, and it does not hide itself.

## Features

- In-game menu drawn into the game's own frames, with hotkeys, profiles and a command box
- ESP (boxes, bones, names, distance)
- God mode, Fly, Fly Through Everything, teleports
- No Recoil, No Spread, Infinite Mag, Damage Multiplier, Fire Rate, Silent Aim (NPCs only)
- Gear Score and Credits editors
- Undetected (enemy NPC sight only), NVG options
- Unloads cleanly from its own menu

The full list is in [dll/docs/features.md](dll/docs/features.md).

## Usage

1. Download the Launchpad and `aurora.dll` from [Releases](../../releases).
2. Put `aurora.dll` in `%APPDATA%\Aurora\Bin\` (the Launchpad creates the folder on first run).
3. Run the Launchpad, press **Launch**, then **Inject** once in the world.
4. **F4** opens the menu.

The Launchpad only loads a stamped `aurora.dll`. A modified or unofficial copy is refused.

## Building

Windows, Rust (edition 2024):

```
cargo build --release
cargo test
```

`tools\package.ps1` builds a release and stamps the DLL. Stamping needs the private signing key, which is not part of this repository; see [tools/README.md](tools/README.md).

## Layout

```
launchpad/   Aurora Launchpad, the loader
dll/         aurora.dll, the mod menu
shared/      code used by both
tools/       build, stamp and swap scripts
research/    reverse engineering tools and notes
docs/        architecture overview
```

## Research

[research/](research/) holds the tools used to map the game: a static analyser for the executables (`grbstatic`), an external memory scanner (`grbresearch`), in-game recorders (`grbresearch-dll`), a Ghidra helper and a set of Python scripts. Findings are in [research/docs/](research/docs/) and [dll/docs/offsets.md](dll/docs/offsets.md). Offsets are for game build 25349072.

## Disclaimer

Not affiliated with or endorsed by Ubisoft. Use at your own risk. Do not use it in PvP.

## License

See [LICENSE](LICENSE).

<img width="428" height="498" alt="More_twinks" src="https://github.com/user-attachments/assets/d32c7d56-ab94-4ebb-9d3b-fcc7b475bbf9" />
