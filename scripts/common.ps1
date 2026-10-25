# Shared helpers for the LocalGallery scripts: start the core headless and talk to its API.
# Dot-source it:  . "$PSScriptRoot\common.ps1"

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

function Start-Core {
    param(
        [Parameter(Mandatory)] [string] $Exe,
        [Parameter(Mandatory)] [string] $Root,
        [string] $Engine = 'managed',
        [int] $Port = 7895,
        [string] $Token = ([guid]::NewGuid().ToString('N')),
        [string] $Extra = ''
    )
    New-Item -ItemType Directory -Force -Path (Join-Path $Root 'data\logs') | Out-Null
    $log = Join-Path $Root 'data\logs\core-script.log'
    # Start-Process joins arguments without quoting, so quote paths ourselves (the root may contain spaces).
    $argLine = "--root `"$Root`" --engine $Engine --port $Port --token $Token $Extra"
    $p = Start-Process -FilePath $Exe -ArgumentList $argLine -PassThru -NoNewWindow `
        -RedirectStandardOutput $log -RedirectStandardError "$log.err"
    $core = [pscustomobject]@{ Process = $p; Base = "http://127.0.0.1:$Port"; Token = $Token; Log = $log }
    $deadline = (Get-Date).AddSeconds(90)
    while ((Get-Date) -lt $deadline) {
        if ($p.HasExited) { throw "core exited early (code $($p.ExitCode)); see $log.err" }
        try { Invoke-RestMethod "$($core.Base)/api/health" -TimeoutSec 2 | Out-Null; return $core } catch { Start-Sleep -Milliseconds 300 }
    }
    throw "core did not answer on $($core.Base) within 90 s"
}

function Invoke-Core {
    param([Parameter(Mandatory)] $Core, [string] $Method = 'GET', [Parameter(Mandatory)] [string] $Path, $Body = $null, [int] $TimeoutSec = 120)
    $req = @{ Uri = "$($Core.Base)$Path"; Method = $Method; Headers = @{ Authorization = "Bearer $($Core.Token)" }; TimeoutSec = $TimeoutSec }
    if ($null -ne $Body) { $req.Body = ($Body | ConvertTo-Json -Depth 30 -Compress); $req.ContentType = 'application/json' }
    Invoke-RestMethod @req
}

function Wait-CoreJob {
    param([Parameter(Mandatory)] $Core, [Parameter(Mandatory)] [string] $Id, [int] $TimeoutSec = 3600)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        $job = @(Invoke-Core $Core GET '/api/jobs') | ForEach-Object { $_ } | Where-Object { $_.id -eq $Id } | Select-Object -First 1
        if ($job -and $job.status -in 'done', 'failed', 'cancelled') { return $job }
        Start-Sleep -Seconds 1
    }
    throw "job $Id did not finish within $TimeoutSec s"
}

function Wait-Engine {
    param([Parameter(Mandatory)] $Core, [string[]] $Until = @('ready', 'stopped'), [int] $TimeoutSec = 3600)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $last = ''
    while ((Get-Date) -lt $deadline) {
        $e = Invoke-Core $Core GET '/api/engine'
        $line = "$($e.state) $($e.detail)"
        if ($line -ne $last) { Write-Host "  engine: $line"; $last = $line }
        if ($e.state -in $Until) { return $e }
        if ($e.state -eq 'error') { throw "engine error: $($e.detail)" }
        Start-Sleep -Seconds 2
    }
    throw "engine did not reach $($Until -join '/') within $TimeoutSec s"
}

function Stop-Core {
    param([Parameter(Mandatory)] $Core)
    try { Invoke-Core $Core POST '/api/engine/stop' -TimeoutSec 30 | Out-Null } catch { }
    if (-not $Core.Process.HasExited) { Stop-Process -Id $Core.Process.Id -Force }
}

# Mean brightness 0..255 of an image file, to catch all-black outputs (a broken attention kernel).
function Get-MeanBrightness {
    param([Parameter(Mandatory)] [string] $Path)
    Add-Type -AssemblyName System.Drawing
    $bmp = [System.Drawing.Bitmap]::FromFile($Path)
    try {
        $sum = 0.0; $n = 0
        $stepX = [Math]::Max(1, [int]($bmp.Width / 64)); $stepY = [Math]::Max(1, [int]($bmp.Height / 64))
        for ($y = 0; $y -lt $bmp.Height; $y += $stepY) {
            for ($x = 0; $x -lt $bmp.Width; $x += $stepX) {
                $c = $bmp.GetPixel($x, $y); $sum += ($c.R + $c.G + $c.B) / 3.0; $n++
            }
        }
        return $sum / $n
    } finally { $bmp.Dispose() }
}
