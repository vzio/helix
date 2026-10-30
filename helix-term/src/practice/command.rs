//! The typable command `:practice`.

use helix_core::command_line::Args;
use helix_core::fuzzy::fuzzy_match;
use helix_view::Editor;

use super::exercise::Exercise;
use super::progress::{Progress, Status};
use super::session::{self, exercises, start_index};
use crate::compositor;
use crate::job::Callback;
use crate::ui::prompt::{Completion, PromptEvent};
use crate::ui::{overlay::overlaid, Picker, PickerColumn};

pub const DOC: &str = "\
Practice with exercises that count your keystrokes: each teaches a feature, then you can come \
back and solve it with fewer keys. Without an argument it continues with the first exercise that \
is not solved yet. Arguments: a topic name (its first unsolved exercise), list (pick an \
exercise), hint (reveal the next hint), reset (start the exercise again), next, prev, lesson \
(show or hide the lesson panel).";

const SUBCOMMANDS: [&str; 6] = ["list", "hint", "reset", "next", "prev", "lesson"];

/// One line of the list of exercises.
pub struct Row {
    pub index: usize,
    pub status: Status,
    pub topic: String,
    pub title: String,
    pub best: Option<usize>,
    pub par: usize,
}

/// The topics in the order of the exercises.
pub fn topics(exercises: &[Exercise]) -> Vec<&str> {
    let mut topics: Vec<&str> = Vec::new();
    for exercise in exercises {
        let topic = topic_of(exercise);
        if topics.last() != Some(&topic) {
            topics.push(topic);
        }
    }
    topics
}

/// The topic of an exercise: the directory in its id, which the order of the exercises is by.
pub fn topic_of(exercise: &Exercise) -> &str {
    exercise.id.split('/').next().unwrap_or_default()
}

/// What to start for `:practice` (all exercises) or `:practice <topic>`: the first exercise that
/// is not solved; then, for all exercises, the first one over par, for a topic its first one.
/// The notice is for the status line.
pub fn choose(
    exercises: &[Exercise],
    progress: &Progress,
    topic: Option<&str>,
) -> anyhow::Result<(usize, Option<&'static str>)> {
    if exercises.is_empty() {
        anyhow::bail!("there are no exercises");
    }
    let candidates = || {
        exercises
            .iter()
            .enumerate()
            .filter(|(_, exercise)| topic.is_none_or(|topic| topic_of(exercise) == topic))
    };
    if candidates().next().is_none() {
        anyhow::bail!(
            "unknown topic `{}`, the topics are: {}",
            topic.unwrap_or_default(),
            topics(exercises).join(", ")
        );
    }
    let first_with = |status: Status| {
        candidates()
            .find(|(_, exercise)| progress.status(exercise) == status)
            .map(|(index, _)| index)
    };
    if let Some(index) = first_with(Status::Unsolved) {
        return Ok((index, None));
    }
    if topic.is_some() {
        return Ok((candidates().next().unwrap().0, None));
    }
    if let Some(index) = first_with(Status::Solved) {
        return Ok((index, None));
    }
    Ok((0, Some("every exercise is at or under par")))
}

/// Shows the attempt if it is the exercise at `index`, else starts that exercise.
fn open(editor: &mut Editor, index: usize) -> anyhow::Result<()> {
    match session::live_attempt(editor) {
        Some(attempt) if attempt.exercise == index => session::focus_attempt(editor),
        _ => start_index(editor, index),
    }
}

pub fn practice(
    cx: &mut compositor::Context,
    args: Args,
    event: PromptEvent,
) -> anyhow::Result<()> {
    if event != PromptEvent::Validate {
        return Ok(());
    }
    // The keys that typed this command don't count, whatever comes of it.
    session::command_ran();

    match args.first() {
        None => resume(cx.editor, None),
        Some("list") => list(cx),
        Some("hint") => hint(cx.editor),
        Some("reset") => {
            let attempt = running(cx.editor)?;
            start_index(cx.editor, attempt.exercise)
        }
        Some("next") => {
            let attempt = running(cx.editor)?;
            if attempt.exercise + 1 >= exercises()?.len() {
                anyhow::bail!("this is the last exercise");
            }
            start_index(cx.editor, attempt.exercise + 1)
        }
        Some("prev") => {
            let attempt = running(cx.editor)?;
            if attempt.exercise == 0 {
                anyhow::bail!("this is the first exercise");
            }
            start_index(cx.editor, attempt.exercise - 1)
        }
        Some("lesson") => {
            session::toggle_panel();
            Ok(())
        }
        Some(topic) => resume(cx.editor, Some(topic)),
    }
}

fn running(editor: &Editor) -> anyhow::Result<session::Attempt> {
    session::live_attempt(editor)
        .ok_or_else(|| anyhow::anyhow!("no exercise is open, start one with :practice"))
}

fn resume(editor: &mut Editor, topic: Option<&str>) -> anyhow::Result<()> {
    let exercises = exercises()?;
    let (progress, error) = session::load_progress();
    let (index, notice) = choose(&exercises, &progress, topic)?;
    open(editor, index)?;
    // after the start, which says if the progress can't be saved
    match (error, notice) {
        (None, Some(notice)) => editor.set_status(notice),
        (Some(error), _) => editor.set_error(format!("progress is not saved: {error}")),
        _ => (),
    }
    Ok(())
}

fn hint(editor: &mut Editor) -> anyhow::Result<()> {
    running(editor)?;
    match session::reveal_hint()? {
        Some((number, text)) => editor.set_status(format!(
            "Hint {number}/{}: {text}",
            super::exercise::HINT_COUNT
        )),
        None => editor.set_status("No more hints"),
    }
    Ok(())
}

fn rows(exercises: &[Exercise], progress: &Progress) -> Vec<Row> {
    exercises
        .iter()
        .enumerate()
        .map(|(index, exercise)| Row {
            index,
            status: progress.status(exercise),
            topic: topic_of(exercise).to_owned(),
            title: exercise.title.clone(),
            best: progress.best(&exercise.id),
            par: exercise.par(),
        })
        .collect()
}

fn list(cx: &mut compositor::Context) -> anyhow::Result<()> {
    let exercises = exercises()?;
    let (progress, error) = session::load_progress();
    if let Some(error) = error {
        cx.editor
            .set_error(format!("progress is not saved: {error}"));
    }
    let initial = choose(&exercises, &progress, None)?.0;
    let rows = rows(&exercises, &progress);
    // Pushed from a callback: a command has no access to the compositor.
    cx.jobs.callback(async move {
        Ok(Callback::EditorCompositor(Box::new(
            move |_editor, compositor| {
                compositor.push(Box::new(overlaid(picker(rows, initial))));
            },
        )))
    });
    Ok(())
}

fn picker(rows: Vec<Row>, initial: usize) -> Picker<Row, ()> {
    let columns = [
        PickerColumn::new(" ", |row: &Row, _: &()| row.status.symbol().into()),
        PickerColumn::new("topic", |row: &Row, _: &()| row.topic.as_str().into()),
        PickerColumn::new("exercise", |row: &Row, _: &()| row.title.as_str().into()),
        PickerColumn::new("best", |row: &Row, _: &()| {
            row.best
                .map(|best| best.to_string())
                .unwrap_or_default()
                .into()
        }),
        PickerColumn::new("par", |row: &Row, _: &()| row.par.to_string().into()),
    ];
    Picker::new(columns, 2, rows, (), |cx, row, _action| {
        if let Err(err) = start_index(cx.editor, row.index) {
            cx.editor.set_error(err.to_string());
        }
    })
    .with_initial_cursor(initial as u32)
}

/// The subcommands and topics that match what is typed.
pub fn complete(_editor: &Editor, input: &str) -> Vec<Completion> {
    let mut words: Vec<String> = SUBCOMMANDS.iter().map(|word| word.to_string()).collect();
    if let Ok(exercises) = exercises() {
        words.extend(topics(&exercises).into_iter().map(str::to_owned));
    }
    fuzzy_match(input, words, false)
        .into_iter()
        .map(|(word, _)| ((0..), word.into()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(id: &str, teach: &str) -> Exercise {
        Exercise {
            id: id.into(),
            title: id.into(),
            topic: String::new(),
            feature: String::new(),
            lesson: String::new(),
            start: String::new(),
            goal: String::new(),
            language: None,
            hints: Vec::new(),
            teach: teach.into(),
            gold: None,
        }
    }

    fn set() -> Vec<Exercise> {
        // par 2 each
        ["a/01-x", "a/02-y", "b/01-z", "b/02-w"]
            .iter()
            .map(|id| exercise(id, "wd"))
            .collect()
    }

    #[test]
    fn topics_in_order() {
        assert_eq!(topics(&set()), ["a", "b"]);
    }

    #[test]
    fn resume_picks_the_first_unsolved() {
        let exercises = set();
        let mut progress = Progress::default();
        assert_eq!(choose(&exercises, &progress, None).unwrap(), (0, None));
        progress.record("a/01-x", 2);
        assert_eq!(choose(&exercises, &progress, None).unwrap(), (1, None));
        progress.record("b/01-z", 2);
        assert_eq!(choose(&exercises, &progress, None).unwrap(), (1, None));
        progress.record("a/02-y", 5);
        assert_eq!(choose(&exercises, &progress, None).unwrap(), (3, None));
    }

    #[test]
    fn resume_when_everything_is_solved() {
        let exercises = set();
        let mut progress = Progress::default();
        for (n, exercise) in exercises.iter().enumerate() {
            progress.record(&exercise.id, if n == 2 { 3 } else { 2 });
        }
        // the first one over par
        assert_eq!(choose(&exercises, &progress, None).unwrap(), (2, None));
        progress.record("b/01-z", 2);
        assert_eq!(
            choose(&exercises, &progress, None).unwrap(),
            (0, Some("every exercise is at or under par"))
        );
    }

    #[test]
    fn a_topic_resumes_in_the_topic() {
        let exercises = set();
        let mut progress = Progress::default();
        assert_eq!(choose(&exercises, &progress, Some("b")).unwrap(), (2, None));
        progress.record("b/01-z", 2);
        assert_eq!(choose(&exercises, &progress, Some("b")).unwrap(), (3, None));
        // all solved: its first exercise, whatever par
        progress.record("b/02-w", 2);
        assert_eq!(choose(&exercises, &progress, Some("b")).unwrap(), (2, None));
        progress.record("b/01-z", 9);
        assert_eq!(choose(&exercises, &progress, Some("b")).unwrap(), (2, None));
    }

    #[test]
    fn an_unknown_topic_lists_the_topics() {
        let err = choose(&set(), &Progress::default(), Some("nope")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown topic `nope`, the topics are: a, b"
        );
        assert!(choose(&[], &Progress::default(), None).is_err());
    }
}
