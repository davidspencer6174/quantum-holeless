# Subtree and route records

The build at `target/logging-safe/release/quantum.exe` enables these records by
default. Existing start/stop/resume commands work with this executable.
`--no-extra-records` disables the additions for timing comparisons; normal
global longest-level records and checkpoints remain enabled.

For checkpoint searches the output directory is
`<checkpoint-directory>/search-records`. For parallel searches without
checkpoints it is `<run-log-directory>/search-records`. A serial search without
checkpoints prints a fresh `search-records-*` directory under its record
directory, and treats the whole search as root 0.

## Files

Play the saved concrete level for each root in a browser:

```powershell
.\scripts\play-root-levels.ps1 -Records runs\5x7-exit-3-4
```

The root list refreshes as new snapshots are saved. Arrow keys or WASD move,
U/Z undo, and R restarts. See the [player instructions](scripts/level-player/README.md)
for other record directories and launch options.

| File | Contents |
| --- | --- |
| `subtrees/root-00000001.txt`, etc. | Best confirmed publishable level found while processing that split-depth root, its forward solution, and root counters. |
| `subtree_summary.tsv` | Root IDs/prefixes, status, best steps, visits, deduplication hits, main BFS calls/time, and elapsed time. |
| `most-pushes.txt` | Most pushes on a recorded shortest-in-steps route. |
| `most-push-edges.txt` | Most distinct directed box-movement edges on such a route. |
| `most-box-directions.txt` | Most distinct (box identity, push direction) pairs on such a route. |

A push edge is a box's source square plus direction. Repeating the same
edge counts again for pushes, but once for push edges. Box/direction pairs
ignore where the push happened. For example, moving one box right repeatedly
uses multiple edges but one box/direction pair. Each record also reports how
many different boxes were pushed.

Each level record includes a move string and an `Input:` section in the
existing solver's input format. When `witness=concrete`, the QB assignment has
already been selected, the exit starts uncovered, and the known route has
been replayed with the actual box masks. The solver's existing proof certifies
that this concrete assignment has no shorter solution. If a witness cannot be
replayed on the supplied route, the step record can instead save the proven
quantum family, labelled `witness=quantum-family`; it contributes no push
records. A zero best-step count means no publishable level was confirmed for
that root.

The counters apply to the current processing attempt. BFS timing covers the
main child/record solves, including adaptive splitting, and excludes auxiliary
covered-exit probes. Best levels persist across attempts. Partial snapshots
save the first improvement and then improvements at most once every 30
seconds. A completed root always saves its final best immediately. Worker
logs show every step-record improvement and a final `[subtree:summary]` row.
The progress monitor's `BestSteps` column shows the active root's latest
reported improvement.

The TSV is an append/update index. After a resume, an unfinished root may get
another row; the last row for an ID wins. Root files contain the latest saved
snapshot. To rank current root results in PowerShell:

```powershell
Import-Csv runs\5x7-exit-3-3\search-records\subtree_summary.tsv -Delimiter "`t" |
    Group-Object id | ForEach-Object { $_.Group[-1] } |
    Sort-Object { [int]$_.best_steps } -Descending |
    Select-Object -First 10 id,prefix,status,best_steps,bfs_seconds,elapsed_seconds
```

## What The Records Establish

Shared backward-state deduplication can send the work needed for a subtree to
another root. The saved level is therefore the best **found while processing
that root**, not a certified maximum over its entire mathematical subtree.
Deduplication-hit counts help identify roots where this distinction matters.

Push records use one clean QB assignment and one verified step-optimal route
per observed publishable solve. They do not enumerate all clean assignments
or all equally short routes. In particular, they do not establish the minimum
number of pushes among shortest routes, nor minimum pushes when walking is
free. Computing those quantities would need additional search bookkeeping.

Likewise, an edge used on one optimal route is not necessarily required by
every optimal solution. These records measure actual witnessed use, rather
than merely counting allowed mask bits or claiming unavoidable push edges.

## Cost And Recovery

The clean assignment is reused from the existing publication check. Route
replay takes time linear in the known solution length; it does not run another
assignment search or solve. A root can require additional record checks on
fast-path boards that previously fell below the global step record. Concrete
covered-exit boards are excluded from those extra checks. Formatting and
snapshot writes occur only on improvements or completion.

Checkpoint journals retain the latest saved root and metric snapshots and
rebuild their readable files on resume, even if a file write was interrupted.
The summary index is regenerated from those snapshots. The new executable
accepts earlier checkpoints from the preceding version, but cannot reconstruct
per-root records for old roots already skipped as completed. Once new snapshot
events have been appended, use this new executable for further resumes; the
older reader does not understand the additional events.

Readable exports may be kept open in a spreadsheet or editor. If that
application denies writes, the search emits one `[output:warning]` per affected
path and continues. Summary rows are retained and refreshed on a later root
completion after the file is released. Checkpoint-backed level exports are
retried on later records/root completions and rebuilt on resume. Their values
are restored from the journal, never from a possibly stale readable file.
Actual checkpoint-journal failures still stop the search rather than claiming
unsaved work was checkpointed.

The journal retains snapshot history on disk while memory retains only the
latest snapshot per output file. Current main global-record files are kept
separately as before. Existing running processes and executables were not
changed or stopped.

## Checks

73 release tests pass in default and all-feature builds. New checks cover
repeated push edges, box identities and directional masks, wall alternatives,
covered exits, illegal/early-ending routes, durable snapshot reconstruction,
and re-solving saved concrete witnesses at their advertised step counts.
Checkpointed stop/kill/resume and worker-count changes remain covered.
Exported mask rows are also checked when every mask is UDLR and for a
single-column board; the input parser now recognizes these forms correctly.
Windows sharing-lock tests cover blocked summaries, blocked root/metric/global
exports, startup while exports remain locked, retry after release, and fatal
journal-write errors.

A complete 4x5 exit (2,3) run with two workers retained record 40 and completed
209 roots. The final logging version took 3.799 seconds versus 3.440 seconds
with extra records disabled; peak working sets were 7.82 and 7.47 MiB. This
small run has many tiny subtrees, making output overhead prominent. The
existing long-running workers were still active, so these are indicative
single-run timings, not a precise prediction for 5x7. Main BFS calls were
19,842 versus 19,610 (about 1.2% more). Example push records reached 12 pushes
and 10 box/direction pairs on a 25-step concrete level, rather than simply
selecting the longest 40-step level.

A 60-second checkpointed 5x5 exit (2,3) pilot with two workers rediscovered
69 and wrote the corresponding root record. Its peak working set was 17.02
MiB. This was a bounded pilot rather than exhaustive 5x5 completion. Output
is under `scratch/benchmarks/records-on-5x5-69-20261018-150922-424`.
