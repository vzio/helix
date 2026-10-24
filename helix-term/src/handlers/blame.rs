//! Inline git blame (fork feature): computing the blame of documents in the background and
//! delaying when it shows for the cursor line.
//!
//! The blame of a file is expensive to compute (it walks the history), so it is never done on
//! the UI thread and the result is cached in the document ([`DocumentBlame`]). Requests are
//! made with [`request`] when something may have changed it: a document is opened, saved or
//! reloaded, the terminal regains focus, or the feature is turned on. Each of them only costs
//! reading `HEAD` when it didn't change. At most one computation runs per document (requests
//! made meanwhile are coalesced into one rerun) and at most [`MAX_COMPUTATIONS`] run at all.
//!
//! When the feature is disabled nothing is requested.

use std::time::Duration;

use helix_event::{register_hook, send_blocking};
use helix_loader::workspace_trust::TrustQuery;
use helix_vcs::{head_commit_id, BlameError, DiffProviderRegistry, FileBlame};
use helix_view::{
    blame::{BlameOutcome, BlameRequest, BlameState, Finished},
    editor::InlineBlameShow,
    events::{ConfigDidChange, DocumentDidOpen, SelectionDidChange},
    handlers::{BlameEvent, Handlers},
    DocumentId, Editor, ViewId,
};
use tokio::{sync::Semaphore, time::Instant};

use crate::job;

/// How many blames are computed at the same time, over all documents.
const MAX_COMPUTATIONS: usize = 2;
static COMPUTATIONS: Semaphore = Semaphore::const_new(MAX_COMPUTATIONS);

/// Request the blame of a document to be brought up to date, if inline blame is enabled.
pub fn request(editor: &mut Editor, doc_id: DocumentId) {
    if editor.config().inline_blame.enable {
        request_unconditionally(editor, doc_id);
    }
}

/// Like [`request`] for all documents.
pub fn request_all(editor: &mut Editor) {
    if !editor.config().inline_blame.enable {
        return;
    }
    let ids: Vec<_> = editor.documents().map(|doc| doc.id()).collect();
    for id in ids {
        request_unconditionally(editor, id);
    }
}

/// Request the blame of a document even if inline blame is disabled. For explicit requests of
/// the user.
pub(crate) fn request_unconditionally(editor: &mut Editor, doc_id: DocumentId) {
    let trust_full = doc_trust_full(editor, doc_id);
    let diff_providers = editor.diff_providers.clone();
    let Some(doc) = editor.document_mut(doc_id) else {
        return;
    };
    // scratch buffers have nothing to blame
    let Some(path) = doc.path().map(ToOwned::to_owned) else {
        return;
    };
    // (there is no runtime in some tests)
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let Some(request) = doc.blame.begin(&path) else {
        // a computation is in flight, it will start another one when it is done
        return;
    };
    tokio::spawn(async move {
        let generation = request.generation;
        let result = compute(request, diff_providers, trust_full).await;
        job::dispatch(move |editor, _| finish(editor, doc_id, generation, result)).await;
    });
}

fn doc_trust_full(editor: &Editor, doc_id: DocumentId) -> bool {
    editor.document(doc_id).is_some_and(|doc| {
        editor
            .workspace_trust
            .query(doc.workspace_root(), TrustQuery::Git)
            .is_trusted()
    })
}

/// What a computation found out and, if the document moved to another commit, the new text of
/// the file at `HEAD` for its diff.
struct Computed {
    outcome: BlameOutcome,
    diff_base: Option<Vec<u8>>,
}

impl From<BlameOutcome> for Computed {
    fn from(outcome: BlameOutcome) -> Self {
        Self {
            outcome,
            diff_base: None,
        }
    }
}

fn finish(editor: &mut Editor, doc_id: DocumentId, generation: u64, computed: Computed) {
    let Some(doc) = editor.document_mut(doc_id) else {
        return;
    };
    match doc.blame.finish(generation, computed.outcome) {
        Finished::Stale => (),
        Finished::Applied { rerun } => {
            // The blame is for the new `HEAD`, which the diff has to be against too to find
            // out which lines of the buffer are not in it. (Nothing else updates it.)
            if let Some(diff_base) = computed.diff_base {
                doc.set_diff_base(diff_base);
            }
            if let Some(line) = doc.blame.take_status_line() {
                crate::commands::blame::show_in_statusline(editor, doc_id, line);
            }
            if rerun {
                request_unconditionally(editor, doc_id);
            }
            helix_event::request_redraw();
        }
    }
}

/// Find out the blame of the file of `request`. Runs on the runtime, the blocking parts on
/// threads for blocking.
async fn compute(
    request: BlameRequest,
    diff_providers: DiffProviderRegistry,
    trust_full: bool,
) -> Computed {
    let BlameRequest {
        path,
        cached_head,
        is_refresh,
        ..
    } = request;

    // The cheap part first, without waiting for a place: is what we have still good?
    let head = {
        let path = path.clone();
        blocking(move || head_commit_id(&path)).await
    };
    match head {
        Ok(Ok(head)) if Some(head) == cached_head => return BlameOutcome::Unchanged.into(),
        Ok(Ok(_)) => (),
        Ok(Err(err)) => return outcome_of_error(&path, err).into(),
        Err(()) => return BlameOutcome::Unavailable.into(),
    }

    let Ok(_permit) = COMPUTATIONS.acquire().await else {
        return BlameOutcome::Unavailable.into();
    };
    let computed = {
        let path = path.clone();
        blocking(move || {
            let blame = FileBlame::try_new(&path);
            let diff_base = match (&blame, is_refresh) {
                (Ok(_), true) => diff_providers.get_diff_base(&path, trust_full),
                _ => None,
            };
            (blame, diff_base)
        })
        .await
    };
    match computed {
        Ok((Ok(blame), diff_base)) => Computed {
            outcome: BlameOutcome::Ready(blame),
            diff_base,
        },
        Ok((Err(err), _)) => outcome_of_error(&path, err).into(),
        Err(()) => BlameOutcome::Unavailable.into(),
    }
}

/// Run `f` on a thread for blocking work. `Err` if it panicked.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ()> {
    tokio::task::spawn_blocking(f).await.map_err(|err| {
        log::debug!("blame task failed: {err}");
    })
}

/// Files outside of repositories are the normal case and not worth a message.
fn outcome_of_error(path: &std::path::Path, err: BlameError) -> BlameOutcome {
    match err {
        BlameError::NotInRepository => BlameOutcome::Unavailable,
        BlameError::NotCommitted => BlameOutcome::NotCommitted,
        BlameError::Other(err) => {
            log::debug!("no blame for {}: {err:#}", path.display());
            BlameOutcome::Unavailable
        }
    }
}

/// Makes blame of the cursor line show once the cursor stayed on it for the configured delay.
///
/// Every event for a different line starts the delay over. Repeating the event for the line
/// being waited for changes nothing, so it is fine to send it repeatedly while rendering.
#[derive(Default)]
pub(super) struct DelayHandler {
    pending: Option<BlameEvent>,
}

impl helix_event::AsyncHook for DelayHandler {
    type Event = BlameEvent;

    fn handle_event(&mut self, event: BlameEvent, timeout: Option<Instant>) -> Option<Instant> {
        if let (Some(pending), Some(timeout)) = (&self.pending, timeout) {
            if (pending.doc, pending.view, pending.line) == (event.doc, event.view, event.line) {
                return Some(timeout);
            }
        }
        let timeout = Instant::now() + event.delay;
        self.pending = Some(event);
        Some(timeout)
    }

    fn finish_debounce(&mut self) {
        let Some(event) = self.pending.take() else {
            return;
        };
        job::dispatch_blocking(move |editor, _| {
            let still_there = editor.tree.try_get(event.view).is_some_and(|view| {
                view.doc == event.doc
                    && editor
                        .document(event.doc)
                        .is_some_and(|doc| cursor_line(doc, event.view) == Some(event.line))
            });
            if still_there {
                if let Some(doc) = editor.document_mut(event.doc) {
                    doc.blame.set_ready_line(event.view, event.line);
                }
                helix_event::request_redraw();
            }
        });
    }
}

fn cursor_line(doc: &helix_view::Document, view: ViewId) -> Option<usize> {
    let selection = doc.selections().get(&view)?;
    Some(selection.primary().cursor_line(doc.text().slice(..)))
}

/// Ask for `line` of the document to be marked as ready to show blame for, after the delay.
pub fn arm_delay(editor: &Editor, doc_id: DocumentId, view: ViewId, line: usize, delay: u64) {
    send_blocking(
        &editor.handlers.blame,
        BlameEvent {
            doc: doc_id,
            view,
            line,
            delay: Duration::from_millis(delay),
        },
    );
}

pub(super) fn register_hooks(handlers: &Handlers) {
    register_hook!(move |event: &mut DocumentDidOpen<'_>| {
        request(event.editor, event.doc);
        Ok(())
    });

    let tx = handlers.blame.clone();
    register_hook!(move |event: &mut SelectionDidChange<'_>| {
        let config = event.doc.config.load();
        let blame_config = &config.inline_blame;
        if !blame_config.enable || blame_config.show != InlineBlameShow::CursorLine {
            return Ok(());
        }
        let Some(line) = cursor_line(event.doc, event.view) else {
            return Ok(());
        };
        if event.doc.blame.ready_line(event.view) == Some(line) {
            return Ok(());
        }
        // moving to another line hides the blame until the delay is over
        event.doc.blame.clear_ready_line(event.view);
        if blame_config.delay > 0 && event.doc.path().is_some() {
            send_blocking(
                &tx,
                BlameEvent {
                    doc: event.doc.id(),
                    view: event.view,
                    line,
                    delay: Duration::from_millis(blame_config.delay),
                },
            );
        }
        Ok(())
    });

    register_hook!(move |event: &mut ConfigDidChange<'_>| {
        match (event.old.inline_blame.enable, event.new.inline_blame.enable) {
            // any document could have outdated blame
            (false, true) => request_all(event.editor),
            // don't hold on to what is not shown
            (true, false) => {
                for doc in event.editor.documents_mut() {
                    doc.blame.reset();
                }
            }
            _ => (),
        }
        Ok(())
    });
}

/// The blame of a line as shown by the `blame_line` command, or why there is none.
pub(crate) fn describe(
    editor: &Editor,
    doc_id: DocumentId,
    line: usize,
) -> Result<String, &'static str> {
    let doc = editor.document(doc_id).ok_or("No such document")?;
    match doc.blame.state() {
        BlameState::Unknown | BlameState::Pending => Err("Blame is not ready yet"),
        BlameState::Unavailable => Err("No blame available for this file"),
        BlameState::NotCommitted | BlameState::Ready(_) => doc
            .line_blame(line)
            .display(&editor.config().inline_blame.format)
            .ok_or("No blame available for this line"),
    }
}
