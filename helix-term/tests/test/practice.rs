use std::cell::RefCell;
use std::path::PathBuf;

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
/// application, and checks that it solves the exercise with exactly as many keys as the
/// solution has, and that the data is well formed.
#[tokio::test(flavor = "multi_thread")]
async fn the_solutions_of_all_exercises_solve_them_in_exactly_their_keys() -> anyhow::Result<()> {
    let dir = practice::exercise::exercises_dir()?;
    let exercises = practice::load_exercises_from(&dir)?;
    assert!(!exercises.is_empty());

    let mut failures = Vec::new();

    // Topics of the index without exercises are the topics that are not written yet. Remove this
    // filter when all of them are.
    let problems: Vec<_> = practice::validate(&exercises, &dir, &test_syntax_loader(None))
        .into_iter()
        .filter(|problem| !problem.ends_with("has no exercises"))
        .collect();
    failures.extend(problems);

    println!("{:<44} {:>3}  gold", "id", "par");
    let progress_dir = tempfile::tempdir()?;
    for exercise in &exercises {
        // (the shell commands of the pipe topic need a POSIX shell)
        if cfg!(windows) && exercise.topic == "pipe" {
            continue;
        }
        println!(
            "{:<44} {:>3}  {}",
            exercise.id,
            exercise.par(),
            exercise
                .gold_par()
                .map_or_else(|| "-".to_owned(), |gold| gold.to_string())
        );
        for (name, keys) in [
            ("teach", Some(&exercise.teach)),
            ("gold", exercise.gold.as_ref()),
        ] {
            let Some(keys) = keys else { continue };
            if let Err(failure) = replay(exercise, keys, progress_dir.path().join("p.toml")).await {
                failures.push(format!("{} [{name}: {keys}]: {failure}", exercise.id));
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

/// Types `keys` into a fresh application that has just started `exercise`.
async fn replay(exercise: &Exercise, keys: &str, progress: PathBuf) -> Result<(), String> {
    let _ = std::fs::remove_file(&progress);
    practice::reset_session();
    practice::set_progress_file(progress);
    practice::set_exercises(vec![exercise.clone()]);

    let mut app = AppBuilder::new().build().map_err(|e| e.to_string())?;
    practice::start_exercise(&mut app.editor, &exercise.id).map_err(|e| e.to_string())?;

    let expected = parse_macro(keys).map_err(|e| e.to_string())?.len();
    let outcome = RefCell::new(Ok(()));
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
                Some(_) => Ok(()),
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
    Exercise {
        id: id.to_owned(),
        title: format!("Title of {id}"),
        topic: id.split('/').next().unwrap().to_owned(),
        feature: "A feature".to_owned(),
        lesson: "The lesson of this exercise.\nA second line of the lesson.\n".to_owned(),
        start: start.to_owned(),
        goal: goal.to_owned(),
        language: None,
        hints: ["first hint", "second hint", "third hint"]
            .map(str::to_owned)
            .to_vec(),
        teach: teach.to_owned(),
        gold: None,
    }
}

fn exercises() -> Vec<Exercise> {
    vec![
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
    ]
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
            // a topic: its first unsolved exercise
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
                        "'practice': unknown topic `nonesuch`, the topics are: a, b"
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
                        row("Title of a/01-first").contains("✓ a Title of a/01-first 4 2"),
                        "{}",
                        row("Title of a/01-first")
                    );
                    assert!(
                        row("Title of a/02-second").contains("★ a Title of a/02-second 3 3"),
                        "{}",
                        row("Title of a/02-second")
                    );
                    assert!(
                        row("Title of b/01-third").contains("· b Title of b/01-third 3"),
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
                        .position(|row| row.contains("practice · a 1/2"))
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
                    .position(|row| row.contains("practice · a 1/2"))
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
async fn completion_offers_subcommands_and_topics() -> anyhow::Result<()> {
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
