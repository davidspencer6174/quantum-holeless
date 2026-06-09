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

use rayon::ThreadPoolBuilder;
use quantum::backward::backward_dfs;
use quantum::bfs::{forward_bfs, BfsResult};
use quantum::io::{format_board, format_board_header, parse_input};

fn main() {
    let args: Vec<String> = env::args().collect();
    ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .expect("Failed to build Rayon thread pool");

    if args.len() != 2 && args.len() != 3 && args.len()  != 5 {
        eprintln!(
            "Usage: {} <input_file> or {} <inner_width> <inner_height> or {} <inner_width> <inner_height> <goal_x> <goal_y>",
            args[0], args[0], args[0]
        );
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
            backward_dfs(goal_x, goal_y, inner_width, inner_height);
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
                backward_dfs(goal_x, goal_y, inner_width, inner_height);
            }
        }

        let elapsed_time = start_time.elapsed();
        println!("Total time taken: {:?}", elapsed_time);
        process::exit(0);
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
        } => {
            println!("Result: SOLUTION AT EXACT STEP COUNT");
            println!(
                "  The player reaches the exit in exactly {} steps",
                parsed.target_steps
            );
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
