# Starting, stopping, and resuming a search

Run these commands from the project root. The executable has been built in
`target/forward-fast/release/quantum.exe`, separately from previous executables.
This build also saves [subtree and route records](SEARCH_RECORDS.md).
It includes the [forward-solve optimizations](PERFORMANCE_ROUND6.md) and can
resume checkpoints from `target/logging-safe` without changing settings.
Wait for the old process to exit before resuming; it is not upgraded in place.

Start a fresh six-worker search for the 5x7 exit at (3,3):

```powershell
.\target\forward-fast\release\quantum.exe 5 7 3 3 --threads 6 --split-depth 10 --checkpoint runs\5x7-exit-3-3
```

Choose a fresh checkpoint directory. The directory holds `checkpoint.bin`,
`run.lock`, and `longest_levels_5_7.txt`. Logs still go to a fresh run directory
under `logs` (or the supplied `--log-dir`). Keep the checkpoint directory to
retain the work. The lock file may remain after shutdown; the OS lock is
released when the process exits, so do not delete the file to resume.

To stop without discarding progress inside active worker subtrees, run this
in a second terminal:

```powershell
.\target\forward-fast\release\quantum.exe --stop runs\5x7-exit-3-3
```

This stops accepting new subtrees, finishes each currently active subtree,
and saves its completion before exiting. It may take a long time if an
active subtree is large. Wait for the search process to exit before shutdown.

For an immediate stop, Ctrl+C is also supported by the checkpoint design.
Completed subtrees and saved records survive; unfinished subtrees restart
from their roots. The same recovery applies after a forced process exit or
power interruption. Checkpoints are saved at subtree boundaries, not every
few minutes inside a subtree. A deeper split depth gives smaller recovery
units, at the cost of more work in the shallow producer and more checkpoints.

Resume with the same dimensions, exit, split depth, and symmetry setting:

```powershell
.\target\forward-fast\release\quantum.exe 5 7 3 3 --threads 6 --split-depth 10 --resume runs\5x7-exit-3-3
```

The worker count can change. Each invocation writes a new log directory.
Elapsed times, board counts, and throughput describe the current invocation.
The old STOP request is cleared when resuming. Checkpoints currently require
an explicit exit; the CLI rejects checkpoint options for an all-exits search.
Old runs made without `--checkpoint` cannot be resumed through this format.

## Monitoring

Monitor the newest run under the project's `logs` directory:

```powershell
.\scripts\search-progress.ps1
```

Use `-LogDirectory PATH` to select a particular invocation, especially when
another search is still running. The report separates newly finished subtrees
from subtrees saved in the checkpoint. Subtree IDs remain stable across
resumes of the same search settings.

The previous frontier TSV predates symmetry pruning and must be regenerated
before using `-Frontier` with the new search. The helper has been updated for
the default checkpointed 5x7 exit (3,3), split depth 10:

```powershell
cargo run --release --offline --manifest-path scratch/frontier-estimate/Cargo.toml --target-dir scratch/frontier-estimate/build
.\scripts\search-progress.ps1 -Frontier scratch/frontier-estimate/frontier.tsv
```

For another exit, pass width, height, exit x/y, split depth, and output path.
For example, a fresh 5x7 exit (3,4) search with ten workers:

```powershell
cargo build --release --offline --target-dir target/forward-fast --bin quantum
cargo run --release --offline --manifest-path scratch/frontier-estimate/Cargo.toml --target-dir scratch/frontier-estimate/build -- 5 7 3 4 10 scratch/frontier-estimate/frontier-5x7-exit-3-4.tsv
.\target\forward-fast\release\quantum.exe 5 7 3 4 --threads 10 --split-depth 10 --checkpoint runs\5x7-exit-3-4
```

Monitor from a second terminal:

```powershell
.\scripts\search-progress.ps1 -Frontier scratch/frontier-estimate/frontier-5x7-exit-3-4.tsv
```

The `Prefix` column identifies the active root. `Prefix25` shows the first
25 moves (or fewer for shorter paths) of the latest reported path within
that root. It stays blank until that root reports progress. Workers traverse
their roots depth-first in U,D,L,R order; backtracking can show shorter paths,
and the work under different prefixes varies greatly.

The frontier remains a set of candidate roots rather than a measure of work
remaining. A few unfinished subtrees can dominate the runtime.

## Symmetry

Symmetry pruning is enabled by default, including without checkpoints.
Only rectangular-grid isometries that fix this search's exit are used:
horizontal/vertical reflections when the exit is on their axes, half-turns
when both apply, and square-board diagonal reflections and quarter-turns
where applicable. The identity is omitted from the reported symmetry count.

For every permitted transformation, compare chronological backward directions
in the existing U,D,L,R order. Keep the smaller direction sequence. Once two
prefixes differ, adding more moves cannot reverse that comparison, so the
whole losing subtree is discarded before its forward solve. For example, on
a center column the first horizontal move must be L. On a center row the
first vertical move must be U. Diagonal symmetries similarly compare U with L
and D with R. QBs and directional push masks transform along with the board;
no assignment is identified solely by its positions.

`--no-symmetry` disables this check for comparisons. It must be supplied
consistently when creating and resuming a checkpoint.

## Checkpoint Details

The checkpoint journal is append-only, checksummed, and flushed to disk after
every record and completed subtree. An incomplete final write is discarded
on recovery. Corruption of a complete frame is reported as an error. Saved
record text is part of the journal, and the readable record file is rebuilt
from it on resume. Two processes cannot own the same checkpoint concurrently.

Completion identities contain the exact root board, concrete and QB push
masks, QB current/start relations and wall alternatives, forward solution
path, and required-wall restrictions. They are not subtree numbers or hash
fingerprints. Dimensions, exit, split depth, symmetry, format/search version,
and shortcut-reuse setting are checked in the journal header.

The shallow producer has a separate seen table and performs its record check
independently of workers' changing best record. This makes its frontier
replayable across worker counts and schedules. On resume it regenerates that
frontier and skips saved roots. Worker seen tables and forward-solver caches
start fresh. Incomplete visits from a killed worker must not cause its replay
to prune unexplored descendants. Overlapping states can therefore be revisited
through unfinished roots after a restart. This trades some repeated work for
small checkpoints and avoids serializing in-flight forward solves.

Use compatible executables and the same search settings when resuming.
The `logging-safe` to `forward-fast` upgrade has been tested. Future changes
to search semantics must update the search-version header before reusing this
format.

## Validation

- The initial checkpoint implementation passed 68 release tests in default
  and all-feature configurations. Current validation is documented in
  [the latest performance report](PERFORMANCE_ROUND6.md).
- Recovery tests kill a running 4x5 exit (2,3) search after saved subtrees,
  append an incomplete final checkpoint frame, and resume with a different
  worker count. The resumed search reaches the 40-step record.
- A separate graceful-stop test finishes active work and resumes to 40.
- A fully completed checkpoint resumes without new worker subtrees or
  duplicate record entries; subtree ID/prefix pairs stay unchanged.
- Locking, settings mismatches, corrupted frames, exact root identities, and
  record-file reconstruction are tested directly.
- Every exit of 3x3, 3x4, and 4x4 searches preserves its record with symmetry
  enabled or disabled. All 4,096 six-direction strings have exactly one
  surviving representative in each center-square symmetry orbit, and a
  rejected prefix remains rejected under every one-move extension.
- Complete 4x5 searches retain record 40 and visit 37,507 boards with symmetry
  versus 41,420 without it (9.4% fewer). Timings are not a clean comparison:
  builds/tests and the user's existing workers competed for CPU time.
- A 60-second 5x5 exit (2,3) pilot with two workers, symmetry, and checkpoints
  rediscovered record 69. This was a bounded pilot, not exhaustive completion;
  its peak working set was 17.09 MiB. Output is under
  `scratch/benchmarks/resumable-checkpoint-5x5-20261018-114702-234`.

The earlier running search and its executable were not modified or stopped.
