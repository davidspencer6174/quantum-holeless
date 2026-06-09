// io.rs — text file parsing and display of QuantumBoards.
//
// # File format
//
// Line 1:   `exit x,y`   — coordinates of the exit cell (1-indexed inner coords,
//                          so (1,1) is the top-left inner cell).
// Line 2:   `steps N`    — target step count.
// Lines 3+: grid rows, one character per cell.
//
// Grid characters:
//   0  → Floor
//   1  → Wall
//   2  → Box  (collapsed, concrete)
//   3  → Exit tile (floor; marks the exit position visually if uncovered)
//   4  → Player
//
// QuantumBlocks are identified by letters A–Z (up to 26 blocks).
// For each letter:
//   uppercase (A) → a cell that is a possible *box* location in that block.
//   lowercase (a) → the cell is the `wall_pos` for that block (box never reached here).
//
// All cells in the same letter group must be collinear (same row or column)
// and contiguous (no gaps), forming exactly one QuantumBlock.
//
// The grid must NOT include the padding border; padding is added automatically.
//
// # Display format (for results)
//
// The same grid characters are used for output, using the same letter
// convention for QuantumBlocks.  The exit position is always shown as '3'
// if it is floor/exit, regardless of whether the QuantumBoard marks it
// specially.

use crate::board::QuantumBoard;
use crate::types::{FlatIdx, QBlockId, Tile};
use std::collections::HashMap;

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Everything read from an input file.
pub struct ParsedInput {
    pub board: QuantumBoard,
    pub exit: FlatIdx,
    pub target_steps: u32,
}

/// Parse a board description from a string.
/// Returns `Err(message)` on any format problem.
pub fn parse_input(input: &str) -> Result<ParsedInput, String> {
    let mut lines = input.lines().peekable();

    // Line 1: exit x,y
    let exit_line = lines.next().ok_or("missing 'exit x,y' line")?;
    let (ex, ey) = parse_exit_line(exit_line)?;

    // Line 2: steps N
    let steps_line = lines.next().ok_or("missing 'steps N' line")?;
    let target_steps = parse_steps_line(steps_line)?;

    // Collect remaining lines, separating grid rows from push mask rows.
    // Grid rows are single-character per cell (no spaces).
    // Push mask rows have space-separated 4-character mask strings.
    let mut grid_rows: Vec<&str> = Vec::new();
    let mut mask_rows: Vec<&str> = Vec::new();
    let mut found_masks = false;

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue; // Skip blank lines
        }
        
        // Detect if this line is a push mask row (contains spaces and #### patterns)
        if trimmed.contains(' ') && (trimmed.contains("####") || trimmed.contains('.')) {
            found_masks = true;
            mask_rows.push(trimmed);
        } else if found_masks {
            return Err(format!(
                "found grid row '{}' after push mask rows have started",
                trimmed
            ));
        } else {
            grid_rows.push(trimmed);
        }
    }

    if grid_rows.is_empty() {
        return Err("no grid rows found".into());
    }
    let inner_height = grid_rows.len() as u16;
    let inner_width = grid_rows[0].len() as u16;

    // Validate all grid rows are the same width.
    for (i, row) in grid_rows.iter().enumerate() {
        if row.len() as u16 != inner_width {
            return Err(format!(
                "row {} has width {} but expected {}",
                i,
                row.len(),
                inner_width
            ));
        }
    }

    // Validate push mask rows if present
    if !mask_rows.is_empty() {
        if mask_rows.len() as u16 != inner_height {
            return Err(format!(
                "expected {} push mask rows but got {}",
                inner_height,
                mask_rows.len()
            ));
        }
    }

    // Validate exit coordinates (1-indexed inner).
    if ex < 1 || ex > inner_width || ey < 1 || ey > inner_height {
        return Err(format!(
            "exit ({},{}) is out of range for a {}×{} inner grid",
            ex, ey, inner_width, inner_height
        ));
    }

    // Build board (all walls initially; padding added by QuantumBoard::new).
    let mut board = QuantumBoard::new(inner_width, inner_height);

    // Collect QuantumBlock cell groups keyed by letter (uppercase = box possible,
    // lowercase = wall_pos).
    // For each letter A..Z we track:
    //   cells: sorted flat indices of uppercase occurrences (box-possible positions)
    //   wall:  flat index of the lowercase occurrence (if any)
    struct QGroup {
        box_cells: Vec<FlatIdx>,
        wall_cell: Option<FlatIdx>,
    }
    let mut qgroups: HashMap<u8, QGroup> = HashMap::new(); // key = b'A'..b'Z'

    for (row_idx, row) in grid_rows.iter().enumerate() {
        let inner_y = (row_idx + 1) as u16; // 1-indexed with padding
        for (col_idx, ch) in row.chars().enumerate() {
            let inner_x = (col_idx + 1) as u16;
            let flat = board.flat(inner_x, inner_y);

            match ch {
                '0' => {
                    board.set_flat(flat, Tile::Floor);
                }
                '1' => { /* already Wall */ }
                '2' => {
                    board.set_flat(flat, Tile::Box);
                    board.set_box_push_mask(flat, crate::types::PUSH_ALL);
                }
                '3' => {
                    board.set_flat(flat, Tile::Floor);
                } // exit is just floor
                '4' => {
                    board.set_flat(flat, Tile::Player);
                }
                'A'..='Z' => {
                    let key = ch as u8;
                    qgroups
                        .entry(key)
                        .or_insert(QGroup {
                            box_cells: vec![],
                            wall_cell: None,
                        })
                        .box_cells
                        .push(flat);
                }
                'a'..='z' => {
                    let key = ch.to_ascii_uppercase() as u8;
                    let group = qgroups.entry(key).or_insert(QGroup {
                        box_cells: vec![],
                        wall_cell: None,
                    });
                    if group.wall_cell.is_some() {
                        return Err(format!("letter '{}' has more than one wall_pos cell", ch));
                    }
                    group.wall_cell = Some(flat);
                    // The wall_pos cell is also part of the block's range,
                    // so we'll include it when computing start/end.
                }
                other => {
                    return Err(format!("unknown grid character '{}'", other));
                }
            }
        }
    }

    // Convert each QGroup into a QuantumBlock on the board.
    // Process in sorted letter order for deterministic QBlockId assignment.
    let mut letters: Vec<u8> = qgroups.keys().cloned().collect();
    letters.sort();

    for letter in letters {
        let group = &qgroups[&letter];
        //if group.box_cells.is_empty() {
        //    return Err(format!(
        //        "QuantumBlock '{}' has no box-possible cells (only a wall_pos)",
        //        letter as char
        //    ));
        //}

        // Collect all cells: box_cells + optional wall_cell.
        let mut all_cells = group.box_cells.clone();
        if let Some(wc) = group.wall_cell {
            all_cells.push(wc);
        }
        all_cells.sort();

        // For multi-cell QBs, validate stride and contiguity.
        /*if all_cells.len() >= 2 {
            let stride = all_cells[1] - all_cells[0];
            if stride != 1 && stride != board.width {
                return Err(format!(
                    "QuantumBlock '{}': cells are not collinear (stride {} is neither 1 nor {})",
                    letter as char, stride, board.width
                ));
            }
            for pair in all_cells.windows(2) {
                if pair[1] - pair[0] != stride {
                    return Err(format!(
                        "QuantumBlock '{}': cells are not contiguous",
                        letter as char
                    ));
                }
            }
        }*/

        let wall_pos = group.wall_cell;
        board.add_quantum_block(all_cells.iter().cloned(), wall_pos);
    }

    // Apply push masks if provided
    if !mask_rows.is_empty() {
        apply_push_masks(&mut board, &grid_rows, &mask_rows, inner_width, inner_height)?;
    }

    // Compute exit flat index (convert from 1-indexed inner to padded coords).
    let exit_flat = board.flat(ex, ey);

    Ok(ParsedInput {
        board,
        exit: exit_flat,
        target_steps,
    })
}

/// Convert a 4-character push mask string to a PushMask.
/// Format: "UDLR" where each position is either the letter (pushable in that direction)
/// or '.' (not pushable). "####" means no directions are pushable (PUSH_NONE).
fn string_to_push_mask(s: &str) -> Result<crate::types::PushMask, String> {
    if s == "####" {
        return Ok(crate::types::PUSH_NONE);
    }
    if s.len() != 4 {
        return Err(format!("invalid push mask string '{}': expected 4 characters", s));
    }
    let chars: Vec<char> = s.chars().collect();
    let mut mask = crate::types::PUSH_NONE;
    
    // Position 0: U (Up)
    if chars[0] == 'U' {
        mask |= crate::types::PUSH_UP;
    } else if chars[0] != '.' {
        return Err(format!("invalid character at position 0 of '{}': expected 'U' or '.'", s));
    }
    
    // Position 1: D (Down)
    if chars[1] == 'D' {
        mask |= crate::types::PUSH_DOWN;
    } else if chars[1] != '.' {
        return Err(format!("invalid character at position 1 of '{}': expected 'D' or '.'", s));
    }
    
    // Position 2: L (Left)
    if chars[2] == 'L' {
        mask |= crate::types::PUSH_LEFT;
    } else if chars[2] != '.' {
        return Err(format!("invalid character at position 2 of '{}': expected 'L' or '.'", s));
    }
    
    // Position 3: R (Right)
    if chars[3] == 'R' {
        mask |= crate::types::PUSH_RIGHT;
    } else if chars[3] != '.' {
        return Err(format!("invalid character at position 3 of '{}': expected 'R' or '.'", s));
    }
    
    Ok(mask)
}

/// Apply push masks from the mask_rows to the board.
/// Each row of masks corresponds to a row of grid cells.
fn apply_push_masks(
    board: &mut QuantumBoard,
    grid_rows: &[&str],
    mask_rows: &[&str],
    inner_width: u16,
    inner_height: u16,
) -> Result<(), String> {
    for (row_idx, mask_row) in mask_rows.iter().enumerate() {
        let inner_y = (row_idx + 1) as u16; // 1-indexed with padding
        
        // Split mask row by spaces to get individual mask strings
        let mask_strings: Vec<&str> = mask_row.split_whitespace().collect();
        
        if mask_strings.len() as u16 != inner_width {
            return Err(format!(
                "mask row {} has {} mask entries but expected {}",
                row_idx,
                mask_strings.len(),
                inner_width
            ));
        }
        
        for (col_idx, mask_str) in mask_strings.iter().enumerate() {
            let inner_x = (col_idx + 1) as u16;
            let flat = board.flat(inner_x, inner_y);
            let mask = string_to_push_mask(mask_str)?;
            
            // Apply the mask based on the tile type at this position
            match board.get_flat(flat) {
                Tile::Box => {
                    board.set_box_push_mask(flat, mask);
                }
                Tile::Quantum(id) => {
                    // For quantum blocks, set the push mask in the quantum block
                    board.quantum_blocks[id as usize].set_push_mask(flat, mask);
                }
                _ => {
                    // For other tiles (Floor, Wall, Player), we generally ignore mask data
                    // but we could warn if they had non-trivial masks
                }
            }
        }
    }
    Ok(())
}

fn parse_exit_line(line: &str) -> Result<(u16, u16), String> {
    let line = line.trim();
    let rest = line
        .strip_prefix("exit ")
        .ok_or_else(|| format!("expected 'exit x,y', got '{}'", line))?;
    let mut parts = rest.split(',');
    let x = parts
        .next()
        .ok_or("missing x in exit")?
        .trim()
        .parse::<u16>()
        .map_err(|_| "exit x is not a number".to_string())?;
    let y = parts
        .next()
        .ok_or("missing y in exit")?
        .trim()
        .parse::<u16>()
        .map_err(|_| "exit y is not a number".to_string())?;
    Ok((x, y))
}

fn parse_steps_line(line: &str) -> Result<u32, String> {
    let line = line.trim();
    let rest = line
        .strip_prefix("steps ")
        .ok_or_else(|| format!("expected 'steps N', got '{}'", line))?;
    rest.trim()
        .parse::<u32>()
        .map_err(|_| format!("'{}' is not a valid step count", rest))
}

// ── Display ───────────────────────────────────────────────────────────────────

/// Format a QuantumBoard for display, given the exit flat index.
///
/// Uses the same character scheme as the input format:
///   0 → Floor, 1 → Wall, 2 → Box, 3 → Exit (floor at exit pos),
///   4 → Player, A/a → QuantumBlock box/wall cells.
///
/// Multiple QuantumBlocks get successive letters (A, B, C, …).
/// QuantumBlock letter assignment follows QBlockId order (skipping tombstones).
fn push_mask_display_for_tile(board: &QuantumBoard, flat: FlatIdx) -> String {
    match board.get_flat(flat) {
        Tile::Wall => "####".to_string(),
        Tile::Floor | Tile::Player => "....".to_string(),
        Tile::Box => crate::types::push_mask_to_string(board.get_box_push_mask(flat)),
        Tile::Quantum(id) => {
            let qb = &board.quantum_blocks[id as usize];
            let mask = qb.box_push_mask_at(flat);
            if mask == crate::types::PUSH_NONE {
                "####".to_string()
            } else {
                crate::types::push_mask_to_string(mask)
            }
        }
    }
}

pub fn format_board(board: &QuantumBoard, exit: FlatIdx) -> String {
    // Build a letter assignment: QBlockId → letter char (skipping tombstones).
    let mut id_to_letter: Vec<Option<char>> = vec![None; board.quantum_blocks.len()];
    let mut next_letter = b'A';
    for (i, qb) in board.quantum_blocks.iter().enumerate() {
        if !qb.is_empty() {
            if next_letter > b'Z' {
                // More than 26 blocks; fall back to '?'.
                id_to_letter[i] = Some('?');
            } else {
                id_to_letter[i] = Some(next_letter as char);
                next_letter += 1;
            }
        }
    }

    let mut letters: Vec<String> = Vec::new();
    let mut masks: Vec<String> = Vec::new();

    for y in 1..(board.height - 1) {
        let mut letter_line = String::new();
        let mut mask_line = String::new();
        for x in 1..(board.width - 1) {
            let flat = board.flat(x, y);
            let tile = board.get(x, y);
            let ch = match tile {
                Tile::Wall => '1',
                Tile::Floor => {
                    if flat == exit {
                        '3'
                    } else {
                        '0'
                    }
                }
                Tile::Box => '2',
                Tile::Player => '4',
                Tile::Quantum(id) => {
                    let letter = id_to_letter[id as usize].unwrap_or('?');
                    if board.quantum_blocks[id as usize].wall_pos == Some(flat) {
                        letter.to_ascii_lowercase()
                    } else {
                        letter
                    }
                }
            };
            if !mask_line.is_empty() {
                mask_line.push(' ');
            }
            mask_line.push_str(&push_mask_display_for_tile(board, flat));
            letter_line.push(ch);
        }
        letters.push(letter_line);
        masks.push(mask_line);
    }

    let mut out = String::new();
    out.push_str("Letters:\n");
    for line in &letters {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("\nPush masks:\n");
    for line in &masks {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Format a QuantumBoard header line showing exit and step information.
pub fn format_board_header(board: &QuantumBoard, exit: FlatIdx, label: &str) -> String {
    let (ex, ey) = board.coords(exit);
    // Convert from padded coords back to 1-indexed inner coords.
    format!("=== {} (exit {},{}) ===\n", label, ex, ey)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SIMPLE: &str = "\
exit 5,1
steps 4
00004
";
    // 5 inner columns, 1 inner row. Player at (1,1) (leftmost).
    // Wait — the player should be at position 1, exit at position 5.
    // But the grid is read left-to-right: col 1..5.
    // '4' is at column 5 (rightmost), which means player is at exit already.
    // Let's fix: player at col 1.

    const CORRIDOR: &str = "\
exit 5,1
steps 4
40000
";

    #[test]
    fn parse_corridor() {
        let p = parse_input(CORRIDOR).expect("parse failed");
        let (px, py) = p.board.coords(p.board.find_player());
        assert_eq!((px, py), (1, 1)); // padded coords
        let (ex, ey) = p.board.coords(p.exit);
        assert_eq!((ex, ey), (5, 1));
        assert_eq!(p.target_steps, 4);
    }

    const QUANTUM: &str = "\
exit 5,1
steps 6
4AAa0
";
    // Row block spanning inner x=2,3,4 with wall_pos at x=4.

    #[test]
    fn parse_quantum_block() {
        let p = parse_input(QUANTUM).expect("parse failed");
        assert_eq!(p.board.quantum_blocks.len(), 1);
        let qb = &p.board.quantum_blocks[0];
        assert!(qb.is_row());
        assert_eq!(qb.len(), 3);
        // wall_pos should be at inner x=4, y=1 → flat = 1*7 + 4...
        // board width = 5+2=7; flat(4,1) = 1*7+4 = 11.
        let wall_flat = p.board.flat(4, 1);
        assert_eq!(qb.wall_pos, Some(wall_flat));
    }

    #[test]
    fn format_roundtrip() {
        let p = parse_input(CORRIDOR).expect("parse failed");
        let formatted = format_board(&p.board, p.exit);
        // Should contain '4' for player and '3' for exit.
        assert!(formatted.contains('4'));
        assert!(formatted.contains('3'));
    }

    #[test]
    fn bad_exit_coords() {
        let bad = "exit 99,1\nsteps 4\n40000\n";
        assert!(parse_input(bad).is_err());
    }

    #[test]
    fn bad_steps() {
        let bad = "exit 5,1\nsteps abc\n40000\n";
        assert!(parse_input(bad).is_err());
    }
}
