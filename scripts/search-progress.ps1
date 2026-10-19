param(
    [string]$LogDirectory,
    [string]$Frontier,
    [switch]$Json
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $LogDirectory) {
    $latest = Get-ChildItem -LiteralPath (Join-Path $root 'logs') -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'coordinator.log') } |
        Sort-Object Name -Descending | Select-Object -First 1
    if (-not $latest) { throw 'No parallel run found. Supply -LogDirectory.' }
    $LogDirectory = $latest.FullName
}
$LogDirectory = (Resolve-Path -LiteralPath $LogDirectory).Path
$files = @(Get-ChildItem -LiteralPath $LogDirectory -Filter '*.log')
if ($files.Count -eq 0) { throw 'No log files found.' }
$jobs = @{}
$workers = @()
$samples = @{}
$elapsed = 0L
$boards = 0L
$best = 0
$identity = $null
$failures = 0
foreach ($file in $files) {
    $last = $null
    $latestRootMoves = $null
    $finished = @{}
    $clock = 0L
    # Read-only sharing allows the search to continue appending while we inspect it.
    $stream = [System.IO.File]::Open($file.FullName, 'Open', 'Read', 'ReadWrite')
    $reader = New-Object System.IO.StreamReader($stream)
    try {
        while ($null -ne ($line = $reader.ReadLine())) {
            if ($line -match 'board=(\d+x\d+) exit=(\d+,\d+) elapsed=(\d+):(\d+):(\d+).*?boards=(\d+) best=(\d+).*?phase=([^ ]+).*? moves=([^ ]+)') {
                $g = $Matches
                $currentIdentity = "$($g[1]) exit $($g[2])"
                if ($identity -and $identity -ne $currentIdentity) { throw 'Logs contain multiple board/exit searches; select one run directory.' }
                $identity = $currentIdentity
                $clock = 3600L * [long]$g[3] + 60L * [long]$g[4] + [long]$g[5]
                $elapsed = [Math]::Max($elapsed, $clock)
                $boards = [Math]::Max($boards, [long]$g[6])
                $best = [Math]::Max($best, [int]$g[7])
                if (-not $samples.ContainsKey($clock) -or $samples[$clock] -lt [long]$g[6]) { $samples[$clock] = [long]$g[6] }
                $last = [pscustomobject]@{ Seconds=$clock; Phase=$g[8]; Moves=$g[9] }
                $latestRootMoves = $last.Moves.Trim()
            }
            if ($line -match '^Visited (\d+) unique boards.*longest found (\d+)') {
                $boards = [Math]::Max($boards, [long]$Matches[1])
                $best = [Math]::Max($best, [int]$Matches[2])
            }
            if ($line -match '^\[worker:failed\]') { $failures++ }
            if ($line -match '^\[subtree:(start|finished|cancelled|saved)\] id=(\d+)(?: depth=\d+)? moves=([UDLR]+)$') {
                $event = $Matches[1]; $id = [long]$Matches[2]; $moves = $Matches[3]
                if (-not $jobs.ContainsKey($id)) {
                    $jobs[$id] = [pscustomobject]@{ Id=$id; Moves=$moves; Worker=$file.BaseName; Started=$false; Finished=$false; Cancelled=$false; Saved=$false; StartUpper=$null; BestSteps=$null }
                }
                $job = $jobs[$id]
                if ($job.Moves -ne $moves -or $job.Worker -ne $file.BaseName) { throw "Inconsistent subtree ID $id in logs." }
                switch ($event) {
                    'start' { $job.Started = $true; $latestRootMoves = $null }
                    'finished' { $job.Finished = $true; $finished[$id] = $true }
                    'cancelled' { $job.Cancelled = $true }
                    'saved' { $job.Saved = $true }
                }
            }
            if ($line -match '^\[subtree:(?:record|summary)\] id=(\d+) (?:steps|best_steps)=(\d+)') {
                $recordId = [long]$Matches[1]
                $recordSteps = [int]$Matches[2]
                if ($jobs.ContainsKey($recordId)) { $jobs[$recordId].BestSteps = $recordSteps }
            }
            # The first timestamp after a start bounds that start from above.
            if ($last -and $line -match 'elapsed=') {
                foreach ($job in $jobs.Values) {
                    if ($job.Worker -eq $file.BaseName -and $job.Started -and $null -eq $job.StartUpper) { $job.StartUpper = $clock }
                }
            }
        }
    } finally { $reader.Dispose() }
    if ($file.BaseName -like 'worker-*') {
        $active = @($jobs.Values | Where-Object { $_.Worker -eq $file.BaseName -and $_.Started -and -not $_.Finished -and -not $_.Cancelled } | Sort-Object Id)
        $age = $null
        if ($active.Count -eq 1 -and $last -and $null -ne $active[0].StartUpper) { $age = [Math]::Max(0, ($last.Seconds - $active[0].StartUpper) / 3600.0) }
        $prefix25 = $null
        if ($active.Count -eq 1 -and $latestRootMoves -match '^[UDLR]+$' -and $latestRootMoves.StartsWith($active[0].Moves)) {
            $prefix25 = $latestRootMoves.Substring(0, [Math]::Min(25, $latestRootMoves.Length))
        }
        $workers += [pscustomobject]@{ Worker=$file.BaseName; Finished=$finished.Count; Active=($active.Id -join ','); BestSteps=$(if ($active.Count -eq 1) { $active[0].BestSteps }); ActiveHoursAtLeast=$age; LatestHours=$(if ($last) { $last.Seconds / 3600.0 }); Phase=$(if ($last) { $last.Phase }); Prefix=($active.Moves -join ','); Prefix25=$prefix25 }
    }
}
$started = @($jobs.Values | Where-Object Started).Count
$completed = @($jobs.Values | Where-Object Finished).Count
$savedCount = @($jobs.Values | Where-Object Saved).Count
$cancelled = @($jobs.Values | Where-Object Cancelled).Count
$activeCount = @($jobs.Values | Where-Object { $_.Started -and -not $_.Finished -and -not $_.Cancelled }).Count
$times = @($samples.Keys | Sort-Object)
$rate = $null
if ($times.Count -gt 1) {
    $end = $times[-1]
    $begin = @($times | Where-Object { $_ -le $end - 3600 } | Select-Object -Last 1)
    if ($begin.Count -eq 0) { $begin = @($times[0]) }
    if ($end -gt $begin[0]) { $rate = ($samples[$end] - $samples[$begin[0]]) * 3600.0 / ($end - $begin[0]) }
}
$reference = $null
if ($Frontier) {
    $roots = @{}
    foreach ($line in [System.IO.File]::ReadLines((Resolve-Path -LiteralPath $Frontier).Path)) {
        if ($line -notmatch '^(\d+)\t([UDLR]+)$') { throw 'Invalid frontier TSV row.' }
        $id = [long]$Matches[1]
        if ($roots.ContainsKey($id)) { throw "Duplicate frontier ID $id." }
        $roots[$id] = $Matches[2]
    }
    foreach ($job in $jobs.Values) {
        if (-not $roots.ContainsKey($job.Id) -or $roots[$job.Id] -ne $job.Moves) { throw "Frontier does not match observed subtree $($job.Id); omit -Frontier or regenerate it for this run." }
    }
    $reference = [pscustomobject]@{ CandidateRoots=$roots.Count; ObservedPrefixesMatch=$true; Scenarios=@(foreach ($hours in @(0.5, 1.0, 2.0)) {
        [pscustomobject]@{ MeanWorkerHoursPerRoot=$hours; TotalDays=$(if ($workers.Count) { $roots.Count * $hours / $workers.Count / 24.0 }) }
    }) }
}
$report = [pscustomobject]@{ Directory=$LogDirectory; Search=$identity; LatestReportedHours=$elapsed/3600.0; LatestObservedBoards=$boards; Best=$best; Started=$started; Finished=$completed; Saved=$savedCount; Active=$activeCount; Cancelled=$cancelled; FailureMessages=$failures; RecentBoardsPerHour=$rate; Workers=$workers; Reference=$reference }
if ($Json) { $report | ConvertTo-Json -Depth 6; return }
Write-Output "Logs: $LogDirectory"
Write-Output ("{0}: latest reported elapsed {1:N2} hours; {2:N0} boards observed; record {3}" -f $identity, ($elapsed/3600.0), $boards, $best)
Write-Output "Subtrees: $started started, $completed finished, $activeCount active, $cancelled cancelled; $savedCount saved from checkpoint; $failures failure messages."
if ($null -ne $rate) { Write-Output ("Recent throughput: {0:N0} boards/hour (global counters, not summed across workers)." -f $rate) }
$workers | Format-Table Worker, Finished, Active, BestSteps, @{n='Age >= hours';e={if ($null -ne $_.ActiveHoursAtLeast) {'{0:N2}' -f $_.ActiveHoursAtLeast}}}, Phase, Prefix, Prefix25 -AutoSize | Out-String -Width 240 | Write-Output
Write-Output 'Elapsed and active ages use logged timestamps, not the current clock. Events may be newer than the last timestamp.'
Write-Output 'Active means started without a terminal log entry; it does not prove the process is still running.'
if ($reference) {
    Write-Output "Reference frontier: $($reference.CandidateRoots) candidate roots; all observed ID/prefix pairs match."
    $reference.Scenarios | Format-Table MeanWorkerHoursPerRoot, @{n='Total days';e={'{0:N1}' -f $_.TotalDays}} -AutoSize | Out-String | Write-Output
    Write-Output 'Scenarios, not ETA: root costs vary greatly; shared pruning can change the frontier. Counts are not percent of work completed.'
}
