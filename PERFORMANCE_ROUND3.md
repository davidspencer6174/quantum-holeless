# Third pass: completion, dominance, and cross-solve hints

2026-10-16. Baseline: the committed second-pass implementation at the start of
this task. The changes were implemented and tested in the requested order.

## Defaults and correctness

The backward search no longer drops a child when its 10,000-state forward
attempt returns `Indeterminate`. It retries without a state limit, seeding the
retry with shortcut constraints proved on that identical board. It does not
apply partial ruled-out slots before the retry. Record-candidate checks remain
unlimited. An unexpected `Indeterminate` at the backward call site is an error,
not a silently discarded branch.

The 10,000-state setting is therefore an initial attempt budget, NOT a memory
cap. Difficult branches may now take more time and memory than before, because
they are actually solved. This is a completeness fix, not a performance claim.

Both additional optimizations are available as independent Cargo features, off
by default because the benchmarks did not establish a consistent speed benefit:

- `indexed-dominance`: use actual settled representatives instead of guessing
  one- or two-slot restorations in the main forward solve. The relaxed solver
  retains its existing imputation check.
- `shortcut-reuse`: carry a small collection of successful movement sequences
  between backward neighbors, replaying every hint under the child's rules.

The existing `forward_bfs` interface and shortcut result format are unchanged.
`forward_bfs_complete` is the complete, non-reusing wrapper;
`forward_bfs_reusing` additionally accepts a mutable `ShortcutPaths` collection.
That API explicitly requests reuse regardless of the backward-search feature.

## Dominance index

Representatives are grouped by exact player position and `BoxState`. A stored
state A dominates B only if its cost is no greater, its valid-slot mask includes
B's, and every slot valid in B has the identical current position in A. Slot
identities and their fixed masks remain unchanged within the owning solve.

This handles three or more restored slots and arbitrary restored positions.
Same-cost supersets replace weaker representatives; a later superset cannot
erase a cheaper subset. The auxiliary index has at most 16,384 representatives
and 64 per bucket. Reaching a cap only loses optional pruning. The ordinary
queued/settled visited table is independent. These caps mean this is not a
complete semantic containment detector, nor a strict superset of every probe
the old imputation heuristic could make after the index fills.

## Shortcut-path reuse

The child prepends its intended forward move to inherited paths. These are only
hints: replay uses the child's player, geometry, concrete push masks, QB masks,
wall interpretations, slot identities, and collision constraints. Only an
actual exit touch strictly below the child's target adds shortcut coverage.
This also handles QB collapse, ID compaction, changed exits, and changed targets
without importing parent-domain assignment proofs.

Keep at most eight distinct hints of at most 1,024 moves, preferring shorter
ones. Paths come from quick shortcut checks and a bounded main-BFS trace arena
of 4,096 nodes. Walking segments are reconstructed from the existing distance
flood; concrete-only solves do not allocate a trace arena. Immutable arena links
prevent a cheaper queued arrival from rewriting a descendant's ancestry.
Traces that cannot be retained are simply unavailable as future hints.

No whole forward frontier or relaxed cache is shared across different boards.

## Benchmarks and records

Release builds, one search worker, separate output directories. Completed-search
times below are medians of three runs, using the program's reported duration
rather than process startup/polling time. Modes were interleaved for these final
comparisons. Memory is the median process peak working-set reading, not Rust
allocation accounting. All three modes found the same record for each row.

| Search | Record | Baseline seconds | Default seconds | Both features seconds | Peak MiB: baseline / default / both |
| --- | ---: | ---: | ---: | ---: | --- |
| Complete 4x4 | 22 | 0.21 | 0.20 | 0.22 | 5.11 / 5.11 / 5.35 |
| Complete 4x5 | 40 | 7.25 | 6.93 | 7.35 | 6.61 / 6.67 / 7.18 |
| 5x5, exit (1,1), completed | 38 | 2.37 | 2.41 | 2.50 | 6.42 / 6.33 / 7.07 |

The 5x5 corner result is an exit-restricted record, NOT the overall 5x5 record.
To check 69, the final default and both-feature builds each searched exit (2,3)
for a 60-second window. Both rediscovered a publishable 69-step level. The
default run last reported 63,000 boards and peaked at 12.06 MiB; both features
last reported 66,000 and peaked at 13.88 MiB. Both were stopped by the benchmark
timeout, not by an internal pruning/state limit. Neither finished that exit's
enumeration. Earlier 180-second baseline and prototype both-feature runs also
found 69 and did not finish; they last reported 139,000 and 144,000 boards.

These board counts are progress indicators, not equivalent fixed-work timing
measurements. Search pruning and system load can affect the comparison. No
complete 5x5 or larger enumeration is claimed.

The first grouped timing batch experienced substantial variability: baseline
4x5 ranged from 9.62 to 11.26 seconds, and both features ranged from 6.99 to 12.60.
It was not used for the table. Even the final interleaved 4x5 samples ranged
6.79-7.41 seconds for baseline, 6.65-7.26 for default, and 6.80-9.34 with both
features. Small differences should not be treated as reliable speedups.

The sequential implementation checks also preserved the records: the
completion-only 4x5 run took 6.62 seconds, and the index-only run took 6.69
seconds, versus 6.78 for the initial baseline run. Those single measurements
are diagnostic only. An early index-only forward benchmark on the slow45
fixture regressed from a five-solve median of 353 ms to 438 ms. More powerful
containment is not automatically a cheaper solve.

**Conclusion:** retain the completion fix by default. The new mechanisms work,
but the measured time/memory tradeoff does not justify enabling them globally.
Use the independent feature switches for larger-board experiments.

The completed-search table comes from directories named
`round3-paired-{baseline,default,experimental}-{4x4,4x5,5x5-corner}-*`.
The final 69-step checks are `round3-final-{default,experimental}-5x5-23-*`.

## Verification

- 36 tests pass with the default release build and with both features enabled
  in debug and release builds.
- A zero-state-budget regression forces the completion fallback and compares
  ruled-out slots and publication eligibility with an unlimited solve.
- Dominance tests cover three restored slots at arbitrary positions, slot
  identity, cost ordering, different players/boxes, replacement, and bucket caps.
- Path tests cover weighted walk/push reconstruction, immutable ancestry,
  bounded hints, actual BFS path capture, and rejection after masks/walls change.
- The independent concrete checks cover 1,024 small concrete cases and all
  assignments of 64 two-QB families. The latter also carry hints across changing
  boards and targets and force budget retries, comparing full result signatures.
- The known 69-step fixture is checked for an initially uncovered exit among
  its clean assignments, not merely solvability as an intermediate state.

The earlier backward-key/context and fingerprint-collision caveats documented
in `PERFORMANCE.md` still apply. This pass is not a proof of exhaustive 6x6
enumeration.

## Reproduction

Build and run in isolated benchmark directories to avoid appending to existing
record files:

```powershell
cargo build --release
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 4,4 -Label default-4x4
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 4,5 -Label default-4x5
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 5,5,1,1 -Label default-5x5-corner

cargo build --release --features indexed-dominance,shortcut-reuse
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 5,5,2,3 -Label experimental-5x5-record -TimeoutSeconds 180

cargo test --release
cargo test --features indexed-dominance,shortcut-reuse
cargo test --release --features indexed-dominance,shortcut-reuse
```

Use either feature alone to isolate its cost. Raw runs and preserved executables
are under `scratch/benchmarks/round3-*` and `scratch/quantum-round3-*.exe`.
The helper now records the best published record, last progress-board count,
and program-reported search duration in addition to process duration and memory.
The final builds/tests used `--target-dir scratch/build-round3` after Windows
intermittently locked generated linker/dependency files; retries succeeded.
