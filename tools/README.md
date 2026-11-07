# tools

Scripts for building a release and getting a new aurora.dll into the game. Run them from the repo root in PowerShell:

```
powershell -NoProfile -ExecutionPolicy Bypass -File tools\<script>.ps1 ...
```

| Script | What it does | When you use it |
|---|---|---|
| `package.ps1 [-Build 26.11.1]` | Builds both in release mode, stamps `target\release\aurora.dll`, and copies the Launchpad to `dist\Aurora.Launchpad <version>.exe`. `-Build` names the build in the mod's first log line. | Making a release |
| `release.ps1 [-Only launchpad\|dll]` | Tests, builds and stamps, checks the files, then asks before it pushes the tags and makes the GitHub releases. Notes come from the top CHANGELOG entry. Needs the GitHub CLI. | Publishing a release |
| `swap-dll.ps1 [-Dll <path>]` | Stamps a DLL and puts it in `%APPDATA%\Aurora\Bin`. The old one becomes `aurora.dll.bak`. Refuses while the game still has the DLL loaded. | Trying a new DLL build in the game. Unload from the menu first. Full routine: [swap-dll.md](swap-dll.md) |
| `sign-dll.ps1 -Dll <path>` | Puts Aurora's stamp into one DLL, in place. `package.ps1` and `swap-dll.ps1` call it for you. | Rarely by hand |
| `new-signing-key.ps1` | Makes the signing key once and writes its public half into `launchpad\src\sign_key.rs`. Stops if a key already exists. | Only once, or after losing the key. Build the Launchpad again afterwards |

## The signing key

- The private key lives at `%USERPROFILE%\.aurora-signing\aurora-dll.key`. It is never in this repo, never in a release, and `.gitignore` blocks `*.key`.
- Without it you can build and test everything, but you can't stamp a DLL, so the official Launchpad refuses your builds.
- Anyone who has the key file can stamp. It guards against changed files, not against someone who has the key.

The research tools (Ghidra helper, Python scripts) are in [../research/](../research/).
