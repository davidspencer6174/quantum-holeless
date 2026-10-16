# Performance and correctness audit

Update: see `PERFORMANCE_ROUND2.md` for the 2026-10-16 optimization pass and the
resolved record-publication rule. The 26-step 4x4 candidate below is a valid
intermediate search state, but not a publishable level because its only clean
assignment initially covers the exit. Current record selection excludes it.

Measured locally on Windows, 2026-10-15. These are before/after measurements,
not an exhaustive validation of the 6x6 search.

## Measurements

Both executables were built with the existing Cargo release profile. Searches
ran sequentially with the existing single-worker setting. Times below are the
program's own elapsed times, excluding process startup. Each table entry is one
run, so small differences should not be interpreted as statistically significant.

| Workload | Before | After | Before peak working set | After peak working set |
| --- | ---: | ---: | ---: | ---: |
| 4x4, all symmetry-unique exits | 0.239 s | 0.229 s | 5.62 MiB | 4.97 MiB |
| 4x5, all symmetry-unique exits | 8.215 s | 7.040 s | 6.87 MiB | 6.53 MiB |
| 5x5, exit (1,1) | 3.040 s | 2.559 s | 6.71 MiB | 6.21 MiB |
| 5x5, exit (1,2) | 40.424 s | 32.629 s | 12.12 MiB | 10.53 MiB |
| Existing `quantum_debug.txt`, target 217, forward only | 0.668 s | 0.124 s | 17.29 MiB | 5.36 MiB |

The 4x5 search used about 14% less time; the 5x5 edge-exit search used about 19%
less. The 217-step forward fixture was about 5.4 times faster and used about 69%
less peak working set. These improvements must not be extrapolated directly to
an exhaustive 6x6 run. The file-based CLI enables verbose output in both versions;
that timing includes logging, not just solver computation.

The benchmark helper samples process metrics every 50 ms. It reads Windows'
peak-working-set counter, but may miss activity after its final live sample.
The separate private-memory statistic is a sampled maximum, not an exact peak.
For the 217-step fixture it fell from 16.91 to 1.61 MiB. Very short runs are poor
memory benchmarks; the longer search runs are more useful comparisons.

Original and optimized record files are byte-for-byte identical for the complete
4x4 and 4x5 runs and the 5x5 exit-(1,2) run. The 217-step fixture produces the same
result and rules out the same four starting positions.

A full baseline 5x5 search was stopped at the benchmark's 180-second limit. No
complete 5x5, 5x6, or 6x6 enumeration is claimed. The 69-step 5x5 fixture and an
available 83-step 6x5 fixture are covered by regression tests; testing those
individual boards is not equivalent to enumerating their board sizes.

## Implemented changes

- Release each processed BFS bucket's backing allocation. Previously, empty
  buckets retained their maximum capacities until the whole search ended.
- Borrow shortcut constraints while expanding children instead of cloning the
  entire collection for every node. Retain the snapshot where a mutable callback
  genuinely requires one.
- Avoid computing child fingerprints for the main solver's no-op parent tracker.
- Use the already-depended-on `FxHasher` for forward state fingerprints.
- Replace temporary heap-backed canonical-ID maps with a fixed ID table and
  small inline vectors in backward fingerprinting.
- Stop the deletion flood fill when it reaches a fixed point, even if the exit
  has already been reached. Previously it kept iterating to the step budget.
- Combine relaxed visited flags, best arrival costs, and parents in one table.
- Hash the relaxed state's existing slot representation directly, eliminating
  construction and sorting of a temporary QB-position map for every lookup.

## Pruning repairs

Several defects affected the usefulness or soundness of the relaxed cache:

- Merging lower bounds kept the smaller value; it now keeps the stronger, larger
  lower bound.
- Exhausting an inclusive budget of N returned `AtLeast(N)`, which did not satisfy
  the main solver's `bound > remaining` pruning condition. It now returns N+1.
- Parent recording compared a child's cost against its parent's cost and rejected
  normal positive-cost edges. It also used a different fingerprint representation
  from relaxed-node processing. Parent tracking now consistently follows the
  cheapest discovered arrival, respecting the weighted push/walk transitions.
- Position-wise OR-ed masks are appropriate for optimistic occupancy/move checks,
  but not sufficient to identify a cached state in this slot-based solver. A
  choice between a left-only and a right-only slot differs from a single slot
  allowing both directions. The key now preserves slot identity, including the
  distinction between deleted and invalid slots. Slots, original static geometry,
  exit, and dimensions remain fixed within the cache's owning forward solve.
- Deletion no longer mutates the original concrete-box table used implicitly by
  the key. Deleted concrete boxes are explicit original-position-to-FREE_SLOT
  entries in the sorted moved-box list.
- Deleted QBs no longer collide with one another at FREE_SLOT or contribute
  occupied/pushable sentinel cells. Invalid slots are checked before bit shifts.
- Restoring choices during cache imputation can prove a lower bound, but its
  exact solution need not be feasible for the restricted state. Such hits are
  now downgraded to lower bounds.
- A cached suffix beyond the current budget does not establish an exact optimum
  outside the part of the search being explored.

The existing core-search threshold is unchanged: it requires deleting at least
four blocks/QB IDs. Therefore many calls still intentionally use only the first
flood-fill phase. A dedicated test forces the core search to run and verifies
that it rejects an exit-blocking push and reuses the resulting cached bound.

## Validation and a benchmark discrepancy

The test suite includes an independent single-step concrete BFS. Unlike the
optimized solver it stores full states with equality checks and shares no move,
pruning, fingerprint, or frontier code with the production search. It checks
1,024 small layouts/mask combinations, plus the concrete interpretations of a
multi-box 4x4 record. Additional tests cover cache bounds, parent replacement,
QB identity, deletion sentinels, and known quantum-board fixtures.

Both the original and optimized searches record a 26-step 4x4 board, rather than
the suggested 22-step reference. The independent solver confirms 26 for the
concrete board in `tests/fixtures/4x4-26-directional.txt` with its directional
push masks. If all its boxes are given unrestricted push masks, its distance is
2. The other concrete interpretation of the recorded QB has distance 2 as well.
This does not prove a new unrestricted record or establish global optimality;
the intended benchmark rules need clarification before comparing record lengths.

## Priorities before exhaustive 6x6

1. **Audit backward-state identity and dominance first.** `board_fingerprint`
   hashes tiles, QB cell/start relationships, and wall positions, but not concrete
   or QB push masks. Distinct push rules can therefore receive the same key by
   construction, independently of accidental hash collisions. Search context
   such as `required_walls` is also omitted although it restricts successor
   generation. Either prove the existing depth-based dominance rule covers these
   differences or retain the relevant information. This audit deliberately does
   not change that rule as an incidental performance optimization.
2. **Resolve hash collisions for a rigorous enumeration claim.** Forward and
   backward visited sets currently treat a 64-bit fingerprint as complete state
   identity. Changing the hash function does not remove that pre-existing risk.
   Collision-resolved compact canonical states are preferable for a proof-grade
   search; a wider hash alone reduces risk but is not a proof.
3. **Measure and budget the live frontier and long-lived tables.** The backward
   `seen` table grows for an entire exit search. Forward limits count processed
   states and are checked only after a depth bucket, so they are not hard memory
   caps. Add counters for pending nodes, assignment-pool storage, relaxed-cache
   size, and table capacities before choosing compression or budget policies.
   A cap on a disposable relaxed cache can be conservative; dropping entries
   from the backward traversal needs a separate revisit/termination analysis.
4. **Deduplicate queued states by best arrival cost.** The main search currently
   deduplicates on removal. An enqueue-time best-distance table could reduce
   duplicate pending nodes, but must allow a later cheaper arrival because one
   transition can include several walking steps. Measure its extra table storage
   against the saved frontier storage.
5. **Parallelize independent work with checkpoints.** The executable explicitly
   sets Rayon to one worker, while retaining scoped-task and shared-lock
   machinery. Independent exit-position jobs are a natural first partition,
   with separate output directories and restartable checkpoints. More workers
   can multiply the live memory footprint; simply increasing the thread count
   is not a memory optimization. A direct single-thread DFS path is also worth
   benchmarking before changing the search architecture.

## Reproduction

From the repository directory in PowerShell:

```powershell
cargo build --release
cargo test
cargo test --release
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 4,5 -Label check-4x5
./scripts/benchmark.ps1 -Executable ./target/release/quantum.exe -SearchArgs 5,5,1,2 -Label check-5x5-edge -TimeoutSeconds 180
```

The helper launches a hidden process in its own timestamped directory under
`scratch/benchmarks`, preserving stdout, stderr, JSON metrics, and generated
record files without appending to the user's existing record files. The original
release executable is preserved at `scratch/quantum-baseline-20261015.exe`.
Raw runs referenced by the table have these directory names:

| Workload | Before directory | After directory |
| --- | --- | --- |
| 4x4 | baseline-4x4-20261015-190043-614 | optimized-final-4x4-20261015-191124-491 |
| 4x5 | baseline-4x5-20261015-190050-393 | optimized-fx-4x5-20261015-190524-814 |
| 5x5 (1,1) | baseline-5x5-corner-20261015-190609-869 | optimized-5x5-corner-20261015-190658-422 |
| 5x5 (1,2) | baseline-5x5-edge-20261015-190723-546 | optimized-5x5-edge-20261015-191024-787 |
| Forward 217 | baseline-forward217-20261015-190423-647 | optimized-final-forward217-20261015-191123-366 |
