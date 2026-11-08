# Builds, checks and publishes a release: tests, package.ps1, tags, push, and the GitHub releases.
# Asks before anything leaves the PC. Stops at the first problem.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1 -Only dll
#
# The version comes from Cargo.toml, the release notes from the top entry of each CHANGELOG.md.
# Needs git, cargo, the GitHub CLI (gh, signed in) and the signing key.

param([ValidateSet('both', 'launchpad', 'dll')][string]$Only = 'both')

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Step($text) { Write-Host ''; Write-Host "== $text" -ForegroundColor Cyan }
function Ok($text) { Write-Host "   OK: $text" -ForegroundColor Green }
function Stop-Here($text) {
    Write-Host ''
    Write-Host "STOPPED: $text" -ForegroundColor Red
    Write-Host 'Nothing after this point was done.' -ForegroundColor Red
    exit 1
}

$version = (Select-String -Path 'Cargo.toml' -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if (-not $version) { Stop-Here 'no version in Cargo.toml' }
$doLaunchpad = $Only -ne 'dll'
$doDll = $Only -ne 'launchpad'
Write-Host "Aurora release $version ($Only)" -ForegroundColor White

Step '1/7 Checking the tools'
foreach ($tool in 'git', 'cargo', 'gh') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        if ($tool -eq 'gh') { Stop-Here 'the GitHub CLI is missing. Install it with: winget install --id GitHub.cli   then close and reopen PowerShell, run: gh auth login   and start this script again.' }
        Stop-Here "$tool is missing."
    }
}
gh auth status 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Stop-Here 'gh is not signed in. Run: gh auth login   then start this script again.' }
$key = Join-Path $env:USERPROFILE '.aurora-signing\aurora-dll.key'
if (-not (Test-Path $key)) { Stop-Here "no signing key at $key" }
if (git ls-files '*.key') { Stop-Here 'a .key file is tracked by git. Remove it from the repo first.' }
Ok 'git, cargo, gh, signing key'

Step '2/7 Getting up to date with GitHub'
git fetch origin 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Stop-Here 'could not reach GitHub (git fetch).' }
git merge-base --is-ancestor origin/main HEAD
if ($LASTEXITCODE -ne 0) {
    git rebase origin/main 2>&1 | Out-Host
    if ($LASTEXITCODE -ne 0) { git rebase --abort 2>&1 | Out-Null; Stop-Here 'could not put the local commits on top of GitHub''s main.' }
}
if (git status --porcelain --untracked-files=no) { Stop-Here 'there are uncommitted changes. Commit or undo them first.' }
Ok "on top of origin/main, commit $(git rev-parse --short HEAD)"

Step '3/7 Running the tests (a few minutes)'
cargo test 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Stop-Here 'a test failed (look for FAILED above).' }
Ok 'all tests passed'

Step '4/7 Building and stamping (a few minutes)'
$target = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath([IO.Path]::Combine($root, $env:CARGO_TARGET_DIR)) } else { Join-Path $root 'target' }
$exe = Join-Path $root "dist\Aurora.Launchpad $version.exe"
$dll = Join-Path $target 'release\aurora.dll'
# Old copies out of the way, so the files checked below are this build's.
if ($doLaunchpad) { Remove-Item $exe -ErrorAction SilentlyContinue }
Remove-Item $dll -ErrorAction SilentlyContinue
$packageArgs = @('-Build', $version)
if (-not $doLaunchpad) { $packageArgs += '-OnlyDll' }
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'package.ps1') @packageArgs
if ($LASTEXITCODE -ne 0) { Stop-Here 'the build failed.' }

Step '5/7 Checking the built files'
$builtFiles = if ($doLaunchpad) { @($exe, $dll) } else { @($dll) }
foreach ($f in $builtFiles) {
    if (-not (Test-Path $f)) { Stop-Here "missing: $f" }
    Ok "$f  $([math]::Round((Get-Item $f).Length / 1MB, 1)) MB  sha256 $((Get-FileHash $f -Algorithm SHA256).Hash.ToLower())"
}
foreach ($f in $builtFiles) {
    $text = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($f))
    if ($env:USERNAME.Length -ge 3 -and $text.IndexOf("\$env:USERNAME\", [StringComparison]::OrdinalIgnoreCase) -ge 0) { Stop-Here "$f contains your user folder path." }
}
Ok 'no user folder paths inside'

function Notes($changelog) {
    $lines = Get-Content $changelog
    $out = @(); $seen = $false
    foreach ($l in $lines) {
        if ($l -like '## *') { if ($seen) { break }; $seen = $true; continue }
        if ($seen) { $out += $l }
    }
    $file = [IO.Path]::GetTempFileName()
    [IO.File]::WriteAllText($file, ($out -join "`n").Trim(), (New-Object Text.UTF8Encoding $false))
    $file
}

Step '6/7 Ready to publish'
$tags = @()
if ($doLaunchpad) { $tags += "launchpad-$version" }
if ($doDll) { $tags += "dll-$version" }
Write-Host "   This will push main and the tags $($tags -join ', ') to GitHub and make the releases."
Write-Host '   It is public once done.'
$answer = Read-Host '   Type YES to publish, anything else to stop'
if ($answer -cne 'YES') { Stop-Here 'you did not type YES. The build is ready; nothing was published.' }

Step '7/7 Publishing'
foreach ($t in $tags) {
    git rev-parse -q --verify "refs/tags/$t" | Out-Null
    if ($LASTEXITCODE -ne 0) {
        $title = if ($t -like 'launchpad-*') { "Aurora Launchpad $version" } else { "aurora.dll $version" }
        git tag -a $t -m $title
        if ($LASTEXITCODE -ne 0) { Stop-Here "could not make tag $t." }
    }
}
git push -u origin main @tags 2>&1 | Out-Host
if ($LASTEXITCODE -ne 0) { Stop-Here 'git push failed.' }
Ok 'code and tags are on GitHub'

if ($doLaunchpad) {
    gh release view "launchpad-$version" 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { Ok "release launchpad-$version already exists, left alone" } else {
        gh release create "launchpad-$version" --title "Aurora Launchpad $version" --latest=false --notes-file (Notes 'launchpad\CHANGELOG.md') "$exe"
        if ($LASTEXITCODE -ne 0) { Stop-Here 'the Launchpad release failed.' }
        Ok "release launchpad-$version"
    }
}
if ($doDll) {
    gh release view "dll-$version" 2>&1 | Out-Null
    if ($LASTEXITCODE -eq 0) { Ok "release dll-$version already exists, left alone" } else {
        gh release create "dll-$version" --title "aurora.dll $version" --latest --notes-file (Notes 'dll\CHANGELOG.md') "$dll"
        if ($LASTEXITCODE -ne 0) { Stop-Here 'the DLL release failed.' }
        Ok "release dll-$version"
    }
}

Write-Host ''
Write-Host "Done. $(gh repo view --json url -q .url)/releases" -ForegroundColor Green
