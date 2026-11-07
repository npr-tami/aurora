# Ghidra helper for Aurora research. Needs Ghidra and a JDK: set GHIDRA_INSTALL_DIR and JAVA_HOME,
# or pass -Ghidra and -Jdk.
# GRB.exe> research\ghidra\ghidra.ps1 decompile -Program GRB_vulkan.exe -Rva 0x11bb332[,0x360a34e]
# [-Out file] import: copies the exe into the Ghidra project once, with no auto-analysis (minutes,
# not hours). Decompile: the whole function around each RVA (start from the exe's exception table).
# Read-only towards the game folder.
param(
    [Parameter(Mandatory, Position = 0)] [ValidateSet('import', 'decompile')] [string] $Action,
    [string] $Exe,
    [string] $Program = 'GRB_vulkan.exe',
    [string[]] $Rva,
    [string] $Out,
    [string] $Ghidra = $env:GHIDRA_INSTALL_DIR,
    [string] $Jdk = $env:JAVA_HOME, # Ghidra 12.2 needs Java 25
    [string] $ProjectDir = (Join-Path $env:LOCALAPPDATA 'aurora-ghidra\projects'),
    [string] $Memory = '8G'
)
$ErrorActionPreference = 'Stop'
$env:JAVA_HOME = $Jdk
$env:PATH = "$Jdk\bin;$env:PATH"
$env:GHIDRA_HEADLESS_MAXMEM = $Memory
# Ghidra's settings, cache and temp files go next to the projects folder (its launch.properties
# honours the XDG variables on Windows too).
$work = Split-Path $ProjectDir
foreach ($d in 'settings', 'cache', 'tmp') { New-Item -ItemType Directory -Force (Join-Path $work $d) | Out-Null }
$env:XDG_CONFIG_HOME = Join-Path $work 'settings'
$env:XDG_CACHE_HOME = Join-Path $work 'cache'
$env:TEMP = Join-Path $work 'tmp'
$env:TMP = $env:TEMP
$headless = Join-Path $Ghidra 'support\analyzeHeadless.bat'
if (-not (Test-Path $headless)) { throw "Ghidra not found at $Ghidra (set GHIDRA_INSTALL_DIR)" }
New-Item -ItemType Directory -Force $ProjectDir | Out-Null
$sw = [Diagnostics.Stopwatch]::StartNew()
switch ($Action) {
    'import' {
        if (-not $Exe) { throw '-Exe is needed' }
        # Ghidra's.bat breaks on the "(x86)" in Steam's path: hand it the short 8.3 name.
        $short = (New-Object -ComObject Scripting.FileSystemObject).GetFile((Resolve-Path $Exe).Path).ShortPath
        & $headless $ProjectDir Aurora -import $short -noanalysis -overwrite 2>&1 |
            Select-String -Pattern 'ERROR|Import succeeded|Exception' | Select-Object -First 20
    }
    'decompile' {
        if (-not $Rva) { throw '-Rva is needed (one or more, comma separated)' }
        # @: with one RVA PowerShell would hand back a plain string and splat it per character.
        $list = @($Rva -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
        if (-not $Out) {
            $Out = "$(Split-Path $ProjectDir)\out\$([IO.Path]::GetFileNameWithoutExtension($Program))-$($list[0]).c"
        }
        New-Item -ItemType Directory -Force (Split-Path $Out) | Out-Null
        & $headless $ProjectDir Aurora -process $Program -noanalysis -scriptPath $PSScriptRoot `
            -postScript AuroraDecompile.java $Out @list 2>&1 |
            Select-String -Pattern 'ERROR|wrote|Exception|AuroraDecompile' | Select-Object -First 20
        if (Test-Path $Out) { "output: $Out" }
    }
}
"took $([int]$sw.Elapsed.TotalSeconds) s"
