//! The vortex behind the dashboard: particles drifting into a whirlpool, drawn in braille dots.
//!
//! It is a flow, not a drawing that turns. Each particle is born at the rim, beyond the corners
//! of the window, and falls towards the eye along a logarithmic spiral with the golden pitch
//! (the radius grows by φ every quarter turn). Its fall speeds up as it nears the eye, so it
//! whirls faster and faster, the way water goes down a drain, while the spiral arms the
//! particles crowd on turn slowly as a whole. Each particle leaves a short trail of where it was
//! a moment before, which draws the streaks: long and curved near the eye, short at the rim.
//!
//! A frame is a pure function of its phase (the time, in seconds of spin) and the size of the
//! area: the same phase draws the same frame, which the tests rely on. Its cost is a few
//! thousand sines and logarithms; see FORK.md for the measured time.
//!
//! The dots are braille characters, 2 × 4 to a cell; with cells about twice as tall as wide the
//! dots are square, so circles stay round. A cell's color comes from its distance to the eye,
//! along a gradient of the theme's colors, fading into the background at the rim.

use std::f64::consts::TAU;

use vortex_view::graphics::{Color, Rect, Style};

use tui::buffer::Buffer as Surface;

/// Below this size a vortex doesn't read as one: no background.
pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 16;

/// The pitch of the arms: a golden spiral grows by φ every quarter turn.
const PITCH: f64 = 0.306_349; // ln(φ) / (π / 2)
/// The spiral arms.
const ARMS: u64 = 3;
/// The share of particles between the arms rather than on one.
const DUST: f64 = 0.14;
/// How long a particle takes from the rim to the eye, in seconds of spin.
const LIFE: f64 = 11.0;
/// How fast the arms turn as a whole, in turns per second of spin.
const TURN: f64 = 0.035;
/// The radius of the eye, as a share of the vortex's.
const EYE: f64 = 0.045;
/// The cells per particle: fewer particles in larger windows would look sparse.
const CELLS_PER_PARTICLE: f64 = 3.2;
/// The points of a trail, and the time between two of them, in seconds of spin.
const TRAIL: usize = 5;
const TRAIL_STEP: f64 = 0.045;
/// The shades of the gradient: fewer colors, so neighbor cells often share one.
const SHADES: f64 = 18.0;

/// A number from 0 to 1 that looks random, from a particle and a key.
fn hash(particle: u64, key: u64) -> f64 {
    let mut h = particle
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(key.wrapping_mul(0xC2B2_AE3D_27D4_EB4F));
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 32;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// The colors of the vortex: a gradient from the eye to the rim, and the background it fades
/// into.
#[derive(Debug, Clone)]
pub struct Colors {
    pub gradient: Vec<Color>,
    pub background: Option<Color>,
}

fn rgb(color: Color) -> Option<(f64, f64, f64)> {
    match color {
        Color::Rgb(r, g, b) => Some((r as f64, g as f64, b as f64)),
        _ => None,
    }
}

impl Colors {
    /// The color at `t`, from the eye (0) to the rim (1).
    fn at(&self, t: f64) -> Option<Color> {
        let stops = &self.gradient;
        let t = ((t.clamp(0.0, 1.0) * SHADES).round() / SHADES).min(1.0);
        match stops.len() {
            0 => return None,
            1 => return Some(stops[0]),
            _ => {}
        }
        let at = t * (stops.len() - 1) as f64;
        let i = (at.floor() as usize).min(stops.len() - 2);
        let (from, to) = (stops[i], stops[i + 1]);
        let (Some(a), Some(b)) = (rgb(from), rgb(to)) else {
            // named colors don't mix: the nearest stop
            return Some(stops[at.round() as usize]);
        };
        let f = at - i as f64;
        let mut mixed = (
            a.0 + (b.0 - a.0) * f,
            a.1 + (b.1 - a.1) * f,
            a.2 + (b.2 - a.2) * f,
        );
        // into the background at the rim
        if let Some(bg) = self.background.and_then(rgb) {
            let fade = ((t - 0.72) / 0.28).clamp(0.0, 1.0) * 0.7;
            mixed = (
                mixed.0 + (bg.0 - mixed.0) * fade,
                mixed.1 + (bg.1 - mixed.1) * fade,
                mixed.2 + (bg.2 - mixed.2) * fade,
            );
        }
        Some(Color::Rgb(
            mixed.0.round() as u8,
            mixed.1.round() as u8,
            mixed.2.round() as u8,
        ))
    }
}

/// The shades of density, from the lightest.
const SHADES_CHARS: [char; 3] = ['░', '▒', '▓'];
/// The density a cell needs for each shade.
const LEVELS: [f32; 3] = [0.30, 1.0, 2.0];

/// Whether a vortex fits in `area`.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

/// The density of the vortex at phase `spin` in each cell of an area of `width` × `height`
/// cells, row by row: each particle and its trail spread over the cells around it, so the
/// shades blend.
pub fn density(width: u16, height: u16, spin: f64) -> Vec<f32> {
    let (cols, rows) = (width as usize, height as usize);
    let mut cells = vec![0f32; cols * rows];
    // in units of a cell's width: cells are about twice as tall as wide
    let (w, h) = (cols as f64, rows as f64 * 2.0);
    let (cx, cy) = (w / 2.0, h / 2.0);
    // the rim goes past the corners, so particles are born out of sight
    let rim = (cx * cx + cy * cy).sqrt() * 1.04;
    let particles = ((cols * rows) as f64 / CELLS_PER_PARTICLE) as u64;
    let turn = TAU * TURN * spin;
    let mut splat = |x: f64, y: f64, weight: f32| {
        // bilinear, over the four cells nearest to the point
        let (fx, fy) = (x - 0.5, y / 2.0 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (dx, dy) = ((fx - x0) as f32, (fy - y0) as f32);
        for (ox, oy, share) in [
            (0, 0, (1.0 - dx) * (1.0 - dy)),
            (1, 0, dx * (1.0 - dy)),
            (0, 1, (1.0 - dx) * dy),
            (1, 1, dx * dy),
        ] {
            let (col, row) = (x0 as i64 + ox, y0 as i64 + oy);
            if col >= 0 && row >= 0 && (col as usize) < cols && (row as usize) < rows {
                cells[row as usize * cols + col as usize] += share * weight;
            }
        }
    };

    for i in 0..particles {
        let dust = hash(i, 1) < DUST;
        let arm = if dust {
            hash(i, 2) * TAU
        } else {
            (i % ARMS) as f64 * TAU / ARMS as f64
        };
        let birth = hash(i, 3);
        // around its arm: wide at the rim, narrow near the eye
        let spread = (hash(i, 4) - 0.5) + (hash(i, 5) - 0.5);
        let wobble = 1.0 + (hash(i, 6) - 0.5) * 0.10;
        let now = (spin / LIFE + birth).rem_euclid(1.0);
        for step in 0..TRAIL {
            let t = spin - step as f64 * TRAIL_STEP;
            let life = (t / LIFE + birth).rem_euclid(1.0);
            if step > 0 && life > now {
                // the trail from before it was born again at the rim
                break;
            }
            if life > 0.97 {
                continue; // swallowed by the eye
            }
            // the fall speeds up towards the eye
            let fall = (1.0 - life).powf(0.8);
            let radius = rim * (EYE + (1.0 - EYE) * fall) * wobble;
            let near = radius / rim;
            let width = if dust { 0.0 } else { 0.30 * near.sqrt() + 0.04 };
            let angle = arm + (rim / radius).ln() / PITCH + turn + spread * width;
            let (sin, cos) = angle.sin_cos();
            let x = cx + radius * cos;
            let y = cy + radius * sin;
            if x < -1.0 || y < -2.0 || x > w + 1.0 || y > h + 2.0 {
                continue;
            }
            // the trail fades behind the particle, dust is fainter
            let weight = (1.0 - step as f32 / TRAIL as f32) * if dust { 0.6 } else { 1.0 };
            splat(x, y, weight);
        }
    }
    cells
}

/// Draws the vortex at phase `spin` over `area`, except in `holes`.
pub fn draw(surface: &mut Surface, area: Rect, spin: f64, holes: &[Rect], colors: &Colors) {
    if !fits(area) {
        return;
    }
    let cells = density(area.width, area.height, spin);
    let (cx, cy) = (area.width as f64 / 2.0, area.height as f64 / 2.0);
    // the distance in cell widths, cells being about twice as tall as wide
    let rim = (cx * cx + cy * cy * 4.0).sqrt();
    let mut last: Option<(usize, Option<Color>)> = None;
    for row in 0..area.height {
        for col in 0..area.width {
            let density = cells[row as usize * area.width as usize + col as usize];
            let Some(level) = LEVELS.iter().rposition(|&level| density >= level) else {
                continue;
            };
            let (x, y) = (area.x + col, area.y + row);
            if holes.iter().any(|hole| contains(hole, x, y)) {
                continue;
            }
            let dx = col as f64 + 0.5 - cx;
            let dy = (row as f64 + 0.5 - cy) * 2.0;
            let t = (dx * dx + dy * dy).sqrt() / rim;
            let shade = (t * SHADES).round() as usize;
            let color = match last {
                Some((last_shade, color)) if last_shade == shade => color,
                _ => {
                    let color = colors.at(t);
                    last = Some((shade, color));
                    color
                }
            };
            let Some(cell) = surface.get_mut(x, y) else {
                continue;
            };
            cell.set_char(SHADES_CHARS[level]);
            if let Some(color) = color {
                cell.set_style(Style::default().fg(color));
            }
        }
    }
}

fn contains(rect: &Rect, x: u16, y: u16) -> bool {
    x >= rect.left() && x < rect.right() && y >= rect.top() && y < rect.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_phase_draws_the_same_frame() {
        assert_eq!(density(120, 40, 3.25), density(120, 40, 3.25));
        assert_ne!(density(120, 40, 3.25), density(120, 40, 3.30));
    }

    #[test]
    fn it_fills_the_area_evenly() {
        // every quarter of the area is shaded somewhere
        let (w, h) = (120usize, 40usize);
        let cells = density(w as u16, h as u16, 7.0);
        for (qx, qy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let count = (0..h / 2)
                .flat_map(|y| (0..w / 2).map(move |x| (x + qx * w / 2, y + qy * h / 2)))
                .filter(|&(x, y)| cells[y * w + x] >= LEVELS[0])
                .count();
            assert!(count > w * h / 4 / 10, "quarter {qx},{qy}: {count}");
        }
    }

    #[test]
    fn the_gradient_mixes_rgb_and_picks_named_colors() {
        let colors = Colors {
            gradient: vec![Color::Rgb(0, 0, 0), Color::Rgb(180, 180, 180)],
            background: None,
        };
        assert_eq!(colors.at(0.0), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(colors.at(0.5), Some(Color::Rgb(90, 90, 90)));
        let named = Colors {
            gradient: vec![Color::Red, Color::Blue, Color::Green],
            background: None,
        };
        assert_eq!(named.at(0.1), Some(Color::Red));
        assert_eq!(named.at(0.5), Some(Color::Blue));
        assert_eq!(named.at(1.0), Some(Color::Green));
    }

    /// The time a frame takes, printed: the vortex alone and the whole dashboard. Run it
    /// optimized: `cargo test -p vortex-term --lib --profile integration frame_cost --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn frame_cost() {
        use crate::dashboard::render::{self, tests as render_tests, Model};
        use crate::dashboard::Timeline;
        use std::time::{Duration, Instant};
        use vortex_view::editor::DashboardBackground;

        let theme = vortex_view::theme::Loader::new(&[]).default();
        let colors = Colors {
            gradient: vec![Color::Rgb(137, 180, 250), Color::Rgb(203, 166, 247), Color::Rgb(250, 179, 135)],
            background: Some(Color::Rgb(30, 30, 46)),
        };
        let frames = 300;
        for (width, height) in [(200u16, 60u16), (160, 50), (100, 44)] {
            let area = Rect::new(0, 0, width, height);
            let mut surface = Surface::empty(area);
            let start = Instant::now();
            for frame in 0..frames {
                draw(&mut surface, area, frame as f64 / 30.0, &[], &colors);
            }
            let vortex = start.elapsed() / frames;

            let born = Instant::now();
            let stats = render_tests::loaded(born);
            let hints = render_tests::hints();
            let model = Model {
                stats: &stats,
                timeline: Timeline::new(born, DashboardBackground::On),
                dir: std::path::Path::new("/src/vortex"),
                hints: &hints,
            };
            let start = Instant::now();
            for frame in 0..frames {
                let now = born + Duration::from_secs(5) + Duration::from_millis(33 * frame as u64);
                render::render(&model, &theme, area, &mut surface, now);
            }
            let whole = start.elapsed() / frames;
            println!("{width}x{height}: vortex {vortex:?}, whole dashboard {whole:?} a frame");
        }
    }
}
