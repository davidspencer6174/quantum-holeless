# False unreachable results on 5x7

The 32-step and 77-step boards in `search.log` are solvable. The failure was
in forward state identification, not backward generation for these boards.

`node_fingerprint` reduced the player, quantum state and concrete boxes to a
64-bit FxHasher value. The forward seen table then used that value as the entire
identity, without comparing the underlying states. Two different states in the
32-step board have exactly the same fingerprint:

| Field | Earlier arrival | Necessary later arrival |
| --- | --- | --- |
| Steps | 6 | 12 |
| Player | (4,4) | (4,4) |
| C's current positions | (4,6), (4,7) | (5,7), (5,7) |
| Concrete displacement | (4,4) to (4,5) | (4,4) to (4,5) |
| Other QB positions | Identical | Identical |

The later arrival has cleared the bottom of column 4 by pushing C right. The
seen table incorrectly discarded it as a more expensive visit to the earlier
state. Disabling relaxed pruning alone does not fix this failure.

The fix uses an exact `StateKey`, containing player position, slot count, each
slot's current position (including invalid/deleted sentinels), and concrete box
displacements. Most small keys fit in 24 inline bytes. Larger keys spill into
allocated storage without truncation. Hash maps still hash keys for lookup,
but compare their full contents to resolve collisions.

The forward seen table, imputation lookups, relaxed cache, relaxed parent links
and optional shortcut path tracker all use these exact keys. Static geometry,
exit and per-slot push masks remain fixed within the owning forward solve.
The backward seen table is separate and still uses its existing fingerprint;
the broader backward-key audit described in `PERFORMANCE.md` remains open.

## Solution

This 32-move solution works for all 48 assignments of the first logged board:

```text
LDLDRUURUULDDDLLLUUURDRRURUULLDD
```

Move 5 pushes C right from (4,7) to (5,7). When C starts at (4,6), move 2 first
pushes it down to (4,7). The originally supplied move string has 31 characters.

## Verification

- Regression reproduces the actual old hash collision and verifies independent
  visited/cache entries for the two states.
- Independent concrete BFS verifies all 48 assignments at distance 32 and all
  20 assignments of the second logged board at distance 77.
- Symbolic solves retain every start slot at those targets, and report
  `AllShortcut` with one additional step allowed.
- A move-by-move test verifies the solution above, including C's right push.
- All 41 tests pass in release mode with default and all optional features.
- Complete 4x4 and 4x5 searches still find records 22 and 40.

A single paired 4x5 timing check measured 7.16 seconds before and 8.47 seconds
after; peak working set was 6.79 and 6.96 MiB respectively. This is a small-board
spot check, not a forecast for 5x7. Exact keys require more space per stored
state than an unchecked 64-bit fingerprint. Benchmark logs are under
`scratch/benchmarks/collision-*`.
