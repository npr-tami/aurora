# Makes Aurora's signing key, once. The Launchpad injects Bin's aurora.dll only with a signature
# made with this key (aurora.dll.sig beside it, see tools\sign-dll.ps1); DLLs from Advanced's Add
# button need none.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\new-signing-key.ps1 [-Key <path>]
#
# The private key goes to -Key (default %USERPROFILE%\.aurora-signing\aurora-dll.key), never into
# the repo. The public half goes into launchpad\src\sign_key.rs, so build the Launchpad again after
# a new key. Stops when a key is already there: a new key refuses every DLL signed with the old one.
param([string]$Key = (Join-Path $env:USERPROFILE '.aurora-signing\aurora-dll.key'))

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
# Read from this PowerShell's folder.
$Key = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Key)
if (Test-Path -LiteralPath $Key) { throw "a signing key is already at $Key. Move it away by hand first if you really want a new one." }
Add-Type -AssemblyName System.Core

# ECDSA on the P-256 curve, Windows' own crypto (CNG): the Launchpad checks with the same.
$p = New-Object System.Security.Cryptography.CngKeyCreationParameters
$p.ExportPolicy = [System.Security.Cryptography.CngExportPolicies]::AllowPlaintextExport
# [NullString]::Value, not $null: PowerShell passes $null to a string as "", and a key named "" is
# stored in Windows' key store, a second copy. A real null keeps it in memory only.
$k = [System.Security.Cryptography.CngKey]::Create([System.Security.Cryptography.CngAlgorithm]::ECDsaP256, [NullString]::Value, $p)
$private = $k.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
$public = $k.Export([System.Security.Cryptography.CngKeyBlobFormat]::EccPublicBlob)
if ($public.Length -ne 72) { throw "unexpected public key size $($public.Length)" }

# A signature over a fixed text, so the Launchpad's tests prove these signatures check there.
$message = 'Aurora signing test'
$ecdsa = New-Object System.Security.Cryptography.ECDsaCng($k)
$ecdsa.HashAlgorithm = [System.Security.Cryptography.CngAlgorithm]::Sha256
$sig = $ecdsa.SignData([Text.Encoding]::ASCII.GetBytes($message))
if ($sig.Length -ne 64) { throw "unexpected signature size $($sig.Length)" }

function Rust-Bytes([byte[]]$b) {
    $lines = for ($i = 0; $i -lt $b.Length; $i += 16) {
        '    ' + (($b[$i..([Math]::Min($i + 15, $b.Length - 1))] | ForEach-Object { '0x{0:x2},' -f $_ }) -join ' ')
    }
    $lines -join "`r`n"
}

$day = Get-Date -Format 'd MMM yyyy'
$rs = @"
//! Aurora's public signing key, made by tools\new-signing-key.ps1 on $day. The private half is
//! not in the repo; tools\sign-dll.ps1 signs with it.

/// X and Y of the P-256 key (ECDSA, SHA-256).
pub const PUBLIC_KEY: [u8; 64] = [
$(Rust-Bytes $public[8..71])
];

/// A text the key signed when it was made, and that signature (r and s): the tests check that
/// what.NET signs is accepted here.
# [cfg(test)]
pub const TEST_MESSAGE: &[u8] = b"$message";
# [cfg(test)]
pub const TEST_SIG: [u8; 64] = [
$(Rust-Bytes $sig)
];
"@
$rsPath = Join-Path $root 'launchpad\src\sign_key.rs'

New-Item -ItemType Directory -Force (Split-Path -Parent $Key) | Out-Null
[IO.File]::WriteAllBytes($Key, $private)
[IO.File]::WriteAllText($rsPath, ($rs -replace "`r?`n", "`r`n") + "`r`n")
Write-Output "Signing key made: $Key (keep it, it is not in the repo). Public key written to $rsPath; build the Launchpad again."
