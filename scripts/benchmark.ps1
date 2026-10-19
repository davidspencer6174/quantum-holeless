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
$cpuSeconds = try { [Math]::Round($process.TotalProcessorTime.TotalSeconds, 3) } catch { $null }
$stdout = Get-Content -LiteralPath (Join-Path $runDir 'stdout.txt') -Raw
$records = [regex]::Matches($stdout, 'New record found at depth (\d+)') |
    ForEach-Object { [int]$_.Groups[1].Value }
$maxRecord = if ($records.Count) { ($records | Measure-Object -Maximum).Maximum } else { $null }
$progress = [regex]::Matches($stdout, 'Visited (\d+) unique boards')
$lastProgress = if ($progress.Count) { [int]$progress[$progress.Count - 1].Groups[1].Value } else { $null }
$searchTime = [regex]::Match($stdout, 'Total time taken: ([\d.]+)(ms|s)')
$searchSeconds = if ($searchTime.Success) {
    $value = [double]::Parse($searchTime.Groups[1].Value, [Globalization.CultureInfo]::InvariantCulture)
    if ($searchTime.Groups[2].Value -eq 'ms') { $value / 1000 } else { $value }
} else { $null }
$result = [pscustomobject]@{
    executable = $exePath
    arguments = $SearchArgs
    elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
    cpu_seconds = $cpuSeconds
    reported_search_seconds = $searchSeconds
    peak_working_set_mib = [Math]::Round($peakWorkingSet / 1MB, 2)
    sampled_peak_private_mib = [Math]::Round($peakPrivate / 1MB, 2)
    timed_out = $timedOut
    exit_code = $process.ExitCode
    best_record_steps = $maxRecord
    last_reported_board_count = $lastProgress
    output_directory = $runDir
}
$result | ConvertTo-Json | Tee-Object -FilePath (Join-Path $runDir 'metrics.json')
Get-Content -LiteralPath (Join-Path $runDir 'stdout.txt') -Tail 5
Get-Content -LiteralPath (Join-Path $runDir 'stderr.txt') -Tail 5
