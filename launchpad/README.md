# Aurora Launchpad

Finds Ghost Recon Breakpoint (Steam or Ubisoft Connect), starts it, checks `aurora.dll` and loads it into the game. It never edits the game's files and can be closed after inject.

Before loading, the DLL must pass four checks:

1. **Stamp**: an ECDSA P-256 signature in the DLL's last 80 bytes, checked against the public key in `src/sign_key.rs`.
2. **Build card**: the DLL must speak the Launchpad's protocol (currently 5).
3. **Match**: the module in the game must be the file that was checked.
4. **Hello**: a fresh random challenge, then a go. Without the go the DLL frees itself after 15 s.

Details: [docs/launchpad.md](docs/launchpad.md). Changes: [CHANGELOG.md](CHANGELOG.md).

```
cargo build --release -p aurora-launchpad
```
