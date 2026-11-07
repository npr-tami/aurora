# Puts a new aurora.dll into %APPDATA%\Aurora\Bin: 1. Unload the mod in the
# game first (menu > Unload); the game may stay open; 2. The new DLL gets Aurora's stamp inside it:
# the Launchpad injects Bin's DLL only with a stamp and build card that check; 3. The DLL in Bin is
# renamed to aurora.dll.bak (an older.bak gets the time in its name, so nothing is ever deleted); a
# left-over aurora.dll.sig from before the stamp goes to.bak too; 4. The new one is copied in under
# the same name; 5. Inject with the Launchpad as usual.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\swap-dll.ps1 [-Dll <path>]
#
# -Dll defaults to the release build (target\release\aurora.dll, or CARGO_TARGET_DIR's). Stops
# without changing anything while a running program has Bin's DLL loaded, when there is no signing
# key (tools\new-signing-key.ps1), or when -Dll is inside Bin (copy it out first: the swap renames
# Bin's files).
param([string]$Dll, [string]$Bin = (Join-Path $env:APPDATA 'Aurora\Bin'))

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $Dll) {
    $target = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath([IO.Path]::Combine($root, $env:CARGO_TARGET_DIR)) } else { Join-Path $root 'target' }
    $Dll = Join-Path $target 'release\aurora.dll'
}
if (-not (Test-Path -LiteralPath $Dll -PathType Leaf)) { throw "no new DLL at $Dll (cargo build --release first)" }
$Dll = (Resolve-Path -LiteralPath $Dll).Path
# A relative -Bin is read from this PowerShell's folder.
$Bin = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Bin)
# A DLL inside Bin would be renamed by the backup below before it is copied.
if ([IO.Path]::GetDirectoryName($Dll).TrimEnd('\') -ieq $Bin.TrimEnd('\')) { throw "$Dll is inside Bin, whose files this swap renames: copy it out of Bin first (to the Desktop, say) and pass that copy." }
New-Item -ItemType Directory -Force $Bin | Out-Null
$to = Join-Path $Bin 'aurora.dll'

# 1. Still loaded somewhere (the game, before unload)? Then stop.
$users = Get-Process -ErrorAction SilentlyContinue | Where-Object {
    try { $_.Modules | Where-Object { $_.FileName -ieq $to } } catch { $false }
}
if ($users) { throw "aurora.dll is still loaded in: $(($users | ForEach-Object { "$($_.Name) ($($_.Id))" }) -join ', '). Unload it from the menu first." }

# 2. Stamped, before Bin is touched (in place: the DLL's own bytes).
& (Join-Path $PSScriptRoot 'sign-dll.ps1') -Dll $Dll | Out-Null

# 3. The old ones to.bak.
function Backup([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return }
    $bak = "$path.bak"
    if (Test-Path -LiteralPath $bak) {
        $stamp = (Get-Item -LiteralPath $bak).LastWriteTime.ToString('yyyy-MM-dd HHmmss')
        Rename-Item -LiteralPath $bak -NewName "$(Split-Path -Leaf $path).$stamp.bak"
    }
    Rename-Item -LiteralPath $path -NewName "$(Split-Path -Leaf $path).bak"
}
Backup $to
Backup "$to.sig" # from before the stamp; the Launchpad no longer reads it

# 4. The new one in, under the same name, checked.
Copy-Item -LiteralPath $Dll -Destination $to
if ((Get-FileHash -LiteralPath $Dll).Hash -ne (Get-FileHash -LiteralPath $to).Hash) { throw "copy check failed: $to differs from $Dll" }
Write-Output "aurora.dll swapped in, stamped: $to (from $Dll). Old one: $to.bak. Now inject with the Launchpad."
