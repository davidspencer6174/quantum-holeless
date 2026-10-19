# Slow-board optimization and adaptive QB splitting

2026-10-17. Baseline is the collision-safe solver at the start of this round,
not the older executable that produced the first version of `search.log`.
The log changed during the usage-limit pause, so both snapshots were sampled.

## Changes

- Witness interning uses an exact slot bitset instead of scanning the historical
  assignment vector. This is not an unchecked hash fingerprint: each possible
  slot has its own bit, and the map compares the entire bitset. Choice order
  does not matter. This change alone reduced the largest saved replay from
  45.04 to 31.30 seconds, with essentially unchanged peak memory.
- Before enumerating assignments, test whether one shortcut covers the whole
  current domain. A shortcut collision that is already impossible in the
  current state cannot exclude a valid assignment. This also handles invalid
  slots and conservatively handles deleted-QB sentinels.
- Periodically refresh a root witness when new shortcut proofs appear. A proof
  that no root assignment remains uncovered can finish the entire solve.
- The default backward search's `forward_bfs_complete` wrapper now splits a
  suitable QB when a solve exceeds a soft 500 ms budget. Each alternative is
  solved sequentially, and slow children may split again. The interrupted
  frontier is freed before beginning the child solves.

The split score is the average number of allowed push directions beyond the
first, across the QB's positions. Wall alternatives contribute zero but count
as branches. Ties prefer the lower QB ID. Only multi-position, root-normalized
QBs with a positive score are candidates. A long QB with only one allowed
direction per position is not automatically a good split choice.

The deadline is checked every 256 settled states. It is a strategy-switch
threshold, not a time limit on the completed solve or a hard memory cap.
Setup and an individual node expansion can overrun it. If no eligible QB
remains, the ordinary unlimited solve completes the branch. The existing
initial state budget still triggers a retry, never a discarded branch.

Raw `forward_bfs` remains unsplit for controlled comparisons and explicit
state-budget callers. The optional `shortcut-reuse` backward-search feature
uses its existing path-reusing wrapper, without adaptive splitting. Use the
default build for the adaptive results below.

## Correctness of splitting

Fixing a QB preserves its selected wall/box interpretation and exact push
mask, clears its other positions, and leaves all other QB IDs stable. Every
alternative is solved completely. A slot survives if it belongs to at least
one clean assignment in at least one child; publication eligibility is the OR
of clean children's publication flags. Initially covered exits remain allowed
as intermediate states but cannot qualify a level for publication.

Child shortcut proofs are conditional on the fixed choice. They are not
exported as unconditional parent proofs; the optional shortcut list is empty
for a merged split result. Interrupted partial ruled-out sets are not applied.

## Why not split any QB?

All seven single-QB splits were timed on the original 59-step slow board.
These measurements used the witness-interner prototype, without recursively
splitting children; times are the sum of every alternative's complete solve.

| QB | Alternatives | Combined seconds |
| --- | ---: | ---: |
| A | 2 | 20.34 |
| B | 2 | 29.23 |
| C | 5 | 50.03 |
| D | 3 | 5.31 |
| E | 2 | 21.75 |
| F | 3 | 48.23 |
| G | 2 | 27.88 |

The unsplit interner prototype took 31.30 seconds. D, which has multiple
direction-rich alternatives, wins decisively; C and F make execution worse.
The score chooses D on this board. It is a heuristic, not a claim that D-like
QBs are always optimal, and switching can waste work on nearly finished cases.

## Paired replay measurements

Release builds, sequential processes, one solve per row and implementation,
with the logged forward move string and no initial state budget. Times are
solver times; memory is process peak working set as observed by the benchmark
helper. These are not medians of repeated trials or full-search speedups.

| Snapshot and line | Target | Result | Baseline seconds | Adaptive seconds | Peak MiB, baseline / adaptive |
| --- | ---: | --- | ---: | ---: | ---: |
| Original, 2252 | 59 | AllShortcut | 44.85 | 5.77 | 753.25 / 58.39 |
| Original, 1168 | 56 | AllShortcut | 37.94 | 8.19 | 740.34 / 92.99 |
| Original, 1121 | 56 | AllShortcut | 21.79 | 11.27 | 387.85 / 59.90 |
| Updated, 1198 | 51 | AllShortcut | 22.57 | 13.97 | 552.33 / 56.12 |
| Updated, 1165 | 38 | SomeClean | 18.03 | 8.69 | 433.83 / 126.45 |
| Updated, 1117 | 49 | SomeClean | 13.69 | 13.80 | 322.95 / 37.93 |
| Updated, 905 | 41 | SomeClean | 8.56 | 4.70 | 206.34 / 37.76 |

Switching is not free. For example, original line 142 regressed from 0.73 to
1.22 seconds; original line 455 went from 5.08 to 5.98 seconds, while its memory
fell from 149 to 55 MiB. These cases are retained in the comparison, not omitted
from the performance assessment. The largest-case improvement was reproduced:
an earlier run measured 5.77 seconds and 58.29 MiB for the adaptive solve.

Across all 28 paired replays (26 distinct input boards, including two boards
present in both snapshots), summed solve time fell from 274.01 to 147.03
seconds, a 46% reduction. Six replay rows were slower. Every pair returned the
same result category and the same sorted ruled-out slots. The old benchmark
did not print publication flags; those are covered by the regression tests,
not claimed as part of this old-executable comparison.

## Validation

All 47 tests pass in default and all-feature release builds. The normal
`target/release/quantum.exe` and benchmark examples were rebuilt with default
features. Existing compiler warnings remain.

Completed searches preserved their records (single paired runs):

| Search | Record | Baseline seconds | Default seconds |
| --- | ---: | ---: | ---: |
| Complete 4x4 | 22 | 0.315 | 0.335 |
| Complete 4x5 | 40 | 7.878 | 7.637 |
| 5x5 exit (1,1), completed | 38 | 2.751 | 2.740 |

The 5x5 corner record is exit-restricted, not the overall optimum. The known
69-step 5x5 fixture still passes the solver/reference tests. No full 5x5
enumeration was run in this round. Small timing differences in this table
should be treated as noise, not demonstrated speed improvements.

The rebuilt standard executable's benchmark example also completed the two
largest-log replays with the actual 10,000-state initial attempt budget:
original target 59 took 5.89 seconds / 50.88 MiB; updated target 51 took
14.08 seconds / 56.12 MiB. Both returned AllShortcut.

The regression suite includes exact witness keys, exhaustive small-domain
coverage checks, every single-QB split of known 5x5/5x7 fixtures at several
targets, recursive deadline fallback, masks, wall alternatives, initially
covered exits, and clean-slot unions. Another 64 small two-QB families are
checked under both forced individual splits and recursive splitting at seven
target lengths, including wall alternatives and slot-specific push masks.
The existing independent concrete BFS comparisons and both rare 5x7 collision
regressions remain in the suite.

No exhaustive 5x7 run was completed here, and these measurements cannot predict
its completion time. The separate backward-state key/context audit noted in
earlier performance reports remains outside this optimization round.

## Reproduction

Build the normal release executable and both benchmark examples:

```powershell
cargo build --release --bin quantum --examples
cargo test --release
cargo test --release --all-features
$manifest = ./scripts/extract-slow-boards.ps1 -LogPath search.log
$cases = Get-Content -LiteralPath $manifest -Raw | ConvertFrom-Json
$case = $cases[-1]
& ./target/release/examples/forward_bench.exe $case.file 1 0 $case.path adaptive
& ./target/release/examples/forward_bench.exe $case.file 1 0 $case.path
& ./target/release/examples/qb_split_bench.exe $case.file $case.path
```

The last command measures every single-QB split; append a zero-based QB ID to
measure only one. The raw and adaptive benchmarks revalidate the supplied
path. `0` means no initial state budget; `10000` reproduces the default
backward child's initial attempt budget when using adaptive mode.

For process peak-memory measurements, use `scripts/benchmark.ps1` with an
absolute input filename. Keep benchmarks sequential and separate from builds.
The original extracted corpus is retained in `scratch/slowboards/manifest.json`;
the paired runs are in `scratch/benchmarks/round4-paired-*`. The extraction
script saves new snapshots without overwriting earlier inputs or the log.
