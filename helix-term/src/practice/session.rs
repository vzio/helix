//! The state of the running attempt, the counting of keys, and the check for the goal.
//!
//! The state is per thread rather than a global, like the marker of `x` on empty lines: the
//! application handles its events on one thread, and the integration tests run an application on
//! each of their threads.
//!
//! [`before_event`] and [`after_event`] wrap the handling of every event that comes from the
//! terminal (keys replayed by a macro or typed by the tests' helpers through the same path count
//! too, the replay of `@` doesn't). The count includes every key, the ones typed into prompts too.
//! `:practice` commands don't count: when the command is run from the command line, the count
//! goes back to what it was before the `:` that opened the command line.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::anyhow;
use helix_core::Rope;
use helix_view::document::Mode;
use helix_view::editor::Action;
use helix_view::input::Event;
use helix_view::{current, Document, DocumentId, Editor};

use super::command::Row;
use super::exercise::{load_exercises, Exercise};
use super::progress::{progress_file, Progress};
use crate::compositor::Compositor;
use crate::ui::overlay::Overlay;
use crate::ui::{Picker, Prompt};

/// The exercise that is being practiced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    /// Index in [`exercises`].
    pub exercise: usize,
    /// The buffer with the exercise.
    pub doc: DocumentId,
    /// Keys pressed so far. Frozen when solved.
    pub keys: usize,
    /// The number of keys it took, once the buffer holds the goal.
    pub solved_at: Option<usize>,
    /// How many hints were revealed.
    pub hints: usize,
    /// The fewest keys this exercise was ever solved with, as of the start of the attempt or
    /// its solution.
    pub best: Option<usize>,
}

#[derive(Default)]
struct Session {
    exercises: Option<Rc<[Exercise]>>,
    attempt: Option<Attempt>,
    panel_hidden: bool,
    /// The count before the key that is being handled.
    count_before_key: usize,
    /// Whether a command line was open after the last key.
    prompt_open: bool,
    /// The count before the key that opened the command line that is open.
    count_before_prompt: Option<usize>,
    /// Set by `:practice`: the keys that typed it don't count.
    restore_count: bool,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

fn with<R>(f: impl FnOnce(&mut Session) -> R) -> R {
    SESSION.with_borrow_mut(f)
}

/// Forgets everything about this thread's session: the attempt, the loaded exercises. For tests
/// that run many applications on a thread.
pub fn reset_session() {
    with(|session| *session = Session::default());
}

/// All the exercises, loaded from the runtime directory on first use.
pub fn exercises() -> anyhow::Result<Rc<[Exercise]>> {
    if let Some(exercises) = with(|session| session.exercises.clone()) {
        return Ok(exercises);
    }
    let loaded: Rc<[Exercise]> = load_exercises()?.into();
    with(|session| session.exercises = Some(loaded.clone()));
    Ok(loaded)
}

/// Uses `exercises` instead of the ones of the runtime directory.
pub fn set_exercises(exercises: Vec<Exercise>) {
    with(|session| session.exercises = Some(exercises.into()));
}

/// The running attempt, if there is one.
pub fn attempt() -> Option<Attempt> {
    with(|session| session.attempt.clone())
}

/// The progress, and what is wrong with the file if it can't be read.
pub(super) fn load_progress() -> (Progress, Option<anyhow::Error>) {
    match Progress::load(&progress_file()) {
        Ok(progress) => (progress, None),
        Err(err) => (Progress::default(), Some(err)),
    }
}

/// Starts the exercise with this id in the current view.
pub fn start_exercise(editor: &mut Editor, id: &str) -> anyhow::Result<()> {
    let exercises = exercises()?;
    let index = exercises
        .iter()
        .position(|exercise| exercise.id == id)
        .ok_or_else(|| anyhow!("no exercise `{id}`"))?;
    start_index(editor, index)
}

/// Starts the exercise at `index` in [`exercises`] in the current view: a new buffer with its
/// text and selection replaces the buffer of the view, and the count starts at 0. A buffer of
/// the attempt before is closed.
pub fn start_index(editor: &mut Editor, index: usize) -> anyhow::Result<()> {
    let exercises = exercises()?;
    let exercise = exercises
        .get(index)
        .ok_or_else(|| anyhow!("no exercise number {index}"))?;
    let (text, selection) = exercise.start_state()?;

    let mut doc = Document::from(
        Rope::from(text),
        None,
        editor.config.clone(),
        editor.syn_loader.clone(),
    );
    // The text has `\n`, whatever line ending is the default.
    doc.line_ending = helix_core::line_ending::LineEnding::LF;
    if let Some(language) = &exercise.language {
        doc.set_language_by_language_id(language, &editor.syn_loader.load())?;
    }

    let doc_id = editor.new_file_from_document(Action::Replace, doc);
    let (view, doc) = current!(editor);
    doc.set_selection(view.id, selection);

    let (best, error) = {
        let (progress, error) = load_progress();
        (progress.best(&exercise.id), error)
    };
    let previous = with(|session| {
        session.restore_count = false;
        session.count_before_prompt = None;
        let previous = session.attempt.replace(Attempt {
            exercise: index,
            doc: doc_id,
            keys: 0,
            solved_at: None,
            hints: 0,
            best,
        });
        // The same exercise again keeps the hints the person has seen.
        if let Some(previous) = &previous {
            if previous.exercise == index {
                session.attempt.as_mut().unwrap().hints = previous.hints;
            }
        }
        previous
    });
    if let Some(previous) = previous {
        if previous.doc != doc_id && editor.documents.contains_key(&previous.doc) {
            let _ = editor.close_document(previous.doc, true);
        }
    }
    if let Some(error) = error {
        editor.set_error(format!("progress is not saved: {error}"));
    }
    Ok(())
}

/// Shows the buffer of the running attempt: in the view that has it, else in the current view.
pub(super) fn focus_attempt(editor: &mut Editor) -> anyhow::Result<()> {
    let attempt =
        attempt().ok_or_else(|| anyhow!("no exercise is open, start one with :practice"))?;
    let shown = editor
        .tree
        .views()
        .find_map(|(view, _)| (view.doc == attempt.doc).then_some(view.id));
    match shown {
        Some(view_id) => editor.focus(view_id),
        None => editor.switch(attempt.doc, Action::Replace),
    }
    Ok(())
}

/// The running attempt of a buffer that is still open.
pub(super) fn live_attempt(editor: &Editor) -> Option<Attempt> {
    attempt().filter(|attempt| editor.documents.contains_key(&attempt.doc))
}

/// `:practice` was run: the keys that typed it don't count.
pub(super) fn command_ran() {
    with(|session| session.restore_count = true);
}

/// Reveals the next hint of the running attempt. Returns its number and text, or `None` when
/// all were shown.
pub(super) fn reveal_hint() -> anyhow::Result<Option<(usize, String)>> {
    let attempt =
        attempt().ok_or_else(|| anyhow!("no exercise is open, start one with :practice"))?;
    let exercises = exercises()?;
    let hints = &exercises[attempt.exercise].hints;
    if attempt.hints >= hints.len() {
        return Ok(None);
    }
    with(|session| session.attempt.as_mut().unwrap().hints += 1);
    Ok(Some((attempt.hints + 1, hints[attempt.hints].clone())))
}

/// Shows or hides the panel, returns whether it shows now.
pub(super) fn toggle_panel() -> bool {
    with(|session| {
        session.panel_hidden = !session.panel_hidden;
        !session.panel_hidden
    })
}

pub(super) fn panel_visible() -> bool {
    with(|session| !session.panel_hidden)
}

/// Ends the session when the buffer of the attempt is gone.
fn sync(editor: &Editor) {
    with(|session| {
        if session
            .attempt
            .as_ref()
            .is_some_and(|attempt| !editor.documents.contains_key(&attempt.doc))
        {
            session.attempt = None;
            session.restore_count = false;
            session.count_before_prompt = None;
        }
    });
}

fn command_line_open(compositor: &mut Compositor) -> bool {
    compositor
        .find::<Prompt>()
        .is_some_and(|prompt| prompt.history_register() == Some(':'))
}

fn picker_open(compositor: &mut Compositor) -> bool {
    compositor.find::<Overlay<Picker<Row, ()>>>().is_some()
}

/// Call before an event from the terminal is handled: counts the keys.
pub fn before_event(event: &Event, editor: &mut Editor, compositor: &mut Compositor) {
    sync(editor);
    if with(|session| session.attempt.is_none()) {
        return;
    }
    // The picker of `:practice list` is open before and after the keys that use it.
    let keys = if picker_open(compositor) {
        0
    } else {
        match event {
            Event::Key(_) => 1,
            Event::Paste(text) => text.chars().count(),
            _ => 0,
        }
    };
    with(|session| {
        let attempt = session.attempt.as_mut().unwrap();
        session.count_before_key = attempt.keys;
        if attempt.solved_at.is_none() {
            attempt.keys += keys;
        }
    });
}

/// Call after an event from the terminal was handled: takes back the keys of `:practice`
/// commands, keeps the buffer of the exercise unmodified so that it never blocks quitting, and
/// checks for the goal.
pub fn after_event(editor: &mut Editor, compositor: &mut Compositor) {
    sync(editor);
    let prompt_open = command_line_open(compositor);
    let doc_id = with(|session| {
        let was_open = std::mem::replace(&mut session.prompt_open, prompt_open);
        let Some(attempt) = session.attempt.as_mut() else {
            session.count_before_prompt = None;
            return None;
        };
        if prompt_open && !was_open {
            session.count_before_prompt = Some(session.count_before_key);
        }
        if std::mem::take(&mut session.restore_count) {
            attempt.keys = session
                .count_before_prompt
                .unwrap_or(session.count_before_key);
        }
        if !prompt_open {
            session.count_before_prompt = None;
        }
        Some(attempt.doc)
    });
    let Some(doc_id) = doc_id else { return };
    if let Some(doc) = editor.documents.get_mut(&doc_id) {
        doc.reset_modified();
    }
    check_solved(editor);
}

/// Whether the buffer holds the goal while in normal mode.
fn check_solved(editor: &mut Editor) {
    let Some(attempt) = attempt().filter(|attempt| attempt.solved_at.is_none()) else {
        return;
    };
    if editor.mode != Mode::Normal {
        return;
    }
    let Ok(exercises) = exercises() else { return };
    let exercise = &exercises[attempt.exercise];
    let Some(doc) = editor.documents.get(&attempt.doc) else {
        return;
    };
    if doc.text() != exercise.goal.as_str() {
        return;
    }
    solved(editor, exercise, attempt.keys);
}

fn solved(editor: &mut Editor, exercise: &Exercise, keys: usize) {
    let path = progress_file();
    let saved = Progress::load(&path).and_then(|mut progress| {
        let new_best = progress.record(&exercise.id, keys);
        progress.save(&path)?;
        Ok((new_best, progress.best(&exercise.id).unwrap_or(keys)))
    });
    with(|session| {
        let attempt = session.attempt.as_mut().unwrap();
        attempt.solved_at = Some(keys);
        if let Ok((_, best)) = &saved {
            attempt.best = Some(*best);
        }
    });
    let par = exercise.par();
    let unit = if keys == 1 { "key" } else { "keys" };
    match saved {
        Ok((true, _)) => {
            editor.set_status(format!("Solved in {keys} {unit} · par {par} · new best"))
        }
        Ok((false, best)) => {
            editor.set_status(format!("Solved in {keys} {unit} · par {par} · best {best}"))
        }
        Err(err) => editor.set_error(format!(
            "Solved in {keys} {unit} · par {par} · progress is not saved: {err}"
        )),
    }
}
