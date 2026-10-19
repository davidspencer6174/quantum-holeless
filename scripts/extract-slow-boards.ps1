param([string]$LogPath = 'search.log')
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$output = Join-Path $root ('scratch/log-boards-' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
New-Item -ItemType Directory -Path $output | Out-Null
$log = Get-Content -LiteralPath $LogPath -Raw
$pattern = 'Warning: forward BFS took ([\d.]+)(ms|s) at depth (\d+)\.\r?\n=== Slow BFS board \(exit (\d+),(\d+)\) ===\r?\nLetters:\r?\n([0-9A-Za-z\r\n]+?)\r?\n\r?\nPush masks:\r?\n([.#UDLR \r\n]+?)\r?\nBFS result: ([^\r\n]+)\r?\nExpected path:\r?\n([UDLR]+)'
$cases = @(foreach ($match in [regex]::Matches($log, $pattern)) {
    $g = $match.Groups
    $line = ($log.Substring(0, $match.Index) -split "`n").Count
    $steps = [int]$g[3].Value
    $label = "steps-$steps-line-$line"
    $file = Join-Path $output "$label.txt"
    $inputText = "exit $($g[4].Value),$($g[5].Value)`nsteps $steps`n$($g[6].Value)`n`n$($g[7].Value)`n"
    Set-Content -LiteralPath $file -Value $inputText -Encoding ascii
    [pscustomobject]@{ label=$label; steps=$steps; line=$line; file=$file; path=$g[9].Value; result=$g[8].Value }
})
if ($cases.Count -eq 0) { throw 'No complete slow-board entries found.' }
$manifest = Join-Path $output 'manifest.json'
ConvertTo-Json -InputObject $cases -Depth 4 | Set-Content -LiteralPath $manifest -Encoding ascii
Write-Output $manifest
