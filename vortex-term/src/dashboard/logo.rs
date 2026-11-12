//! The logo of the dashboard, in its own file so it is easy to replace: a spiral and the word
//! VORTEX, in three sizes for the room there is.
//!
//! Each logo is drawn from plain text. In the large and the medium one, the spiral is the part
//! before the first two spaces in a row, and in the word the block characters (`█`) are the
//! letters and the other ones their shadow, each with its own theme scope. The spiral draws
//! itself from its center outwards when the dashboard opens, while the word sweeps in from the
//! left ([`Logo::reveal_order`]).

/// One size of the logo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Logo {
    /// The rows of the spiral, all as wide (in columns) as the first one.
    pub spiral: &'static [&'static str],
    /// The columns between the spiral and the word.
    pub gap: u16,
    /// The rows of the word, all as wide as the first one.
    pub word: &'static [&'static str],
}

/// The large logo: 6 rows, 64 columns.
pub const LARGE: Logo = Logo {
    spiral: &[
        "╭─────────╮",
        "│ ╭─────╮ │",
        "│ │ ╶─╮ │ │",
        "│ ╰───╯ │ │",
        "╰───────╯ │",
        "╶─────────╯",
    ],
    gap: 2,
    word: &[
        "██╗   ██╗ ██████╗ ██████╗ ████████╗███████╗██╗  ██╗",
        "██║   ██║██╔═══██╗██╔══██╗╚══██╔══╝██╔════╝╚██╗██╔╝",
        "██║   ██║██║   ██║██████╔╝   ██║   █████╗   ╚███╔╝ ",
        "╚██╗ ██╔╝██║   ██║██╔══██╗   ██║   ██╔══╝   ██╔██╗ ",
        " ╚████╔╝ ╚██████╔╝██║  ██║   ██║   ███████╗██╔╝ ██╗",
        "  ╚═══╝   ╚═════╝ ╚═╝  ╚═╝   ╚═╝   ╚══════╝╚═╝  ╚═╝",
    ],
};

/// The medium logo: 3 rows, 25 columns.
pub const MEDIUM: Logo = Logo {
    spiral: &["╭───╮", "│╶╮ │", "╰─╯ │"],
    gap: 2,
    word: &[
        "╻ ╻┏━┓┏━┓╺┳╸┏━╸╻ ╻",
        "┃┏┛┃ ┃┣┳┛ ┃ ┣╸ ┏╋┛",
        "┗┛ ┗━┛╹┗╸ ╹ ┗━╸╹ ╹",
    ],
};

/// The small logo, one row, for windows too narrow or too short for the others.
pub const SMALL: Logo = Logo {
    spiral: &["╶╮"],
    gap: 2,
    word: &["V O R T E X"],
};

/// The logos from the largest to the smallest.
pub const ALL: [Logo; 3] = [LARGE, MEDIUM, SMALL];

/// The line under the logo.
pub const CREDIT: &str = "fork of helix-editor/helix";

fn columns(row: &str) -> u16 {
    row.chars().count() as u16
}

impl Logo {
    pub fn width(&self) -> u16 {
        self.spiral_width() + self.gap + columns(self.word[0])
    }

    pub fn height(&self) -> u16 {
        self.spiral.len().max(self.word.len()) as u16
    }

    pub fn spiral_width(&self) -> u16 {
        columns(self.spiral[0])
    }

    /// Whether a character of the word is a letter rather than its shadow.
    pub fn is_letter(c: char) -> bool {
        c == '█' || !matches!(c, '╗' | '║' | '╔' | '═' | '╝' | '╚')
    }

    /// The cells of the spiral (row, column) in the order the line runs from its inner end, the
    /// `╶`, outwards. Cells off the line (spaces) aren't in it.
    pub fn reveal_order(&self) -> Vec<(u16, u16)> {
        let grid: Vec<Vec<char>> = self
            .spiral
            .iter()
            .map(|row| row.chars().collect())
            .collect();
        let at = |row: isize, col: isize| -> char {
            if row < 0 || col < 0 {
                return ' ';
            }
            grid.get(row as usize)
                .and_then(|r| r.get(col as usize))
                .copied()
                .unwrap_or(' ')
        };
        // the directions a box-drawing character connects: up, down, left, right
        let exits = |c: char| -> &'static [(isize, isize)] {
            match c {
                '─' | '━' => &[(0, -1), (0, 1)],
                '│' | '┃' => &[(-1, 0), (1, 0)],
                '╭' => &[(1, 0), (0, 1)],
                '╮' => &[(1, 0), (0, -1)],
                '╰' => &[(-1, 0), (0, 1)],
                '╯' => &[(-1, 0), (0, -1)],
                '╶' => &[(0, 1)],
                '╴' => &[(0, -1)],
                _ => &[],
            }
        };
        let Some(start) = grid.iter().enumerate().find_map(|(row, cells)| {
            let col = cells.iter().position(|&c| c == '╶')?;
            Some((row as isize, col as isize))
        }) else {
            return Vec::new();
        };
        let mut order = vec![start];
        let mut previous = None;
        let mut current = start;
        loop {
            let next = exits(at(current.0, current.1))
                .iter()
                .map(|(dr, dc)| (current.0 + dr, current.1 + dc))
                .find(|&cell| Some(cell) != previous && at(cell.0, cell.1) != ' ');
            match next {
                Some(cell) if !order.contains(&cell) => {
                    order.push(cell);
                    previous = Some(current);
                    current = cell;
                }
                _ => break,
            }
        }
        order
            .into_iter()
            .map(|(row, col)| (row as u16, col as u16))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rows_of_each_part_are_as_wide() {
        for logo in ALL {
            for part in [logo.spiral, logo.word] {
                let width = columns(part[0]);
                assert!(part.iter().all(|row| columns(row) == width), "{part:?}");
            }
        }
    }

    #[test]
    fn the_spiral_is_one_line_from_its_center() {
        for logo in [LARGE, MEDIUM] {
            let drawn = logo
                .spiral
                .iter()
                .flat_map(|row| row.chars())
                .filter(|&c| c != ' ')
                .count();
            let order = logo.reveal_order();
            assert_eq!(order.len(), drawn, "{:?}", logo.spiral);
            assert_eq!(
                logo.spiral[order[0].0 as usize]
                    .chars()
                    .nth(order[0].1 as usize),
                Some('╶')
            );
        }
    }

    #[test]
    fn sizes() {
        assert_eq!((LARGE.width(), LARGE.height()), (64, 6));
        assert_eq!((MEDIUM.width(), MEDIUM.height()), (25, 3));
        assert_eq!((SMALL.width(), SMALL.height()), (15, 1));
    }
}
