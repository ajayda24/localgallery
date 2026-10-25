# Build the portable LocalGallery folder and zip.
#
#   pwsh scripts/build-portable.ps1            # -> dist/LocalGallery/ and dist/LocalGallery-<version>-win64.zip
#   pwsh scripts/build-portable.ps1 -NoZip -SkipUi
#
# The folder holds the app only; ComfyUI, Python and PyTorch are installed into engines/ on first
# run (the app asks first), and models go into models/.

param(
    [string] $Out = 'dist',
    [switch] $SkipUi,
    [switch] $NoZip
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$desktop = Join-Path $repo 'apps/desktop'
$shell = Join-Path $desktop 'src-tauri'
$exe = if ($IsWindows -or $env:OS -eq 'Windows_NT') { '.exe' } else { '' }

function Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Run($file, [string[]] $argv, $cwd) {
    Push-Location $cwd
    try { & $file @argv; if ($LASTEXITCODE) { throw "$file $($argv -join ' ') failed ($LASTEXITCODE)" } } finally { Pop-Location }
}

if (-not $SkipUi) {
    Step 'UI'
    Run npm @('ci', '--no-audit', '--no-fund') $desktop
    Run npm @('run', 'build') $desktop
}
Step 'core (headless binary)'
Run cargo @('build', '--release', '--locked', '-p', 'lg-server', '--bin', 'localgallery-core') $repo
Step 'desktop shell'
Run cargo @('build', '--release', '--locked') $shell

$version = (Get-Content (Join-Path $repo 'Cargo.toml') | Select-String '^version = "(.+)"').Matches[0].Groups[1].Value
$outDir = Join-Path $repo $Out
$app = Join-Path $outDir 'LocalGallery'
Step "assemble $app"
if (Test-Path $app) { Remove-Item -Recurse -Force $app }
New-Item -ItemType Directory -Force -Path $app | Out-Null

Copy-Item (Join-Path $shell "target/release/LocalGallery$exe") $app
Copy-Item (Join-Path $repo "target/release/localgallery-core$exe") $app
Copy-Item -Recurse (Join-Path $desktop 'dist') (Join-Path $app 'ui')
Copy-Item -Recurse (Join-Path $repo 'manifests') (Join-Path $app 'manifests')
Copy-Item (Join-Path $repo 'engines.lock.json') $app
# Scripts for checking the app on this PC (smoke.ps1, bench.ps1 and their shared helpers).
New-Item -ItemType Directory -Force -Path (Join-Path $app 'tools') | Out-Null
foreach ($f in 'common.ps1', 'smoke.ps1', 'bench.ps1') { Copy-Item (Join-Path $PSScriptRoot $f) (Join-Path $app 'tools') }
foreach ($d in 'models', 'engines') { New-Item -ItemType Directory -Force -Path (Join-Path $app $d) | Out-Null }
@"
LocalGallery $version

Double-click LocalGallery.exe. Everything the app writes stays in this folder:
  data\      settings, gallery, database, logs, the window's browser profile
  models\    model files (copy your own .safetensors / .gguf files into the subfolders)
  engines\   the image engine (ComfyUI + Python + PyTorch), installed on first run

Nothing is written to the registry or to %APPDATA%. To uninstall, delete this folder.

localgallery-core.exe runs the same app without a window (open the URL it prints).

Checking it on your PC (close the app first):
  powershell -ExecutionPolicy Bypass -File tools\smoke.ps1    first image, speed and memory use
  powershell -ExecutionPolicy Bypass -File tools\bench.ps1    timings for every downloaded model
"@ | Set-Content -Encoding utf8 (Join-Path $app 'README.txt')
"LocalGallery keeps all of its data in this folder.`nDelete the folder to uninstall.`n" |
    Set-Content -Encoding utf8 -NoNewline (Join-Path $app 'portable.txt')

$size = (Get-ChildItem -Recurse -File $app | Measure-Object Length -Sum).Sum / 1MB
Write-Host ("    {0:N1} MB" -f $size)

if (-not $NoZip) {
    $zip = Join-Path $outDir "LocalGallery-$version-win64.zip"
    Step "zip $zip"
    if (Test-Path $zip) { Remove-Item -Force $zip }
    Compress-Archive -Path $app -DestinationPath $zip -CompressionLevel Optimal
    $hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
    "$hash  $(Split-Path -Leaf $zip)" | Set-Content -Encoding ascii "$zip.sha256"
    Write-Host "    sha256 $hash"
}
