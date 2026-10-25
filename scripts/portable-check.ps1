# Prove the portable build runs from any folder and writes nothing outside it.
#
#   pwsh scripts/portable-check.ps1 -App dist/LocalGallery
#
# Copies the app to a path with spaces and non-ASCII characters, snapshots %APPDATA%, %LOCALAPPDATA%
# and HKCU\Software, runs a job through the headless core, opens the desktop window (and a second
# launch, which must hand over to the first), then diffs the snapshots.

param(
    [string] $App = 'dist/LocalGallery',
    [string] $FakeObjectInfo = (Join-Path (Split-Path -Parent $PSScriptRoot) 'tests/fixtures/object_info.json'),
    [string] $Work = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }),
    [switch] $NoWindow
)
. "$PSScriptRoot/common.ps1"
$onWindows = $IsWindows -or $env:OS -eq 'Windows_NT'
$exe = if ($onWindows) { '.exe' } else { '' }

$dest = Join-Path $Work "Local Gallery $([char]0xFC) test"
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Recurse (Resolve-Path $App) $dest
$root = Join-Path $dest (Split-Path -Leaf (Resolve-Path $App))
Write-Host "app folder: $root"

# OS bookkeeping that Windows writes for any program that runs; not app state.
$allowFs = '\\Temp(\\|$)|\\Microsoft\\Windows\\|\\D3DSCache(\\|$)|\\Microsoft\\CLR_|\\NVIDIA|\\CrashDumps(\\|$)|\\pwsh|\\PowerShell|\\npm-cache|\\cargo|\\rustup'
# Written by the Microsoft WebView2 runtime (a shared Windows component) and by Windows on behalf
# of any WebView2 window: allowed only during the window phase, never for the headless core.
$webviewFs = '\\Microsoft\\(TokenBroker|Credentials|EdgeWebView)(\\|$)'
$webviewReg = '\\Microsoft\\EdgeWebView(\\|\]|$)|\\Microsoft\\EdgeUpdate\\ClientState\\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5\}|\\CapabilityAccessManager\\ConsentStore'
$allowReg = '\\Microsoft\\Windows\\CurrentVersion\\(Explorer|Search|AppHost|CloudStore|Diagnostics|Notifications|PushNotifications)|\\Microsoft\\Windows NT\\CurrentVersion\\AppCompatFlags|\\Classes\\Local Settings|\\MuiCache|\\Microsoft\\Direct3D|\\Microsoft\\DirectX|\\Microsoft\\Windows\\Shell|\\Microsoft\\PowerShell|\\Microsoft\\Windows\\DWM|\\Microsoft\\Input|\\Microsoft\\IdentityCRL'

function Snapshot-Fs {
    $set = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($base in @($env:APPDATA, $env:LOCALAPPDATA) | Where-Object { $_ -and (Test-Path $_) }) {
        Get-ChildItem -Path $base -Recurse -Depth 3 -Force -ErrorAction SilentlyContinue |
            ForEach-Object { [void]$set.Add($_.FullName) }
    }
    , $set
}
function Snapshot-Reg {
    $set = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    if (-not $onWindows) { return , $set }
    $f = Join-Path $Work "hkcu-$([guid]::NewGuid().ToString('N')).reg"
    & reg.exe export 'HKCU\Software' $f /y | Out-Null
    $key = ''
    foreach ($line in [IO.File]::ReadLines($f)) {
        if ($line.StartsWith('[')) { $key = $line; [void]$set.Add($key) }
        elseif ($line -match 'localgallery') { [void]$set.Add("$key $line") }
    }
    Remove-Item $f
    , $set
}

$fs0 = Snapshot-Fs
$reg0 = Snapshot-Reg

# 1. Headless core: a whole job through the fake engine.
$core = Start-Core -Exe (Join-Path $root "localgallery-core$exe") -Root $root -Engine fake -Port 7896 `
    -Extra "--fake-object-info `"$FakeObjectInfo`" --fake-step-ms 10"
try {
    $sys = Invoke-Core $core GET '/api/system'
    if ($sys.root -ne (Resolve-Path $root).Path) { throw "core root is $($sys.root), expected $root" }
    $job = Invoke-Core $core POST '/api/jobs' @{ model = 'z-image-turbo'; op = 'txt2img'; values = @{ prompt = 'portable check, a lighthouse' } }
    $done = Wait-CoreJob $core $job.id -TimeoutSec 120
    if ($done.status -ne 'done') { throw "job $($done.status): $($done.error)" }
    $out = Invoke-Core $core GET "/api/outputs/$($done.outputs[0])"
    $file = Join-Path $root "data/$($out.output.path)"
    if (-not (Test-Path $file)) { throw "output not in the app folder: $file" }
    Write-Host "job ok -> $($out.output.path)"
} finally { Stop-Core $core }
$fs1 = Snapshot-Fs
$reg1 = Snapshot-Reg

# 2. Desktop window (and the single-instance hand-over).
if (-not $NoWindow -and $onWindows) {
    $env:LG_ENGINE = 'fake'; $env:LG_FAKE_OBJECT_INFO = $FakeObjectInfo
    try {
        $win = Start-Process -FilePath (Join-Path $root "LocalGallery$exe") -PassThru
        Start-Sleep -Seconds 15
        if ($win.HasExited) { throw "LocalGallery.exe exited early (code $($win.ExitCode)); log: $(Get-Content -Raw (Join-Path $root 'data/logs/app.log') -ErrorAction SilentlyContinue)" }
        $second = Start-Process -FilePath (Join-Path $root "LocalGallery$exe") -PassThru
        if (-not $second.WaitForExit(20000)) { $second.Kill(); throw 'second launch did not hand over to the running window' }
        if ($win.HasExited) { throw 'the first window closed when a second launch started' }
        $cores = @(Select-String -Path (Join-Path $root 'data/logs/app.log') -Pattern 'LocalGallery core on').Count
        if ($cores -ne 1) { throw "expected one core start in app.log, found $cores" }
        if (-not (Test-Path (Join-Path $root 'data/webview'))) { throw 'WebView2 profile is not in data/webview' }
        Write-Host 'window ok; second launch handed over'
        Stop-Process -Id $win.Id
        Start-Sleep -Seconds 3
        Get-Process -Name msedgewebview2 -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.CommandLine -like "*$root*" } | Stop-Process -Force -ErrorAction SilentlyContinue
    } finally { Remove-Item Env:LG_ENGINE, Env:LG_FAKE_OBJECT_INFO -ErrorAction SilentlyContinue }
}

# 3. Diff, phase by phase: nothing at all from the headless core; only WebView2-runtime and
# Windows bookkeeping from the window. Anything naming the app fails in either phase.
function Compare-Phase($fsA, $regA, $fsB, $regB, [string] $extraFs, [string] $extraReg, [string] $phase) {
    $found = @()
    foreach ($p in $fsB) {
        if ($fsA.Contains($p)) { continue }
        if ($p -match 'localgallery') { $found += "file ($phase): $p" }
        elseif ($p -match $allowFs) { Write-Host "  (windows) $p" }
        elseif ($extraFs -and $p -match $extraFs) { Write-Host "  (webview2 runtime) $p" }
        else { $found += "file ($phase): $p" }
    }
    foreach ($k in $regB) {
        if ($regA.Contains($k)) { continue }
        $key = ($k -split '\] ', 2)[0]
        if ($key -match 'localgallery') { $found += "registry ($phase): $k" }
        elseif ($k -match $allowReg) { Write-Host "  (windows) $k" }
        elseif ($extraReg -and $k -match $extraReg) { Write-Host "  (webview2 runtime) $k" }
        else { $found += "registry ($phase): $k" }
    }
    , $found
}
$fs2 = Snapshot-Fs
$reg2 = Snapshot-Reg
$bad = @()
$bad += Compare-Phase $fs0 $reg0 $fs1 $reg1 '' '' 'core'
$bad += Compare-Phase $fs1 $reg1 $fs2 $reg2 $webviewFs $webviewReg 'window'
if ($bad) {
    $bad | ForEach-Object { Write-Host "  NEW $_" -ForegroundColor Red }
    throw "the app wrote $($bad.Count) item(s) outside its folder"
}
$inside = (Get-ChildItem -Recurse -File (Join-Path $root 'data') | Measure-Object).Count
Write-Host "portable check passed: nothing written outside the app folder ($inside files inside data/)" -ForegroundColor Green
