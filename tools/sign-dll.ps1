# Stamps a built aurora.dll: an ECDSA P-256 signature over the DLL's bytes, made with Aurora's
# signing key, goes INTO the DLL, at its end (the "stamp": 64-byte signature, format 1, size 80,
# label "AURORA-STAMP"; layout in shared\aurora-core\src\stamp.rs). One file to share; no.sig any
# more. Windows ignores bytes after a DLL's last section, so the stamped DLL loads as before. The
# Launchpad injects Bin's aurora.dll only with a stamp that matches, and only when the DLL carries
# its build card (shared\aurora-core\src\card.rs). Tools\swap-dll.ps1 and tools\package.ps1 call
# this; it is rarely needed by hand.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\sign-dll.ps1 -Dll <path> [-Key <path>]
#
# -Key defaults to %USERPROFILE%\.aurora-signing\aurora-dll.key (tools\new-signing-key.ps1). A DLL
# stamped before is stamped again over its original bytes (the old stamp comes off first).
param([Parameter(Mandatory = $true)][string]$Dll, [string]$Key = (Join-Path $env:USERPROFILE '.aurora-signing\aurora-dll.key'))

$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Dll -PathType Leaf)) { throw "no DLL at $Dll" }
$Dll = (Resolve-Path -LiteralPath $Dll).Path
# Read from this PowerShell's folder, as -Dll.
$Key = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Key)
if (-not (Test-Path -LiteralPath $Key -PathType Leaf)) { throw "no signing key at $Key (tools\new-signing-key.ps1 makes one; then build the Launchpad again)" }
Add-Type -AssemblyName System.Core

# The stamp's shape (shared\aurora-core\src\stamp.rs): keep these the same there and here.
$label = [Text.Encoding]::ASCII.GetBytes('AURORA-STAMP')
$format = 1
$stampSize = 64 + 2 + 2 + $label.Length
# The card's label (shared\aurora-core\src\card.rs): a DLL without one is from before the cards, and
# the Launchpad would refuse it after the stamp check.
$cardLabel = [Text.Encoding]::ASCII.GetBytes('AURORA-CARD') + [byte[]]@(0)

function Ends-With([byte[]]$bytes, [byte[]]$tail) {
    if ($bytes.Length -lt $tail.Length) { return $false }
    for ($i = 0; $i -lt $tail.Length; $i++) { if ($bytes[$bytes.Length - $tail.Length + $i] -ne $tail[$i]) { return $false } }
    return $true
}

function Find-Bytes([byte[]]$bytes, [byte[]]$needle) {
    $hits = 0
    for ($i = 0; $i -le $bytes.Length - $needle.Length; $i++) {
        $j = 0
        while ($j -lt $needle.Length -and $bytes[$i + $j] -eq $needle[$j]) { $j++ }
        if ($j -eq $needle.Length) { $hits++ }
    }
    return $hits
}

$bytes = [IO.File]::ReadAllBytes($Dll)
# Stamped before (a second run, or a DLL already in Bin): the original bytes are what gets signed.
if (Ends-With $bytes $label) {
    $n = $bytes.Length
    $oldFormat = [BitConverter]::ToUInt16($bytes, $n - $label.Length - 4)
    $oldSize = [BitConverter]::ToUInt16($bytes, $n - $label.Length - 2)
    if ($oldFormat -ne $format -or $oldSize -ne $stampSize -or $n -lt $stampSize) { throw "$Dll ends with a stamp of format $oldFormat, size $oldSize, which this script does not know; it stamps format $format only" }
    $bytes = $bytes[0..($n - $stampSize - 1)]
    Write-Output "old stamp taken off: $Dll"
}
if ($bytes.Length -lt 2 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) { throw "$Dll is not a DLL (no MZ header)" }
$cards = Find-Bytes $bytes $cardLabel
if ($cards -eq 0) { throw "$Dll carries no build card (aurora-dll's AURORA_CARD): build it from code with the card first, or the Launchpad will refuse it" }

$k = [System.Security.Cryptography.CngKey]::Import([IO.File]::ReadAllBytes($Key), [System.Security.Cryptography.CngKeyBlobFormat]::EccPrivateBlob)
$ecdsa = New-Object System.Security.Cryptography.ECDsaCng($k)
$ecdsa.HashAlgorithm = [System.Security.Cryptography.CngAlgorithm]::Sha256
$sig = $ecdsa.SignData($bytes)
if ($sig.Length -ne 64 -or -not $ecdsa.VerifyData($bytes, $sig)) { throw 'the new signature does not check' }

# The stamped file: the DLL's bytes, the signature, format, size, label. Written beside the DLL
# first, then moved over it, so a failure half way leaves the DLL as it was.
$out = New-Object IO.MemoryStream
$out.Write($bytes, 0, $bytes.Length)
$out.Write($sig, 0, 64)
$out.Write([BitConverter]::GetBytes([uint16]$format), 0, 2)
$out.Write([BitConverter]::GetBytes([uint16]$stampSize), 0, 2)
$out.Write($label, 0, $label.Length)
$stamped = $out.ToArray()
if ($stamped.Length -ne $bytes.Length + $stampSize) { throw 'the stamped file has the wrong size' }
$tmp = "$Dll.stamping"
[IO.File]::WriteAllBytes($tmp, $stamped)
Move-Item -LiteralPath $tmp -Destination $Dll -Force
# Read back: the signed part checks, the stamp is at the end.
$check = [IO.File]::ReadAllBytes($Dll)
if ($check.Length -ne $stamped.Length -or -not (Ends-With $check $label) -or -not $ecdsa.VerifyData($check[0..($check.Length - $stampSize - 1)], $check[($check.Length - $stampSize)..($check.Length - $stampSize + 63)])) { throw "the stamp on $Dll does not read back" }
# A .sig from before the stamp is of no use any more; say so, touch nothing.
if (Test-Path -LiteralPath "$Dll.sig") { Write-Output "note: $Dll.sig is no longer used (the stamp is inside the DLL); delete it when you like" }
Write-Output "stamped: $Dll (signature inside, $stampSize bytes at the end; build card found)"
