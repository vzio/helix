//! The exercises: loading them from the runtime directory, and checking the data files.
//!
//! ```text
//! runtime/practice/index.toml                    topic order
//! runtime/practice/<topic>/<nn>-<name>.toml      one exercise
//! ```
//!
//! The order of the exercises is the order of the topics in `index.toml`, then the file names. The
//! id of an exercise is `<topic>/<file stem>`, progress is saved under it. The fields of an
//! exercise file are documented on [`Exercise`]. There is no `par` field: it is the number of keys
//! of `teach`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use helix_core::{syntax, Selection};
use helix_view::input::parse_macro;
use serde::Deserialize;

/// The file with the order of the topics, in the directory of the exercises.
pub const INDEX_FILE: &str = "index.toml";

/// The directory with the exercises inside a runtime directory.
pub const DIR_NAME: &str = "practice";

/// Longest line of the lesson, in characters. The panel wraps them, but a lesson is written for a
/// narrow terminal.
pub const MAX_LESSON_LINE: usize = 80;
/// Most lines in a lesson.
pub const MAX_LESSON_LINES: usize = 5;
/// Longest line of `start` and `goal`, in characters.
pub const MAX_TEXT_LINE: usize = 40;
/// Most lines in `start` and `goal`.
pub const MAX_TEXT_LINES: usize = 12;
/// The number of hints of every exercise.
pub const HINT_COUNT: usize = 3;

/// One exercise. The fields are the keys of the TOML file, except `id`, which comes from the path.
/// Unknown keys in the file are an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exercise {
    /// `<topic>/<file stem>`, like `motions/01-word-motions`.
    pub id: String,
    pub title: String,
    /// Equal to the name of the directory.
    pub topic: String,
    /// The feature that is taught, one line.
    pub feature: String,
    /// What to know to solve it, a few short lines.
    pub lesson: String,
    /// The initial text, with the initial selection in the markup of `helix_core::test::print`
    /// (`#[|x]#`, `#[x|]#` primary, `#(|y)#` secondary). Without markup the cursor is on the first
    /// character. See [`Exercise::start_state`].
    pub start: String,
    /// The text to produce. Only the text is checked, not the selection.
    pub goal: String,
    /// The language id (as in `languages.toml`) of the buffer, for the syntax based features.
    pub language: Option<String>,
    /// Three hints: vague, names the feature, names the keys.
    pub hints: Vec<String>,
    /// A solution that uses the feature that is taught, in the notation of macros (`wd<esc>`).
    pub teach: String,
    /// The shortest known solution, strictly shorter than `teach`.
    pub gold: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExercise {
    title: String,
    topic: String,
    feature: String,
    lesson: String,
    start: String,
    goal: String,
    language: Option<String>,
    hints: Vec<String>,
    teach: String,
    gold: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    topics: Vec<String>,
}

/// The number of keys of a solution in the notation of macros, `None` if it doesn't parse.
pub fn key_count(keys: &str) -> Option<usize> {
    parse_macro(keys).ok().map(|keys| keys.len())
}

impl Exercise {
    /// The number of keys of the solution that uses the feature.
    pub fn par(&self) -> usize {
        key_count(&self.teach).unwrap_or(0)
    }

    /// The number of keys of the shortest known solution.
    pub fn gold_par(&self) -> Option<usize> {
        self.gold.as_deref().and_then(key_count)
    }

    /// The initial text and selection: `start` without its markup.
    pub fn start_state(&self) -> anyhow::Result<(String, Selection)> {
        parse_start(&self.start)
    }

    fn from_raw(id: String, raw: RawExercise) -> anyhow::Result<Self> {
        let exercise = Exercise {
            id,
            title: raw.title,
            topic: raw.topic,
            feature: raw.feature,
            lesson: raw.lesson,
            start: raw.start,
            goal: raw.goal,
            language: raw.language,
            hints: raw.hints,
            teach: raw.teach,
            gold: raw.gold,
        };
        // What the runner relies on, so that starting an exercise can't fail on its data.
        exercise.start_state().context("start")?;
        parse_macro(&exercise.teach).context("teach")?;
        if let Some(gold) = &exercise.gold {
            parse_macro(gold).context("gold")?;
        }
        Ok(exercise)
    }
}

/// Splits the initial selection markup off `start`. `print` panics on malformed markup, which is
/// turned into an error here.
fn parse_start(start: &str) -> anyhow::Result<(String, Selection)> {
    if !start.contains("#[") && !start.contains("#(") {
        return Ok((start.to_owned(), Selection::point(0)));
    }
    std::panic::catch_unwind(|| helix_core::test::print(start))
        .map_err(|_| anyhow!("malformed selection markup"))
}

/// The first runtime directory that has exercises.
pub fn exercises_dir() -> anyhow::Result<PathBuf> {
    helix_loader::runtime_dirs()
        .iter()
        .map(|dir| dir.join(DIR_NAME))
        .find(|dir| dir.join(INDEX_FILE).is_file())
        .ok_or_else(|| {
            anyhow!(
                "no exercises found: there is no {DIR_NAME}/{INDEX_FILE} in the runtime directories"
            )
        })
}

/// Loads the exercises of the first runtime directory that has any.
pub fn load_exercises() -> anyhow::Result<Vec<Exercise>> {
    load_exercises_from(&exercises_dir()?)
}

fn read_index(dir: &Path) -> anyhow::Result<Index> {
    let path = dir.join(INDEX_FILE);
    let text =
        std::fs::read_to_string(&path).map_err(|err| anyhow!("{}: {err}", path.display()))?;
    toml::from_str(&text).map_err(|err| anyhow!("{}: {err}", path.display()))
}

/// The `.toml` files in `dir`, ordered by name.
fn toml_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "toml") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Loads the exercises in `dir`: the topics of its `index.toml` in order, and in each the files in
/// order of their names. A topic of the index without a directory has no exercises. Errors name
/// the file.
pub fn load_exercises_from(dir: &Path) -> anyhow::Result<Vec<Exercise>> {
    let index = read_index(dir)?;
    let mut exercises = Vec::new();
    for topic in &index.topics {
        let topic_dir = dir.join(topic);
        if !topic_dir.is_dir() {
            continue;
        }
        let files =
            toml_files(&topic_dir).map_err(|err| anyhow!("{}: {err}", topic_dir.display()))?;
        for path in files {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let text = std::fs::read_to_string(&path)
                .map_err(|err| anyhow!("{}: {err}", path.display()))?;
            let raw: RawExercise =
                toml::from_str(&text).map_err(|err| anyhow!("{}: {err}", path.display()))?;
            let exercise = Exercise::from_raw(format!("{topic}/{stem}"), raw)
                .map_err(|err| anyhow!("{}: {err:#}", path.display()))?;
            exercises.push(exercise);
        }
    }
    Ok(exercises)
}

/// Splits `01-word-motions` into its number and name.
fn split_stem(stem: &str) -> Option<(usize, &str)> {
    let (number, name) = stem.split_once('-')?;
    let kebab = !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        });
    (number.len() == 2 && number.bytes().all(|b| b.is_ascii_digit()) && kebab)
        .then(|| (number.parse().unwrap(), name))
}

fn check_lines(
    problems: &mut Vec<String>,
    id: &str,
    what: &str,
    text: &str,
    max_lines: usize,
    max_width: usize,
) {
    let lines = text.lines().count();
    if lines == 0 {
        problems.push(format!("{id}: {what} is empty"));
    }
    if lines > max_lines {
        problems.push(format!(
            "{id}: {what} has {lines} lines, at most {max_lines}"
        ));
    }
    for (n, line) in text.lines().enumerate() {
        let width = line.chars().count();
        if width > max_width {
            problems.push(format!(
                "{id}: {what} line {} has {width} characters, at most {max_width}",
                n + 1
            ));
        }
    }
    if text.contains('\r') {
        problems.push(format!("{id}: {what} contains a carriage return"));
    }
}

/// Checks the data in `dir` and the `exercises` loaded from it, and returns the problems found
/// (empty if all is well), each naming the exercise or file:
///
/// - `index.toml` lists exactly the directories (and each has exercises), nothing else is in `dir`
/// - files are `<nn>-<name>.toml`, numbered from 01 without gaps in each topic, `name` kebab-case
/// - `topic` is the name of the directory
/// - sizes: lesson at most 5 lines of 80 characters, `start` and `goal` at most 12 lines of 40
/// - exactly 3 hints, none empty
/// - `start` and `goal` have no markup left, and they differ
/// - `teach` and `gold` parse, and `gold` has strictly fewer keys than `teach`
/// - `language` is a language of `loader`
pub fn validate(exercises: &[Exercise], dir: &Path, loader: &syntax::Loader) -> Vec<String> {
    let mut problems = Vec::new();

    let index = match read_index(dir) {
        Ok(index) => index,
        Err(err) => {
            problems.push(err.to_string());
            return problems;
        }
    };

    let mut seen = BTreeSet::new();
    for topic in &index.topics {
        if !seen.insert(topic) {
            problems.push(format!("{INDEX_FILE}: topic `{topic}` is listed twice"));
        }
        if !exercises
            .iter()
            .any(|exercise| exercise.id.split('/').next() == Some(topic))
        {
            problems.push(format!("{INDEX_FILE}: topic `{topic}` has no exercises"));
        }
    }

    // what is in the directory
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| {
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        entry.path(),
                    )
                })
                .collect();
            names.sort();
            for (name, path) in names {
                if path.is_dir() {
                    if !index.topics.contains(&name) {
                        problems.push(format!("{name}/: directory is not a topic of {INDEX_FILE}"));
                    }
                } else if name != INDEX_FILE {
                    problems.push(format!("{name}: unexpected file, only {INDEX_FILE} and topic directories belong here"));
                }
            }
        }
        Err(err) => problems.push(format!("{}: {err}", dir.display())),
    }

    // the files of each topic
    for topic in &index.topics {
        let topic_dir = dir.join(topic);
        let Ok(entries) = std::fs::read_dir(&topic_dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut next = 1;
        for name in names {
            let Some(stem) = name.strip_suffix(".toml") else {
                problems.push(format!("{topic}/{name}: not a .toml file"));
                continue;
            };
            match split_stem(stem) {
                None => problems.push(format!(
                    "{topic}/{name}: file names are `<nn>-<kebab-case-name>.toml`"
                )),
                Some((number, _)) if number != next => {
                    problems.push(format!(
                        "{topic}/{name}: expected number {next:02}, numbering starts at 01 and has no gaps"
                    ));
                    next = number + 1;
                }
                Some(_) => next += 1,
            }
        }
    }

    let mut ids = BTreeSet::new();
    for exercise in exercises {
        let id = exercise.id.as_str();
        if !ids.insert(id) {
            problems.push(format!("{id}: duplicate id"));
        }
        let dir_topic = id.split('/').next().unwrap_or_default();
        if exercise.topic != dir_topic {
            problems.push(format!(
                "{id}: topic is `{}`, the directory is `{dir_topic}`",
                exercise.topic
            ));
        }
        for (what, text) in [("title", &exercise.title), ("feature", &exercise.feature)] {
            if text.trim().is_empty() || text.contains('\n') {
                problems.push(format!("{id}: {what} must be one non-empty line"));
            }
        }

        check_lines(
            &mut problems,
            id,
            "lesson",
            &exercise.lesson,
            MAX_LESSON_LINES,
            MAX_LESSON_LINE,
        );

        match exercise.start_state() {
            Err(err) => problems.push(format!("{id}: start: {err}")),
            Ok((text, _)) => {
                check_lines(
                    &mut problems,
                    id,
                    "start",
                    &text,
                    MAX_TEXT_LINES,
                    MAX_TEXT_LINE,
                );
                if text.contains("#[") || text.contains("#(") {
                    problems.push(format!("{id}: start has markup left after the selection"));
                }
                if text == exercise.goal {
                    problems.push(format!("{id}: start and goal are the same"));
                }
            }
        }
        check_lines(
            &mut problems,
            id,
            "goal",
            &exercise.goal,
            MAX_TEXT_LINES,
            MAX_TEXT_LINE,
        );
        if exercise.goal.contains("#[") || exercise.goal.contains("#(") {
            problems.push(format!(
                "{id}: goal has `#[` or `#(`, goal has no selection markup"
            ));
        }

        if exercise.hints.len() != HINT_COUNT {
            problems.push(format!(
                "{id}: {} hints, exactly {HINT_COUNT}: vague, names the feature, names the keys",
                exercise.hints.len()
            ));
        }
        if exercise.hints.iter().any(|hint| hint.trim().is_empty()) {
            problems.push(format!("{id}: a hint is empty"));
        }

        let teach = key_count(&exercise.teach);
        if teach.is_none_or(|count| count == 0) {
            problems.push(format!("{id}: teach is empty or not a key sequence"));
        }
        if let Some(gold) = &exercise.gold {
            match (key_count(gold), teach) {
                (None, _) => problems.push(format!("{id}: gold is not a key sequence")),
                (Some(gold), Some(teach)) if gold >= teach => problems.push(format!(
                    "{id}: gold has {gold} keys, it must be strictly shorter than teach ({teach})"
                )),
                _ => (),
            }
        }

        if let Some(language) = &exercise.language {
            if loader.language_for_name(language.as_str()).is_none() {
                problems.push(format!("{id}: unknown language `{language}`"));
            }
        }
    }

    if exercises.is_empty() {
        problems.push("no exercises".to_string());
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
title = "Word by word"
topic = "motions"
feature = "Word motions: w, b, e"
lesson = """
`w` selects to the start of the next word.
"""
start = """
The #[q|]#uick quick brown fox
"""
goal = """
The quick brown fox
"""
hints = ["a", "b", "c"]
teach = "wd"
"#;

    fn exercise(text: &str) -> anyhow::Result<Exercise> {
        let raw: RawExercise = toml::from_str(text)?;
        Exercise::from_raw("motions/01-word-motions".into(), raw)
    }

    #[test]
    fn parses_an_exercise() {
        let exercise = exercise(GOOD).unwrap();
        assert_eq!(exercise.par(), 2);
        assert_eq!(exercise.gold_par(), None);
        let (text, selection) = exercise.start_state().unwrap();
        assert_eq!(text, "The quick quick brown fox\n");
        assert_eq!(selection.primary().from(), 4);
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn par_counts_keys_not_characters() {
        let text = GOOD.replace("teach = \"wd\"", "teach = \"3w<esc>d<C-w>\"");
        assert_eq!(exercise(&text).unwrap().par(), 5);
    }

    #[test]
    fn start_without_markup_has_the_cursor_on_the_first_character() {
        let text = GOOD.replace("#[q|]#", "q");
        let (text, selection) = exercise(&text).unwrap().start_state().unwrap();
        assert_eq!(text, "The quick quick brown fox\n");
        assert_eq!(selection, Selection::point(0));
    }

    #[test]
    fn unknown_fields_are_an_error() {
        let text = format!("{GOOD}par = 2\n");
        assert!(exercise(&text).is_err());
    }

    #[test]
    fn malformed_markup_and_keys_are_errors() {
        let text = GOOD.replace("#[q|]#", "#[q");
        assert!(exercise(&text).is_err());
        let text = GOOD.replace("teach = \"wd\"", "teach = \"<nope>\"");
        assert!(exercise(&text).is_err());
    }

    #[test]
    fn file_stems() {
        assert_eq!(split_stem("01-word-motions"), Some((1, "word-motions")));
        assert_eq!(split_stem("12-a1"), Some((12, "a1")));
        assert_eq!(split_stem("1-word"), None);
        assert_eq!(split_stem("01-Word"), None);
        assert_eq!(split_stem("01-"), None);
        assert_eq!(split_stem("01-a--b"), None);
        assert_eq!(split_stem("01_word"), None);
    }

    /// Writes an `index.toml` and the files given as (path, content) to a new directory.
    fn tree(files: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        dir
    }

    fn loader() -> syntax::Loader {
        syntax::Loader::new(
            helix_loader::config::default_lang_config()
                .try_into()
                .unwrap(),
        )
        .unwrap()
    }

    fn problems(dir: &Path) -> Vec<String> {
        let exercises = load_exercises_from(dir).unwrap();
        validate(&exercises, dir, &loader())
    }

    #[test]
    fn loads_in_the_order_of_the_index_then_the_file_names() {
        let other = GOOD.replace("motions", "selections");
        let dir = tree(&[
            (
                "index.toml",
                "topics = [\"selections\", \"motions\"]".into(),
            ),
            ("motions/02-b.toml", GOOD.into()),
            ("motions/01-a.toml", GOOD.into()),
            ("selections/01-c.toml", other),
            ("motions/README", "not an exercise".into()),
        ]);
        let ids: Vec<_> = load_exercises_from(dir.path())
            .unwrap()
            .into_iter()
            .map(|exercise| exercise.id)
            .collect();
        assert_eq!(ids, ["selections/01-c", "motions/01-a", "motions/02-b"]);
    }

    #[test]
    fn errors_name_the_file() {
        let dir = tree(&[
            ("index.toml", "topics = [\"motions\"]".into()),
            ("motions/01-a.toml", format!("{GOOD}bogus = 1\n")),
        ]);
        let err = load_exercises_from(dir.path()).unwrap_err().to_string();
        assert!(err.contains("01-a.toml"), "{err}");
        let err = load_exercises_from(&dir.path().join("nowhere")).unwrap_err();
        assert!(err.to_string().contains("index.toml"), "{err}");
    }

    #[test]
    fn a_good_tree_has_no_problems() {
        let dir = tree(&[
            ("index.toml", "topics = [\"motions\"]".into()),
            ("motions/01-a.toml", GOOD.into()),
            ("motions/02-b.toml", GOOD.into()),
        ]);
        assert_eq!(problems(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn markup_left_in_the_text_is_a_problem() {
        let dir = tree(&[("index.toml", "topics = [\"motions\"]".into())]);
        let mut exercise = exercise(GOOD).unwrap();
        exercise.goal = "#[x|]#\n".into();
        let out = validate(&[exercise], dir.path(), &loader()).join("\n");
        assert!(out.contains("goal has `#[`"), "{out}");
    }

    #[test]
    fn lints() {
        let good = |n: &str| (format!("motions/{n}.toml"), GOOD.to_string());
        let case = |files: Vec<(String, String)>, index: &str| {
            let mut all = vec![("index.toml".to_string(), index.to_string())];
            all.extend(files);
            let refs: Vec<(&str, String)> =
                all.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
            let dir = tree(&refs);
            problems(dir.path()).join("\n")
        };
        let idx = "topics = [\"motions\"]";

        // numbering
        let out = case(vec![good("02-a")], idx);
        assert!(out.contains("expected number 01"), "{out}");
        let out = case(vec![good("01-a"), good("03-c")], idx);
        assert!(out.contains("expected number 02"), "{out}");
        let out = case(vec![good("1-a")], idx);
        assert!(out.contains("file names are"), "{out}");
        // the index and the directories
        let out = case(vec![good("01-a")], "topics = [\"motions\", \"goto\"]");
        assert!(out.contains("`goto` has no exercises"), "{out}");
        let out = case(
            vec![
                good("01-a"),
                ("goto/01-a.toml".into(), GOOD.replace("motions", "goto")),
            ],
            idx,
        );
        assert!(out.contains("goto/: directory is not a topic"), "{out}");
        // the topic field
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                GOOD.replace("topic = \"motions\"", "topic = \"goto\""),
            )],
            idx,
        );
        assert!(out.contains("topic is `goto`"), "{out}");
        // sizes
        let long = "x".repeat(41);
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                GOOD.replace("The quick brown fox\n", &format!("{long}\n")),
            )],
            idx,
        );
        assert!(out.contains("goal line 1 has 41 characters"), "{out}");
        let lesson = "l\n".repeat(6);
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                GOOD.replace("`w` selects to the start of the next word.\n", &lesson),
            )],
            idx,
        );
        assert!(out.contains("lesson has 6 lines"), "{out}");
        // hints, keys
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                GOOD.replace("[\"a\", \"b\", \"c\"]", "[\"a\", \"b\"]"),
            )],
            idx,
        );
        assert!(out.contains("2 hints"), "{out}");
        let out = case(
            vec![("motions/01-a.toml".into(), format!("{GOOD}gold = \"wd\"\n"))],
            idx,
        );
        assert!(out.contains("strictly shorter"), "{out}");
        let out = case(
            vec![("motions/01-a.toml".into(), format!("{GOOD}gold = \"d\"\n"))],
            idx,
        );
        assert_eq!(out, "");
        // language
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                format!("{GOOD}language = \"nonesuch\"\n"),
            )],
            idx,
        );
        assert!(out.contains("unknown language `nonesuch`"), "{out}");
        let out = case(
            vec![(
                "motions/01-a.toml".into(),
                format!("{GOOD}language = \"rust\"\n"),
            )],
            idx,
        );
        assert_eq!(out, "");
    }
}
