param(
    [string]$Records,
    [int]$Port = 0,
    [switch]$NoBrowser
)
$ErrorActionPreference = 'Stop'
$bundledPython = Join-Path $env:USERPROFILE '.cache\codex-runtimes\codex-primary-runtime\dependencies\python\python.exe'
if (Test-Path -LiteralPath $bundledPython) { $python = $bundledPython }
else {
    $command = Get-Command python -ErrorAction SilentlyContinue
    if (-not $command) { throw 'Python 3 is required. Install Python or use the bundled Codex runtime.' }
    $python = $command.Source
}
$playerArgs = @((Join-Path $PSScriptRoot 'level_player.py'), '--port', "$Port")
if ($Records) { $playerArgs += @('--records', $Records) }
if ($NoBrowser) { $playerArgs += '--no-browser' }
& $python @playerArgs
