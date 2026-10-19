// main.rs — CLI entry point for the quantum forward solver.
//
// Usage:
//   quantum <input_file>
//
// # Input file format
//
//   exit x,y        ← exit position in 1-indexed INNER coordinates
//                     (1,1) = top-left playfield cell, NOT counting border walls
//   steps N         ← target step count
//   <grid rows>     ← one character per INNER cell; do NOT include border walls
//                     (padding is added automatically)
//
// Example: a 6-wide by 1-tall corridor, player at left, exit at right:
//   exit 6,1
//   steps 5
//   400030
//
// Grid characters:
//   0  Floor
//   1  Wall
//   2  Concrete box
//   3  Exit position (floor; visually marks the exit when uncovered)
//   4  Player
//   A–Z  QuantumBlock box-possible cell (one letter per block)
//   a–z  QuantumBlock wall_pos cell (same block as the uppercase letter;
//          this cell is the "box never reached here" wall possibility)
//
// # Output
//
// Reports whether the board is Unreachable, has a Shortcut, or has an
// exact solution, and in the last case prints all constrained QuantumBoards.

use std::env;
use std::fs;
use std::process;

use quantum::backward::{backward_dfs_with_options, BackwardSearchOptions};
use quantum::bfs::{forward_bfs, BfsResult};
use quantum::io::{format_board, format_board_header, parse_input};
use quantum::types::Dir;

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let usage = format!("Usage: {} <input_file>\n       {} <width> <height> [goal_x goal_y] [--threads N] [--split-depth N] [--log-dir DIR]\n       Search options: [--checkpoint DIR | --resume DIR] [--no-symmetry] [--no-extra-records]\n       Stop after active subtrees: quantum --stop CHECKPOINT_DIR",
        raw_args[0], raw_args[0]);
    if raw_args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{usage}\n\nSearch defaults: --threads 1 --split-depth 10 --log-dir logs; symmetry enabled.\nCheckpoints require an explicit exit. Resume may change worker count.\nCtrl+C also preserves completed subtrees; unfinished subtrees will restart.");
        return;
    }
    if raw_args.get(1).is_some_and(|arg| arg == "--stop") {
        if raw_args.len() != 3 || !std::path::Path::new(&raw_args[2]).join("checkpoint.bin").is_file() {
            eprintln!("Specify an existing checkpoint directory: quantum --stop DIR"); process::exit(1);
        }
        fs::File::create(std::path::Path::new(&raw_args[2]).join("STOP")).unwrap_or_else(|error| {
            eprintln!("Could not request stop: {error}"); process::exit(1);
        });
        println!("Stop requested. Active subtrees will finish and save before the search exits.");
        return;
    }
    let mut args = vec![raw_args[0].clone()];
    let mut options = BackwardSearchOptions::default();
    let mut search_options_used = false;
    let mut remaining = raw_args.into_iter().skip(1);
    while let Some(arg) = remaining.next() {
        match arg.as_str() {
            "--no-symmetry" => { options.symmetry = false; search_options_used = true; }
            "--no-extra-records" => { options.extra_records = false; search_options_used = true; }
            "--threads" | "--split-depth" | "--log-dir" | "--checkpoint" | "--resume" => {
                search_options_used = true;
                let value = remaining.next().unwrap_or_else(|| {
                    eprintln!("Missing value for {arg}.\n{usage}"); process::exit(1);
                });
                if arg == "--checkpoint" || arg == "--resume" {
                    if options.checkpoint_dir.is_some() { eprintln!("Use only one of --checkpoint or --resume."); process::exit(1); }
                    options.checkpoint_dir = Some(value.into());
                    options.resume = arg == "--resume";
                }
                else if arg == "--log-dir" { options.log_dir = value.into(); }
                else {
                    let number = value.parse::<usize>().ok().filter(|&n| n > 0).unwrap_or_else(|| {
                        eprintln!("{arg} requires a positive integer.\n{usage}"); process::exit(1);
                    });
                    if arg == "--threads" { options.threads = number; } else { options.split_depth = number; }
                }
            }
            _ if arg.starts_with("--") => { eprintln!("Unknown option: {arg}\n{usage}"); process::exit(1); }
            _ => args.push(arg),
        }
    }

    if args.len() != 2 && args.len() != 3 && args.len()  != 5 {
        eprintln!("{usage}");
        process::exit(1);
    }
    if options.checkpoint_dir.is_some() && args.len() != 5 {
        eprintln!("Checkpoint searches require dimensions and an explicit exit: width height goal_x goal_y.");
        process::exit(1);
    }

    if args.len() == 3 || args.len() == 5 {
        let inner_width: u16 = args[1].parse().unwrap_or_else(|e| {
            eprintln!("Error parsing inner width '{}': {}", args[1], e);
            process::exit(1);
        });
        let inner_height: u16 = args[2].parse().unwrap_or_else(|e| {
            eprintln!("Error parsing inner height '{}': {}", args[2], e);
            process::exit(1);
        });
        
        if args.len() == 5 {
            let start_time = std::time::Instant::now();
            let goal_x: u16 = args[3].parse().unwrap_or_else(|e| {
                eprintln!("Error parsing goal x '{}': {}", args[3], e);
                process::exit(1);
            });
            let goal_y: u16 = args[4].parse().unwrap_or_else(|e| {
                eprintln!("Error parsing goal y '{}': {}", args[4], e);
                process::exit(1);
            });
            println!("Goal at ({}, {}), starting backward DFS...", goal_x, goal_y);
            backward_dfs_with_options(goal_x, goal_y, inner_width, inner_height, &options).unwrap_or_else(|error| {
                eprintln!("Search failed: {error}"); process::exit(1);
            });
            let elapsed_time = start_time.elapsed();
            println!("Total time taken: {:?}", elapsed_time);
            process::exit(0);
        }
        let start_time = std::time::Instant::now();
        for goal_x in 1..=inner_width {
            for goal_y in 1..=inner_height {
                if (goal_x - 1) * 2 >= inner_width || (goal_y - 1) * 2 >= inner_height {
                    continue;
                }
                if inner_width == inner_height && goal_x > goal_y {
                    continue;
                }

                println!("Goal at ({}, {}), starting backward DFS...", goal_x, goal_y);
                backward_dfs_with_options(goal_x, goal_y, inner_width, inner_height, &options).unwrap_or_else(|error| {
                    eprintln!("Search failed: {error}"); process::exit(1);
                });
            }
        }

        let elapsed_time = start_time.elapsed();
        println!("Total time taken: {:?}", elapsed_time);
        process::exit(0);
    }

    if search_options_used {
        eprintln!("Search options apply to backward searches with board dimensions.\n{usage}");
        process::exit(1);
    }
    let path = &args[1];
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading '{}': {}", path, e);
        process::exit(1);
    });

    let parsed = parse_input(&content).unwrap_or_else(|e| {
        eprintln!("Parse error: {}", e);
        process::exit(1);
    });

    println!(
        "Board loaded: {}×{} inner grid (border walls added automatically)",
        parsed.board.width - 2,
        parsed.board.height - 2
    );
    println!(
        "Exit: inner ({},{})",
        parsed.board.coords(parsed.exit).0,
        parsed.board.coords(parsed.exit).1
    );
    println!("Target steps: {}", parsed.target_steps);
    println!();
    println!("Input board:");
    print!("{}", format_board(&parsed.board, parsed.exit));
    println!();

    let start_time = std::time::Instant::now();
    //&[Dir::Down, Dir::Right, Dir::Down, Dir::Down, Dir::Down, Dir::Left, Dir::Left, Dir::Up, Dir::Left,
    //    Dir::Up, Dir::Left, Dir::Up, Dir::Down, Dir::Down, Dir::Right, Dir::Right,
    //    Dir::Down, Dir::Right, Dir::Right, Dir::Up, Dir::Left, Dir::Up, Dir::Left, Dir::Up, Dir::Left],
    match forward_bfs(
        &parsed.board,
        parsed.exit,
        parsed.target_steps,
        &[],
        &[],
        false,
        1,
        None,
    ) {
        BfsResult::Unreachable => {
            // This line is correct
            println!("Result: NO SOLUTION");
            println!(
                "  The player cannot reach the exit within {} steps.",
                parsed.target_steps
            );
            println!("  (The exit is either unreachable or requires more steps.)");
        }

        BfsResult::AllShortcut => {
            println!("Result: SHORTCUT EXISTS");
            println!("  Every possible starting configuration allows the player");
            println!(
                "  to reach the exit in fewer than {} steps.",
                parsed.target_steps
            );
            println!("  The backward DFS should prune this branch.");
            println!();
            //println!("  Shortcut configurations ({}):", shortcut_boards.len());
            //for (i, sboard) in shortcut_boards.iter().enumerate() {
            //    let label = format!("Shortcut {}", i + 1);
            //    print!("{}", format_board_header(sboard, parsed.exit, &label));
            //    print!("{}", format_board(sboard, parsed.exit));
            //}
        }

        BfsResult::SomeClean {
            ruled_out,
            shortcuts: _,
            publishable,
        } => {
            println!("Result: SOLUTION AT EXACT STEP COUNT");
            println!(
                "  The player reaches the exit in exactly {} steps",
                parsed.target_steps
            );
            if !publishable {
                println!("  Intermediate search state only: no clean assignment starts with the exit uncovered.");
            }
            //display ruled out starts
            if !ruled_out.is_empty() {
                println!(
                    "  The following {} starting positions are ruled out:",
                    ruled_out.len()
                );
                for &pos in &ruled_out {
                    let (x, y) = parsed.board.coords(pos.1 as u16);
                    println!("    ({}, {})", x, y);
                }
            } else {
                println!("  No starting positions are ruled out; the board is fully determined.");
            }

            /*println!("  for the following {} constraint(s) on QuantumBlock starting positions:",
                     restriction_boards.len());
            println!();

            for (i, rboard) in restriction_boards.iter().enumerate() {
                let label = format!("Constraint {}", i + 1);
                print!("{}", format_board_header(rboard, parsed.exit, &label));
                print!("{}", format_board(rboard, parsed.exit));

                // Summarise which QuantumBlocks were constrained.
                let original_blocks = &parsed.board.quantum_blocks;
                let constrained_blocks = &rboard.quantum_blocks;
                let mut any_constrained = false;
                for (id, (orig, constrained)) in original_blocks.iter()
                    .zip(constrained_blocks.iter())
                    .enumerate()
                {
                    if orig.is_empty() || constrained.is_empty() { continue; }
                    if orig.num_positions() != constrained.num_positions() {
                        if !any_constrained {
                            println!("  Constrained blocks:");
                            any_constrained = true;
                        }
                        let letter = (b'A' + id as u8) as char;
                        println!("    Block {}: {} positions -> {}",
                                 letter,
                                 orig.num_positions(), constrained.num_positions());
                    }
                }
                if !any_constrained {
                    println!("  (No QuantumBlock constraints — board is fully determined)");
                }
                println!();
            }*/
        }
        BfsResult::Indeterminate { .. } => {
            println!("Result: INDETERMINATE");
            println!("  The quick shortcut check could not prove AllShortcut.");
        }
    }

    let end_time = std::time::Instant::now();
    let duration = end_time.duration_since(start_time);
    println!("Time taken: {:?}", duration);
}
