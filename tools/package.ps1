# Builds the release "Aurora.Launchpad <version>.exe" and the mod's DLL, apart: the exe carries no
# DLL. Bin holds the DLL; put a new one in with tools\swap-dll.ps1 (unload first). Cargo names can't
# hold dots or spaces, so the name is given here.
#
#   powershell -File tools\package.ps1
#   powershell -File tools\package.ps1 -Build 26.11.1
#
# -Build names the build in the mod's first log line, with the commit:
# "aurora-dll 26.11.1 (26.11.1, commit 1a2b3c4) attached". Without -Build the commit alone;
# "+ changes" when files differ from the commit.
#
# Out: dist\Aurora.Launchpad <version>.exe, and target\release\aurora.dll with Aurora's stamp inside
# it. The Launchpad only injects %APPDATA%\Aurora\Bin\aurora.dll, only when its stamp and build card
# check, and never writes it.

param([string]$Build = '', [switch]$OnlyDll)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
# target\ (which may hold an old build).
$target = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath([IO.Path]::Combine($root, $env:CARGO_TARGET_DIR)) } else { Join-Path $root 'target' }

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if (-not $version) { throw 'no version in Cargo.toml' }

# No build-machine paths in the exe (user name, folders): map them to neutral names. Encoded flags
# (0x1f between them) because the project path has a space in it.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustup = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$env:CARGO_ENCODED_RUSTFLAGS = @(
    "--remap-path-prefix=$cargoHome=cargo",
    "--remap-path-prefix=$rustup=rustup",
    "--remap-path-prefix=$root=aurora"
) -join [char]0x1f

# The build's name for the mod's first log line (aurora-dll's BUILD); only the DLL build sees it, so
# a later plain cargo build says "dev build" again.
$commit = $null
$changed = $null
if (Get-Command git -ErrorAction SilentlyContinue) {
    $ErrorActionPreference = 'Continue'
    $commit = git -C $root rev-parse --short HEAD 2>$null
    if ($LASTEXITCODE -ne 0) { $commit = $null }
    $changed = git -C $root status --porcelain --untracked-files=no 2>$null
    $ErrorActionPreference = 'Stop'
}
$name = @()
if ($Build) { $name += $Build }
if ($commit) { $name += "commit $commit" + $(if ($changed) { ' + changes' } else { '' }) }
$env:AURORA_BUILD = if ($name.Count) { $name -join ', ' } else { 'release build' }
try {
    cargo build --release -p aurora-dll
    if ($LASTEXITCODE -ne 0) { throw 'DLL build failed' }
} finally {
    Remove-Item Env:\AURORA_BUILD -ErrorAction SilentlyContinue
}
$dll = Join-Path $target 'release\aurora.dll'
if (-not (Test-Path $dll)) { throw "no DLL at $dll" }
& (Join-Path $PSScriptRoot 'sign-dll.ps1') -Dll $dll | Out-Null

if (-not $OnlyDll) {
    try {
        cargo build --release -p aurora-launchpad
        if ($LASTEXITCODE -ne 0) { throw 'Launchpad build failed' }
    } finally {
        Remove-Item Env:\CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    }

    $dist = Join-Path $root 'dist'
    New-Item -ItemType Directory -Force $dist | Out-Null
    $out = Join-Path $dist "Aurora.Launchpad $version.exe"
    Copy-Item (Join-Path $target 'release\aurora-launchpad.exe') $out -Force
    $size = [math]::Round((Get-Item $out).Length / 1MB, 1)
    Write-Output "Built $out ($size MB, no DLL inside). New DLL: $dll (stamped inside); put it in Bin with tools\swap-dll.ps1 (unload first)."
} else {
    Remove-Item Env:\CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    Write-Output "Built DLL only: $dll (stamped inside)."
}
