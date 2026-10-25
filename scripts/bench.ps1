# Benchmark the installed models on this PC: models x sizes x planner policies, N runs each.
#
# Close LocalGallery first, then from the app folder:
#   powershell -ExecutionPolicy Bypass -File tools\bench.ps1
#   powershell -ExecutionPolicy Bypass -File tools\bench.ps1 -Models z-image-turbo -Sizes 1024x1024,1536x1536 -Runs 5
#
# Writes data\bench\bench-<time>.csv and prints a summary. Every run is also stored in the app's
# benchmark history, which the planner uses for time estimates and for choosing formats.

param(
    [string] $Root = (Split-Path -Parent $PSScriptRoot),
    # Testing only: run against another engine (fake, or an existing ComfyUI URL).
    [string] $Engine = 'managed',
    [string] $CoreArgs = '',
    [string[]] $Models = @(),
    [string[]] $Sizes = @('768x768', '1024x1024'),
    [string[]] $Policies = @('balanced', 'quality', 'speed'),
    [int] $Runs = 3,
    [string] $Prompt = 'a lighthouse on a cliff at sunset, dramatic clouds, detailed photo'
)
. (Join-Path $PSScriptRoot 'common.ps1')

# `-File` passes "a,b" as one string; accept both forms.
function Split-List($v) { @($v | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ }) }
$Models = Split-List $Models; $Sizes = Split-List $Sizes; $Policies = Split-List $Policies | ForEach-Object { $_.ToLower() }
foreach ($p in $Policies) { if ($p -notin 'balanced', 'quality', 'speed') { throw "unknown policy $p (use balanced, quality or speed)" } }
foreach ($s in $Sizes) { if ($s -notmatch '^\d+x\d+$') { throw "size must look like 1024x1024, got $s" } }

$coreExe = Join-Path $Root 'localgallery-core.exe'
if (-not (Test-Path $coreExe) -and $env:OS -ne 'Windows_NT') { $coreExe = Join-Path $Root 'localgallery-core' }
if (-not (Test-Path $coreExe)) { throw "localgallery-core.exe not found in $Root" }
if (Get-Process -Name LocalGallery -ErrorAction SilentlyContinue) { throw 'Close LocalGallery first (the benchmark uses the same data folder).' }

$core = Start-Core -Exe $coreExe -Root $Root -Engine $Engine -Port 7899 -Extra $CoreArgs
$rows = @()
$before = $null
try {
    $before = (Invoke-Core $core GET '/api/settings').planner.policy
    $all = @(Invoke-Core $core GET '/api/models') | ForEach-Object { $_ }
    $pick = if ($Models.Count) { $all | Where-Object { $Models -contains $_.id } } else { $all | Where-Object { $_.installed -and $_.media -eq 'image' -and $_.id -notlike 'bp-*' } }
    $pick = @($pick)
    if (-not $pick.Count) { throw 'No downloaded image models to benchmark. Download one in the app (Models tab) or run smoke.ps1 first.' }
    Write-Host ("Models: {0}" -f (($pick | ForEach-Object { $_.id }) -join ', '))

    foreach ($policy in $Policies) {
        Invoke-Core $core PUT '/api/settings' @{ planner = @{ policy = $policy } } | Out-Null
        foreach ($s in $pick) {
            $m = Invoke-Core $core GET "/api/models/$($s.id)"
            $op = $m.main_op
            $sizeInput = @($m.inputs | Where-Object { $_.kind -eq 'size' }) | Select-Object -First 1
            $promptInput = @($m.inputs | Where-Object { $_.kind -eq 'text' }) | Select-Object -First 1
            $seedInput = @($m.inputs | Where-Object { $_.kind -eq 'seed' }) | Select-Object -First 1
            foreach ($size in $Sizes) {
                $w, $h = $size.Split('x') | ForEach-Object { [int]$_ }
                for ($i = 1; $i -le $Runs; $i++) {
                    $values = @{}
                    if ($promptInput) { $values[$promptInput.id] = $Prompt }
                    if ($sizeInput) { $values[$sizeInput.id] = @{ w = $w; h = $h } }
                    if ($seedInput) { $values[$seedInput.id] = 1000 + $i }
                    $job = Invoke-Core $core POST '/api/jobs' @{ model = $s.id; op = $op; values = $values }
                    $done = Wait-CoreJob $core $job.id -TimeoutSec 3600
                    $row = [pscustomobject][ordered]@{
                        policy = $policy; model = $s.id; size = $size; run = $i; status = $done.status
                        seconds = $(if ($done.metrics) { [Math]::Round($done.metrics.seconds, 2) } else { $null })
                        peak_vram_gb = $(if ($done.metrics) { $done.metrics.peak_vram_gb } else { $null })
                        peak_ram_gb = $(if ($done.metrics) { $done.metrics.peak_ram_gb } else { $null })
                        shared_gpu_gb = $(if ($done.metrics) { $done.metrics.shared_gpu_gb } else { $null })
                        precision = $(if ($done.metrics) { $done.metrics.precision } else { $null })
                        plan = $(if ($done.metrics) { $done.metrics.plan } else { $null })
                        error = $done.error
                    }
                    Write-Host ("  {0,-9} {1,-22} {2,-10} #{3}  {4,7:N2} s  VRAM {5,5:N2}  RAM {6,5:N2}  spill {7,4:N2}  {8}" -f $policy, $s.id, $size, $i, $row.seconds, $row.peak_vram_gb, $row.peak_ram_gb, $row.shared_gpu_gb, $(if ($row.status -eq 'done') { $row.precision } else { "FAILED: $($row.error)" }))
                    $rows += $row
                }
            }
        }
    }
} finally {
    if ($before) { try { Invoke-Core $core PUT '/api/settings' @{ planner = @{ policy = $before } } | Out-Null } catch { } }
    Stop-Core $core
}

$dir = Join-Path $Root 'data\bench'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$csv = Join-Path $dir ("bench-{0}.csv" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
$rows | Export-Csv -NoTypeInformation -Encoding UTF8 $csv

Write-Host ''
Write-Host 'Median seconds (first run of each group is the cold load and is left out when there are more):' -ForegroundColor Cyan
foreach ($g in @($rows | Where-Object { $_.status -eq 'done' } | Group-Object policy, model, size)) {
    $t = @($g.Group | Sort-Object run | ForEach-Object { [double]$_.seconds })
    if ($t.Count -gt 1) { $t = @($t[1..($t.Count - 1)]) }
    $sorted = @($t | Sort-Object)
    $median = $sorted[[int][Math]::Floor(($sorted.Count - 1) / 2)]
    Write-Host ("  {0,-50} {1,8:N2} s   ({2} runs)" -f $g.Name, $median, $g.Count)
}
Write-Host "CSV: $csv" -ForegroundColor Green
