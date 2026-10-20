# Building and using the search tools

This repository searches for long Pathology levels without holes. The Rust
program can search backward from the exit to generate candidate levels, or
check an individual concrete or quantum level with its forward solver.
Optional tools monitor a running search and let you play saved concrete
levels in a browser.

Run the commands below from the repository root. The main examples use
Windows PowerShell. Linux/macOS command differences are listed at the end.

## Requirements

- Rust and Cargo. Rust 1.85.0 has been used successfully; the project does
  not declare a minimum supported Rust version. Install Rust through
  [rustup](https://rustup.rs/) if it is not already installed.
- A native linker for your Rust toolchain. On Windows with the default MSVC
  toolchain, install the Visual Studio Build Tools with the C++ build tools
  and Windows SDK if the Rust installer requests them.
- Git, to clone the repository.
- Python 3 for the optional browser player; Python 3.12 has been tested.
  It uses only the standard library, so no `pip install` is required.
- PowerShell for the monitoring, extraction, and benchmark scripts.

Node.js and Playwright are only needed for optional browser automation tests,
not for building the solver or playing levels.

## Clone and build

```powershell
git clone https://github.com/davidspencer6174/quantum-holeless.git
cd quantum-holeless
cargo build --release --locked --target-dir target/search --bin quantum
.\target\search\release\quantum.exe --help
```

The first build downloads the Rust dependencies. Once they are cached, add
`--offline` to Cargo commands to prevent network access. Keep `--release` for
searches: debug builds are intended for development and are much slower.

The commands deliberately use a separate build directory, `target/search`.
The directory name is arbitrary; use the corresponding executable path in
later commands. Build a new version in a different directory if a search is
already using the old executable, especially on Windows where a running
executable cannot be overwritten. Updating source files does not upgrade a
running search.

## Start a search

Start with a small board to check your setup:

```powershell
.\target\search\release\quantum.exe 4 4 2 2 --threads 2 --split-depth 10 --checkpoint runs\4x4-exit-2-2
```

This searches a 4-by-4 playfield with exit `(2,2)` and should find the known
22-step record. Dimensions exclude the surrounding wall border. Coordinates
are **1-based**, with `(1,1)` at the top-left of the playfield.

For a longer search, for example a 5-by-7 board with exit `(3,4)`:

```powershell
.\target\search\release\quantum.exe 5 7 3 4 --threads 6 --split-depth 10 --checkpoint runs\5x7-exit-3-4
```

Choose a fresh checkpoint directory for each new search. Worker subtrees can
vary enormously in cost; large searches may take weeks or months. Increase
the worker count according to your CPU and available memory: each worker
has its own forward-solver working memory.

| Option | Meaning |
| --- | --- |
| `--threads N` | Number of subtree workers; default `1`. |
| `--split-depth N` | Depth where the coordinator hands subtrees to workers; default `10`. |
| `--checkpoint DIR` | Start a new checkpointed search. Requires an explicit exit. |
| `--resume DIR` | Resume an existing checkpoint. |
| `--log-dir DIR` | Parent directory for invocation-specific logs; default `logs`. |
| `--no-symmetry` | Disable symmetry pruning; enabled by default. |
| `--no-extra-records` | Disable per-root and push-metric records. Global step records and checkpoints remain enabled. |

Workers traverse their assigned subtrees depth-first in U,D,L,R order.
`--split-depth` controls the size of work and recovery units. A deeper split
can improve load balancing and reduce lost work on interruption, but gives
the coordinator more work and produces more checkpoint entries.

Omitting the exit searches symmetry-representative exits sequentially:

```powershell
.\target\search\release\quantum.exe 4 4 --threads 2
```

Checkpointing currently requires a single explicit exit. Prefer separate
checkpointed invocations for long searches.

## Stop and resume

To stop gracefully, run this in a **second terminal**:

```powershell
.\target\search\release\quantum.exe --stop runs\5x7-exit-3-4
```

The search stops dispatching new work, finishes active subtrees, saves them,
and exits. This can take hours if an active subtree is large. Wait for the
search process to exit before shutting down the computer.

Ctrl+C stops immediately. Completed subtrees and saved records survive,
but **unfinished subtrees restart from their roots**. Checkpoints do not
save the internal position of an active subtree. The same recovery model
applies after a forced termination or power interruption.

Resume after the old process has exited:

```powershell
.\target\search\release\quantum.exe 5 7 3 4 --threads 6 --split-depth 10 --resume runs\5x7-exit-3-4
```

Keep the dimensions, exit, split depth, symmetry setting, and compatible
search build settings unchanged. The worker count may change. The program
checks checkpoint compatibility and clears a previous stop request on
resume. Each invocation creates a new log directory.

Keep the entire checkpoint directory. A leftover `run.lock` file is normal;
the operating-system lock is released when the process exits. Do not delete
checkpoint files to force a second process to use the same search.

See [checkpoint details](RESUMING_SEARCHES.md) for recovery and compatibility.

## Monitor progress

In another PowerShell terminal, summarize the newest run:

```powershell
.\scripts\search-progress.ps1
```

The report reads logs and exits; rerun it for an updated snapshot. To select
a specific invocation or get machine-readable output:

```powershell
.\scripts\search-progress.ps1 -LogDirectory logs\YOUR_RUN_DIRECTORY
.\scripts\search-progress.ps1 -LogDirectory logs\YOUR_RUN_DIRECTORY -Json
```

`Prefix` identifies a worker's active root. `Prefix25` is the first 25 moves
of its latest reported backward path, or fewer when that path is shorter.
It is blank until the root reports progress. `BestSteps` is the latest
reported best for that root and may be blank until an improvement is logged.

Periodic worker reports occur every ten minutes, with additional shallow
progress messages. Displayed ages and phases refer to logged timestamps,
not a live sample of the process. `forward-solve` denotes a candidate solve;
`exit-check` denotes auxiliary checks for a board whose exit is covered.

### Optional reference frontier

The helper below enumerates candidate split roots without exploring their
full subtrees. Its arguments are width, height, exit x/y, split depth, and
output TSV path:

```powershell
cargo run --release --locked --manifest-path scratch/frontier-estimate/Cargo.toml --target-dir scratch/frontier-estimate/build -- 5 7 3 4 10 scratch/frontier-estimate/frontier-5x7-exit-3-4.tsv
.\scripts\search-progress.ps1 -Frontier scratch/frontier-estimate/frontier-5x7-exit-3-4.tsv
```

Use the same dimensions, exit, split depth, and default symmetry settings
as the actual search. The helper enables symmetry; do not use its frontier
with a `--no-symmetry` search. Regenerate the frontier after changes that
alter the search tree. This is an optional monitoring aid, not an input
required to start a search. Candidate-root counts are **not a percentage of
work completed**; a small number of roots can dominate runtime.

## Find and play saved levels

For a checkpointed search, output includes:

```text
runs/5x7-exit-3-4/
  checkpoint.bin
  run.lock
  longest_levels_5_7.txt
  search-records/
    subtree_summary.tsv
    subtrees/root-00000001.txt
    ...
```

`longest_levels_5_7.txt` contains global step records. Per-root files contain
the best publishable level saved while processing that root, its solution,
and counters. Shared deduplication means a root's saved best is not a
separately certified maximum over its entire mathematical subtree.

Parallel logs live in a fresh directory under `logs`, with `coordinator.log`
and `worker-01.log`, etc. The search prints the log directory at startup.
For non-checkpointed parallel searches, extra records live under that
invocation's log directory. See [record formats](SEARCH_RECORDS.md) for all
output files and push-metric definitions.

To open the browser player:

```powershell
.\scripts\play-root-levels.ps1 -Records runs\5x7-exit-3-4
```

Or launch it directly with Python:

```powershell
python scripts/level_player.py --records runs/5x7-exit-3-4
```

Select a root in the sidebar. Move with arrow keys or WASD, undo with U or
Z, and restart with R. Orange box edges mark the sides the player may push
against. The player uses saved concrete witnesses, validates their supplied
solutions, and skips records that contain only a quantum family.

The level list refreshes every 30 seconds. A newer record does not reset
your current game; select its root again or click "Load new record".
Keep the player terminal open. Ctrl+C in that terminal stops the player
without affecting the search. The server binds to localhost and chooses
the first free port starting at 8765; its address is printed at startup.
Use `--no-browser` for manual browsing or `--port 8800` for a fixed port.

See [player options](scripts/level-player/README.md) for details.

## Check an individual level

```powershell
.\target\search\release\quantum.exe tests\fixtures\5x5-69.txt
```

The forward CLI checks the supplied target step count and reports whether
all assignments have shortcuts, whether a clean assignment exists at that
count, or whether no solution was found within the step budget. For quantum
levels, an assignment chooses one possibility for each QB.

A minimal concrete input file looks like this:

```text
exit 4,1
steps 3
42030

.... ...R .... .... ....
```

This is a 5-by-1 corridor, with the player on the left and one box that may
be pushed right. The exit is at `(4,1)`, so the player pushes the box three
times to reach it. Pushing a box onto the exit during play is allowed.

| Grid character | Meaning |
| --- | --- |
| `0` | Floor. |
| `1` | Wall. |
| `2` | Concrete box. |
| `3` | Exit marker; the `exit x,y` header determines its location. |
| `4` | Player; supply exactly one. |
| `A` through `Z` | Possible box positions for a QB, grouped by letter. |
| `a` through `z` | The corresponding QB's wall alternative. |

Do not include the surrounding wall border. Optional mask rows follow the
grid: one space-separated token per cell and one row per grid row. Tokens
use four fixed positions, `UDLR`, with `.` for forbidden directions. For
example, `U..R` allows up and right; `....` allows no pushes; `####` denotes
a wall. If mask rows are omitted, concrete boxes allow all four directions.

Saved root files include metadata before an `Input:` section. To check one
with the CLI, put only the content after `Input:` into a separate input
file. Display-only `Letters:` and `Push masks:` headings are not input syntax.

## Tests and diagnostics

Run the Rust tests, including examples and process-recovery tests:

```powershell
cargo test --release --locked --all-targets --target-dir target/checks
cargo test --release --locked --all-features --all-targets --target-dir target/checks-all
```

Build diagnostic examples:

```powershell
cargo build --release --locked --examples --target-dir target/checks
.\target\checks\release\examples\forward_bench.exe tests\fixtures\5x5-69.txt 3
```

`forward_bench` repeats a forward check and reports timings. Other examples
inspect concrete assignments or compare QB splits. The default search build
already includes the optimizations enabled for normal runs. Cargo features
are optional experiments or profiling tools; `--all-features` is a test
configuration, not a recommendation for production searches. Performance
reports describe the experiments, including [round 6](PERFORMANCE_ROUND6.md).

Run browser-player record parsing and recovery tests:

```powershell
python -m unittest discover -s scripts -p test_level_player.py
```

## Other platforms and common setup issues

- On Linux/macOS, use `./target/search/release/quantum` in place of
  `.\target\search\release\quantum.exe`. Cargo commands are the same.
  The examples have been exercised on Windows; other platforms have not
  received the same end-to-end validation.
- The direct Python player command works without the PowerShell launcher.
  Use `python3` if that is your system's Python command. The monitor scripts
  require PowerShell; invoke them with `pwsh -File scripts/search-progress.ps1`
  where PowerShell is installed.
- If PowerShell blocks local scripts, review your execution policy. You can
  run a trusted script in a separate process with
  `powershell -ExecutionPolicy Bypass -File scripts/search-progress.ps1`
  without changing your persistent execution policy.
- If a build reports `link.exe` missing, install the Windows MSVC C++ build
  tools and SDK, then reopen the terminal. If Cargo cannot download a
  dependency, resolve the connection issue before using `--offline`.
- If Windows refuses to replace an executable, use a different
  `--target-dir` or wait for the process using that executable to exit.
- Logs, checkpoints, generated frontiers, and most scratch outputs are local
  run data and are ignored by Git. Back up checkpoint directories separately
  if you need to preserve a long-running search across machines or disk loss.
