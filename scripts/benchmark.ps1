param(
    [Parameter(Mandatory=$true)][string]$Executable,
    [string[]]$SearchArgs = @('4', '4'),
    [string]$Label = 'run',
    [int]$TimeoutSeconds = 120
)
$ErrorActionPreference = 'Stop'
$exePath = (Resolve-Path -LiteralPath $Executable).Path
$root = Split-Path -Parent $PSScriptRoot
$runDir = Join-Path $root ('scratch/benchmarks/' + $Label + '-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
New-Item -ItemType Directory -Path $runDir -Force | Out-Null
$watch = [Diagnostics.Stopwatch]::StartNew()
$process = Start-Process -FilePath $exePath -ArgumentList $SearchArgs -WorkingDirectory $runDir -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $runDir 'stdout.txt') -RedirectStandardError (Join-Path $runDir 'stderr.txt')
$peakWorkingSet = 0L
$peakPrivate = 0L
$timedOut = $false
while (-not $process.HasExited) {
    $process.Refresh()
    $peakWorkingSet = [Math]::Max($peakWorkingSet, $process.PeakWorkingSet64)
    $peakPrivate = [Math]::Max($peakPrivate, $process.PrivateMemorySize64)
    if ($watch.Elapsed.TotalSeconds -ge $TimeoutSeconds) {
        $timedOut = $true
        $process.Kill()
        break
    }
    Start-Sleep -Milliseconds 50
}
$process.WaitForExit()
$watch.Stop()
$result = [pscustomobject]@{
    executable = $exePath
    arguments = $SearchArgs
    elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
    peak_working_set_mib = [Math]::Round($peakWorkingSet / 1MB, 2)
    sampled_peak_private_mib = [Math]::Round($peakPrivate / 1MB, 2)
    timed_out = $timedOut
    exit_code = $process.ExitCode
    output_directory = $runDir
}
$result | ConvertTo-Json | Tee-Object -FilePath (Join-Path $runDir 'metrics.json')
Get-Content -LiteralPath (Join-Path $runDir 'stdout.txt') -Tail 5
Get-Content -LiteralPath (Join-Path $runDir 'stderr.txt') -Tail 5
