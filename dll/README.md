# aurora.dll

The mod menu itself. It draws the menu and ESP into the game's frames (Vulkan and DirectX 11), finds its own game addresses by pattern, keeps its files under `%APPDATA%\Aurora` and unloads from its own menu.

- Menu key: **F4**
- Log: `%APPDATA%\Aurora\Dumper\dll.log`

## Docs

- [features.md](docs/features.md): every feature and how it works
- [menu.md](docs/menu.md): pages, rows, keys, profiles, hotkeys
- [injected-module.md](docs/injected-module.md): load to unload, step by step
- [offsets.md](docs/offsets.md): game addresses and code patterns
- [dx11.md](docs/dx11.md): DirectX 11 differences

Changes: [CHANGELOG.md](CHANGELOG.md).

```
cargo build --release -p aurora-dll
```

A fresh build has no stamp, so the Launchpad refuses it. `tools\swap-dll.ps1` stamps it and puts it in Bin.
