# First-run smoke test on a real NVIDIA PC (written for the RTX 5060 8 GB, works on any NVIDIA GPU).
#
# Close LocalGallery first, then from the app folder:
#   powershell -ExecutionPolicy Bypass -File tools\smoke.ps1
#   powershell -ExecutionPolicy Bypass -File tools\smoke.ps1 -Model flux2-klein-4b -Yes
#
# Steps: GPU check, engine install (asks), engine start with a CUDA self-check, model download
# (asks, shows the size), then the same image twice (cold load, then warm) with time, peak VRAM/RAM,
# shared-memory spill and an all-black check. Results go to data\bench\smoke-<time>.json.

param(
    [string] $Root = (Split-Path -Parent $PSScriptRoot),
    # Testing only: run against another engine (fake, or an existing ComfyUI URL).
    [string] $Engine = 'managed',
    [string] $CoreArgs = '',
    [string] $Model = 'z-image-turbo',
    [string] $Prompt = 'a red fox reading a newspaper in a sunlit cafe, detailed photo',
    [int] $Width = 1024,
    [int] $Height = 1024,
    [switch] $Yes
)
. (Join-Path $PSScriptRoot 'common.ps1')

function Confirm-Step([string] $question) {
    if ($Yes) { return }
    $a = Read-Host "$question [y/N]"
    if ($a -notmatch '^(y|yes)$') { throw 'stopped by user' }
}
function Section([string] $t) { Write-Host ''; Write-Host "== $t" -ForegroundColor Cyan }

$coreExe = Join-Path $Root 'localgallery-core.exe'
if (-not (Test-Path $coreExe) -and $env:OS -ne 'Windows_NT') { $coreExe = Join-Path $Root 'localgallery-core' }
if (-not (Test-Path $coreExe)) { throw "localgallery-core.exe not found in $Root (run this from the LocalGallery folder)" }
if (Get-Process -Name LocalGallery -ErrorAction SilentlyContinue) { throw 'Close LocalGallery first (the test uses the same data folder).' }

$real = $Engine -eq 'managed'
Section 'GPU'
$gpu = @('none', '', '0', '0')
if ($real) {
$smi = Get-Command nvidia-smi -ErrorAction SilentlyContinue
if (-not $smi) { throw 'nvidia-smi not found: install the NVIDIA driver first.' }
$gpu = (& nvidia-smi --query-gpu=name,driver_version,memory.total,compute_cap --format=csv,noheader,nounits | Select-Object -First 1).Split(',') | ForEach-Object { $_.Trim() }
Write-Host ("{0}, driver {1}, {2:N1} GB VRAM, compute {3}" -f $gpu[0], $gpu[1], ([double]$gpu[2] / 1024), $gpu[3])
if ([double]$gpu[3] -ge 12) { Write-Host 'Blackwell GPU: fp8 and NVFP4 formats are available.' }
}

$core = Start-Core -Exe $coreExe -Root $Root -Engine $Engine -Port 7898 -Extra $CoreArgs
$report = [ordered]@{ time = (Get-Date).ToString('s'); gpu = $gpu[0]; driver = $gpu[1]; vram_gb = [double]$gpu[2] / 1024; compute = $gpu[3]; model = $Model }
try {
    $sys = Invoke-Core $core GET '/api/system'
    $report.ram_gb = $sys.hardware.ram_total_gb

    Section 'Engine'
    $e = Invoke-Core $core GET '/api/engine'
    if ($real -and -not $e.installed) {
        Confirm-Step 'Install the image engine (ComfyUI + Python + PyTorch CUDA, about 3 GB) into this folder?'
        Invoke-Core $core POST '/api/engine/install' @{ profile = 'cuda' } | Out-Null
        $e = Wait-Engine $core -Until 'stopped', 'not_installed' -TimeoutSec 7200
        if ($e.state -ne 'stopped') { throw "install failed: $($e.detail)" }
    }
    Write-Host $e.detail
    Invoke-Core $core POST '/api/engine/start' -TimeoutSec 600 | Out-Null
    $e = Wait-Engine $core -Until 'ready' -TimeoutSec 600
    $stats = Invoke-RestMethod "$($e.url)/system_stats"
    $dev = $stats.devices | Select-Object -First 1
    Write-Host "ComfyUI $($stats.system.comfyui_version), torch $($stats.system.pytorch_version), device: $($dev.name)"
    if ($real -and $dev.type -ne 'cuda') { throw "ComfyUI is not using CUDA (device type $($dev.type))" }
    $report.torch = $stats.system.pytorch_version
    $report.comfyui = $stats.system.comfyui_version

    Section "Model: $Model"
    $m = Invoke-Core $core GET "/api/models/$Model"
    $op = $m.main_op
    $missing = @($m.files | Where-Object { -not $_.optional -and @($_.present).Count -eq 0 -and (@($_.ops).Count -eq 0 -or @($_.ops) -contains $op) })
    if ($real -and $missing.Count) {
        $gb = ($missing | ForEach-Object { $_.variants[0].size_gb } | Measure-Object -Sum).Sum
        $missing | ForEach-Object { Write-Host ("  {0}: {1} ({2:N1} GB)" -f $_.role, $_.variants[0].name, $_.variants[0].size_gb) }
        Confirm-Step ("Download {0:N1} GB of model files into models\?" -f $gb)
        $started = Invoke-Core $core POST '/api/downloads' @{ model = $Model; op = $op }
        $ids = @($started.started)
        do {
            Start-Sleep -Seconds 2
            $dl = @(Invoke-Core $core GET '/api/downloads') | ForEach-Object { $_ } | Where-Object { $ids -contains $_.id }
            $recv = ($dl | Measure-Object received -Sum).Sum; $tot = ($dl | Measure-Object total -Sum).Sum
            if ($tot) { Write-Progress -Activity 'Downloading' -Status ("{0:N1} / {1:N1} GB" -f ($recv / 1GB), ($tot / 1GB)) -PercentComplete ([Math]::Min(100, $recv * 100 / $tot)) }
            $failed = @($dl | Where-Object { $_.state -eq 'failed' })
            if ($failed.Count) { throw "download failed: $($failed[0].name): $($failed[0].error)" }
        } while (@($dl | Where-Object { $_.state -ne 'done' }).Count)
        Write-Progress -Activity 'Downloading' -Completed
        Invoke-Core $core POST '/api/library/rescan' | Out-Null
        $m = Invoke-Core $core GET "/api/models/$Model"
    }
    if ($m.fit) { Write-Host "Plan: $($m.fit.badge), $($m.fit.precision). $(@($m.fit.explain) -join ' ')" }

    Section 'Generate (cold, then warm)'
    $sizeInput = @($m.inputs | Where-Object { $_.kind -eq 'size' }) | Select-Object -First 1
    $promptInput = @($m.inputs | Where-Object { $_.kind -eq 'text' }) | Select-Object -First 1
    $seedInput = @($m.inputs | Where-Object { $_.kind -eq 'seed' }) | Select-Object -First 1
    $values = @{}
    if ($promptInput) { $values[$promptInput.id] = $Prompt }
    if ($sizeInput) { $values[$sizeInput.id] = @{ w = $Width; h = $Height } }
    if ($seedInput) { $values[$seedInput.id] = 42 }
    $runs = @()
    foreach ($label in 'cold', 'warm') {
        $job = Invoke-Core $core POST '/api/jobs' @{ model = $Model; op = $op; values = $values }
        $done = Wait-CoreJob $core $job.id -TimeoutSec 3600
        if ($done.status -ne 'done') { throw "$label run $($done.status): $($done.error)" }
        $o = (Invoke-Core $core GET "/api/outputs/$($done.outputs[0])").output
        $file = Join-Path (Join-Path $Root 'data') $o.path
        $r = [ordered]@{ run = $label; seconds = $done.metrics.seconds; peak_vram_gb = $done.metrics.peak_vram_gb; peak_ram_gb = $done.metrics.peak_ram_gb; shared_gpu_gb = $done.metrics.shared_gpu_gb; plan = $done.metrics.plan; precision = $done.metrics.precision; file = $file }
        if ($o.media -eq 'image' -and $env:OS -eq 'Windows_NT') {
            $r.brightness = [Math]::Round((Get-MeanBrightness $file), 1)
            if ($r.brightness -lt 4) { throw "$label output is black (mean brightness $($r.brightness)): the attention backend or precision is broken on this GPU" }
        }
        Write-Host ("  {0}: {1:N1} s, peak VRAM {2:N2} GB, peak RAM {3:N2} GB, shared spill {4:N2} GB, {5}" -f $label, $r.seconds, $r.peak_vram_gb, $r.peak_ram_gb, $r.shared_gpu_gb, $r.plan)
        $runs += [pscustomobject]$r
    }
    $report.runs = $runs
    if ($runs[1].shared_gpu_gb -gt 0.5) { Write-Host 'Warning: the warm run spilled into shared GPU memory (slow). Try the Speed policy or a smaller format.' -ForegroundColor Yellow }
} finally {
    Stop-Core $core
}

$dir = Join-Path $Root 'data\bench'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$out = Join-Path $dir ("smoke-{0}.json" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
$report | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $out
Write-Host ''
Write-Host "Smoke test passed. Report: $out" -ForegroundColor Green
