# Install the real image engine through the packaged app, start it, and run the real-engine tests.
#
#   pwsh scripts/engine-check.ps1 -App dist/LocalGallery              # CPU PyTorch (CI)
#   pwsh scripts/engine-check.ps1 -App dist/LocalGallery -Profile cuda # on an NVIDIA PC
#
# This downloads uv, Python, the pinned ComfyUI and PyTorch (about 1 GB for CPU, about 3 GB for CUDA)
# into the app folder copy, exactly as a user's first run would.

param(
    [string] $App = 'dist/LocalGallery',
    [ValidateSet('cpu', 'cuda')] [string] $Profile = 'cpu',
    [string] $Work = $(if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }),
    [switch] $SkipTests
)
. "$PSScriptRoot/common.ps1"
$repo = Split-Path -Parent $PSScriptRoot
$exe = if ($IsWindows -or $env:OS -eq 'Windows_NT') { '.exe' } else { '' }

$dest = Join-Path $Work "Local Gallery $([char]0xFC) engine"
if (Test-Path $dest) { Remove-Item -Recurse -Force $dest }
New-Item -ItemType Directory -Force -Path $dest | Out-Null
Copy-Item -Recurse (Resolve-Path $App) $dest
$root = Join-Path $dest (Split-Path -Leaf (Resolve-Path $App))
Write-Host "app folder: $root"

$core = Start-Core -Exe (Join-Path $root "localgallery-core$exe") -Root $root -Engine managed -Port 7897
try {
    $e = Invoke-Core $core GET '/api/engine'
    if ($e.installed) { throw 'a fresh copy should not have an engine yet' }

    $t0 = Get-Date
    Invoke-Core $core POST '/api/engine/install' @{ profile = $Profile } | Out-Null
    $e = Wait-Engine $core -Until 'stopped', 'not_installed' -TimeoutSec 3600
    if ($e.state -ne 'stopped' -or -not $e.installed) { throw "install failed: $($e.detail)" }
    Write-Host ("installed in {0:N0} s: {1}" -f ((Get-Date) - $t0).TotalSeconds, $e.detail)
    if ($Profile -eq 'cuda' -and $e.detail -notmatch 'cuda|NVIDIA|RTX') { throw "expected a CUDA build: $($e.detail)" }

    foreach ($p in 'engines/full/ComfyUI/main.py', 'engines/full/constraints.txt', 'engines/python') {
        if (-not (Test-Path (Join-Path $root $p))) { throw "missing $p after install" }
    }
    if (Select-String -Path (Join-Path $root 'engines/full/constraints.txt') -Pattern '^xformers' -Quiet) { throw 'xformers must never be installed' }

    $t0 = Get-Date
    Invoke-Core $core POST '/api/engine/start' -TimeoutSec 600 | Out-Null
    $e = Wait-Engine $core -Until 'ready' -TimeoutSec 600
    Write-Host ("engine ready in {0:N0} s at {1}, flags: {2}" -f ((Get-Date) - $t0).TotalSeconds, $e.url, ($e.flags -join ' '))
    $stats = Invoke-RestMethod "$($e.url)/system_stats"
    Write-Host "ComfyUI $($stats.system.comfyui_version), Python $($stats.system.python_version.Split(' ')[0]), torch $($stats.system.pytorch_version)"

    if (-not $SkipTests) {
        $env:LG_REAL_COMFY_URL = $e.url
        $env:LG_COMFY_DIR = (Join-Path $root 'engines/full/ComfyUI')
        Push-Location $repo
        try {
            foreach ($t in @(
                    @('-p', 'lg-workflow', '--test', 'blueprints'),
                    @('-p', 'lg-engine', '--test', 'real_builtin', '--test', 'real_blueprints'),
                    @('-p', 'lg-server', '--test', 'real_e2e'))) {
                Write-Host "==> cargo test $($t -join ' ')" -ForegroundColor Cyan
                & cargo test --locked @t -- --include-ignored --nocapture
                if ($LASTEXITCODE) { throw "cargo test $($t -join ' ') failed" }
            }
        } finally { Pop-Location }
    }
} finally {
    Stop-Core $core
}
Write-Host 'engine check passed' -ForegroundColor Green
