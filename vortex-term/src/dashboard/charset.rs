//! The characters the dashboard draws its shades with (`dashboard-charset` in a theme's
//! `[effects]`): shaded blocks, or ASCII characters ordered by how much ink they have.
//!
//! The ASCII ramp was measured, not guessed: each character's ink, the share of its cell it
//! covers, averaged over DejaVu Sans Mono, JetBrains Mono and Liberation Mono ([`ASCII_INK`]).
//! Its densest character stands for the full block and its lightest for `·`, at the same
//! thresholds of light, so the same cells are lit. The others sit where the blocks' ink (`░`
//! a quarter, `▒` a half, `▓` three quarters) reaches theirs, relative to the densest: the
//! gradient reads the same, only finer.

use vortex_view::theme::DashboardCharset;

/// The characters of light, from the faintest.
pub const BLOCKS: [char; 5] = ['·', '░', '▒', '▓', '█'];
/// The light each block needs.
pub const BLOCK_LEVELS: [f32; 5] = [0.10, 0.30, 0.52, 0.78, 1.15];

/// The ASCII characters of light, from the faintest.
pub const ASCII: [char; 6] = ['.', ':', '=', '#', '&', '@'];
/// Their measured ink: the share of a cell each covers.
pub const ASCII_INK: [f32; 6] = [0.0255, 0.0498, 0.1011, 0.1984, 0.2218, 0.2699];
/// The light each needs: `.` and `@` at the thresholds of `·` and `█`, the others where the
/// blocks' ink, from a quarter for `░` to all of it for `█`, reaches their ink relative to `@`.
pub const ASCII_LEVELS: [f32; 6] = [0.10, 0.215, 0.409, 0.764, 0.887, 1.15];

/// The characters of light of `charset`, from the faintest, with the light each needs.
pub fn ramp(charset: DashboardCharset) -> (&'static [char], &'static [f32]) {
    match charset {
        DashboardCharset::Blocks => (&BLOCKS, &BLOCK_LEVELS),
        DashboardCharset::Ascii => (&ASCII, &ASCII_LEVELS),
    }
}

/// The block shades, light to dense.
pub const BLOCKS_SHADES: [char; 3] = ['░', '▒', '▓'];

/// The three shades a letter of the title goes through while it appears, light to dense.
pub fn shades(charset: DashboardCharset) -> [char; 3] {
    match charset {
        DashboardCharset::Blocks => BLOCKS_SHADES,
        // the ASCII characters nearest in ink to a quarter, a half and three quarters of `@`
        DashboardCharset::Ascii => [':', '=', '#'],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The light at which the blocks' ink reaches `ink` (as a share of a full block): through
    /// `·` (taken as the ASCII ramp's lightest), `░`, `▒`, `▓` and `█` at their thresholds.
    fn block_light(ink: f32) -> f32 {
        let lightest = ASCII_INK[0] / ASCII_INK[ASCII_INK.len() - 1];
        let points = [
            (BLOCK_LEVELS[0], lightest),
            (BLOCK_LEVELS[1], 0.25),
            (BLOCK_LEVELS[2], 0.5),
            (BLOCK_LEVELS[3], 0.75),
            (BLOCK_LEVELS[4], 1.0),
        ];
        for pair in points.windows(2) {
            let ((l0, i0), (l1, i1)) = (pair[0], pair[1]);
            if ink <= i1 {
                return l0 + (ink - i0) / (i1 - i0) * (l1 - l0);
            }
        }
        BLOCK_LEVELS[4]
    }

    #[test]
    fn every_shade_has_a_character_of_more_ink_than_the_one_before() {
        for charset in [DashboardCharset::Blocks, DashboardCharset::Ascii] {
            let (chars, levels) = ramp(charset);
            assert_eq!(chars.len(), levels.len());
            assert!(
                levels.windows(2).all(|pair| pair[0] < pair[1]),
                "{levels:?}"
            );
            assert!(chars
                .iter()
                .all(|c| charset == DashboardCharset::Blocks || c.is_ascii()));
        }
        assert!(ASCII_INK.windows(2).all(|pair| pair[0] < pair[1]));
        // the ends at the thresholds of the blocks' ends: the same cells are lit
        assert_eq!(ASCII_LEVELS[0], BLOCK_LEVELS[0]);
        assert_eq!(ASCII_LEVELS[5], BLOCK_LEVELS[4]);
        // the shades of the title are in the ramp, in order
        let shades = shades(DashboardCharset::Ascii);
        let at = |c| ASCII.iter().position(|&a| a == c).unwrap();
        assert!(at(shades[0]) < at(shades[1]) && at(shades[1]) < at(shades[2]));
    }

    #[test]
    fn the_ascii_levels_follow_the_blocks_ink() {
        let densest = ASCII_INK[ASCII_INK.len() - 1];
        for (i, (&ink, &level)) in ASCII_INK.iter().zip(&ASCII_LEVELS).enumerate() {
            if i == 0 || i == ASCII.len() - 1 {
                continue;
            }
            let expected = block_light(ink / densest);
            assert!(
                (expected - level).abs() < 0.005,
                "{}: {expected} vs {level}",
                ASCII[i]
            );
        }
    }
}
