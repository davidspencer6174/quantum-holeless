# Forward expansion and bounded optional pruning

2026-10-19. This round targets the slow forward solves in the running 5x7
exit (3,3) search. The existing search, its executable, and its checkpoint
were not stopped or modified. Builds and benchmark working directories are
separate from the live run.

## Default changes

- Compute QB slot masks by current position once per expanded node, instead
  of repeatedly scanning all slots for each neighboring interaction.
- Use the existing wall/box occupancy masks to reject impossible concrete
  pushes before copying box state. For QB interactions, borrow unchanged
  concrete-box state until a child actually needs to enter the queue.
- Probe exact queued/settled state costs before assignment-witness work.
  This probe is read-only: a rejected witness must not reserve a state.
  A cheaper arrival can still replace a more expensive queued arrival.
  Reuse the resulting exact key for insertion; do not substitute fingerprints
  for equality. Both the main and relaxed BFS use this path.
- Avoid computing partial forced-slot exclusions at an interrupted adaptive
  solve, since its caller never uses them. Retain shortcut proofs only for
  an identical-board retry; do not transfer conditional proofs into split
  children. Skip unused shortcut-result exports on completed adaptive solves.
- Reuse a completed child solve for the immediate record-candidate check
  only when there are no ruled-out slots and the normalized board is
  unchanged. Worker roots and altered boards still get their own checks.
- Honor an explicit relaxed-core state limit inside the cost bucket. An
  interrupted core returns only the independently established deletion-flood
  lower bound, and does not cache incomplete work as an exact result or
  deadlock. The main solver remains exhaustive.

## Optional experiments

These Cargo features are separate, opt-in experiments, not default pruning:

- `walking-dominance`: keep a bounded index of minimum arrival costs at
  floor tiles for an identical concrete-box and QB-slot configuration.
  Record only expanded nodes. Do not suppress exit arrivals: they must still
  report shortcut/clean assignments. The index holds at most 4,096 layouts.
- `split-shortcut-reuse`: carry bounded route hints into adaptive QB split
  children and between siblings. Every hint must be replayed under the
  receiving board's walls, concrete boxes, push masks, and slots before
  becoming a proof. This differs from the older `shortcut-reuse` feature,
  which selects the nonadaptive backward-solve wrapper.
- `relaxed-budget`: amortize optional core-search work across outer queries.
  Start with 1,024 state expansions, grant four per query, and reward a
  completed pruning proof with 1,024, capped at 16,384. When exhausted, keep
  the cheap flood bound and let the exact main BFS continue. Cache lookups
  and the cheap flood remain available.

## Measurement method

The live ten-worker search adds timing noise. The adaptive solver also uses
a 500 ms split threshold, so timing differences can change its split choices.
Replay results are not a measured speedup for an exhaustive 5x7 search.
Peak working set is for one benchmark process, not all running workers.

The frozen pre-round baseline is `scratch/build-round6-base`. Slow boards
come from `scratch/log-boards-20261018-233027-062/manifest.json`, extracted
from worker 09. Replays use one repetition, the 10,000-state initial budget,
the logged intended path, and adaptive splitting. All raw outputs and
memory measurements are retained under `scratch/benchmarks/round6-*`.

The initial profile of the hardest replay recorded 95 million witness
refreshes and 9.9 million expansions. Expansion time was 133 seconds out
of 203 seconds total, inclusive of nested work. Nine completed subtrees
in the running logs spent 99.3% of worker elapsed time in forward solves.
This is why the default changes concentrate on expansion costs rather
than worker scheduling.

## Slow-board comparisons

Solve times in seconds, with matching result type, publication flag, and
ruled-out slots in every completed replay:

| Build | 53 steps, line 48 | 55 steps, line 78 | 55 steps, line 185 | Hardest peak MiB |
| --- | ---: | ---: | ---: | ---: |
| Pre-round baseline | 0.348 | 9.613 | 190.623 | 113.25 |
| Initial cheaper expansion | 0.320 | 9.647 | 160.982 | 113.12 |
| Initial expansion + walking dominance | 0.472 | 9.866 | 171.741 | 115.61 |
| Final default | 0.281 | 8.904 | 155.961 | 113.14 |
| Final + relaxed budget only | 0.310 | 9.771 | 161.210 | 113.23 |
| Final + split-path reuse only | 0.348 | 9.410 | 148.980 | 113.57 |

The final default took 18.2% less time on the hardest replay than the first
baseline run, with effectively unchanged peak memory. Walking dominance
slowed all three early comparisons and added about 2.5 MiB. The optional
relaxed budget did not help these cases. Split-path reuse improved the
hardest case by 4.5% relative to the final default, but slowed the other two;
that is not enough evidence under this timing noise to enable it generally.
All three experimental features therefore remain off by default.

The initial expansion/walking builds precede the final deferred box copies
and adaptive-result cleanup. Compare the walking row against the initial
expansion row, not against the final default, to isolate that experiment.
There was also an intermediate walking-plus-split build (181.490 seconds
on the hardest case); it is not a pure split-path ablation.

### Final paired repeat

The hardest case was repeated sequentially with the frozen baseline and
the recommended executable, with CPU accounting enabled and no other
task-owned builds/tests running:

| Build | Solve seconds | Process CPU seconds | Peak working set MiB |
| --- | ---: | ---: | ---: |
| Baseline repeat | 170.564 | 165.344 | 113.34 |
| Final default repeat | 146.565 | 140.875 | 113.79 |

The result signatures still match exactly. This pair used 14.1% less solve
wall time and 14.8% less process CPU time. It supports a real reduction in
work for this case, but is still not a whole-search estimate. The earlier
baseline's 190.623 seconds versus this repeat's 170.564 illustrates why the
initial 18.2% figure should not be treated as a precise expected gain.
Peak memory is effectively unchanged; this round is primarily a time
optimization, not a large memory reduction. Outputs are under
`scratch/benchmarks/round6-base-repeat-hardest-20261019-092923-314` and
`scratch/benchmarks/round6-final-repeat-hardest-20261019-093242-260`.

## Complete small searches

Two sequential paired trials, using the time reported by the search:

| Search | Record | Baseline seconds | Final default seconds |
| --- | ---: | ---: | ---: |
| Complete 4x4 | 22 | 0.553, 0.622 | 0.580, 0.585 |
| Complete 4x5 | 40 | 22.309, 23.171 | 21.629, 20.072 |
| 5x5 exit (1,1) | 38 | 3.938, 3.848 | 4.052, 3.556 |

The complete 4x5 trials average 8.3% less wall time, but two trials under
load are not a confidence interval. The other two searches show no clear
timing improvement. Peak working sets stayed around 5-7 MiB. Final board
counts and records match the baseline for every tested exit: 37,507 boards
across the six 4x5 exit representatives, and 7,648 at the 5x5 corner.
The corner record 38 is not the overall 5x5 record 69.

A 60-second final-build pilot at 5x5 exit (2,3) rediscovered record 69,
last reported 23,000 boards, and peaked at 13.86 MiB. This was a bounded
pilot, not a completed exhaustive 5x5 search. The helper stopped only its
own pilot process at the time limit. Output is under
`scratch/benchmarks/round6-final-5x5-69-20261019-092823-213`.

A split-path-only 4x5 trial took 21.249 seconds and 8.18 MiB, compared with
20.850 seconds average and 6.61-6.65 MiB for the default. This also favors
keeping that feature optional pending a broader corpus.

The benchmark helper now records process CPU seconds as well as wall time
and peak working set, to help separate solver work from scheduling noise
when testing alongside a long-running search. Older measurements lack that
field; a missing CPU value is not zero.

## Build and checkpoint compatibility

The default and all-feature release suites each pass 77 tests (58 unit,
2 process-recovery, 4 parallel-search, and 13 solver-regression tests).
Coverage includes independent concrete BFS comparisons, directional masks,
assignment/slot exclusions, both rare 5x7 failures, known record fixtures,
every single-QB split on the regression fixtures, locked summary exports,
parallel ordering, and interrupted/graceful checkpoint recovery.

New focused cases check cost-aware read-only duplicate probes in both
visited-table implementations; exact layout and cost requirements for
walking dominance; terminal exit reporting; and a zero-budget relaxed core
that must neither claim a proof nor contaminate the cache. The known 5x5
69-step fixture is checked independently of the search benchmarks.

```powershell
cargo build --release --offline --target-dir target/forward-fast --bin quantum --examples
cargo test --release --offline --target-dir target/forward-fast
cargo test --release --offline --all-features --target-dir scratch/build-round6-all
```

To replay the hardest logged case with the prepared build:

```powershell
$cases = Get-Content scratch/log-boards-20261018-233027-062/manifest.json -Raw | ConvertFrom-Json
$case = $cases[5]
.\scripts\benchmark.ps1 -Executable target/forward-fast/release/examples/forward_bench.exe -SearchArgs @($case.file,'1','10000',$case.path,'adaptive') -Label round6-replay -TimeoutSeconds 300
```

The checkpoint format, search settings, symmetry rules, and root identities
are unchanged. An old-build 4x4 exit (2,2) checkpoint completed with two
workers; the new executable resumed it with one, skipped all ten saved
roots, and preserved the record file byte-for-byte (record 22).
This test used a scratch checkpoint, never the user's live checkpoint.

After the existing process has exited, resume with:

```powershell
.\target\forward-fast\release\quantum.exe 5 7 3 3 --threads 10 --split-depth 10 --resume runs\5x7-exit-3-3
```

The monitor and frontier file remain usable without regeneration for this
round. The frontier helper was updated for the internal recursion signature
and checked separately. See [resuming instructions](RESUMING_SEARCHES.md)
for stopping: graceful stop finishes active roots, whereas Ctrl+C retains
completed roots but restarts unfinished roots on resume.
