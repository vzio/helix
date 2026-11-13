//! The phosphor filter of the terminal panel (this fork, `terminal-phosphor` in a theme's
//! `[effects]`): every color the programs in the panel use is drawn as the phosphor's color at
//! the original's brightness, like an old monochrome monitor. `ls`, `htop` and the prompt all
//! turn phosphor and stay readable, since what was bright stays bright and what was dark stays
//! dark.
//!
//! Indexed colors are resolved to RGB first, with xterm's default palette (the outer terminal's
//! own palette can't be read), RGB colors are taken as they are, and the default colors are the
//! panel's (`ui.terminal`, else `ui.background`), or xterm's light grey on black when the theme
//! leaves them to the terminal. Brightness is the perceived one (HSP: the square root of the
//! weighted squares of the channels), which ranks a pure blue as dim but visible, not as black.
//! Up to the phosphor's own brightness the color is the phosphor scaled down; brighter colors go
//! on from the phosphor towards white.

use vortex_view::graphics::{Color, Style};

type Rgb = (f64, f64, f64);

/// xterm's default palette for the 16 named colors.
const ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// The levels of each channel in xterm's 6 × 6 × 6 color cube (indices 16 to 231).
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// The text and background the panel assumes when the theme leaves them to the terminal.
const DEFAULT_FG: (u8, u8, u8) = ANSI[7];
const DEFAULT_BG: (u8, u8, u8) = ANSI[0];

/// A color as RGB, indexed and named ones in xterm's default palette; `None` for the default
/// color (`Reset`).
pub fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    let index = match color {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => return Some((r, g, b)),
        Color::Indexed(index) => index,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::LightGray => 7,
        Color::Gray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    Some(match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let i = index - 16;
            (
                CUBE[(i / 36) as usize],
                CUBE[(i / 6 % 6) as usize],
                CUBE[(i % 6) as usize],
            )
        }
        _ => {
            let grey = 8 + 10 * (index - 232);
            (grey, grey, grey)
        }
    })
}

/// The perceived brightness of a color, from 0 (black) to 1 (white).
pub fn brightness((r, g, b): (u8, u8, u8)) -> f64 {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    (0.299 * r * r + 0.587 * g * g + 0.114 * b * b).sqrt()
}

/// The filter for one phosphor color.
#[derive(Debug, Clone, Copy)]
pub struct Phosphor {
    color: Rgb,
    brightness: f64,
    fg: (u8, u8, u8),
    bg: (u8, u8, u8),
}

impl Phosphor {
    /// The filter of `phosphor` for a panel whose own colors are `base`; `None` for a phosphor
    /// that is black or the default color, which would hide everything.
    pub fn new(phosphor: Color, base: Style) -> Option<Self> {
        let color = rgb(phosphor)?;
        let brightness = brightness(color);
        if brightness <= 0.0 {
            return None;
        }
        Some(Self {
            color: (color.0 as f64, color.1 as f64, color.2 as f64),
            brightness,
            fg: base.fg.and_then(rgb).unwrap_or(DEFAULT_FG),
            bg: base.bg.and_then(rgb).unwrap_or(DEFAULT_BG),
        })
    }

    /// `color` (an RGB color) as the phosphor at its brightness.
    pub fn map(&self, color: (u8, u8, u8)) -> Color {
        let level = brightness(color);
        let (r, g, b) = self.color;
        let (r, g, b) = if level <= self.brightness {
            let k = level / self.brightness;
            (r * k, g * k, b * k)
        } else {
            // towards white by the share `k` whose brightness is `level`: the square of the
            // brightness is a quadratic in `k`
            let k = self.towards_white(level);
            (
                r + (255.0 - r) * k,
                g + (255.0 - g) * k,
                b + (255.0 - b) * k,
            )
        };
        Color::Rgb(r.round() as u8, g.round() as u8, b.round() as u8)
    }

    /// The share of the way from the phosphor to white whose brightness is `level`.
    fn towards_white(&self, level: f64) -> f64 {
        let (r, g, b) = self.color;
        let (mut sq, mut lin, mut quad) = (0.0, 0.0, 0.0);
        for (weight, channel) in [(0.299, r), (0.587, g), (0.114, b)] {
            let (a, d) = (channel / 255.0, 1.0 - channel / 255.0);
            sq += weight * a * a;
            lin += 2.0 * weight * a * d;
            quad += weight * d * d;
        }
        // quad k² + lin k + sq - level² = 0
        let c = sq - level * level;
        if quad <= 1e-12 {
            return 1.0;
        }
        let k = (-lin + (lin * lin - 4.0 * quad * c).max(0.0).sqrt()) / (2.0 * quad);
        k.clamp(0.0, 1.0)
    }

    /// A cell's style through the filter: both colors set, the default ones being the panel's.
    pub fn style(&self, style: Style) -> Style {
        let fg = style.fg.and_then(rgb).unwrap_or(self.fg);
        let bg = style.bg.and_then(rgb).unwrap_or(self.bg);
        let mut style = style.fg(self.map(fg)).bg(self.map(bg));
        if let Some(underline) = style.underline_color.and_then(rgb) {
            style = style.underline_color(self.map(underline));
        }
        style
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green() -> Phosphor {
        Phosphor::new(Color::Rgb(0x33, 0xdd, 0x66), Style::default()).unwrap()
    }

    fn channels(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("not RGB: {other:?}"),
        }
    }

    #[test]
    fn indexed_colors_resolve_to_xterms_palette() {
        assert_eq!(rgb(Color::Indexed(1)), Some((205, 0, 0)));
        assert_eq!(rgb(Color::Red), rgb(Color::Indexed(1)));
        assert_eq!(rgb(Color::White), Some((255, 255, 255)));
        assert_eq!(rgb(Color::Indexed(16)), Some((0, 0, 0)));
        assert_eq!(rgb(Color::Indexed(196)), Some((255, 0, 0)));
        assert_eq!(rgb(Color::Indexed(231)), Some((255, 255, 255)));
        assert_eq!(rgb(Color::Indexed(232)), Some((8, 8, 8)));
        assert_eq!(rgb(Color::Indexed(255)), Some((238, 238, 238)));
        assert_eq!(rgb(Color::Reset), None);
    }

    #[test]
    fn every_color_becomes_the_phosphor_at_its_own_brightness() {
        let phosphor = green();
        for color in [
            Color::Rgb(255, 0, 0),
            Color::Rgb(0, 0, 255),
            Color::Indexed(4),
            Color::Indexed(244),
            Color::Yellow,
            Color::Rgb(10, 10, 10),
        ] {
            let original = rgb(color).unwrap();
            let mapped = channels(phosphor.map(original));
            // the phosphor's hue: green leads, red and blue in the phosphor's proportions
            assert!(
                mapped.1 >= mapped.0 && mapped.1 >= mapped.2,
                "{color:?} {mapped:?}"
            );
            // the original's brightness, within rounding
            let (a, b) = (brightness(original), brightness(mapped));
            assert!((a - b).abs() < 0.02, "{color:?}: {a} vs {b}");
        }
        // black stays black, white stays white
        assert_eq!(phosphor.map((0, 0, 0)), Color::Rgb(0, 0, 0));
        assert_eq!(phosphor.map((255, 255, 255)), Color::Rgb(255, 255, 255));
    }

    #[test]
    fn brighter_stays_brighter() {
        let phosphor = green();
        let mut levels: Vec<(f64, f64)> = (0..=255u8)
            .step_by(5)
            .flat_map(|v| [(v, 0, 0), (0, v, 0), (0, 0, v), (v, v, v)])
            .map(|c| (brightness(c), brightness(channels(phosphor.map(c)))))
            .collect();
        levels.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert!(levels.windows(2).all(|pair| pair[0].1 <= pair[1].1 + 1e-3));
    }

    #[test]
    fn the_default_colors_are_the_panels() {
        let base = Style::default()
            .fg(Color::Rgb(200, 200, 200))
            .bg(Color::Rgb(0, 0, 0));
        let phosphor = Phosphor::new(Color::Rgb(0x33, 0xdd, 0x66), base).unwrap();
        let style = phosphor.style(Style::default());
        assert_eq!(style.bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(style.fg, Some(phosphor.map((200, 200, 200))));
        // a theme that leaves them to the terminal: xterm's light grey on black
        let phosphor = green();
        let style = phosphor.style(Style::default());
        assert_eq!(style.fg, Some(phosphor.map(DEFAULT_FG)));
        assert_eq!(style.bg, Some(Color::Rgb(0, 0, 0)));
    }

    #[test]
    fn a_black_phosphor_is_no_filter() {
        assert!(Phosphor::new(Color::Rgb(0, 0, 0), Style::default()).is_none());
        assert!(Phosphor::new(Color::Reset, Style::default()).is_none());
    }
}
