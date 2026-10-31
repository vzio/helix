//! The panel of `:practice`, checked on the screen of the test backend.

use helix_term::application::Application;
use helix_term::practice::{self, exercise::Kind, Exercise};
use helix_view::graphics::{Color, Modifier};

use super::*;

/// The exercises of the tests that don't look at the details: no gold, a lesson of two lines.
fn exercise(id: &str, start: &str, goal: &str, teach: &str) -> Exercise {
    let chapter = id.split('/').next().unwrap();
    Exercise {
        id: id.to_owned(),
        title: format!("Title of {id}"),
        chapter: chapter.to_owned(),
        kind: Kind::Learn,
        feature: "A feature".to_owned(),
        lesson: "The lesson of this exercise.\nA second line of the lesson.\n".to_owned(),
        before: Vec::new(),
        start: start.to_owned(),
        goal: goal.to_owned(),
        language: None,
        hints: ["first hint", "second hint", "third hint"]
            .map(str::to_owned)
            .to_vec(),
        teach: teach.to_owned(),
        gold: None,
        naive: teach.to_owned(),
        chapter_number: 1,
        chapter_title: format!("Chapter {chapter}"),
        number: 0,
        chapter_len: 2,
    }
}

fn small_exercises() -> Vec<Exercise> {
    let mut first = exercise(
        "a/01-first",
        "The #[q|]#uick quick brown fox\n",
        "The quick brown fox\n",
        "wd",
    );
    first.number = 1;
    let mut second = exercise("a/02-second", "foo\nbar\n", "foo\n", "jxd");
    second.number = 2;
    vec![first, second]
}

/// The exercise of the snapshots: chapter 3 of 7 exercises, a goal of 10 lines, a lesson of 3
/// lines with keys in it, and a gold solution.
fn fixture() -> Vec<Exercise> {
    let start = "\
fn main() {
    let a = 40;
    let b = 2;
    let label = \"answer\";
#[ |]#   let total = add(a, b)
    println!(\"{label}: {total}\");
}

fn add(x: i32, y: i32) -> i32 { x + y }
// the end
";
    let main = Exercise {
        title: "Append at the end of a line".to_owned(),
        lesson: "\
Typing at the end of a line is common, so Helix has `A`: it jumps to the end
of the line and starts inserting there. Without it you walk with `l`, or use
`gl` and then `a`. Press `<esc>` when you are done.
"
        .to_owned(),
        hints: [
            "Something is missing at the very end of a line.",
            "`A` starts inserting at the end of the line.",
            "`A`, type `;`, then `<esc>`.",
        ]
        .map(str::to_owned)
        .to_vec(),
        gold: Some("A;<esc>".to_owned()),
        naive: format!("{}a;<esc>", "l".repeat(24)),
        chapter_number: 3,
        chapter_title: "Words, lines and counts".to_owned(),
        number: 2,
        chapter_len: 7,
        ..exercise(
            "motions/02-end-of-line",
            start,
            &start
                .replace("#[ |]#", " ")
                .replace("add(a, b)", "add(a, b);"),
            "gla;<esc>",
        )
    };
    let mut next = exercise("motions/03-next", "x\n", "y\n", "ry");
    next.chapter_number = 3;
    next.number = 3;
    next.chapter_len = 7;
    vec![main, next]
}

/// An application with these exercises and a progress file in the returned directory.
fn setup(exercises: Vec<Exercise>) -> (tempfile::TempDir, Application) {
    let dir = tempfile::tempdir().unwrap();
    practice::reset_session();
    practice::set_progress_file(dir.path().join("practice.toml"));
    practice::set_exercises(exercises);
    (dir, AppBuilder::new().build().unwrap())
}

fn rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    (0..screen.area.height)
        .map(|y| {
            (0..screen.area.width)
                .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                .collect()
        })
        .collect()
}

fn on_screen(app: &Application, text: &str) -> bool {
    rows(app).iter().any(|row| row.contains(text))
}

/// The rows of the panel on a screen of `width` columns, from the top: the characters of its
/// area, up to the row with its bottom border.
fn panel_text(app: &Application, panel_width: usize) -> String {
    let rows = rows(app);
    let mut panel = Vec::new();
    for row in &rows {
        let columns: Vec<char> = row.chars().collect();
        let line: String = columns[columns.len() - panel_width..].iter().collect();
        let last = line.starts_with('└');
        panel.push(line);
        if last {
            return panel.join("\n");
        }
    }
    panic!("no bottom border in the panel: {rows:#?}");
}

/// The screen, with the editor beside the panel.
fn screen_text(app: &Application) -> String {
    rows(app)
        .iter()
        .map(|row| row.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prints the whole screen, to see the buffer beside the panel (with `--nocapture`).
fn show(name: &str, app: &Application) {
    println!("=== {name} ===\n{}\n", screen_text(app));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_shows_the_lesson_and_the_count() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(small_exercises());
    // wide: a column at the top right
    app.resize_screen(100, 30);
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<esc>"),
                Some(&|app| assert!(!on_screen(app, "Chapter a"))),
            ),
            (
                Some(":practice<ret>l"),
                Some(&|app| {
                    let rows = rows(app);
                    let title = rows
                        .iter()
                        .position(|row| row.contains("1.1 / 2 · Chapter a"))
                        .expect("the title");
                    assert_eq!(title, 0, "{rows:#?}");
                    assert!(rows[0].find("1.1").unwrap() >= 50, "{}", rows[0]);
                    for text in [
                        "Title of a/01-first",
                        "Lesson",
                        "The lesson of this exercise.",
                        "Goal",
                        "▎ The quick brown fox",
                        "Keys 1 · par 2",
                        ":practice hint gives a hint (0/3)",
                    ] {
                        assert!(
                            rows.iter().any(|row| row.contains(text)),
                            "{text} in {rows:#?}"
                        );
                    }
                    // the text of the buffer is still on the left
                    assert!(rows[0].contains("The quick quick brown fox"), "{}", rows[0]);
                }),
            ),
            // hidden and shown by :practice lesson
            (
                Some(":practice lesson<ret>"),
                Some(&|app| assert!(!on_screen(app, "Goal"))),
            ),
            (
                Some(":practice lesson<ret>"),
                Some(&|app| assert!(on_screen(app, "Goal"))),
            ),
            // the which-key popup wins
            (
                Some("<space>"),
                Some(&|app| {
                    assert!(!on_screen(app, "Goal"), "{:#?}", rows(app));
                    assert!(on_screen(app, "Space"), "{:#?}", rows(app));
                }),
            ),
            (Some("<esc>"), Some(&|app| assert!(on_screen(app, "Goal")))),
            // after the solution: the result, the solution and the way on
            (
                Some("hwd"),
                Some(&|app| {
                    for text in [
                        "Solved in 6 keys · par 2 · best 6",
                        "Teach  wd",
                        "Without the feature: 2 keys",
                        ":practice next",
                    ] {
                        assert!(on_screen(app, text), "{text} in {:#?}", rows(app));
                    }
                    assert!(!on_screen(app, "gives a hint"), "{:#?}", rows(app));
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_is_a_band_at_the_bottom_on_a_narrow_terminal() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(small_exercises());
    app.resize_screen(50, 20);
    test_key_sequences(
        &mut app,
        vec![(
            Some(":practice<ret>l"),
            Some(&|app| {
                let rows = rows(app);
                assert_eq!(rows.len(), 20);
                let title = rows
                    .iter()
                    .position(|row| row.contains("1.1 / 2 · Chapter a"))
                    .expect("the title");
                // below the lines of the buffer
                assert!(rows[0].contains("The quick quick brown fox"), "{rows:#?}");
                assert!(title > 1, "{rows:#?}");
                for text in ["Title of a/01-first", "Goal", "▎ The quick brown fox"] {
                    assert!(on_screen(app, text), "{text} in {rows:#?}");
                }
                let keys = rows
                    .iter()
                    .position(|row| row.contains("Keys 1 · par 2"))
                    .expect("the count");
                assert!(keys < 17, "{rows:#?}");
                let bottom = rows.iter().position(|row| row.starts_with('└')).unwrap();
                assert_eq!(bottom, 17, "{rows:#?}");
                assert!(rows[18].contains("NOR"), "{rows:#?}");
                // full width
                assert!(
                    rows[title].starts_with('┌') && rows[title].trim_end().ends_with('┐'),
                    "{}",
                    rows[title]
                );
            }),
        )],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_clips_on_tiny_terminals() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(small_exercises());
    app.resize_screen(20, 6);
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>l"),
                Some(&|app| assert_eq!(rows(app).len(), 6)),
            ),
            (
                Some(":practice hint<ret>:practice hint<ret>:practice hint<ret>"),
                None,
            ),
        ],
        false,
    )
    .await?;

    for (width, height) in [(8, 5), (30, 4), (60, 3)] {
        let (_dir, mut app) = setup(small_exercises());
        app.resize_screen(width, height);
        test_key_sequence(&mut app, Some(":practice<ret>l"), None, false).await?;
    }
    Ok(())
}

/// The panel with the most content on terminals of all sizes: it draws, clipped to the screen.
#[tokio::test(flavor = "multi_thread")]
async fn the_panel_stays_inside_the_screen() -> anyhow::Result<()> {
    for (width, height) in [(50, 20), (100, 12), (99, 30), (160, 45), (160, 8)] {
        let (_dir, mut app) = setup(fixture());
        app.resize_screen(width, height);
        test_key_sequence(
            &mut app,
            Some(":practice<ret>:practice hint<ret>:practice hint<ret>:practice hint<ret>"),
            Some(&|app| {
                let rows = rows(app);
                assert_eq!(rows.len(), usize::from(app.screen().area.height));
                // the count is the last to go
                assert!(on_screen(app, "Keys 0 · par 5"), "{rows:#?}");
                assert!(rows.iter().any(|row| row.contains('┌')), "{rows:#?}");
                assert!(rows.iter().any(|row| row.contains('└')), "{rows:#?}");
            }),
            false,
        )
        .await?;
    }
    Ok(())
}

// The panel as it is drawn on a wide terminal. The whole screen is printed (see `--nocapture`).

const WIDTH: u16 = 160;
const PANEL: usize = 56;

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_of_the_fresh_panel() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(fixture());
    app.resize_screen(WIDTH, 45);
    test_key_sequence(
        &mut app,
        Some(":practice<ret>"),
        Some(&|app| {
            show("fresh", app);
            assert_eq!(
                panel_text(app, PANEL),
                "\
┌3.2 / 7 · Words, lines and counts─────────────────────┐
│                                                      │
│ Append at the end of a line                          │
│                                                      │
│ Lesson                                               │
│ Typing at the end of a line is common, so Helix has  │
│ A: it jumps to the end of the line and starts        │
│ inserting there. Without it you walk with l, or use  │
│ gl and then a. Press <esc> when you are done.        │
│                                                      │
│ Goal                                                 │
│ ▎ fn main() {                                        │
│ ▎     let a = 40;                                    │
│ ▎     let b = 2;                                     │
│ ▎     let label = \"answer\";                          │
│ ▎     let total = add(a, b);                         │
│ ▎     println!(\"{label}: {total}\");                  │
│ ▎ }                                                  │
│ ▎                                                    │
│ ▎ fn add(x: i32, y: i32) -> i32 { x + y }            │
│ ▎ // the end                                         │
│                                                      │
│ Keys 0 · par 5                                       │
│ :practice hint gives a hint (0/3)                    │
│                                                      │
└──────────────────────────────────────────────────────┘"
            );
        }),
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_with_two_hints() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(fixture());
    app.resize_screen(WIDTH, 45);
    test_key_sequence(
        &mut app,
        Some(":practice<ret>:practice hint<ret>:practice hint<ret>"),
        Some(&|app| {
            show("two hints", app);
            assert_eq!(
                panel_text(app, PANEL),
                "\
┌3.2 / 7 · Words, lines and counts─────────────────────┐
│                                                      │
│ Append at the end of a line                          │
│                                                      │
│ Lesson                                               │
│ Typing at the end of a line is common, so Helix has  │
│ A: it jumps to the end of the line and starts        │
│ inserting there. Without it you walk with l, or use  │
│ gl and then a. Press <esc> when you are done.        │
│                                                      │
│ Goal                                                 │
│ ▎ fn main() {                                        │
│ ▎     let a = 40;                                    │
│ ▎     let b = 2;                                     │
│ ▎     let label = \"answer\";                          │
│ ▎     let total = add(a, b);                         │
│ ▎     println!(\"{label}: {total}\");                  │
│ ▎ }                                                  │
│ ▎                                                    │
│ ▎ fn add(x: i32, y: i32) -> i32 { x + y }            │
│ ▎ // the end                                         │
│                                                      │
│ Hints 2/3                                            │
│ 1 Something is missing at the very end of a line.    │
│ 2 A starts inserting at the end of the line.         │
│                                                      │
│ Keys 0 · par 5                                       │
│ :practice hint gives a hint (2/3)                    │
│                                                      │
└──────────────────────────────────────────────────────┘"
            );
        }),
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshot_of_the_solved_panel() -> anyhow::Result<()> {
    let (_dir, mut app) = setup(fixture());
    app.resize_screen(WIDTH, 45);
    test_key_sequence(
        &mut app,
        Some(":practice<ret>gla;<esc>"),
        Some(&|app| {
            show("solved", app);
            assert_eq!(
                panel_text(app, PANEL),
                "\
┌3.2 / 7 · Words, lines and counts─────────────────────┐
│                                                      │
│ Append at the end of a line                          │
│                                                      │
│ Lesson                                               │
│ Typing at the end of a line is common, so Helix has  │
│ A: it jumps to the end of the line and starts        │
│ inserting there. Without it you walk with l, or use  │
│ gl and then a. Press <esc> when you are done.        │
│                                                      │
│ Goal                                                 │
│ ▎ fn main() {                                        │
│ ▎     let a = 40;                                    │
│ ▎     let b = 2;                                     │
│ ▎     let label = \"answer\";                          │
│ ▎     let total = add(a, b);                         │
│ ▎     println!(\"{label}: {total}\");                  │
│ ▎ }                                                  │
│ ▎                                                    │
│ ▎ fn add(x: i32, y: i32) -> i32 { x + y }            │
│ ▎ // the end                                         │
│                                                      │
│ Solved in 5 keys · par 5 · best 5                    │
│                                                      │
│ Teach  gla;<esc>                                     │
│ Gold   A;<esc>                                       │
│ Without the feature: 27 keys                         │
│                                                      │
│ :practice next                                       │
│                                                      │
└──────────────────────────────────────────────────────┘"
            );
        }),
        false,
    )
    .await
}

// The styles of the panel, for a theme that styles it and for one that doesn't.

const GREEN: Color = Color::Rgb(0, 0xff, 0);
const RED: Color = Color::Rgb(0xff, 0, 0);
const GRAY: Color = Color::Rgb(0x88, 0x88, 0x88);
const BLOCK: Color = Color::Rgb(0x11, 0x22, 0x33);
const POPUP: Color = Color::Rgb(0x20, 0x20, 0x20);

fn theme(scopes: &str) -> helix_view::Theme {
    helix_view::Theme::from(
        toml::from_str::<toml::Value>(&format!(
            "\"ui.selection\" = {{ bg = \"#333333\" }}\n\"ui.popup\" = {{ bg = \"#202020\" }}\n{scopes}"
        ))
        .unwrap(),
    )
}

/// The cell of the screen where `text` starts, in the part of the screen from column 100 on,
/// where the panel is.
fn cell_of(app: &Application, text: &str) -> tui::buffer::Cell {
    let screen = app.screen();
    for (y, row) in rows(app).iter().enumerate() {
        let columns: Vec<char> = row.chars().collect();
        let right: String = columns[100..].iter().collect();
        if let Some(at) = right.find(text) {
            let x = 100 + right[..at].chars().count();
            return screen.get(x as u16, y as u16).unwrap().clone();
        }
    }
    panic!("{text:?} not in {:#?}", rows(app));
}

async fn styled_panel(scopes: &str, check: &dyn Fn(&Application)) -> anyhow::Result<()> {
    let (_dir, mut app) = setup(fixture());
    app.editor.theme = theme(scopes);
    app.resize_screen(WIDTH, 45);
    test_key_sequence(&mut app, Some(":practice<ret>"), Some(check), false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_goal_uses_the_style_of_its_scope() -> anyhow::Result<()> {
    styled_panel(
        r##"
"ui.practice.goal" = { fg = "#00ff00", bg = "#112233" }
"markup.raw.block" = { fg = "#ff0000" }
"ui.practice.dim" = { fg = "#888888" }
"##,
        &|app| {
            let goal = cell_of(app, "let total = add(a, b);");
            assert_eq!((goal.fg, goal.bg), (GREEN, BLOCK));
            // the whole row is the block, up to the margin
            let row = rows(app)
                .iter()
                .position(|row| row.contains("▎     let total"))
                .unwrap();
            let screen = app.screen();
            let end = screen.get(WIDTH - 2 - 1, row as u16).unwrap();
            assert_eq!((end.symbol.as_str(), end.bg), (" ", BLOCK));
            // the bar is dim on the block
            let bar = cell_of(app, "▎     let total");
            assert_eq!((bar.fg, bar.bg), (GRAY, BLOCK));
            // the rest of the panel is not
            let label = cell_of(app, "Goal");
            assert_eq!(label.bg, POPUP);
            assert!(label.modifier.contains(Modifier::BOLD));
            assert_ne!(cell_of(app, "Typing at the end").bg, BLOCK);
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_goal_falls_back_to_the_markup_scopes() -> anyhow::Result<()> {
    styled_panel(
        r##"
"markup.raw" = { fg = "#00ff00" }
"markup.raw.block" = { fg = "#ff0000" }
"##,
        &|app| {
            let goal = cell_of(app, "let total = add(a, b);");
            assert_eq!((goal.fg, goal.bg), (RED, POPUP));
        },
    )
    .await?;
    styled_panel(
        r##"
"markup.raw" = { fg = "#00ff00" }
"##,
        &|app| {
            let goal = cell_of(app, "let total = add(a, b);");
            assert_eq!((goal.fg, goal.bg), (GREEN, POPUP));
            // keys in the lesson have the style of inline code, which falls back to the same
            let key = cell_of(app, "A: it jumps");
            assert_eq!(key.fg, GREEN);
            assert_eq!(cell_of(app, "Typing").fg, Color::Reset);
        },
    )
    .await?;
    // nothing for the goal in the theme: the text style, on the panel
    styled_panel(
        r##"
"ui.text" = { fg = "#aaaaaa" }
"##,
        &|app| {
            let goal = cell_of(app, "let total = add(a, b);");
            assert_eq!((goal.fg, goal.bg), (Color::Rgb(0xaa, 0xaa, 0xaa), POPUP));
            // keys still stand out
            assert!(cell_of(app, "A: it jumps")
                .modifier
                .contains(Modifier::BOLD));
        },
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_scope_of_the_panel_is_its_background() -> anyhow::Result<()> {
    styled_panel(
        r##"
"ui.practice" = { bg = "#445566" }
"##,
        &|app| {
            let cell = cell_of(app, "Typing");
            assert_eq!(cell.bg, Color::Rgb(0x44, 0x55, 0x66));
            assert_eq!(cell_of(app, "Goal").bg, Color::Rgb(0x44, 0x55, 0x66));
            assert_eq!(cell_of(app, "Keys 0").bg, Color::Rgb(0x44, 0x55, 0x66));
            // the border too
            assert_eq!(cell_of(app, "└").bg, Color::Rgb(0x44, 0x55, 0x66));
        },
    )
    .await
}
