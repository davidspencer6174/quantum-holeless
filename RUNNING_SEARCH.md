# Running a 5x7 search

A 5x7 pilot is useful for measuring search throughput and memory growth. There
is not yet enough data to estimate its completion time. Start with one exit
using the default release build; the experimental pruning features remain
optional. The previously documented backward-state key audit is still needed
before treating a finished large run as a proof of exhaustive enumeration.

The default build now adaptively splits selected QBs after a soft 0.5-second
forward-solve threshold, completing every branch sequentially. No new launch
arguments are needed. This reduces peak memory on the tested slow boards but
is not a hard memory cap; see `PERFORMANCE_ROUND4.md` for measurements and the
tradeoff on boards where splitting takes longer.

## Launch one exit

From the repository folder, these PowerShell commands build the program, create
a separate output folder, and display and save its output:

```powershell
cargo build --release
$runPath = Join-Path (Get-Location) ('runs/5x7-1-1-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $runPath | Out-Null
Push-Location $runPath
try {
    & ../../target/release/quantum.exe 5 7 1 1 2>&1 |
        Tee-Object -FilePath search.log
} finally {
    Pop-Location
}
```

The final two arguments select the exit in 1-based inner coordinates. Change
them to select another exit. The log and `longest_levels_5_7.txt` are written
inside this run's output folder. The default is one search worker; add
`--threads 4` to use four subtree workers.

To run every exit representative sequentially, omit the last two arguments:

```powershell
cargo run --release -- 5 7 2>&1 | Tee-Object -FilePath run-5x7.log
```

This command writes record files in the current directory. For 5x7, the CLI
selects the 12 exit representatives with x=1..3 and y=1..4 using reflection
symmetry. Each exit has its own seen table, best record, and progress timer.

## Use multiple workers

For example, run a single exit with four workers and a split depth of 10:

```powershell
cargo build --release --target-dir target/parallel
./target/parallel/release/quantum.exe 5 7 3 3 --threads 4 --split-depth 10 --log-dir logs
```

Use the executable path relative to your run folder when launching from one.
The separate `target/parallel` directory contains the worker build produced
in this session; Windows refused to replace the existing executable in
`target/release`.
Worker options also work when the exit arguments are omitted. `--threads`
defaults to 1, `--split-depth` to 10, and `--log-dir` to `logs`. Both numeric
options must be positive. `--help` prints the supported syntax. Input-file
forward solves continue to use their existing command without worker options.

Each parallel exit search creates a fresh directory under the log directory
and prints its absolute location. It contains `coordinator.log` and one file
per worker: `worker-01.log`, `worker-02.log`, and so on. All ordinary backward
and forward solver output follows the worker that produced it. Record
announcements and final statistics also appear in the main console output.

The coordinator visits the prefix tree in the usual Up, Down, Left, Right
order and puts subtrees at the selected depth into a bounded FIFO queue.
Each worker processes one whole subtree at a time, sequentially, then takes
the next available subtree. Its log therefore moves forward in that DFS
order, including between subtrees. No nested work stealing changes a worker's
traversal order. Subtree start and finish lines identify the queue ID and the
backward move prefix, for example:

```text
[subtree:start] id=17 depth=10 moves=UURDDLDRRU
[subtree:finished] id=17 moves=UURDDLDRRU
```

The queue holds at most twice the number of workers. It is filled as the
search runs, without first enumerating an entire frontier. The seen table,
record threshold, and timing totals are shared. Record checks, file writes,
and best-record updates are serialized so each record entry stays intact and
record depths increase within an exit search. Thread scheduling can change
which tied level is saved and how many duplicate boards are encountered.

Every worker has its own ten-minute progress reporter, including during a
long forward solve. Worker progress lines start with `[worker:01]` etc.; the
coordinator is worker 00. An idle worker retains its last move string with
`phase=idle`. The coordinator uses `phase=queue-wait` while the bounded queue
is full and `phase=waiting-workers` after it has dispatched the final subtree.
A failed worker cancels queued work, wakes the coordinator, and fails the
search; its log includes a failure message.

A deeper split such as `--split-depth 15` creates smaller subtrees and can
improve load balance, but gives the coordinator more sequential work. A very
large final subtree is owned by one worker until completion. Each worker has
its own forward frontier and relaxed cache, so memory increases with worker
count even though the backward seen table is shared.

## Read the progress log

Summarize the latest parallel run without starting another search:

```powershell
.\scripts\search-progress.ps1
```

Select a run explicitly with `-LogDirectory logs/<run-folder>`. Add
`-Frontier scratch/frontier-estimate/frontier.tsv` to compare against the
separately generated shallow frontier and print conditional runtime scenarios.
The script validates observed subtree IDs and prefixes against that file.
Use only a frontier generated for the same board, exit, split depth, and search
configuration. Matching observed prefixes does not guarantee later agreement.
The frontier command itself only generates candidate roots; it does not read
worker logs. Root counts are not a percentage of work completed. `-Json`
returns machine-readable results. Logs are read without modifying the run.

The program prints a progress line whenever it enters an accepted backward
state at depth below 8, every ten minutes, and when an exit search finishes.
The periodic reporter runs independently, so it also prints during a long
forward solve. Progress lines are explicitly flushed when output is redirected.
The existing reports every 1,000 boards and record announcements remain.

An illustrative line:

```text
[progress:periodic] board=5x7 exit=1,1 elapsed=00:10:00 depth=3 boards=1234 best=69 slowest_bfs_ms=250 phase=forward-solve phase_elapsed_s=12 moves=RUL solution=RDL
```

- `moves` is the sequence of backward player moves from the initial solved
  board to the reported candidate. `solution` is the corresponding forward
  sequence to the exit. They are reversed AND direction-inverted relative to
  one another. At depth zero, both are `-`.
- `boards` counts entries in this exit's backward seen table. It is not a
  count of concrete QB assignments. `best` is this exit's published record.
- `phase` distinguishes backward traversal, a child forward solve, a record
  check, or a covered-exit check. `phase_elapsed_s` shows how long the reported
  phase has been active. The path identifies the most recently entered
  candidate; the periodic report does not require a new candidate to appear.
- `elapsed` is time since this exit started. A finished line gives the final
  table size and record even if fewer than ten minutes elapsed.

The move string is diagnostic information, not a saved search checkpoint.
The live seen table is still in memory. Monitor process memory during the pilot
alongside board growth and individual forward-solve times.

## Validation of worker scheduling

Completed 4x4 searches for exit (2,2) at worker counts 1, 2, and 4 and split
depths 3, 10, and 15 preserve the 22-step record. Completed 4x5 searches preserve 40 in all the
following release runs:

| Workers | Split depth | Seconds | Peak working set MiB |
| --- | ---: | ---: | ---: |
| 1 | sequential | 7.88 | 6.71 |
| 2 | 10 | 4.59 | 8.89 |
| 4 | 10 | 3.16 | 10.63 |
| 4 | 15 | 3.43 | 10.37 |

These are individual complete runs, not repeated-trial medians. Raw output
is under `scratch/benchmarks/workers-*`. The tests cover FIFO order, exactly
once job consumption, worker log isolation, restoration of thread-local log
routing, cancellation after worker failure, and increasing record depths with
intact board entries. Existing concrete-reference solver tests remain enabled.

All 54 tests pass in default and all-feature release builds. A 15-second 5x7
exit (3,3) smoke test exercised all four workers, reached a 27-step record,
and peaked at 35.31 MiB before the benchmark stopped it. This was a startup
and logging check, not a completed 5x7 search. Its files are under
`scratch/benchmarks/workers-smoke-5x7-*`.

## Original logging validation

38 release tests pass, including direction conversion and a timer test that
reports without any search activity and stops when its owner is dropped.
Completed 4x4 and 4x5 searches still find 22 and 40. A short, externally timed
5x7 smoke test checks startup and shallow progress output; it is not a completed
5x7 search. Test output and metrics are under `scratch/benchmarks/progress-*`.
