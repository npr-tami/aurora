# Putting a new DLL in

The Launchpad is only the injector. `%APPDATA%\Aurora\Bin\aurora.dll` is the DLL. The Launchpad never writes it, and injects it only with Aurora's stamp inside it and a build card that fits.

1. **Unload** Aurora in the game (menu > Unload). The game may stay open.
2. **Build** the DLL: `cargo build --release` in `code\`.
3. **Swap**: `powershell -NoProfile -ExecutionPolicy Bypass -File tools\swap-dll.ps1`
   - It stops without changing anything if a program still has Bin's DLL loaded, or if there is no signing key.
   - It stamps the new DLL first (`tools\sign-dll.ps1`, in place: the DLL's own bytes get the stamp; a DLL stamped before is stamped again over its original bytes; a DLL without a build card is refused).
   - Bin's `aurora.dll` becomes `aurora.dll.bak` (a left-over `aurora.dll.sig` from before the stamp becomes `aurora.dll.sig.bak`). An older `.bak` is renamed to `<name>.<its time>.bak`, so nothing is deleted.
   - The new DLL is copied in under the same name and checked.
   - `-Dll <path>` takes another build. A `-Dll` inside Bin is refused (the swap renames Bin's files): copy it out first.
4. **Inject** with the Launchpad as usual.

Going back: unload, copy `aurora.dll.bak` out of Bin (to the Desktop, say), then run `tools\swap-dll.ps1 -Dll <that copy>`. It stamps it (again, over its original bytes) and keeps the one in Bin as `.bak`. A `.bak` from before the stamp carries no build card and is refused: it needs a build from code with the card.

## The signing key
- Made once with `tools\new-signing-key.ps1`: the private key goes to `%USERPROFILE%\.aurora-signing\aurora-dll.key` (never into the repo), its public half into `launchpad\src\sign_key.rs`.
- Lost key: move the old file away, run `tools\new-signing-key.ps1`, build the Launchpad again (`tools\package.ps1`), swap the DLL in again. DLLs signed with the old key are refused then.
- Anyone who can read the key file can sign. It is a check against changed files, not a lock.

## The stamp and the card
- Stamp (`shared\aurora-core\src\stamp.rs`): the DLL's last 80 bytes: 64-byte signature (r, s), format (1), size (80), label `AURORA-STAMP`. The signed bytes are everything before. `tools\sign-dll.ps1` writes it; the Launchpad reads it. A later format grows the trailer; an older Launchpad then says "needs a newer Launchpad".
- Card (`shared\aurora-core\src\card.rs`): 96 bytes inside the DLL (`dll\src\lib.rs`, `AURORA_CARD`, section `.aurora`): label `AURORA-CARD`, format, size, lowest and highest Launchpad protocol it speaks, version, build name. The Launchpad accepts a DLL when its own `PROTO_VERSION` is in that range. Built at compile time; the build name is what `tools\package.ps1 -Build` gives (the same as dll.log's first line).
- For a friend: the Launchpad exe and `aurora.dll`, nothing else.
