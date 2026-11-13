//! WCAG contrast checks of a theme's colors: the matrix theme must meet them, the vortex theme
//! is only reported (`cargo test -p vortex-view contrast -- --nocapture`).

use super::*;

fn linear(channel: u8) -> f64 {
    let c = channel as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(color: Color) -> f64 {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        // the palette name `black` is the terminal's black, taken as pure black here
        Color::Black => (0, 0, 0),
        color => panic!("expected an rgb color, found {color:?}"),
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn ratio(a: Color, b: Color) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The foreground of a scope, or the text's when it has none.
fn fg(theme: &Theme, scope: &str) -> Color {
    theme
        .get(scope)
        .fg
        .or(theme.get("ui.text").fg)
        .unwrap_or_else(|| panic!("no foreground for {scope}"))
}

/// The background of a scope, or the editor's when it has none.
fn bg(theme: &Theme, scope: &str) -> Color {
    theme
        .get(scope)
        .bg
        .or(theme.get("ui.background").bg)
        .unwrap_or_else(|| panic!("no background for {scope}"))
}

/// Which color of a scope a row takes.
#[derive(Clone, Copy)]
enum Part {
    Fg,
    Bg,
}
use Part::{Bg, Fg};

/// (description, first scope and part, second scope and part, minimum ratio)
type Row = (
    &'static str,
    (&'static str, Part),
    (&'static str, Part),
    f64,
);

const ROWS: &[Row] = &[
    (
        "text on background",
        ("ui.text", Fg),
        ("ui.background", Bg),
        7.0,
    ),
    (
        "comment on background",
        ("comment", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "inactive text on background",
        ("ui.text.inactive", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "text on cursorline",
        ("ui.text", Fg),
        ("ui.cursorline.primary", Bg),
        4.5,
    ),
    (
        "text on selection",
        ("ui.text", Fg),
        ("ui.selection", Bg),
        4.5,
    ),
    (
        "text on primary selection",
        ("ui.text", Fg),
        ("ui.selection.primary", Bg),
        4.5,
    ),
    (
        "string on its background",
        ("string", Fg),
        ("string", Bg),
        4.5,
    ),
    (
        "keyword on cursorline",
        ("keyword", Fg),
        ("ui.cursorline.primary", Bg),
        4.5,
    ),
    (
        "comment on cursorline",
        ("comment", Fg),
        ("ui.cursorline.primary", Bg),
        4.5,
    ),
    (
        "diff plus on background",
        ("diff.plus", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "diff minus on background",
        ("diff.minus", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "error on background",
        ("error", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "warning on background",
        ("warning", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "info on background",
        ("info", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "hint on background",
        ("hint", Fg),
        ("ui.background", Bg),
        4.5,
    ),
    (
        "selection vs background",
        ("ui.selection", Bg),
        ("ui.background", Bg),
        1.3,
    ),
    (
        "cursor vs background",
        ("ui.cursor.primary", Bg),
        ("ui.background", Bg),
        3.0,
    ),
    (
        "cursor text on cursor",
        ("ui.cursor.primary", Fg),
        ("ui.cursor.primary", Bg),
        4.5,
    ),
];

fn color_of(theme: &Theme, (scope, part): (&str, Part)) -> Color {
    match part {
        Fg => fg(theme, scope),
        Bg => bg(theme, scope),
    }
}

/// Each row of [`ROWS`] on `theme`: its description, the ratio, the minimum, and whether the
/// ratio reaches it.
fn report(theme: &Theme) -> Vec<(String, f64, f64, bool)> {
    ROWS.iter()
        .map(|&(description, a, b, minimum)| {
            let ratio = ratio(color_of(theme, a), color_of(theme, b));
            (description.to_string(), ratio, minimum, ratio >= minimum)
        })
        .collect()
}

fn print_report(theme: &Theme) {
    println!("contrast of the {} theme", theme.name());
    println!("{:<32} {:>7} {:>5}  result", "pair", "ratio", "min");
    for (description, ratio, minimum, pass) in report(theme) {
        let result = if pass { "PASS" } else { "SHORT" };
        println!("{description:<32} {ratio:>7.2} {minimum:>5.1}  {result}");
    }
}

#[test]
fn the_wcag_formula_matches_known_values() {
    let black = Color::Rgb(0, 0, 0);
    let white = Color::Rgb(255, 255, 255);
    assert!((ratio(black, white) - 21.0).abs() < 1e-9);
    assert!((ratio(white, white) - 1.0).abs() < 1e-9);
    // #777777 on white is the well-known 4.48
    assert!((ratio(Color::Rgb(0x77, 0x77, 0x77), white) - 4.48).abs() < 0.01);
}

#[test]
fn contrast_of_the_matrix_theme_meets_its_minimums() {
    let theme = Loader::new(&[]).load(MATRIX_THEME_NAME).unwrap();
    print_report(&theme);
    let short: Vec<_> = report(&theme)
        .into_iter()
        .filter(|(_, _, _, pass)| !pass)
        .map(|(description, ratio, minimum, _)| format!("{description}: {ratio:.2} < {minimum}"))
        .collect();
    assert!(short.is_empty(), "too little contrast: {short:#?}");
}

/// Reports, never asserts: the vortex theme is not to be changed.
#[test]
fn contrast_of_the_vortex_theme_is_reported() {
    let theme = Loader::new(&[]).load(DEFAULT_THEME_NAME).unwrap();
    print_report(&theme);
    assert_eq!(report(&theme).len(), ROWS.len());
}
