use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use helix_term::application::Application;
use helix_term::practice::{self, Exercise};
use helix_view::input::parse_macro;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[cfg(windows)]
use crossterm::event::{Event, KeyEvent};
#[cfg(not(windows))]
use termina::event::{Event, KeyEvent};

use super::*;

/// Replays every solution of every exercise in the runtime directory, as keys typed in an
/// application, and checks the data and the rules of the exercise format:
///
/// - `teach`, `gold` and `naive` each solve the exercise in exactly as many keys as they have,
/// - `naive` has at least twice as many keys as `teach` (the par), and so has retyping the goal
///   (`%c`, its characters, `<esc>`: the characters of the goal and 3),
/// - the commands recorded while `teach` and `naive` run are all keys of the chapter of the
///   exercise or of a chapter before it,
/// - every key a chapter lists is recorded in the `teach` or `gold` of one of its exercises,
/// - `CURRICULUM.md` agrees with `index.toml` (see `practice::validate`).
///
/// All failures are collected and reported together. A table of the numbers of keys is printed:
/// run it with `--nocapture` to see it.
///
/// For authoring, two environment variables:
///
/// - `PRACTICE_DIR=<dir>` loads the exercises, `index.toml` and `CURRICULUM.md` from this
///   directory instead of the runtime one,
/// - `PRACTICE_FILTER=<substring>` replays only the exercises whose id contains it. The static
///   checks still run on everything, and the check that every key of a chapter is used is skipped.
#[tokio::test(flavor = "multi_thread")]
async fn the_solutions_of_all_exercises_solve_them_in_exactly_their_keys() -> anyhow::Result<()> {
    let dir = match std::env::var_os("PRACTICE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => practice::exercise::exercises_dir()?,
    };
    let filter = std::env::var("PRACTICE_FILTER")
        .ok()
        .filter(|f| !f.is_empty());
    let chapters = practice::exercise::load_chapters(&dir)?;
    let exercises = practice::load_exercises_from(&dir)?;
    assert!(!exercises.is_empty());

    let mut failures = Vec::new();

    failures.extend(practice::validate(
        &exercises,
        &dir,
        &test_syntax_loader(None),
    ));

    // the commands recorded for the teach and gold solutions of each chapter
    let mut used: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();

    println!(
        "{:<44} {:<9} {:>3} {:>4} {:>5} {:>6} {:>5}",
        "id", "kind", "par", "gold", "naive", "retype", "n/par"
    );
    let progress_dir = tempfile::tempdir()?;
    for exercise in &exercises {
        if filter
            .as_ref()
            .is_some_and(|filter| !exercise.id.contains(filter))
        {
            continue;
        }
        // (the shell commands of the shell chapter need a POSIX shell)
        if cfg!(windows) && exercise.chapter == "shell" {
            continue;
        }
        let par = exercise.par();
        let naive = exercise.naive_keys();
        let retype = exercise.goal.chars().count() + 3;
        println!(
            "{:<44} {:<9} {:>3} {:>4} {:>5} {:>6} {:>5.1}",
            exercise.id,
            exercise.kind.name(),
            par,
            exercise
                .gold_par()
                .map_or_else(|| "-".to_owned(), |gold| gold.to_string()),
            naive,
            retype,
            naive as f64 / par.max(1) as f64
        );
        if naive < 2 * par {
            failures.push(format!(
                "{}: naive has {naive} keys, at least 2 x par ({par}) = {}",
                exercise.id,
                2 * par
            ));
        }
        if retype < 2 * par {
            failures.push(format!(
                "{}: retyping the goal takes {retype} keys, at least 2 x par ({par}) = {}",
                exercise.id,
                2 * par
            ));
        }

        // the keys of the chapter and of the ones before it
        let allowed: BTreeSet<&str> = chapters
            .iter()
            .take(exercise.chapter_number)
            .flat_map(|chapter| chapter.keys.iter().map(String::as_str))
            .collect();
        for (name, keys) in [
            ("teach", Some(&exercise.teach)),
            ("gold", exercise.gold.as_ref()),
            ("naive", Some(&exercise.naive)),
        ] {
            let Some(keys) = keys else { continue };
            let progress = progress_dir.path().join("p.toml");
            let commands = match replay(exercise, keys, progress).await {
                Ok(commands) => commands,
                Err(failure) => {
                    failures.push(format!("{} [{name}: {keys}]: {failure}", exercise.id));
                    continue;
                }
            };
            if name != "gold" {
                let unknown: BTreeSet<&str> = commands
                    .iter()
                    .map(String::as_str)
                    .filter(|command| !allowed.contains(command))
                    .collect();
                if !unknown.is_empty() {
                    failures.push(format!(
                        "{} [{name}: {keys}]: uses keys that are not taught yet: {unknown:?} (recorded {commands:?})",
                        exercise.id
                    ));
                }
            }
            if name != "naive" {
                used.entry(exercise.chapter.as_str())
                    .or_default()
                    .extend(commands);
            }
        }
    }

    if filter.is_none() {
        for chapter in &chapters {
            if cfg!(windows) && chapter.dir == "shell" {
                continue;
            }
            let recorded = used.get(chapter.dir.as_str());
            for key in &chapter.keys {
                if !recorded.is_some_and(|recorded| recorded.contains(key)) {
                    failures.push(format!(
                        "chapter {}: the key `{key}` is not recorded in the teach or gold of any of its exercises",
                        chapter.dir
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
    Ok(())
}

/// Types `keys` into a fresh application that has just started `exercise`, and returns the
/// commands that were recorded.
async fn replay(exercise: &Exercise, keys: &str, progress: PathBuf) -> Result<Vec<String>, String> {
    let _ = std::fs::remove_file(&progress);
    practice::reset_session();
    practice::set_progress_file(progress);
    practice::set_exercises(vec![exercise.clone()]);

    let mut app = AppBuilder::new().build().map_err(|e| e.to_string())?;
    practice::start_exercise(&mut app.editor, &exercise.id).map_err(|e| e.to_string())?;

    let expected = parse_macro(keys).map_err(|e| e.to_string())?.len();
    let outcome = RefCell::new(Ok(Vec::new()));
    test_key_sequence(
        &mut app,
        Some(keys),
        Some(&|app| {
            let attempt = practice::attempt().expect("the attempt is running");
            let text = helix_view::doc!(app.editor).text().to_string();
            *outcome.borrow_mut() = match attempt.solved_at {
                None => Err(format!(
                    "not solved after {} keys, the text is {text:?}",
                    attempt.keys
                )),
                Some(at) if at != expected => Err(format!(
                    "solved after {at} keys, but the solution has {expected}, the text is {text:?}"
                )),
                Some(_) if text != exercise.goal => {
                    Err(format!("solved, but the text is {text:?}"))
                }
                Some(_) => Ok(attempt.commands),
            };
        }),
        false,
    )
    .await
    .map_err(|e| e.to_string())?;
    outcome.into_inner()
}

// The tests of the runner use small exercises of their own, not the ones of the runtime directory.

fn exercise(id: &str, start: &str, goal: &str, teach: &str) -> Exercise {
    let chapter = id.split('/').next().unwrap();
    Exercise {
        id: id.to_owned(),
        title: format!("Title of {id}"),
        chapter: chapter.to_owned(),
        kind: practice::exercise::Kind::Learn,
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
        // (set by `exercises`)
        chapter_number: 0,
        chapter_title: format!("Chapter {chapter}"),
        number: 0,
        chapter_len: 0,
    }
}

/// Numbers the exercises like the loader does: the chapters in order of first appearance.
fn numbered(mut exercises: Vec<Exercise>) -> Vec<Exercise> {
    let mut chapters: Vec<String> = Vec::new();
    for exercise in &exercises {
        if !chapters.contains(&exercise.chapter) {
            chapters.push(exercise.chapter.clone());
        }
    }
    for (n, chapter) in chapters.iter().enumerate() {
        let len = exercises.iter().filter(|e| &e.chapter == chapter).count();
        for (i, exercise) in exercises
            .iter_mut()
            .filter(|e| &e.chapter == chapter)
            .enumerate()
        {
            exercise.chapter_number = n + 1;
            exercise.number = i + 1;
            exercise.chapter_len = len;
        }
    }
    exercises
}

fn exercises() -> Vec<Exercise> {
    numbered(vec![
        // 2 keys
        exercise(
            "a/01-first",
            "The #[q|]#uick quick brown fox\n",
            "The quick brown fox\n",
            "wd",
        ),
        // 3 keys
        exercise("a/02-second", "foo\nbar\n", "foo\n", "jxd"),
        // 3 keys, types text
        exercise("b/01-third", "x\n", "xy\n", "ay<esc>"),
    ])
}

/// An application with the exercises above and a progress file in the returned directory.
fn setup() -> (tempfile::TempDir, Application) {
    let dir = tempfile::tempdir().unwrap();
    practice::reset_session();
    practice::set_progress_file(dir.path().join("practice.toml"));
    practice::set_exercises(exercises());
    (dir, AppBuilder::new().build().unwrap())
}

fn progress(dir: &tempfile::TempDir) -> String {
    std::fs::read_to_string(dir.path().join("practice.toml")).unwrap_or_default()
}

fn keys() -> usize {
    practice::attempt().expect("an attempt is running").keys
}

fn exercise_index() -> usize {
    practice::attempt().expect("an attempt is running").exercise
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(message, _)| message.to_string())
        .unwrap_or_default()
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

#[tokio::test(flavor = "multi_thread")]
async fn every_key_counts() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>"),
                Some(&|_| {
                    assert_eq!(exercise_index(), 0);
                    assert_eq!(keys(), 0);
                }),
            ),
            // Esc, a count and a motion (2 keys), an edit and its undo, after a motion
            (
                Some("l<esc>3ldu"),
                Some(&|app| {
                    assert_eq!(keys(), 6, "{}", status(app));
                }),
            ),
            // keys typed into a prompt count too: `s`, the regex, <ret>, then insert mode
            (
                Some("%sq<ret>cz<esc>"),
                Some(&|_| assert_eq!(keys(), 6 + 7)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn practice_commands_do_not_count_other_commands_do() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (Some(":practice<ret>ll"), Some(&|_| assert_eq!(keys(), 2))),
            (
                Some(":practice hint<ret>"),
                Some(&|app| {
                    assert_eq!(keys(), 2);
                    assert_eq!(status(app), "Hint 1/3: first hint");
                }),
            ),
            // also when the command line was left, started again and left
            (
                Some(":practice h<esc>:practice lesson<ret>:practice lesson<ret>"),
                // `:practice h<esc>` was typed and left: 12 keys
                Some(&|_| assert_eq!(keys(), 2 + 12)),
            ),
            // another command does count: `:`, `sort`, <ret>
            (Some(":sort<ret>"), Some(&|_| assert_eq!(keys(), 14 + 6))),
            // reset starts again at 0
            (
                Some(":practice reset<ret>"),
                Some(&|_| assert_eq!(keys(), 0)),
            ),
            (Some("l"), Some(&|_| assert_eq!(keys(), 1))),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_practice_command_from_a_key_binding_does_not_count() -> anyhow::Result<()> {
    let (_dir, _) = setup();
    let mut config = test_config();
    config.keys = toml::from_str("[normal]\nH = \":practice hint\"")?;
    let mut app = AppBuilder::new().with_config(config).build()?;
    test_key_sequences(
        &mut app,
        vec![
            (Some(":practice<ret>l"), Some(&|_| assert_eq!(keys(), 1))),
            (
                Some("H"),
                Some(&|app| {
                    assert_eq!(keys(), 1);
                    assert_eq!(status(app), "Hint 1/3: first hint");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn solving_freezes_the_count_and_saves_the_best() -> anyhow::Result<()> {
    let (dir, mut app) = setup();
    let dir = &dir;
    test_key_sequences(
        &mut app,
        vec![
            // 4 keys: not at par, a first solution is the new best
            (
                Some(":practice<ret>lhwd"),
                Some(&|app| {
                    let attempt = practice::attempt().unwrap();
                    assert_eq!(attempt.solved_at, Some(4));
                    assert_eq!(status(app), "Solved in 4 keys · par 2 · new best");
                    assert_eq!(progress(dir), "[exercises.\"a/01-first\"]\nbest = 4\n");
                }),
            ),
            // more keys change nothing
            (
                Some("jkjk"),
                Some(&|_| {
                    assert_eq!(keys(), 4);
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(4));
                }),
            ),
            // a worse attempt keeps the best
            (
                Some(":practice reset<ret>llhhwd"),
                Some(&|app| {
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(6));
                    assert_eq!(status(app), "Solved in 6 keys · par 2 · best 4");
                    assert!(progress(dir).contains("best = 4"));
                }),
            ),
            // a better one replaces it
            (
                Some(":practice reset<ret>wd"),
                Some(&|app| {
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(2));
                    assert_eq!(status(app), "Solved in 2 keys · par 2 · new best");
                    assert_eq!(progress(dir), "[exercises.\"a/01-first\"]\nbest = 2\n");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_solution_that_types_text_is_solved_in_normal_mode() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice b<ret>ay"),
                Some(&|app| {
                    // the text is right, but still in insert mode
                    assert_eq!(helix_view::doc!(app.editor).text().to_string(), "xy\n");
                    assert_eq!(practice::attempt().unwrap().solved_at, None);
                }),
            ),
            (
                Some("<esc>"),
                Some(&|_| {
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(3));
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn practice_resumes_at_the_first_unsolved_exercise() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 0)),
            ),
            // the same exercise, still unsolved: the count is kept
            (
                Some("ll:practice<ret>"),
                Some(&|_| {
                    assert_eq!(exercise_index(), 0);
                    assert_eq!(keys(), 2);
                }),
            ),
            (
                Some("hhwd:practice<ret>"),
                Some(&|_| {
                    assert_eq!(exercise_index(), 1);
                    assert_eq!(keys(), 0);
                }),
            ),
            // a chapter: its first unsolved exercise
            (
                Some(":practice b<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 2)),
            ),
            (
                Some(":practice a<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 1)),
            ),
            (
                Some(":practice nonesuch<ret>"),
                Some(&|app| {
                    assert_eq!(exercise_index(), 1);
                    assert_eq!(
                        status(app),
                        "'practice': unknown chapter `nonesuch`, the chapters are: a, b"
                    );
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn next_and_prev_walk_the_global_order() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice prev<ret>"),
                Some(&|app| {
                    assert!(practice::attempt().is_none());
                    assert!(
                        status(app).contains("no exercise is open"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some(":practice<ret>:practice prev<ret>"),
                Some(&|app| {
                    assert_eq!(exercise_index(), 0);
                    assert!(
                        status(app).contains("this is the first exercise"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some(":practice next<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 1)),
            ),
            (
                Some(":practice next<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 2)),
            ),
            (
                Some(":practice next<ret>"),
                Some(&|app| {
                    assert_eq!(exercise_index(), 2);
                    assert!(
                        status(app).contains("this is the last exercise"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some(":practice prev<ret>"),
                Some(&|_| assert_eq!(exercise_index(), 1)),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn hints_are_revealed_one_by_one_and_reset_needs_an_exercise() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice reset<ret>"),
                Some(&|app| {
                    assert!(
                        status(app).contains("no exercise is open"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some(":practice hint<ret>"),
                Some(&|app| {
                    assert!(
                        status(app).contains("no exercise is open"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some(":practice<ret>:practice hint<ret>"),
                Some(&|app| {
                    assert_eq!(status(app), "Hint 1/3: first hint");
                    assert!(on_screen(app, "Hint 1: first hint"));
                    assert!(!on_screen(app, "Hint 2"));
                }),
            ),
            (
                Some(":practice hint<ret>:practice hint<ret>"),
                Some(&|app| {
                    assert_eq!(status(app), "Hint 3/3: third hint");
                    assert!(on_screen(app, "Hint 2: second hint"));
                    assert_eq!(keys(), 0);
                }),
            ),
            (
                Some(":practice hint<ret>"),
                Some(&|app| assert_eq!(status(app), "No more hints")),
            ),
            // the hints stay after a reset of the same exercise
            (
                Some(":practice reset<ret>"),
                Some(&|app| {
                    assert!(on_screen(app, "Hint 3: third hint"));
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_replaces_the_buffer_and_closes_the_one_before() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    let first = RefCell::new(None);
    let first = &first;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>"),
                Some(&|app| {
                    *first.borrow_mut() = Some(practice::attempt().unwrap().doc);
                    assert_eq!(
                        helix_view::doc!(app.editor).text().to_string(),
                        "The quick quick brown fox\n"
                    );
                    let sel = helix_view::doc!(app.editor)
                        .selection(helix_view::view!(app.editor).id)
                        .primary();
                    assert_eq!((sel.from(), sel.to()), (4, 5));
                }),
            ),
            (
                Some("x:practice next<ret>"),
                Some(&|app| {
                    let attempt = practice::attempt().unwrap();
                    assert_ne!(Some(attempt.doc), *first.borrow());
                    assert!(!app.editor.documents.contains_key(&first.borrow().unwrap()));
                    assert_eq!(
                        helix_view::doc!(app.editor).text().to_string(),
                        "foo\nbar\n"
                    );
                    assert_eq!(app.editor.documents.len(), 1);
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_the_buffer_ends_the_session() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            (Some(":practice<ret>l"), Some(&|_| assert_eq!(keys(), 1))),
            (
                Some(":bc<ret>"),
                Some(&|app| {
                    assert!(practice::attempt().is_none(), "{}", status(app));
                    assert!(!on_screen(app, "practice ·"));
                }),
            ),
            (
                Some("lll"),
                Some(&|_| assert!(practice::attempt().is_none())),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_buffer_never_blocks_quitting() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    // edited, and undone, and edited again
    test_key_sequence(
        &mut app,
        Some(":practice<ret>xdu<esc>wd:q<ret>"),
        None,
        true,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_picks_an_exercise_and_its_keys_do_not_count() -> anyhow::Result<()> {
    let (dir, mut app) = setup();
    let dir = &dir;
    test_key_sequences(
        &mut app,
        vec![
            (Some(":practice<ret>ll"), Some(&|_| assert_eq!(keys(), 2))),
            (
                Some(":practice list<ret>"),
                Some(&|app| {
                    assert_eq!(keys(), 2);
                    assert!(on_screen(app, "Title of a/01-first"), "{:#?}", rows(app));
                    assert!(on_screen(app, "Title of b/01-third"));
                    assert!(on_screen(app, "·"));
                }),
            ),
            // moving, filtering and leaving the picker
            (
                Some("jk<esc>"),
                Some(&|app| {
                    assert_eq!(keys(), 2);
                    assert!(!on_screen(app, "Title of b/01-third"));
                }),
            ),
            // choosing starts the exercise at 0
            (Some(":practice list<ret>"), None),
            (
                Some("<C-n><C-n><ret>"),
                Some(&|_| {
                    assert_eq!(exercise_index(), 2);
                    assert_eq!(keys(), 0);
                }),
            ),
        ],
        false,
    )
    .await?;
    assert_eq!(progress(dir), "");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_shows_the_status_best_and_par() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![
            // solved over par (4 > 2), and the second at par (3)
            (Some(":practice<ret>lhwd:practice<ret>jxd"), None),
            (
                Some(":practice list<ret>"),
                Some(&|app| {
                    let rows = rows(app);
                    let row = |title: &str| {
                        rows.iter()
                            .find(|row| row.contains(title))
                            .unwrap_or_else(|| panic!("{title} in {rows:#?}"))
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    };
                    assert!(
                        row("Title of a/01-first")
                            .contains("✓ 2 4 1.1 Title of a/01-first Chapter a"),
                        "{}",
                        row("Title of a/01-first")
                    );
                    assert!(
                        row("Title of a/02-second")
                            .contains("★ 3 3 1.2 Title of a/02-second Chapter a"),
                        "{}",
                        row("Title of a/02-second")
                    );
                    assert!(
                        row("Title of b/01-third")
                            .contains("· 3 2.1 Title of b/01-third Chapter b"),
                        "{}",
                        row("Title of b/01-third")
                    );
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_shows_the_lesson_and_the_count() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    // wide: a column at the top right
    app.resize_screen(100, 30);
    test_key_sequences(
        &mut app,
        vec![
            (
                Some("<esc>"),
                Some(&|app| assert!(!on_screen(app, "practice ·"))),
            ),
            (
                Some(":practice<ret>l"),
                Some(&|app| {
                    let rows = rows(app);
                    let title = rows
                        .iter()
                        .position(|row| row.contains("practice · 1.1 / 2 · Chapter a"))
                        .expect("the title");
                    assert_eq!(title, 0, "{rows:#?}");
                    assert!(rows[0].find("practice").unwrap() >= 50, "{}", rows[0]);
                    for text in [
                        "Title of a/01-first",
                        "The lesson of this exercise.",
                        "Goal:",
                        "  The quick brown fox",
                        "Keys 1 · par 2",
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
                Some(&|app| assert!(!on_screen(app, "Goal:"))),
            ),
            (
                Some(":practice lesson<ret>"),
                Some(&|app| assert!(on_screen(app, "Goal:"))),
            ),
            // the which-key popup wins
            (
                Some("<space>"),
                Some(&|app| {
                    assert!(!on_screen(app, "Goal:"), "{:#?}", rows(app));
                    assert!(on_screen(app, "Space"), "{:#?}", rows(app));
                }),
            ),
            (Some("<esc>"), Some(&|app| assert!(on_screen(app, "Goal:")))),
            // after the solution: the result, the solution and the way on
            (
                Some("hwd"),
                Some(&|app| {
                    for text in [
                        "Solved in 6 keys · par 2 · best 6",
                        "Teach: wd",
                        ":practice next",
                    ] {
                        assert!(on_screen(app, text), "{text} in {:#?}", rows(app));
                    }
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_panel_is_a_band_at_the_bottom_on_a_narrow_terminal() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
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
                    .position(|row| row.contains("practice · 1.1 / 2 · Chapter a"))
                    .expect("the title");
                // at most half the height, above the statusline and the command line
                assert!(title >= 9, "{rows:#?}");
                let keys = rows
                    .iter()
                    .position(|row| row.contains("Keys 1 · par 2"))
                    .expect("the count");
                assert_eq!(keys, 16, "{rows:#?}");
                assert!(rows[17].starts_with('└'), "{rows:#?}");
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
    let (_dir, mut app) = setup();
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
        let (_dir, mut app) = setup();
        app.resize_screen(width, height);
        test_key_sequence(&mut app, Some(":practice<ret>l"), None, false).await?;
    }
    Ok(())
}

/// Sends events to an application, other than keys too.
struct Driver {
    app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    input: UnboundedReceiverStream<std::io::Result<Event>>,
}

impl Driver {
    fn new(app: Application) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app,
            tx,
            input: UnboundedReceiverStream::new(rx),
        }
    }

    async fn send(&mut self, events: Vec<Event>) {
        for event in events {
            self.tx.send(Ok(event)).unwrap();
        }
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    async fn close(mut self) {
        let events = parse_macro("<esc>:q<ret>")
            .unwrap()
            .into_iter()
            .map(|key| Event::Key(KeyEvent::from(key)));
        for event in events {
            self.tx.send(Ok(event)).unwrap();
        }
        // (this one ends the application, the buffer of the exercise doesn't stop it)
        assert!(!self.app.event_loop_until_idle(&mut self.input).await);
        let errors = self.app.close().await;
        assert!(errors.is_empty(), "{errors:?}");
    }

    async fn keys(&mut self, keys: &str) {
        let events = parse_macro(keys)
            .unwrap()
            .into_iter()
            .map(|key| Event::Key(KeyEvent::from(key)))
            .collect();
        self.send(events).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_keys_and_pastes_count() -> anyhow::Result<()> {
    let (_dir, app) = setup();
    let mut driver = Driver::new(app);
    driver.keys(":practice<ret>").await;
    assert_eq!(keys(), 0);

    #[cfg(windows)]
    let others = vec![Event::FocusLost, Event::FocusGained, Event::Resize(80, 24)];
    #[cfg(not(windows))]
    let others = vec![
        Event::FocusOut,
        Event::FocusIn,
        Event::WindowResized(termina::WindowSize {
            cols: 80,
            rows: 24,
            pixel_width: None,
            pixel_height: None,
        }),
    ];
    driver.send(others).await;
    assert_eq!(keys(), 0);

    // a paste counts for each character, `ä` is one
    driver.keys("a").await;
    driver.send(vec![Event::Paste("päste".to_owned())]).await;
    assert_eq!(keys(), 1 + 5);
    driver.keys("<esc>").await;
    assert_eq!(keys(), 1 + 5 + 1);

    driver.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_corrupt_progress_file_is_not_overwritten() -> anyhow::Result<()> {
    let (dir, mut app) = setup();
    std::fs::write(dir.path().join("practice.toml"), "this is = not [valid")?;
    let dir = &dir;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>"),
                Some(&|app| {
                    assert!(
                        status(app).contains("progress is not saved"),
                        "{}",
                        status(app)
                    );
                }),
            ),
            (
                Some("wd"),
                Some(&|app| {
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(2));
                    assert!(
                        status(app).contains("progress is not saved"),
                        "{}",
                        status(app)
                    );
                    assert_eq!(progress(dir), "this is = not [valid");
                }),
            ),
        ],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn completion_offers_subcommands_and_chapters() -> anyhow::Result<()> {
    let (_dir, mut app) = setup();
    test_key_sequences(
        &mut app,
        vec![(
            Some(":practice <tab>"),
            Some(&|app| {
                // the first completion is filled in, the rest is listed
                let rows = rows(app);
                assert!(rows.iter().any(|row| row.contains("list")), "{rows:#?}");
                assert!(rows.iter().any(|row| row.contains("hint")), "{rows:#?}");
            }),
        )],
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn before_texts_are_the_undo_history_of_the_buffer() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    practice::reset_session();
    practice::set_progress_file(dir.path().join("practice.toml"));
    let mut with_history = exercise("a/01-history", "t#[h|]#ree\n", "four\n", "cwfour<esc>");
    with_history.before = vec!["one\n".into(), "two\nmore\n".into()];
    practice::set_exercises(numbered(vec![with_history]));
    let mut app = AppBuilder::new().build()?;

    let text = |app: &Application| helix_view::doc!(app.editor).text().to_string();
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>"),
                Some(&|app| {
                    // it starts at `start`, with its selection, never modified
                    assert_eq!(text(app), "three\n");
                    assert_eq!(
                        helix_view::doc!(app.editor)
                            .selection(helix_view::view!(app.editor).id)
                            .primary()
                            .from(),
                        1
                    );
                    assert!(!helix_view::doc!(app.editor).is_modified());
                    assert_eq!(keys(), 0);
                }),
            ),
            // `u` goes back through the `before` texts one by one
            (
                Some("u"),
                Some(&|app| {
                    assert_eq!(text(app), "two\nmore\n");
                    assert!(!helix_view::doc!(app.editor).is_modified());
                }),
            ),
            (Some("u"), Some(&|app| assert_eq!(text(app), "one\n"))),
            // nothing before the first
            (Some("u"), Some(&|app| assert_eq!(text(app), "one\n"))),
            // `U` goes forward again, to `start`
            (Some("U"), Some(&|app| assert_eq!(text(app), "two\nmore\n"))),
            (Some("U"), Some(&|app| assert_eq!(text(app), "three\n"))),
            // an edit after the history is a revision of its own: `u` gives `start` back
            (Some("xd"), Some(&|app| assert_eq!(text(app), ""))),
            (Some("u"), Some(&|app| assert_eq!(text(app), "three\n"))),
            (Some("u"), Some(&|app| assert_eq!(text(app), "two\nmore\n"))),
        ],
        false,
    )
    .await
}

/// An exercise whose goal can't be reached by accident, to look at the recorded commands.
fn sandbox() -> Vec<Exercise> {
    numbered(vec![exercise(
        "a/01-sandbox",
        "foo (bar) foo\nsecond line\nthird line\n",
        "never\n",
        "x",
    )])
}

/// The commands recorded after `:practice<ret>` and `keys`.
async fn recorded(keys: &str) -> anyhow::Result<Vec<String>> {
    let dir = tempfile::tempdir()?;
    practice::reset_session();
    practice::set_progress_file(dir.path().join("practice.toml"));
    practice::set_exercises(sandbox());
    let mut app = AppBuilder::new().build()?;
    let commands = RefCell::new(Vec::new());
    let keys = format!(":practice<ret>{keys}");
    test_key_sequence(
        &mut app,
        Some(&keys),
        Some(&|_| {
            *commands.borrow_mut() = practice::attempt().expect("an attempt is running").commands
        }),
        false,
    )
    .await?;
    Ok(commands.into_inner())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_keys_are_named_by_the_state_before_them() -> anyhow::Result<()> {
    let cases: &[(&str, &[&str])] = &[
        // counts, and a command that is complete
        ("3w", &["count", "w"]),
        ("10w", &["count", "count", "w"]),
        ("gg", &["gg"]),
        ("<A-o>", &["<A-o>"]),
        ("<gt>", &["<gt>"]),
        ("<lt>", &["<lt>"]),
        ("w<space>", &["w"]),
        // the argument of a command is text
        ("mi(", &["mi"]),
        ("\"ay", &["\"", "y"]),
        ("fo", &["f"]),
        ("rx", &["r"]),
        // insert mode: the typed text is nothing
        ("ifoo<esc>", &["i", "<esc>"]),
        ("ifoo bar<ret><tab><backspace><esc>", &["i", "<esc>"]),
        ("i<C-w><esc>", &["i", "insert:<C-w>", "<esc>"]),
        ("ia<left><esc>", &["i", "insert:<left>", "<esc>"]),
        ("i<C-r>a<esc>", &["i", "insert:<C-r>", "<esc>"]),
        // prompts: the text is nothing, but the command line names the command
        ("%sfoo<ret>", &["%", "s"]),
        ("/bar<ret>", &["/"]),
        (":sort<ret>", &[":sort"]),
        (":sort -r<ret>", &[":sort"]),
        (":foo<esc>", &[]),
        // sticky mode
        ("Mk", &["M", "k"]),
        // a practice command leaves no trace
        (":practice hint<ret>", &[]),
        ("w:practice hint<ret>l", &["w", "l"]),
        (":practice lesson<ret>:practice lesson<ret>", &[]),
    ];
    for (keys, expected) in cases {
        assert_eq!(&recorded(keys).await?, expected, "{keys}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_practice_binding_leaves_no_trace_and_the_record_freezes_when_solved(
) -> anyhow::Result<()> {
    let (_dir, _) = setup();
    let mut config = test_config();
    config.keys = toml::from_str("[normal]\nH = \":practice hint\"")?;
    let mut app = AppBuilder::new().with_config(config).build()?;
    let commands = || practice::attempt().unwrap().commands;
    test_key_sequences(
        &mut app,
        vec![
            (
                Some(":practice<ret>hHl"),
                Some(&|_| assert_eq!(commands(), ["h", "l"])),
            ),
            // solved by `w`, `d`: the record stops there
            (
                Some("wd"),
                Some(&|_| {
                    assert_eq!(practice::attempt().unwrap().solved_at, Some(4));
                    assert_eq!(commands(), ["h", "l", "w", "d"]);
                }),
            ),
            (
                Some("ll3j"),
                Some(&|_| assert_eq!(commands(), ["h", "l", "w", "d"])),
            ),
            // starting again forgets them
            (
                Some(":practice reset<ret>"),
                Some(&|_| assert!(commands().is_empty())),
            ),
        ],
        false,
    )
    .await
}

/// An authoring tool, not a test: feeds the keys to an exercise one at a time and shows what
/// happens after each. Run it with
///
/// ```text
/// PRACTICE_TRY=path/to/01-name.toml cargo integration-test -- practice_try --ignored --nocapture
/// ```
///
/// - `PRACTICE_TRY=<file>` (required) is an exercise file, which can be anywhere,
/// - `PRACTICE_KEYS=<keys>` are the keys to type, the `teach` solution by default,
/// - `PRACTICE_SOLUTION=gold|naive` types that solution of the exercise instead.
///
/// After each key it prints the key, the mode, the commands recorded so far, and the buffer with
/// the selections in the markup of the tests (`#[a|]#` primary, `#(b|)#` others). At the end it
/// says whether the exercise is solved and when, and if not, how the text differs from the goal.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "an authoring tool, see its doc comment"]
async fn practice_try() -> anyhow::Result<()> {
    let path = std::env::var_os("PRACTICE_TRY")
        .ok_or_else(|| anyhow::anyhow!("PRACTICE_TRY is not set: the exercise file to try"))?;
    let exercise = practice::load_exercise_file(Path::new(&path))?;
    let keys = match (
        std::env::var("PRACTICE_KEYS").ok(),
        std::env::var("PRACTICE_SOLUTION").ok().as_deref(),
    ) {
        (Some(keys), _) => keys,
        (None, None | Some("teach")) => exercise.teach.clone(),
        (None, Some("naive")) => exercise.naive.clone(),
        (None, Some("gold")) => exercise
            .gold
            .clone()
            .ok_or_else(|| anyhow::anyhow!("the exercise has no gold solution"))?,
        (None, Some(other)) => anyhow::bail!("PRACTICE_SOLUTION is gold or naive, not `{other}`"),
    };
    let events = parse_macro(&keys)?;
    anyhow::ensure!(!events.is_empty(), "there are no keys to type");
    println!("{}: {keys}  ({} keys)", exercise.id, events.len());

    let dir = tempfile::tempdir()?;
    practice::reset_session();
    practice::set_progress_file(dir.path().join("practice.toml"));
    practice::set_exercises(vec![exercise.clone()]);
    let mut app = AppBuilder::new().build()?;
    practice::start_exercise(&mut app.editor, &exercise.id)?;

    fn frame(app: &Application) -> String {
        let (view, doc) = helix_view::current_ref!(app.editor);
        let text = helix_core::test::plain(doc.text().slice(..), doc.selection(view.id));
        let mut out = String::from("----\n");
        for line in text.lines() {
            out += &format!("    {line}\n");
        }
        out + "----"
    }
    println!("start\n{}", frame(&app));

    let total = events.len();
    // (`<{key}>` is how every key, plain or not, is spelled in a macro)
    let names: Vec<String> = events.iter().map(|event| format!("<{event}>")).collect();
    type Check = Box<dyn Fn(&Application)>;
    let checks: Vec<Check> = names
        .iter()
        .enumerate()
        .map(|(n, name)| {
            let (n, name) = (n + 1, name.clone());
            let exercise = exercise.clone();
            Box::new(move |app: &Application| {
                let attempt = practice::attempt().expect("the attempt is running");
                println!(
                    "{n}/{total} key {name}  mode {:?}  commands {:?}\n{}",
                    app.editor.mode(),
                    attempt.commands,
                    frame(app)
                );
                if n < total {
                    return;
                }
                match attempt.solved_at {
                    Some(at) if at == total => println!("SOLVED at the last key ({at})"),
                    Some(at) => println!("SOLVED early, at key {at} of {total}"),
                    None => {
                        println!("NOT SOLVED after {total} keys (goal -, text +)");
                        let text = helix_view::doc!(app.editor).text().to_string();
                        let (goal, text): (Vec<_>, Vec<_>) =
                            (exercise.goal.lines().collect(), text.lines().collect());
                        for n in 0..goal.len().max(text.len()) {
                            match (goal.get(n), text.get(n)) {
                                (Some(a), Some(b)) if a == b => println!("    {a}"),
                                (a, b) => {
                                    if let Some(a) = a {
                                        println!("  - {a}");
                                    }
                                    if let Some(b) = b {
                                        println!("  + {b}");
                                    }
                                }
                            }
                        }
                    }
                }
            }) as Check
        })
        .collect();
    let inputs = names
        .iter()
        .zip(&checks)
        .map(|(name, check)| (Some(name.as_str()), Some(&**check)))
        .collect();
    test_key_sequences(&mut app, inputs, false).await
}
