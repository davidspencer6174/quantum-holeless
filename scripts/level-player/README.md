# Root level player

From the project directory:

```powershell
.\scripts\play-root-levels.ps1 -Records runs\5x7-exit-3-4
```

The launcher opens a browser tab and runs a local, read-only server. Leave
the terminal open; Ctrl+C stops the player without affecting the search.
No build or package installation is needed on this computer. The launcher
uses the bundled Python runtime, falling back to `python` on other machines.

`-Records` accepts a checkpoint directory or a `search-records` directory.
Omitting it selects the most recently updated records under `runs` or `logs`.
The default port is the first free one starting at 8765. `-Port 8800` selects
a fixed port, and `-NoBrowser` suppresses opening a tab.

Choose a root in the sidebar. Move with arrow keys or WASD, undo with U or Z,
and restart with R. On-screen direction, undo, and restart buttons also work.
Orange block edges indicate the sides the player can push against. Masks
move with their boxes; boxes can cover the exit during play. Reaching the
exit completes the level, and undo remains available afterward.

Only saved concrete witnesses with an uncovered starting exit are included.
Every record's supplied solution is replayed to validate its board and masks
before it is exposed to the browser. Quantum-family-only records are skipped.
The list refreshes every 30 seconds. An improved record appears in the list;
the current game stays intact until its root or "Load new record" is clicked.
Snapshots reflect saved exports, which can lag search improvements by around
30 seconds. A partially rewritten export retains its last valid snapshot.

The server binds only to 127.0.0.1 and reads exports, never checkpoint journals.
All assets, including Lucide icons (see lucide-LICENSE), are served locally.

Parser and live-export recovery checks:

```powershell
python -m unittest discover -s scripts -p test_level_player.py
```

With the player running and Playwright plus Edge available, browser checks
replay every current record and exercise controls at desktop/mobile sizes:

```powershell
node scripts/test-level-player.cjs
```

Set `LEVEL_PLAYER_URL` to test a different port. Screenshots are saved under
`scratch/level-player-check`.
