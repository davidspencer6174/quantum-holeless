# Second optimization pass

2026-10-16. The baseline for this pass is the optimized code from the first
pass, not the original repository version.

## Record eligibility

Pathology allows a box to be pushed onto the exit, but not to cover it in the
starting position of a published level. Covered-exit intermediate states must
remain in the backward search.

`BfsResult::SomeClean` now includes `publishable`. At an exact-length clean exit,
the solver checks whether the same state has a non-shortcut assignment after
excluding all QB start slots at the exit. A concrete box initially on the exit
makes this flag false. This is a joint assignment check, not a test of the
unfiltered board's QB position count or an independent union of per-slot choices.

The flag controls record publication, not backward traversal. The CLI labels
non-publishable successful solves as intermediate states. Existing user record
files have not been rewritten. The complete 4x4 search now records 22, and the
complete 4x5 search records 40. In particular, the old 26-step QB candidate has
an exit-uncovered alternative, but that alternative has a two-step shortcut;
only its exit-covered assignment takes 26 steps.

## Timings

Release builds, quiet forward solves, no supplied solution path, no state limit.
Each entry is the median of five solves in one process. Before/after processes
were run sequentially. The three 6x5 fixtures were transcribed from the existing
slow-solve logs, but these measurements are unlimited re-solves, not reproductions
of the logs' 10,000-state, supplied-path calls.

| Fixture | First pass | Second pass | Time reduction | Peak working set before / after |
| --- | ---: | ---: | ---: | ---: |
| 6x5 slow45 | 457.659 ms | 355.975 ms | 22.2% | 12.62 / 11.18 MiB |
| 6x5 slow49 | 299.164 ms | 232.653 ms | 22.2% | 10.25 / 9.20 MiB |
| 6x5 slow45-edge | 234.441 ms | 182.658 ms | 22.1% | 11.82 / 11.32 MiB |
| Existing 217-step fixture | 98.681 ms | 86.342 ms | 12.5% | 5.38 / 5.56 MiB |

Result classifications and sorted ruled-out slots match the baseline in every
case. The first two cases remain SomeClean, the edge case remains AllShortcut,
and the 217-step fixture retains the same four ruled-out starting positions.

Memory is the process high-water working-set counter observed by the benchmark
helper, not a sum of Rust allocations. The private-memory sampled maxima for
slow45 fell from 12.68 to 8.86 MiB; for slow49, from 7.61 to 6.52 MiB. The 217-step
fixture's private-memory sample increased from 1.48 to 2.09 MiB. Thus this pass
does not improve memory uniformly: best-arrival tracking trades additional
table storage for fewer queued nodes, with the benefit depending on the workload.

No complete 5x5, 5x6, or 6x6 enumeration is claimed for this pass.

## Changes retained

- Precompute wall-respecting exit distances once per forward solve, treating
  boxes and QBs as floor. Apply this admissible lower bound before expensive node
  processing, at walking-frontier cells, and before enqueueing interactions.
- Deduplicate pending states by their best arrival cost. A later cheaper path
  replaces a queued cost; stale arrivals are skipped. Queued states are not
  exposed as settled to imputation pruning. Relaxed parent tracking uses the
  same enqueue rejection rule while preserving cheapest-path parents.
- Keep the carried witness fast path, but search the current QB domains directly
  when it fails instead of scanning the entire historical assignment pool. Reuse
  an existing identical assignment before growing the pool.
- Compute an assignment's slot mask once per shortcut-coverage query rather than
  once for every shortcut.
- Build imputation candidates from unique current positions, shared between the
  normal and relaxed solvers. Limit optional two-slot probing to six candidates
  (at most 15 pairs instead of 66). Omitting an optional dominance check only
  reduces pruning strength; it does not discard additional states.
- Reuse the cheap block-deletion flood inside core BFS expansions. It can prove
  deadlock or a too-large lower bound without invoking another core BFS, and
  stops on the first optimistic exit touch when a full deletion closure is not
  needed.
- The outer deletion flood still computes the closure needed for selecting the
  core. Its fallback Exact value is now the exit-touch distance, not the later
  closure-completion iteration. Exact distances exceeding the remaining budget
  can consequently be used for pruning too.
- Resolve each concrete box's original position once when building push masks,
  rather than once for each direction. QB slots still OR their masks at shared
  current positions.

## Experiments and tradeoffs

The wall-distance change by itself did little for these slow fixtures, though
it is a cheap stronger bound for winding static layouts. Enqueue deduplication
alone lowered peak memory but slightly increased runtime. Witness refresh and
the other hot-path changes made the combined version faster.

Reducing the core-search trigger from four deleted blocks to two increased one
slow45 solve from about 0.4 seconds to 0.91 seconds and increased memory. That
experiment was reverted; the four-block trigger is unchanged.

An opt-in `profile-search` feature reports inclusive, thread-local timings for
imputation, witness refresh, assignment search, relaxed solving, and expansion.
Categories overlap: their times must not be summed. Timers do not exist in
normal builds. Profiling helped identify witness-pool scans and repeated
imputation work; it is not enabled in the performance table above.

## Verification

28 tests pass in debug and release builds. Coverage includes:

- The existing independent concrete solver comparison over 1,024 small cases.
- Every concrete assignment of 64 two-QB board/mask families (256 concrete
  boards), checking classifications, ruled-out slots, and publication eligibility
  at multiple target distances.
- Covered-exit concrete and quantum intermediate states, the 26-step candidate's
  shortcut alternative, and a valid level that pushes a box onto and off the exit.
- Weighted queued arrivals, static-wall detours, relaxed-cache reuse, and the
  three slow quantum fixtures' baseline results.

The first pass's remaining backward-key/dominance and hash-collision concerns
are unchanged; these tests are not a proof of exhaustive 6x6 correctness.

## Reproduction

```powershell
cargo build --release --example forward_bench
./target/release/examples/forward_bench.exe tests/fixtures/6x5-slow45.txt 5 0
cargo run --release --features profile-search --example forward_bench -- tests/fixtures/6x5-slow45.txt 1
cargo test
cargo test --release
```

The benchmark example accepts `FILE [REPEATS] [STATE_LIMIT_OR_0] [PATH]`. PATH is
an optional U/D/L/R string. Rebuild without `profile-search` before measuring
normal performance. The first-pass benchmark executable is preserved as
`scratch/forward-round1.exe`.

Raw measurements are under `scratch/benchmarks`:

| Fixture | Before directory | After directory |
| --- | --- | --- |
| slow45 | final-baseline-slow45-20261016-021434-829 | final-optimized-slow45-20261016-021438-145 |
| slow49 | final-baseline-slow49-20261016-021440-906 | final-optimized-slow49-20261016-021443-423 |
| edge | final-baseline-edge-20261016-021454-650 | final-optimized-edge-20261016-021456-853 |
| 217 | final-baseline-217-20261016-021458-734 | final-optimized-217-20261016-021500-240 |
