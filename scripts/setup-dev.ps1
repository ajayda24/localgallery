# One-time developer setup on Windows: checks the toolchain, installs UI packages, builds everything.
#
#   pwsh scripts/setup-dev.ps1
#
# Afterwards:
#   cargo run -p lg-server --bin localgallery-core -- --root .\dev-root --engine fake --ui apps\desktop\dist --manifests manifests
#   cd apps\desktop; npm run dev        # UI with hot reload, proxied to the core on :7890
#   cd apps\desktop\src-tauri; cargo run # the desktop window (set LG_ROOT, LG_UI, LG_MANIFESTS for a dev tree)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$ok = $true

function Need($name, $cmd, $hint) {
    $c = Get-Command $cmd -ErrorAction SilentlyContinue
    if ($c) { Write-Host ("  ok  {0,-10} {1}" -f $name, $c.Source) } else { Write-Host ("  --  {0,-10} missing: {1}" -f $name, $hint) -ForegroundColor Yellow; $script:ok = $false }
}

Write-Host 'Toolchain'
Need 'rustup' 'rustup' 'install from https://rustup.rs (the repo pins its toolchain in rust-toolchain.toml)'
Need 'node' 'node' 'install Node.js 24 LTS from https://nodejs.org'
Need 'npm' 'npm' 'comes with Node.js'
Need 'git' 'git' 'install Git for Windows'

$wv = Get-ItemProperty -ErrorAction SilentlyContinue 'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
if ($wv) { Write-Host "  ok  WebView2   $($wv.pv)" } else { Write-Host '  --  WebView2   runtime not found (Windows 11 ships it; otherwise install the Evergreen runtime)' -ForegroundColor Yellow }
if (Get-Command nvidia-smi -ErrorAction SilentlyContinue) {
    Write-Host ("  ok  GPU        {0}" -f (& nvidia-smi --query-gpu=name,driver_version --format=csv,noheader | Select-Object -First 1))
} else { Write-Host '  --  GPU        nvidia-smi not found: the app will run the engine on the CPU' -ForegroundColor Yellow }
if (-not $ok) { throw 'install the missing tools above, then run this again' }

Push-Location (Join-Path $repo 'apps/desktop')
try {
    npm ci --no-audit --no-fund; if ($LASTEXITCODE) { throw 'npm ci failed' }
    npm run build; if ($LASTEXITCODE) { throw 'UI build failed' }
} finally { Pop-Location }
Push-Location $repo
try { cargo build --workspace; if ($LASTEXITCODE) { throw 'cargo build failed' } } finally { Pop-Location }
Push-Location (Join-Path $repo 'apps/desktop/src-tauri')
try { cargo build; if ($LASTEXITCODE) { throw 'desktop shell build failed' } } finally { Pop-Location }
Write-Host 'Ready. Run tests with: cargo test --workspace; cd apps\desktop; npm test; npm run e2e' -ForegroundColor Green
