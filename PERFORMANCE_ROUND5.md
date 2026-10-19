# Forward-state reuse and backward frozen exits

2026-10-17. Baseline is the six-worker-capable source at the beginning of
this round. The user's existing six-worker search continued throughout.
All builds and benchmark outputs were isolated from that running executable.

## Changes and correctness

- Backward exit check: remove all walls and QBs, ignore push masks, and let
  the player stand anywhere. Repeatedly delete any concrete box with two
  clear inner cells in a pull direction. If the concrete exit box survives
  this fixed point, prune the subtree before its forward solve. Any first
  real pull of a surviving box would also have been possible in the
  relaxation, a contradiction. New walls/boxes cannot help that first pull;
  masks may grow during backward search, which is why this check ignores
  them. An uncovered exit or a QB with alternatives is not rejected.
- Assignment coverage: check whether a shortcut covers every completion of
  a partial QB choice. The remaining domain includes the chosen slots and
  every slot of each unchosen QB. Proving that entire superset covered is
  sufficient to discard the branch. Forced-slot queries use the same search.
- Forward containment probes reuse one exact state-key buffer while restoring
  candidate slot bytes. They perform the same one/two-slot probes as before,
  with no weaker hash equality or loss of slot identity.
- Returning a concrete box to its origin removes its redundant displacement
  entry. Concrete boxes sharing an identical push mask are canonicalized by
  pairing sorted current positions with sorted origins. This is a symmetry
  of the forward game's transitions and exit condition. Different masks and
  all QB slot identities stay distinct. Deleted concrete boxes in relaxed
  solves participate using the existing FREE_SLOT representation.

The frozen-exit check is only a publication/subtree rule. It is not used to
claim that a covered-exit board is unsolvable in the forward game. The
existing more expensive covered-exit shortcut checks are retained.

## Unused-floor proposal

Making a tile unused by both the player and boxes a wall preserves the
particular solution that avoids it. A tile the player never visits may still
be essential box storage. Even with the stronger definition, this establishes
an equal-length representative, not a strict improvement.
Pruning all backward descendants additionally requires proving that their
representatives survive in another branch under its assignment restrictions.

Forward lexicographic order also differs from the backward enumeration order:
with U,D,L,R, forward UR precedes RU, but their backward paths LD and DL have
the opposite ordering. The current weighted forward BFS does not promise a
lexicographically first solution either. No unused-floor pruning was enabled.
A future version would need a canonical order matching backward paths and
a proof covering QB alternatives and descendants, rather than just one path.

## Validation

All 61 tests pass in default and all-feature release builds, including independent concrete
BFS comparisons, QB split/result comparisons, both rare 5x7 regressions,
and parallel record/log-order tests. New checks cover:

- Every 3x3 box layout under an independent exact pull search with a
  teleporting player, testing all exits declared frozen.
- The odd-coordinate lattice, a removable lattice blocker, cascading
  deletions, walls, masks, and QB deletion in the backward relaxation.
- 2,048 three-QB domains with multiple shortcut constraints, checking every
  concrete assignment and every forced slot, including deleted QBs.
- 512 three-concrete-box boards against the identity-preserving reference
  solver, including equal and unequal directional masks.
- Box round trips, permutation equivalence, and deleted-box canonicalization.

## Measurement notes

Benchmarks run sequentially in isolated working directories. The existing
six-worker search remains a source of timing noise. Adaptive solves retain
the 500 ms split threshold and 10,000-state initial budget, so elapsed-time
variation can also alter the split decisions. These are replay measurements
and small complete searches, not a measured speedup for an exhaustive 5x7 run.

The current log corpus is extracted under
`scratch/log-boards-20261017-201826-313` (worker 03) and
`scratch/log-boards-20261017-202514-635` (worker 06).
Raw paired results are under `scratch/benchmarks/round5-*`.

Seven slow 5x7 forward replays (solve time in seconds, peak working set in MiB):

| Worker / target / log line | Baseline time | Updated time | Baseline MiB | Updated MiB |
| --- | ---: | ---: | ---: | ---: |
| 03 / 53 / 270 | 103.385 | 83.083 | 232.25 | 222.84 |
| 03 / 51 / 201 | 42.269 | 26.558 | 106.91 | 106.78 |
| 03 / 52 / 224 | 31.577 | 28.117 | 62.70 | 61.96 |
| 03 / 53 / 246 | 40.979 | 42.560 | 57.41 | 54.76 |
| 06 / 58 / 26 | 8.986 | 8.108 | 32.01 | 33.86 |
| 06 / 59 / 48 | 16.067 | 14.466 | 25.04 | 24.51 |
| 06 / 59 / 70 | 35.969 | 31.171 | 58.22 | 57.07 |

Total solve time fell from 279.232 to 234.062 seconds, a 16.18% reduction.
Six cases improved; one regressed by 3.9%. All seven result signatures matched,
including publication flags and ruled-out slots. Memory use was broadly
similar; the largest replay decreased from 232 to 223 MiB, while one small
case increased from 32 to 34 MiB. These are single paired replays under load,
not repeated ablation trials or evidence of a 16% whole-search improvement.
Structured results are saved in `scratch/round5-replays.json`.

Complete searches, two paired trials (seconds reported by the search):

| Search | Record | Baseline trials | Updated trials |
| --- | ---: | ---: | ---: |
| Complete 4x4 | 22 | 0.426, 0.609 | 0.504, 0.555 |
| Complete 4x5 | 40 | 12.094, 11.903 | 11.643, 11.454 |
| 5x5 exit (1,1) | 38 | 4.396, 4.260 | 4.214, 4.412 |

The corner's 38 is an exit-restricted record, not the overall 5x5 optimum.
The 4x5 search visits 41,420 boards instead of 41,680 and reports 55
frozen-exit prunes (all at exit (2,2)); both trials take about 4% less time.
The other two small searches show no convincing timing improvement. Peak
working set remains around 5-7 MiB. These are two trials, not a statistical
confidence interval.

A final 120-second pilot using the built `target/optimized` executable at
5x5 exit (2,3) rediscovered the 69-step record and last reported 42,000 boards.
Peak working set was 14.35 MiB. The benchmark helper stopped its own pilot at
the time limit; this was not a completed exhaustive 5x5 search. Output is in
`scratch/benchmarks/round5-final-5x5-69-20261017-214722-916`.

## Build and run

Build separately from the running executable:

```powershell
cargo build --release --offline --target-dir target/optimized --bin quantum --examples
```

For a new run, give it a separate working directory for its record files and
logs as well:

```powershell
New-Item -ItemType Directory -Force runs/optimized | Out-Null
Push-Location runs/optimized
try { ..\..\target\optimized\release\quantum.exe 5 7 3 3 --threads 6 --split-depth 10 }
finally { Pop-Location }
```

Changes cannot be applied to an already running process. The existing search
has no checkpoint/resume mechanism; launching this executable begins a new
search. No existing process was stopped or restarted during this work.
