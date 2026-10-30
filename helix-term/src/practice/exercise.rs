//! The exercises: loading them from the runtime directory, and checking the data files.
//!
//! ```text
//! runtime/practice/index.toml                      the chapters, in order, with the keys they introduce
//! runtime/practice/<chapter>/<nn>-<name>.toml      one exercise
//! ```
//!
//! The order of the exercises is the order of the chapters in `index.toml`, then the file names.
//! The id of an exercise is `<chapter>/<file stem>`, progress is saved under it. The fields of an
//! exercise file are documented on [`Exercise`]. There is no `par` field: it is the number of keys
//! of `teach`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _};
use helix_core::{syntax, Selection};
use helix_view::input::parse_macro;
use serde::Deserialize;

/// The file with the chapters, in the directory of the exercises.
pub const INDEX_FILE: &str = "index.toml";

/// The directory with the exercises inside a runtime directory.
pub const DIR_NAME: &str = "practice";

/// Longest line of the lesson, in characters. The panel wraps them, but a lesson is written for a
/// narrow terminal.
pub const MAX_LESSON_LINE: usize = 80;
/// Most lines in a lesson.
pub const MAX_LESSON_LINES: usize = 6;
/// Longest line of `start`, `goal` and the `before` texts, in characters.
pub const MAX_TEXT_LINE: usize = 50;
/// Most lines in `start`, `goal` and the `before` texts.
pub const MAX_TEXT_LINES: usize = 16;
/// Longest title of an exercise, in characters.
pub const MAX_TITLE: usize = 40;
/// The number of hints of every exercise.
pub const HINT_COUNT: usize = 3;

/// A chapter of the index: a directory of exercises and the keys it introduces.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chapter {
    /// The name of the directory, kebab-case.
    pub dir: String,
    /// One line.
    pub title: String,
    /// The commands the chapter introduces, in the key names of the replay test (`w`, `gh`,
    /// `count`, `<A-.>`, ...).
    pub keys: Vec<String>,
}

/// What an exercise is for. The exercises of a chapter go from `Learn` to `Challenge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Teaches a key of the chapter.
    Learn,
    /// Uses it in a realistic task.
    Apply,
    /// Combines it with other keys.
    Combine,
    /// A task without a lesson to lean on.
    Challenge,
}

impl Kind {
    /// The name in the TOML files.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Learn => "learn",
            Kind::Apply => "apply",
            Kind::Combine => "combine",
            Kind::Challenge => "challenge",
        }
    }
}

/// One exercise. The fields are the keys of the TOML file, except `id` and the last four, which
/// the loader derives. Unknown keys in the file are an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exercise {
    /// `<chapter>/<file stem>`, like `motions/01-word-motions`.
    pub id: String,
    /// At most [`MAX_TITLE`] characters.
    pub title: String,
    /// Equal to the name of the directory.
    pub chapter: String,
    pub kind: Kind,
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
    /// The texts the buffer had before `start`, oldest first: they become its undo history, so
    /// that `u` goes back through them (see `session::start_index`). No selection markup.
    pub before: Vec<String>,
    /// A solution that uses the feature that is taught, in the notation of macros (`wd<esc>`).
    pub teach: String,
    /// The shortest known solution, strictly shorter than `teach`.
    pub gold: Option<String>,
    /// What a beginner types without the feature, in the same notation.
    pub naive: String,
    /// 1-based position of the chapter in the index. Set by the loader.
    pub chapter_number: usize,
    /// The title of the chapter. Set by the loader.
    pub chapter_title: String,
    /// 1-based position in the chapter. Set by the loader.
    pub number: usize,
    /// The number of exercises in the chapter. Set by the loader.
    pub chapter_len: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExercise {
    title: String,
    chapter: String,
    kind: Kind,
    feature: String,
    lesson: String,
    #[serde(default)]
    before: Vec<String>,
    start: String,
    goal: String,
    language: Option<String>,
    hints: Vec<String>,
    teach: String,
    gold: Option<String>,
    naive: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    chapters: Vec<Chapter>,
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

    /// The number of keys of the solution without the feature.
    pub fn naive_keys(&self) -> usize {
        key_count(&self.naive).unwrap_or(0)
    }

    /// The number of keys of the shortest known solution.
    pub fn gold_par(&self) -> Option<usize> {
        self.gold.as_deref().and_then(key_count)
    }

    /// The initial text and selection: `start` without its markup.
    pub fn start_state(&self) -> anyhow::Result<(String, Selection)> {
        parse_start(&self.start)
    }

    /// An exercise with this id and `teach` solution and nothing else, for the unit tests of
    /// other modules: `chapter` is the directory of the id, the position fields are 1 of 1.
    #[cfg(test)]
    pub(crate) fn for_test(id: &str, teach: &str) -> Self {
        let chapter = id.split('/').next().unwrap_or_default().to_owned();
        Exercise {
            id: id.into(),
            title: id.into(),
            chapter,
            kind: Kind::Learn,
            feature: String::new(),
            lesson: String::new(),
            before: Vec::new(),
            start: String::new(),
            goal: String::new(),
            language: None,
            hints: Vec::new(),
            teach: teach.into(),
            gold: None,
            naive: String::new(),
            chapter_number: 1,
            chapter_title: "Chapter".into(),
            number: 1,
            chapter_len: 1,
        }
    }

    fn from_raw(id: String, raw: RawExercise) -> anyhow::Result<Self> {
        let exercise = Exercise {
            id,
            title: raw.title,
            chapter: raw.chapter,
            kind: raw.kind,
            feature: raw.feature,
            lesson: raw.lesson,
            before: raw.before,
            start: raw.start,
            goal: raw.goal,
            language: raw.language,
            hints: raw.hints,
            teach: raw.teach,
            gold: raw.gold,
            naive: raw.naive,
            chapter_number: 0,
            chapter_title: String::new(),
            number: 0,
            chapter_len: 0,
        };
        // What the runner relies on, so that starting an exercise can't fail on its data.
        exercise.start_state().context("start")?;
        parse_macro(&exercise.teach).context("teach")?;
        parse_macro(&exercise.naive).context("naive")?;
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

/// The chapters of the `index.toml` in `dir`, in order.
pub fn load_chapters(dir: &Path) -> anyhow::Result<Vec<Chapter>> {
    Ok(read_index(dir)?.chapters)
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

/// Loads the exercises in `dir`: the chapters of its `index.toml` in order, and in each the files
/// in order of their names. A chapter of the index without a directory has no exercises. Errors
/// name the file.
pub fn load_exercises_from(dir: &Path) -> anyhow::Result<Vec<Exercise>> {
    let mut exercises = Vec::new();
    for (n, chapter) in load_chapters(dir)?.iter().enumerate() {
        let chapter_dir = dir.join(&chapter.dir);
        if !chapter_dir.is_dir() {
            continue;
        }
        let files =
            toml_files(&chapter_dir).map_err(|err| anyhow!("{}: {err}", chapter_dir.display()))?;
        let first = exercises.len();
        for path in files {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy();
            let text = std::fs::read_to_string(&path)
                .map_err(|err| anyhow!("{}: {err}", path.display()))?;
            let raw: RawExercise =
                toml::from_str(&text).map_err(|err| anyhow!("{}: {err}", path.display()))?;
            let exercise = Exercise::from_raw(format!("{}/{stem}", chapter.dir), raw)
                .map_err(|err| anyhow!("{}: {err:#}", path.display()))?;
            exercises.push(exercise);
        }
        let len = exercises.len() - first;
        for (i, exercise) in exercises[first..].iter_mut().enumerate() {
            exercise.chapter_number = n + 1;
            exercise.chapter_title = chapter.title.clone();
            exercise.number = i + 1;
            exercise.chapter_len = len;
        }
    }
    Ok(exercises)
}

/// Whether `name` is kebab-case: words of lowercase letters and digits joined by single `-`.
fn is_kebab(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Splits `01-word-motions` into its number and name.
fn split_stem(stem: &str) -> Option<(usize, &str)> {
    let (number, name) = stem.split_once('-')?;
    (number.len() == 2 && number.bytes().all(|b| b.is_ascii_digit()) && is_kebab(name))
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
/// (empty if all is well), each naming the exercise, chapter or file:
///
/// - the chapters of `index.toml`: `dir` kebab-case and unique, `title` one line, `keys` non-empty
///   and no key in two chapters; every chapter has a directory with exercises, and nothing else is
///   in `dir`
/// - files are `<nn>-<name>.toml`, numbered from 01 without gaps in each chapter, `name` kebab-case
/// - `chapter` is the name of the directory
/// - `kind` order in a chapter: the first exercise is `learn`, the kinds never go back, and the
///   chapter ends with 1 or 2 `challenge`s and has no other
/// - sizes: title at most 40 characters, lesson at most 6 lines of 80, `start`, `goal` and every
///   `before` text at most 16 lines of 50
/// - exactly 3 hints, none empty
/// - `start`, `goal` and the `before` texts have no markup left (`start` has its selection
///   markup only), and `start` and `goal` differ
/// - `teach`, `gold` and `naive` parse, and `gold` has strictly fewer keys than `teach`
/// - `language` is a language of `loader`
pub fn validate(exercises: &[Exercise], dir: &Path, loader: &syntax::Loader) -> Vec<String> {
    let mut problems = Vec::new();

    let chapters = match load_chapters(dir) {
        Ok(chapters) => chapters,
        Err(err) => {
            problems.push(err.to_string());
            return problems;
        }
    };
    let dir_of = |exercise: &Exercise| exercise.id.split('/').next().unwrap_or_default().to_owned();

    let mut seen = BTreeSet::new();
    let mut key_owner: BTreeMap<&str, &str> = BTreeMap::new();
    for chapter in &chapters {
        let name = chapter.dir.as_str();
        if !seen.insert(name) {
            problems.push(format!("{INDEX_FILE}: chapter `{name}` is listed twice"));
        }
        if !is_kebab(name) {
            problems.push(format!("{INDEX_FILE}: chapter `{name}` is not kebab-case"));
        }
        if chapter.title.trim().is_empty() || chapter.title.contains('\n') {
            problems.push(format!(
                "{INDEX_FILE}: the title of chapter `{name}` must be one non-empty line"
            ));
        }
        if chapter.keys.is_empty() {
            problems.push(format!("{INDEX_FILE}: chapter `{name}` has no keys"));
        }
        for key in &chapter.keys {
            if key.is_empty() {
                problems.push(format!("{INDEX_FILE}: chapter `{name}` has an empty key"));
            } else if let Some(other) = key_owner.insert(key, name) {
                problems.push(if other == name {
                    format!("{INDEX_FILE}: key `{key}` is listed twice in chapter `{name}`")
                } else {
                    format!(
                        "{INDEX_FILE}: key `{key}` is listed by chapters `{other}` and `{name}`"
                    )
                });
            }
        }
        if !exercises.iter().any(|exercise| dir_of(exercise) == name) {
            problems.push(format!("{INDEX_FILE}: chapter `{name}` has no exercises"));
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
                    if !chapters.iter().any(|chapter| chapter.dir == name) {
                        problems.push(format!(
                            "{name}/: directory is not a chapter of {INDEX_FILE}"
                        ));
                    }
                } else if name != INDEX_FILE {
                    problems.push(format!("{name}: unexpected file, only {INDEX_FILE} and chapter directories belong here"));
                }
            }
        }
        Err(err) => problems.push(format!("{}: {err}", dir.display())),
    }

    // the files of each chapter
    for chapter in &chapters {
        let chapter_dir = dir.join(&chapter.dir);
        let Ok(entries) = std::fs::read_dir(&chapter_dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let mut next = 1;
        for name in names {
            let chapter = &chapter.dir;
            let Some(stem) = name.strip_suffix(".toml") else {
                problems.push(format!("{chapter}/{name}: not a .toml file"));
                continue;
            };
            match split_stem(stem) {
                None => problems.push(format!(
                    "{chapter}/{name}: file names are `<nn>-<kebab-case-name>.toml`"
                )),
                Some((number, _)) if number != next => {
                    problems.push(format!(
                        "{chapter}/{name}: expected number {next:02}, numbering starts at 01 and has no gaps"
                    ));
                    next = number + 1;
                }
                Some(_) => next += 1,
            }
        }
    }

    // the kinds in each chapter
    for chapter in &chapters {
        let own: Vec<&Exercise> = exercises
            .iter()
            .filter(|exercise| dir_of(exercise) == chapter.dir)
            .collect();
        let Some(first) = own.first() else { continue };
        if first.kind != Kind::Learn {
            problems.push(format!(
                "{}: the first exercise of a chapter is `learn`, not `{}`",
                first.id,
                first.kind.name()
            ));
        }
        for pair in own.windows(2) {
            if pair[1].kind < pair[0].kind {
                problems.push(format!(
                    "{}: `{}` after `{}`, the kinds go learn, apply, combine, challenge",
                    pair[1].id,
                    pair[1].kind.name(),
                    pair[0].kind.name()
                ));
            }
        }
        let challenges = own
            .iter()
            .filter(|exercise| exercise.kind == Kind::Challenge)
            .count();
        if !(1..=2).contains(&challenges) {
            problems.push(format!(
                "{}/: {challenges} challenges, a chapter ends with 1 or 2",
                chapter.dir
            ));
        }
    }

    let mut ids = BTreeSet::new();
    for exercise in exercises {
        let id = exercise.id.as_str();
        if !ids.insert(id) {
            problems.push(format!("{id}: duplicate id"));
        }
        let dir_chapter = dir_of(exercise);
        if !chapters.iter().any(|chapter| chapter.dir == dir_chapter) {
            problems.push(format!(
                "{id}: chapter `{dir_chapter}` is not in {INDEX_FILE}"
            ));
        }
        if exercise.chapter != dir_chapter {
            problems.push(format!(
                "{id}: chapter is `{}`, the directory is `{dir_chapter}`",
                exercise.chapter
            ));
        }
        for (what, text) in [("title", &exercise.title), ("feature", &exercise.feature)] {
            if text.trim().is_empty() || text.contains('\n') {
                problems.push(format!("{id}: {what} must be one non-empty line"));
            }
        }
        let title = exercise.title.chars().count();
        if title > MAX_TITLE {
            problems.push(format!(
                "{id}: title has {title} characters, at most {MAX_TITLE}"
            ));
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
        for (n, text) in exercise.before.iter().enumerate() {
            let what = format!("before text {}", n + 1);
            check_lines(
                &mut problems,
                id,
                &what,
                text,
                MAX_TEXT_LINES,
                MAX_TEXT_LINE,
            );
            if text.contains("#[") || text.contains("#(") {
                problems.push(format!(
                    "{id}: {what} has `#[` or `#(`, before texts have no selection markup"
                ));
            }
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
        if key_count(&exercise.naive).is_none_or(|count| count == 0) {
            problems.push(format!("{id}: naive is empty or not a key sequence"));
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
chapter = "motions"
kind = "learn"
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
naive = "dwwwd"
"#;

    const INDEX: &str =
        "[[chapters]]\ndir = \"motions\"\ntitle = \"Motions\"\nkeys = [\"w\", \"d\"]\n";

    /// `GOOD` as the last exercise of its chapter.
    fn challenge() -> String {
        GOOD.replace("kind = \"learn\"", "kind = \"challenge\"")
    }

    fn exercise(text: &str) -> anyhow::Result<Exercise> {
        let raw: RawExercise = toml::from_str(text)?;
        Exercise::from_raw("motions/01-word-motions".into(), raw)
    }

    #[test]
    fn parses_an_exercise() {
        let exercise = exercise(GOOD).unwrap();
        assert_eq!(exercise.par(), 2);
        assert_eq!(exercise.naive_keys(), 5);
        assert_eq!(exercise.gold_par(), None);
        assert_eq!(exercise.kind, Kind::Learn);
        assert_eq!(exercise.chapter, "motions");
        assert!(exercise.before.is_empty());
        let (text, selection) = exercise.start_state().unwrap();
        assert_eq!(text, "The quick quick brown fox\n");
        assert_eq!(selection.primary().from(), 4);
        assert_eq!(selection.len(), 1);
    }

    #[test]
    fn kinds_are_ordered() {
        assert!(Kind::Learn < Kind::Apply);
        assert!(Kind::Apply < Kind::Combine);
        assert!(Kind::Combine < Kind::Challenge);
        for kind in ["learn", "apply", "combine", "challenge"] {
            let text = GOOD.replace("kind = \"learn\"", &format!("kind = \"{kind}\""));
            assert_eq!(exercise(&text).unwrap().kind.name(), kind);
        }
        assert!(exercise(&GOOD.replace("\"learn\"", "\"Learn\"")).is_err());
        assert!(exercise(&GOOD.replace("\"learn\"", "\"easy\"")).is_err());
    }

    #[test]
    fn before_texts_are_read() {
        let text = format!("{GOOD}before = [\"one\\n\", \"two\\n\"]\n");
        assert_eq!(exercise(&text).unwrap().before, ["one\n", "two\n"]);
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
    fn unknown_and_missing_fields_are_errors() {
        let text = format!("{GOOD}par = 2\n");
        assert!(exercise(&text).is_err());
        // the old name of `chapter`
        assert!(exercise(&format!("{GOOD}topic = \"motions\"\n")).is_err());
        // naive and kind are required
        assert!(exercise(&GOOD.replace("naive = \"dwwwd\"\n", "")).is_err());
        assert!(exercise(&GOOD.replace("kind = \"learn\"\n", "")).is_err());
    }

    #[test]
    fn malformed_markup_and_keys_are_errors() {
        let text = GOOD.replace("#[q|]#", "#[q");
        assert!(exercise(&text).is_err());
        let text = GOOD.replace("teach = \"wd\"", "teach = \"<nope>\"");
        assert!(exercise(&text).is_err());
        let text = GOOD.replace("naive = \"dwwwd\"", "naive = \"<nope>\"");
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

    fn chapter(dir: &str, keys: &str) -> String {
        format!("[[chapters]]\ndir = \"{dir}\"\ntitle = \"The {dir}\"\nkeys = [{keys}]\n")
    }

    #[test]
    fn loads_in_the_order_of_the_index_then_the_file_names() {
        let other = GOOD.replace("motions", "selections");
        let dir = tree(&[
            (
                "index.toml",
                format!(
                    "{}{}",
                    chapter("selections", "\"s\""),
                    chapter("motions", "\"w\"")
                ),
            ),
            ("motions/02-b.toml", GOOD.into()),
            ("motions/01-a.toml", GOOD.into()),
            ("selections/01-c.toml", other),
            ("motions/README", "not an exercise".into()),
        ]);
        let exercises = load_exercises_from(dir.path()).unwrap();
        let ids: Vec<_> = exercises
            .iter()
            .map(|exercise| exercise.id.as_str())
            .collect();
        assert_eq!(ids, ["selections/01-c", "motions/01-a", "motions/02-b"]);
        // the derived fields
        let derived: Vec<_> = exercises
            .iter()
            .map(|e| {
                (
                    e.chapter_number,
                    e.chapter_title.as_str(),
                    e.number,
                    e.chapter_len,
                )
            })
            .collect();
        assert_eq!(
            derived,
            [
                (1, "The selections", 1, 1),
                (2, "The motions", 1, 2),
                (2, "The motions", 2, 2)
            ]
        );
        let chapters = load_chapters(dir.path()).unwrap();
        assert_eq!(chapters[1].dir, "motions");
        assert_eq!(chapters[1].keys, ["w"]);
    }

    #[test]
    fn errors_name_the_file() {
        let dir = tree(&[
            ("index.toml", INDEX.into()),
            ("motions/01-a.toml", format!("{GOOD}bogus = 1\n")),
        ]);
        let err = load_exercises_from(dir.path()).unwrap_err().to_string();
        assert!(err.contains("01-a.toml"), "{err}");
        let err = load_exercises_from(&dir.path().join("nowhere")).unwrap_err();
        assert!(err.to_string().contains("index.toml"), "{err}");
        // the index denies unknown fields
        let dir = tree(&[("index.toml", format!("{INDEX}extra = 1\n"))]);
        assert!(load_chapters(dir.path()).is_err());
        let dir = tree(&[("index.toml", "topics = [\"motions\"]".into())]);
        assert!(load_chapters(dir.path()).is_err());
    }

    #[test]
    fn a_good_tree_has_no_problems() {
        let dir = tree(&[
            ("index.toml", INDEX.into()),
            ("motions/01-a.toml", GOOD.into()),
            ("motions/02-b.toml", challenge()),
        ]);
        assert_eq!(problems(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn markup_left_in_the_text_is_a_problem() {
        let dir = tree(&[("index.toml", INDEX.into())]);
        let mut exercise = exercise(GOOD).unwrap();
        exercise.goal = "#[x|]#\n".into();
        let out = validate(&[exercise], dir.path(), &loader()).join("\n");
        assert!(out.contains("goal has `#[`"), "{out}");
    }

    /// The problems of a tree with this index and these (path, content) files.
    fn case(files: Vec<(&str, String)>, index: &str) -> String {
        let mut all = vec![("index.toml", index.to_string())];
        all.extend(files);
        let dir = tree(&all);
        problems(dir.path()).join("\n")
    }

    /// A tree of two exercises, the second a challenge, the first changed by `change`.
    fn first(change: impl Fn(&str) -> String) -> String {
        case(
            vec![
                ("motions/01-a.toml", change(GOOD)),
                ("motions/02-b.toml", challenge()),
            ],
            INDEX,
        )
    }

    #[test]
    fn numbering_and_the_directories() {
        let good = |n: &str| (format!("motions/{n}.toml"), GOOD.to_string());
        let run = |files: Vec<(String, String)>, index: &str| {
            let refs: Vec<(&str, String)> =
                files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
            case(refs, index)
        };
        let out = run(vec![good("02-a")], INDEX);
        assert!(out.contains("expected number 01"), "{out}");
        let out = run(vec![good("01-a"), good("03-c")], INDEX);
        assert!(out.contains("expected number 02"), "{out}");
        let out = run(vec![good("1-a")], INDEX);
        assert!(out.contains("file names are"), "{out}");
        // a chapter of the index without exercises
        let two = format!("{INDEX}{}", chapter("goto", "\"gg\""));
        let out = run(vec![good("01-a")], &two);
        assert!(out.contains("chapter `goto` has no exercises"), "{out}");
        // a directory that is not in the index
        let out = run(
            vec![
                good("01-a"),
                ("goto/01-a.toml".into(), GOOD.replace("motions", "goto")),
            ],
            INDEX,
        );
        assert!(out.contains("goto/: directory is not a chapter"), "{out}");
        // a chapter listed twice, not kebab-case
        let out = run(
            vec![good("01-a")],
            &format!("{INDEX}{}", chapter("motions", "\"x\"")),
        );
        assert!(out.contains("chapter `motions` is listed twice"), "{out}");
        let out = run(vec![good("01-a")], &chapter("Motions", "\"w\""));
        assert!(out.contains("`Motions` is not kebab-case"), "{out}");
    }

    #[test]
    fn an_exercise_of_an_unknown_chapter_is_a_problem() {
        let dir = tree(&[("index.toml", INDEX.into())]);
        let mut stray = exercise(GOOD).unwrap();
        stray.id = "goto/01-a".into();
        stray.chapter = "goto".into();
        let out = validate(&[stray], dir.path(), &loader()).join("\n");
        assert!(
            out.contains("goto/01-a: chapter `goto` is not in index.toml"),
            "{out}"
        );
    }

    #[test]
    fn the_chapter_field_is_the_directory() {
        let out = first(|text| text.replace("chapter = \"motions\"", "chapter = \"goto\""));
        assert!(out.contains("chapter is `goto`"), "{out}");
    }

    #[test]
    fn chapters_in_the_index() {
        let index = |chapter: &str| format!("[[chapters]]\ndir = \"motions\"\n{chapter}");
        let run = |index: String| {
            case(
                vec![
                    ("motions/01-a.toml", GOOD.to_string()),
                    ("motions/02-b.toml", challenge()),
                ],
                &index,
            )
        };
        let out = run(index("title = \"One\\nTwo\"\nkeys = [\"w\"]\n"));
        assert!(
            out.contains("title of chapter `motions` must be one"),
            "{out}"
        );
        let out = run(index("title = \"\"\nkeys = [\"w\"]\n"));
        assert!(
            out.contains("title of chapter `motions` must be one"),
            "{out}"
        );
        let out = run(index("title = \"T\"\nkeys = []\n"));
        assert!(out.contains("chapter `motions` has no keys"), "{out}");
        let out = run(index("title = \"T\"\nkeys = [\"\"]\n"));
        assert!(out.contains("an empty key"), "{out}");
        let out = run(index("title = \"T\"\nkeys = [\"w\", \"w\"]\n"));
        assert!(
            out.contains("key `w` is listed twice in chapter `motions`"),
            "{out}"
        );
    }

    #[test]
    fn a_key_in_two_chapters_is_a_problem() {
        let two = format!(
            "{}{}",
            chapter("motions", "\"w\", \"b\""),
            chapter("goto", "\"gg\", \"b\"")
        );
        let other = |text: String| text.replace("motions", "goto");
        let out = case(
            vec![
                ("motions/01-a.toml", GOOD.into()),
                ("motions/02-b.toml", challenge()),
                ("goto/01-a.toml", other(GOOD.into())),
                ("goto/02-b.toml", other(challenge())),
            ],
            &two,
        );
        assert!(
            out.contains("key `b` is listed by chapters `motions` and `goto`"),
            "{out}"
        );
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    #[test]
    fn kinds_go_from_learn_to_challenge() {
        let kind = |kind: &str| GOOD.replace("kind = \"learn\"", &format!("kind = \"{kind}\""));
        let run = |kinds: &[&str]| {
            let files: Vec<_> = kinds
                .iter()
                .enumerate()
                .map(|(n, k)| (format!("motions/{:02}-a.toml", n + 1), kind(k)))
                .collect();
            let refs = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
            case(refs, INDEX)
        };
        assert_eq!(run(&["learn", "challenge"]), "");
        assert_eq!(
            run(&[
                "learn",
                "learn",
                "apply",
                "combine",
                "challenge",
                "challenge"
            ]),
            ""
        );
        // the first is a learn
        let out = run(&["apply", "challenge"]);
        assert!(
            out.contains("motions/01-a: the first exercise of a chapter is `learn`, not `apply`"),
            "{out}"
        );
        // they never go back
        let out = run(&["learn", "combine", "apply", "challenge"]);
        assert!(
            out.contains("motions/03-a: `apply` after `combine`"),
            "{out}"
        );
        let out = run(&["learn", "challenge", "apply"]);
        assert!(
            out.contains("motions/03-a: `apply` after `challenge`"),
            "{out}"
        );
        // a challenge is the end: one or two
        let out = run(&["learn", "apply"]);
        assert!(out.contains("motions/: 0 challenges"), "{out}");
        let out = run(&["learn"]);
        assert!(out.contains("motions/: 0 challenges"), "{out}");
        let out = run(&["learn", "challenge", "challenge", "challenge"]);
        assert!(out.contains("motions/: 3 challenges"), "{out}");
    }

    #[test]
    fn sizes() {
        // the new limits: 50 characters and 16 lines of text, 6 lines of lesson, 40 of title
        let long = "x".repeat(50);
        assert_eq!(
            first(|t| t.replace("The quick brown fox\n", &format!("{long}\n"))),
            ""
        );
        let long = "x".repeat(51);
        let out = first(|t| t.replace("The quick brown fox\n", &format!("{long}\n")));
        assert!(
            out.contains("goal line 1 has 51 characters, at most 50"),
            "{out}"
        );
        let lines = "y\n".repeat(16);
        assert_eq!(first(|t| t.replace("The quick brown fox\n", &lines)), "");
        let lines = "y\n".repeat(17);
        let out = first(|t| t.replace("The quick brown fox\n", &lines));
        assert!(out.contains("goal has 17 lines, at most 16"), "{out}");
        let lesson = "l\n".repeat(6);
        let ok = |t: &str| t.replace("`w` selects to the start of the next word.\n", &lesson);
        assert_eq!(first(ok), "");
        let lesson = "l\n".repeat(7);
        let out = first(|t| t.replace("`w` selects to the start of the next word.\n", &lesson));
        assert!(out.contains("lesson has 7 lines"), "{out}");
        let title = "t".repeat(40);
        assert_eq!(first(|t| t.replace("Word by word", &title)), "");
        let title = "t".repeat(41);
        let out = first(|t| t.replace("Word by word", &title));
        assert!(out.contains("title has 41 characters, at most 40"), "{out}");
    }

    #[test]
    fn before_texts_are_checked() {
        let before = |list: &str| {
            let list = list.to_owned();
            move |t: &str| format!("{t}before = {list}\n")
        };
        assert_eq!(first(before("[\"a\\n\", \"b\\n\"]")), "");
        let out = first(before("[\"#[a|]#\\n\"]"));
        assert!(out.contains("before text 1 has `#[`"), "{out}");
        let out = first(before("[\"a\\n\", \"#(b|)#\\n\"]"));
        assert!(out.contains("before text 2 has `#[` or `#(`"), "{out}");
        let long = "x".repeat(51);
        let out = first(before(&format!("[\"{long}\\n\"]")));
        assert!(
            out.contains("before text 1 line 1 has 51 characters"),
            "{out}"
        );
        let out = first(before(&format!("[\"{}\"]", "a\\n".repeat(17))));
        assert!(out.contains("before text 1 has 17 lines"), "{out}");
    }

    #[test]
    fn hints_keys_and_language() {
        let out = first(|t| t.replace("[\"a\", \"b\", \"c\"]", "[\"a\", \"b\"]"));
        assert!(out.contains("2 hints"), "{out}");
        let out = first(|t| format!("{t}gold = \"wd\"\n"));
        assert!(out.contains("strictly shorter"), "{out}");
        assert_eq!(first(|t| format!("{t}gold = \"d\"\n")), "");
        let out = first(|t| t.replace("naive = \"dwwwd\"", "naive = \"\""));
        assert!(out.contains("naive is empty"), "{out}");
        let out = first(|t| format!("{t}language = \"nonesuch\"\n"));
        assert!(out.contains("unknown language `nonesuch`"), "{out}");
        assert_eq!(first(|t| format!("{t}language = \"rust\"\n")), "");
    }
}
