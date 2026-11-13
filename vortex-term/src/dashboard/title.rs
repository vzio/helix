//! The title of the dashboard, in its own file so it is easy to replace: `VX` while the
//! dashboard loads, opening to `VORTEX` once it is ready, in three sizes for the room there is.
//!
//! Each size is six letters drawn from plain text. The large one is a theme's choice
//! (`dashboard-logo` in its `[effects]`): the runic wordmark ([`RUNES`], the default: a diamond
//! O, an up-arrow T, Σ for E, a sharp R, its diagonals and bars in half blocks so every stroke
//! is as thick as the R's) or the classic block letters ([`LARGE`]), whose box-drawing characters are
//! their shadow, with its own theme scope. The opening ([`Title::cells`]): `V` and `X` slide
//! apart from the center, and `O`, `R`, `T` and `E` appear in the room they leave, through three
//! shades (`░▒▓`, or ASCII ones), the inner letters first.

use vortex_view::theme::{DashboardCharset, DashboardLogo};

use super::charset;

/// One size of the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Title {
    /// V, O, R, T, E and X: the rows of each, all as wide (in columns) as its first.
    pub letters: [&'static [&'static str]; 6],
    /// The columns between two letters of `VORTEX`.
    pub gap: u16,
}

/// The runic wordmark, the default large title: 5 rows, 66 columns.
pub const RUNES: Title = Title {
    letters: [
        &[
            "▀█      █▀",
            " ▀█    █▀ ",
            "  ██  ██  ",
            "   █▄▄█   ",
            "    ██    ",
        ],
        &[
            "   ▄██▄   ",
            " ▄█▀  ▀█▄ ",
            "██      ██",
            " ▀█▄  ▄█▀ ",
            "   ▀██▀   ",
        ],
        &["██▀▀▄▄  ", "██   ▄█▀", "██▄█▀▀  ", "██  ▀█▄ ", "██    ▀█"],
        &[
            "   ▄██▄   ",
            " ▄█▀██▀█▄ ",
            "▀▀  ██  ▀▀",
            "    ██    ",
            "    ██    ",
        ],
        &["██▀▀▀▀▀▀", " ▀█▄    ", "   ██   ", " ▄█▀    ", "██▄▄▄▄▄▄"],
        &[
            "▀█▄    ▄█▀",
            "  ▀█▄▄█▀  ",
            "    ██    ",
            "  ▄█▀▀█▄  ",
            "▄█▀    ▀█▄",
        ],
    ],
    gap: 2,
};

/// The classic large title: 6 rows, 51 columns ("ANSI Shadow").
pub const LARGE: Title = Title {
    letters: [
        &[
            "██╗   ██╗",
            "██║   ██║",
            "██║   ██║",
            "╚██╗ ██╔╝",
            " ╚████╔╝ ",
            "  ╚═══╝  ",
        ],
        &[
            " ██████╗ ",
            "██╔═══██╗",
            "██║   ██║",
            "██║   ██║",
            "╚██████╔╝",
            " ╚═════╝ ",
        ],
        &[
            "██████╗ ",
            "██╔══██╗",
            "██████╔╝",
            "██╔══██╗",
            "██║  ██║",
            "╚═╝  ╚═╝",
        ],
        &[
            "████████╗",
            "╚══██╔══╝",
            "   ██║   ",
            "   ██║   ",
            "   ██║   ",
            "   ╚═╝   ",
        ],
        &[
            "███████╗",
            "██╔════╝",
            "█████╗  ",
            "██╔══╝  ",
            "███████╗",
            "╚══════╝",
        ],
        &[
            "██╗  ██╗",
            "╚██╗██╔╝",
            " ╚███╔╝ ",
            " ██╔██╗ ",
            "██╔╝ ██╗",
            "╚═╝  ╚═╝",
        ],
    ],
    gap: 0,
};

/// The medium title: 3 rows, 18 columns.
pub const MEDIUM: Title = Title {
    letters: [
        &["╻ ╻", "┃┏┛", "┗┛ "],
        &["┏━┓", "┃ ┃", "┗━┛"],
        &["┏━┓", "┣┳┛", "╹┗╸"],
        &["╺┳╸", " ┃ ", " ╹ "],
        &["┏━╸", "┣╸ ", "┗━╸"],
        &["╻ ╻", "┏╋┛", "╹ ╹"],
    ],
    gap: 0,
};

/// The small title, plain text on one row, for windows too narrow or too short for the others.
pub const SMALL: Title = Title {
    letters: [&["v"], &["o"], &["r"], &["t"], &["e"], &["x"]],
    gap: 1,
};

/// The titles from the largest to the smallest.
pub const ALL: [Title; 4] = [RUNES, LARGE, MEDIUM, SMALL];

/// The large title of a logo.
pub fn large(logo: DashboardLogo) -> Title {
    match logo {
        DashboardLogo::Runes => RUNES,
        DashboardLogo::Classic => LARGE,
    }
}

/// The line under the title.
pub const CREDIT: &str = "fork of helix-editor/helix, by vzio";

/// One cell of the title as it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The column, from the left of `VORTEX`; while it opens, `VX` is centered on it too.
    pub x: u16,
    pub y: u16,
    pub c: char,
    /// A letter (or a shade of one) rather than its shadow.
    pub letter: bool,
}

fn columns(row: &str) -> u16 {
    row.chars().count() as u16
}

/// Ease in and out: slow at both ends.
fn ease(t: f64) -> f64 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// A number from 0 to 1 that looks random for a cell, the same every frame.
fn noise(x: u16, y: u16) -> f64 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f64 / 65536.0
}

/// When each letter starts to appear, in the title's progress: the inner ones first.
const APPEAR: [f64; 6] = [0.0, 0.34, 0.16, 0.16, 0.34, 0.0];
/// How long a letter takes to appear, in the title's progress.
const FADE: f64 = 0.5;
#[cfg(test)]
const SHADES: [char; 3] = charset::BLOCKS_SHADES;

impl Title {
    fn letter_width(&self, letter: usize) -> u16 {
        columns(self.letters[letter][0])
    }

    /// The width of `VORTEX`.
    pub fn width(&self) -> u16 {
        (0..6).map(|i| self.letter_width(i)).sum::<u16>() + self.gap * 5
    }

    pub fn height(&self) -> u16 {
        self.letters[0].len() as u16
    }

    /// Where each letter of `VORTEX` starts.
    fn lefts(&self) -> [u16; 6] {
        let mut lefts = [0; 6];
        let mut x = 0;
        for (i, left) in lefts.iter_mut().enumerate() {
            *left = x;
            x += self.letter_width(i) + self.gap;
        }
        lefts
    }

    /// Whether a character is a letter rather than its shadow.
    pub fn is_letter(c: char) -> bool {
        !matches!(c, '╗' | '║' | '╔' | '═' | '╝' | '╚')
    }

    /// The cells of the title at `progress`, from `VX` (0) to `VORTEX` (1), in blocks.
    pub fn cells(&self, progress: f64) -> Vec<Cell> {
        self.cells_in(progress, DashboardCharset::Blocks)
    }

    /// The cells of the title at `progress`, its letters appearing through the shades of
    /// `charset`.
    pub fn cells_in(&self, progress: f64, charset: DashboardCharset) -> Vec<Cell> {
        let shades = charset::shades(charset);
        let p = progress.clamp(0.0, 1.0);
        let width = self.width();
        let lefts = self.lefts();
        let (v, x) = (self.letter_width(0), self.letter_width(5));
        let gap = self.gap.max(1);
        // V and X side by side in the middle, sliding out to the ends
        let start = width.saturating_sub(v + gap + x) / 2;
        let slide = ease(p);
        let lerp = |from: u16, to: u16| (from as f64 + (to as f64 - from as f64) * slide).round();
        let v_left = lerp(start, lefts[0]) as u16;
        let x_left = lerp(start + v + gap, lefts[5]) as u16;

        let mut cells = Vec::new();
        // a letter's cells; `shown` says for each whether it is hidden (`None`), drawn as itself
        // (`Some(None)`) or as a shade (`Some(Some(shade))`)
        type Shown<'a> = &'a dyn Fn(u16, u16) -> Option<Option<char>>;
        let push = |letter: usize, left: u16, cells: &mut Vec<Cell>, shown: Shown| {
            for (y, row) in self.letters[letter].iter().enumerate() {
                for (col, c) in row.chars().enumerate() {
                    if c == ' ' {
                        continue;
                    }
                    let cell_x = left + col as u16;
                    if let Some(shade) = shown(cell_x, y as u16) {
                        let c = shade.unwrap_or(c);
                        cells.push(Cell {
                            x: cell_x,
                            y: y as u16,
                            c,
                            letter: Title::is_letter(c),
                        });
                    }
                }
            }
        };
        push(0, v_left, &mut cells, &|_, _| Some(None));
        push(5, x_left, &mut cells, &|_, _| Some(None));
        // the letters between them, in the room they leave, each cell through the shades
        for letter in 1..5 {
            let local = ((p - APPEAR[letter]) / FADE).clamp(0.0, 1.0);
            if local == 0.0 {
                continue;
            }
            let room = v_left + v..x_left;
            push(letter, lefts[letter], &mut cells, &|cell_x, y| {
                if !room.contains(&cell_x) {
                    return None;
                }
                if local >= 1.0 {
                    return Some(None);
                }
                // each cell a little ahead or behind, so a letter condenses rather than fades
                let at = local * 1.35 - 0.35 * noise(cell_x, y);
                match (at * 4.0).floor() as i64 {
                    ..=-1 => None,
                    stage @ 0..=2 => Some(Some(shades[stage as usize])),
                    _ => Some(None),
                }
            });
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(title: &Title, progress: f64) -> Vec<String> {
        let mut rows = vec![vec![' '; title.width() as usize]; title.height() as usize];
        for cell in title.cells(progress) {
            rows[cell.y as usize][cell.x as usize] = cell.c;
        }
        rows.into_iter()
            .map(|row| row.into_iter().collect::<String>().trim_end().to_string())
            .collect()
    }

    #[test]
    fn the_rows_of_each_letter_are_as_wide() {
        for title in ALL {
            for letter in title.letters {
                let width = columns(letter[0]);
                assert!(letter.iter().all(|row| columns(row) == width), "{letter:?}");
                assert_eq!(letter.len(), title.letters[0].len());
            }
        }
    }

    #[test]
    fn sizes() {
        assert_eq!((RUNES.width(), RUNES.height()), (66, 5));
        assert_eq!((LARGE.width(), LARGE.height()), (51, 6));
        assert_eq!((MEDIUM.width(), MEDIUM.height()), (18, 3));
        assert_eq!((SMALL.width(), SMALL.height()), (11, 1));
    }

    #[test]
    fn vx_opens_to_vortex() {
        assert_eq!(text(&SMALL, 0.0), ["    v x"]);
        assert_eq!(text(&SMALL, 1.0), ["v o r t e x"]);
        assert_eq!(
            text(&MEDIUM, 1.0),
            [
                "╻ ╻┏━┓┏━┓╺┳╸┏━╸╻ ╻",
                "┃┏┛┃ ┃┣┳┛ ┃ ┣╸ ┏╋┛",
                "┗┛ ┗━┛╹┗╸ ╹ ┗━╸╹ ╹"
            ]
        );
        assert_eq!(
            text(&LARGE, 1.0)[0],
            "██╗   ██╗ ██████╗ ██████╗ ████████╗███████╗██╗  ██╗"
        );
        // VX in the middle
        assert_eq!(
            text(&MEDIUM, 0.0),
            ["     ╻ ╻ ╻ ╻", "     ┃┏┛ ┏╋┛", "     ┗┛  ╹ ╹"]
        );
    }

    #[test]
    fn the_letters_between_appear_in_the_room_v_and_x_leave() {
        for title in ALL {
            let mut previous = 0;
            for step in 0..=20 {
                let p = step as f64 / 20.0;
                let cells = title.cells(p);
                // never two cells in one place
                let mut places: Vec<_> = cells.iter().map(|cell| (cell.x, cell.y)).collect();
                places.sort();
                places.dedup();
                assert_eq!(places.len(), cells.len(), "{p}");
                // more and more of it
                let full = cells
                    .iter()
                    .filter(|cell| !SHADES.contains(&cell.c))
                    .count();
                assert!(full >= previous, "{p}");
                previous = full;
            }
        }
    }

    #[test]
    fn the_runes_title_draws_its_wordmark() {
        assert_eq!(
            text(&RUNES, 1.0),
            [
                "▀█      █▀     ▄██▄     ██▀▀▄▄       ▄██▄     ██▀▀▀▀▀▀  ▀█▄    ▄█▀",
                " ▀█    █▀    ▄█▀  ▀█▄   ██   ▄█▀   ▄█▀██▀█▄    ▀█▄        ▀█▄▄█▀",
                "  ██  ██    ██      ██  ██▄█▀▀    ▀▀  ██  ▀▀     ██         ██",
                "   █▄▄█      ▀█▄  ▄█▀   ██  ▀█▄       ██       ▄█▀        ▄█▀▀█▄",
                "    ██         ▀██▀     ██    ▀█      ██      ██▄▄▄▄▄▄  ▄█▀    ▀█▄",
            ]
        );
        assert_eq!(
            text(&LARGE, 1.0)[0],
            "██╗   ██╗ ██████╗ ██████╗ ████████╗███████╗██╗  ██╗"
        );
        assert_eq!(large(DashboardLogo::Runes), RUNES);
        assert_eq!(large(DashboardLogo::Classic), LARGE);
    }

    #[test]
    fn the_ascii_charset_opens_the_title_through_ascii_shades_only() {
        let ascii = charset::shades(DashboardCharset::Ascii);
        for title in ALL {
            let mut shaded = 0;
            for step in 1..20 {
                let p = step as f64 / 20.0;
                for cell in title.cells_in(p, DashboardCharset::Ascii) {
                    if ascii.contains(&cell.c) {
                        shaded += 1;
                    }
                    assert!(!"░▒▓".contains(cell.c), "{:?} at {p}: {:?}", title, cell.c);
                }
            }
            // the shades are used, in the sizes that have inner letters
            assert!(shaded > 0 || title == SMALL || title == MEDIUM, "{title:?}");
        }
        // blocks still use the blocks
        let blocks = RUNES
            .cells_in(0.4, DashboardCharset::Blocks)
            .iter()
            .any(|cell| "░▒▓".contains(cell.c));
        assert!(blocks);
    }
}
