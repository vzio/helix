//! The vortex behind the dashboard: a whirlpool filling the whole area, drawn in shades.
//!
//! It is a flow, not a drawing that turns, made of two layers:
//!
//! - **The current**: eight arms of logarithmic spirals with the golden pitch (the radius grows
//!   by φ every quarter turn), and five finer ones of another pitch across them. A log spiral that
//!   turns looks the same as one that flows inwards, so the arms seem to pour into the eye, and
//!   as the two sets drift at their own speeds their crossings shift like ripples on water. It
//!   gives every cell some light, brighter towards the eye.
//! - **Particles** riding the arms: each falls from beyond the corners towards the eye, faster and
//!   faster as it nears it, the way water goes down a drain, leaving a short trail that draws a
//!   bright streak, long and curved near the eye.
//!
//! A frame is a pure function of its phase (the time, in seconds of spin) and the size of the
//! area: the same phase draws the same frame, which the tests rely on. The geometry of each cell
//! is kept for the size, so a frame costs a few multiplications a cell and a logarithm and a sine
//! for each point of a particle's trail; see FORK.md for the measured time.
//!
//! A cell's light picks its character, from `·` through `░▒▓` to `█`, and dims its color into
//! the background; its distance to the eye picks the color, along a gradient (by default two
//! tones: the title's color at the eye, the text's at the rim). Cells are about twice as tall as
//! wide, so distances count a row as two columns and circles stay round.

use std::f64::consts::TAU;

use vortex_view::graphics::{Color, Rect, Style};

use tui::buffer::Buffer as Surface;

/// Below this size a vortex doesn't read as one: no background.
pub const MIN_WIDTH: u16 = 48;
pub const MIN_HEIGHT: u16 = 16;

/// The pitch of the arms: a golden spiral grows by φ every quarter turn.
const PITCH: f64 = 0.306_349; // ln(φ) / (π / 2)
/// The arms of the current, and how fast they pour in (radians of phase a second of spin).
const ARMS: f64 = 8.0;
const POUR: f64 = 2.4;
/// The finer arms across them: how many, their pitch, and their speed.
const RIPPLES: f64 = 5.0;
const RIPPLE_PITCH: f64 = PITCH * 1.7;
const RIPPLE_POUR: f64 = -1.1;
/// The share of particles between the arms rather than on one.
const DUST: f64 = 0.2;
/// How long a particle takes from the rim to the eye, in seconds of spin.
const LIFE: f64 = 11.0;
/// The radius of the eye, as a share of the vortex's.
const EYE: f64 = 0.045;
/// The cells per particle.
const CELLS_PER_PARTICLE: f64 = 4.0;
/// The points of a trail, and the time between two of them, in seconds of spin.
const TRAIL: usize = 4;
const TRAIL_STEP: f64 = 0.045;
/// The shades of the gradient and of the light: fewer colors, so neighbor cells often share one.
const SHADES: f64 = 10.0;
const GLOWS: f64 = 3.0;

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

/// The colors of the vortex: a gradient from the eye to the rim, and the background it dims
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

fn mix(a: (f64, f64, f64), b: (f64, f64, f64), f: f64) -> (f64, f64, f64) {
    (
        a.0 + (b.0 - a.0) * f,
        a.1 + (b.1 - a.1) * f,
        a.2 + (b.2 - a.2) * f,
    )
}

impl Colors {
    /// The color at `t`, from the eye (0) to the rim (1), at `glow`, from dim (0) to full (1).
    fn at(&self, t: f64, glow: f64) -> Option<Color> {
        let stops = &self.gradient;
        let t = ((t.clamp(0.0, 1.0) * SHADES).round() / SHADES).min(1.0);
        let glow = (glow.clamp(0.0, 1.0) * GLOWS).round() / GLOWS;
        let base = match stops.len() {
            0 => return None,
            1 => stops[0],
            _ => {
                // the eye's color holds through the middle, the rim's comes late
                let at = t.powf(1.7) * (stops.len() - 1) as f64;
                let i = (at.floor() as usize).min(stops.len() - 2);
                match (rgb(stops[i]), rgb(stops[i + 1])) {
                    (Some(a), Some(b)) => {
                        let (r, g, b) = mix(a, b, at - i as f64);
                        Color::Rgb(r.round() as u8, g.round() as u8, b.round() as u8)
                    }
                    // named colors don't mix: the nearest stop
                    _ => stops[at.round() as usize],
                }
            }
        };
        // dimmed into the background where the light is low, and towards the rim
        let (Some(color), Some(bg)) = (rgb(base), self.background.and_then(rgb)) else {
            return Some(base);
        };
        let dim = (1.0 - glow) * 0.72 + ((t - 0.75) / 0.25).clamp(0.0, 1.0) * 0.25;
        let (r, g, b) = mix(color, bg, dim.min(0.85));
        Some(Color::Rgb(
            r.round() as u8,
            g.round() as u8,
            b.round() as u8,
        ))
    }
}

/// The characters of light, from the faintest, and the light each needs.
const CHARS: [char; 5] = ['·', '░', '▒', '▓', '█'];
const LEVELS: [f32; 5] = [0.10, 0.30, 0.52, 0.78, 1.15];

/// Whether a vortex fits in `area`.
pub fn fits(area: Rect) -> bool {
    area.width >= MIN_WIDTH && area.height >= MIN_HEIGHT
}

/// For each cell, row by row: see [`geometry`].
type Geometry = std::rc::Rc<Vec<[f32; 6]>>;

thread_local! {
    /// The geometry of the last size drawn: the vortex is drawn at one size frame after frame.
    static GEOMETRY: std::cell::RefCell<Option<(u16, u16, Geometry)>> =
        const { std::cell::RefCell::new(None) };
}

/// For each cell of an area of `width` × `height` cells, row by row: the cosine and sine of the
/// angle of the arms and of the ripples at its place, its weight (brighter towards the eye, dark
/// in it), and its distance to the eye from 0 to 1 at the corners. Kept in memory for the last
/// size.
fn geometry(width: u16, height: u16) -> Geometry {
    GEOMETRY.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((w, h, cells)) = cache.as_ref() {
            if (*w, *h) == (width, height) {
                return cells.clone();
            }
        }
        let (cols, rows) = (width as usize, height as usize);
        let (w, h) = (cols as f64, rows as f64 * 2.0);
        let (cx, cy) = (w / 2.0, h / 2.0);
        let rim = (cx * cx + cy * cy).sqrt() * 1.04;
        let mut cells = Vec::with_capacity(cols * rows);
        for row in 0..rows {
            let dy = (row as f64 + 0.5) * 2.0 - cy;
            for col in 0..cols {
                let dx = col as f64 + 0.5 - cx;
                let r = (dx * dx + dy * dy).sqrt().max(0.5);
                let near = r / rim;
                let theta = dy.atan2(dx);
                let wound = (rim / r).ln();
                let (arm_sin, arm_cos) = (ARMS * (theta - wound / PITCH)).sin_cos();
                let (ripple_sin, ripple_cos) = (RIPPLES * (theta - wound / RIPPLE_PITCH)).sin_cos();
                let inner = 0.7 + 0.45 * (1.0 - near);
                let eye = ((near - EYE) / (EYE * 2.0)).clamp(0.0, 1.0);
                cells.push([
                    arm_cos as f32,
                    arm_sin as f32,
                    ripple_cos as f32,
                    ripple_sin as f32,
                    (inner * eye) as f32,
                    (r * 1.04 / rim) as f32,
                ]);
            }
        }
        let cells = std::rc::Rc::new(cells);
        *cache = Some((width, height, cells.clone()));
        cells
    })
}

/// The light of the vortex at phase `spin` in each cell of an area of `width` × `height`
/// cells, row by row: the current, and the particles spread over the cells around them.
pub fn light(width: u16, height: u16, spin: f64) -> Vec<f32> {
    let (cols, rows) = (width as usize, height as usize);
    let mut cells = vec![0f32; cols * rows];
    // in units of a cell's width: cells are about twice as tall as wide
    let (w, h) = (cols as f64, rows as f64 * 2.0);
    let (cx, cy) = (w / 2.0, h / 2.0);
    // the rim goes past the corners, so particles are born out of sight
    let rim = (cx * cx + cy * cy).sqrt() * 1.04;

    // the current: each cell's phases are cos(a - pour), sin(a - pour) for an angle `a` of the
    // cell's own, so with cos a and sin a kept for the size there is no trigonometry per cell
    let geometry = geometry(width, height);
    let (pour_sin, pour_cos) = ((POUR * spin) as f32).sin_cos();
    let (ripple_sin, ripple_cos) = ((RIPPLE_POUR * spin) as f32).sin_cos();
    for (light, cell) in cells.iter_mut().zip(geometry.iter()) {
        let [arm_cos, arm_sin, ripple_cos_a, ripple_sin_a, weight, _] = *cell;
        let arms = 0.5 + 0.5 * (arm_cos * pour_cos + arm_sin * pour_sin);
        let ripples = 0.5 + 0.5 * (ripple_cos_a * ripple_cos + ripple_sin_a * ripple_sin);
        // sharper arms
        *light = (0.22 + 0.50 * arms * arms + 0.20 * ripples * arms + 0.14 * ripples) * weight;
    }

    // the particles
    let particles = ((cols * rows) as f64 / CELLS_PER_PARTICLE) as u64;
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
        // on an arm of the current: where its phase is 0, at the time the particle is born
        let arm = if dust {
            hash(i, 2) * TAU
        } else {
            (i % ARMS as u64) as f64 * TAU / ARMS
        };
        let birth = hash(i, 3);
        let spread = ((hash(i, 4) - 0.5) + (hash(i, 5) - 0.5)) * 0.18;
        let wobble = 1.0 + (hash(i, 6) as f32 - 0.5) * 0.08;
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
            // the fall speeds up towards the eye: about (1 - life)^0.8, without a power
            let left = (1.0 - life) as f32;
            let fall = left * (1.3 - 0.3 * left);
            let share = (EYE as f32 + (1.0 - EYE as f32) * fall) * wobble;
            // on its arm as the arm pours in (the turns so far taken out in f64, the rest is
            // precise enough in f32)
            let turned = (arm + POUR * t / ARMS + spread).rem_euclid(TAU) as f32;
            let angle = turned - share.ln() / PITCH as f32;
            let (sin, cos) = angle.sin_cos();
            let radius = rim * share as f64;
            let x = cx + radius * cos as f64;
            let y = cy + radius * sin as f64;
            if x < -1.0 || y < -2.0 || x > w + 1.0 || y > h + 2.0 {
                continue;
            }
            // the trail fades behind the particle, dust is fainter
            let weight = (1.0 - step as f32 / TRAIL as f32) * if dust { 0.35 } else { 0.6 };
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
    let cells = light(area.width, area.height, spin);
    let geometry = geometry(area.width, area.height);
    let full = LEVELS[LEVELS.len() - 1] as f64;
    // every color a cell can take, by its shade and its glow
    let glows = GLOWS as usize + 1;
    let table: Vec<Option<Color>> = (0..=SHADES as usize)
        .flat_map(|shade| {
            (0..glows).map(move |glow| colors.at(shade as f64 / SHADES, glow as f64 / GLOWS))
        })
        .collect();
    for row in 0..area.height {
        for col in 0..area.width {
            let light = cells[row as usize * area.width as usize + col as usize];
            let Some(level) = LEVELS.iter().rposition(|&level| light >= level) else {
                continue;
            };
            let (x, y) = (area.x + col, area.y + row);
            if holes.iter().any(|hole| contains(hole, x, y)) {
                continue;
            }
            let t = geometry[row as usize * area.width as usize + col as usize][5] as f64;
            let glow = (light as f64 / full).min(1.0);
            let shade = ((t.min(1.0) * SHADES).round() as usize).min(SHADES as usize);
            let color = table[shade * glows + (glow * GLOWS).round() as usize];
            let Some(cell) = surface.get_mut(x, y) else {
                continue;
            };
            cell.set_char(CHARS[level]);
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
        assert_eq!(light(120, 40, 3.25), light(120, 40, 3.25));
        assert_ne!(light(120, 40, 3.25), light(120, 40, 3.30));
    }

    #[test]
    fn it_fills_the_area_evenly() {
        // every quarter of the area is lit almost everywhere
        let (w, h) = (120usize, 40usize);
        let cells = light(w as u16, h as u16, 7.0);
        for (qx, qy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let count = (0..h / 2)
                .flat_map(|y| (0..w / 2).map(move |x| (x + qx * w / 2, y + qy * h / 2)))
                .filter(|&(x, y)| cells[y * w + x] >= LEVELS[0])
                .count();
            assert!(count > w * h / 4 * 9 / 10, "quarter {qx},{qy}: {count}");
        }
    }

    #[test]
    fn the_gradient_mixes_rgb_and_picks_named_colors() {
        let colors = Colors {
            gradient: vec![Color::Rgb(0, 0, 0), Color::Rgb(180, 180, 180)],
            background: None,
        };
        assert_eq!(colors.at(0.0, 1.0), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(colors.at(1.0, 1.0), Some(Color::Rgb(180, 180, 180)));
        // the eye's color holds through the middle: halfway out is nearer to it
        assert!(matches!(colors.at(0.5, 1.0), Some(Color::Rgb(r, _, _)) if r > 0 && r < 90));
        // dimmed into the background
        let dimmed = Colors {
            background: Some(Color::Rgb(0, 0, 0)),
            ..colors.clone()
        };
        assert!(matches!(dimmed.at(0.5, 0.0), Some(Color::Rgb(r, _, _)) if r < 40));
        let named = Colors {
            gradient: vec![Color::Red, Color::Blue, Color::Green],
            background: None,
        };
        assert_eq!(named.at(0.1, 1.0), Some(Color::Red));
        assert_eq!(named.at(0.5, 0.0), Some(Color::Blue));
        assert_eq!(named.at(1.0, 1.0), Some(Color::Green));
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
            gradient: vec![
                Color::Rgb(137, 180, 250),
                Color::Rgb(203, 166, 247),
                Color::Rgb(250, 179, 135),
            ],
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
                title: true,
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
