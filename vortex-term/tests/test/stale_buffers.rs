//! Buffers whose file changed on disk: reloaded when nothing is lost, protected otherwise.
//!
//! The disk is changed with `std::fs::write` and its modification time moved forward, so the
//! change is visible whatever the resolution of the file system's clock.

use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_term::application::Application;
use vortex_view::{current_ref, editor::Severity, input::parse_macro};

use termina::event::{Event, KeyEvent};

use super::*;

const FOCUS_GAINED: Event = Event::FocusIn;

struct Harness {
    app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<io::Result<Event>>,
    rx: UnboundedReceiverStream<io::Result<Event>>,
    _dir: tempfile::TempDir,
    files: Vec<PathBuf>,
}

impl Harness {
    /// An editor with these files (name, content) open, the first one focused.
    fn new(files: &[(&str, &str)]) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()?;
        let mut builder = AppBuilder::new();
        let mut paths = Vec::new();
        for (name, content) in files {
            // paths are compared canonicalized
            let path = std::fs::canonicalize(dir.path())?.join(name);
            std::fs::write(&path, content)?;
            builder = builder.with_file(&path, None);
            paths.push(path);
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Ok(Self {
            app: builder.build()?,
            tx,
            rx: UnboundedReceiverStream::new(rx),
            _dir: dir,
            files: paths,
        })
    }

    async fn event(&mut self, event: Event) {
        self.tx.send(Ok(event)).unwrap();
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            self.app.event_loop_until_idle(&mut self.rx),
        )
        .await;
        // callbacks queued by the hooks run in the next iteration
        let _ = tokio::time::timeout(
            Duration::from_millis(300),
            self.app.event_loop_until_idle(&mut self.rx),
        )
        .await;
    }

    async fn keys(&mut self, keys: &str) -> anyhow::Result<()> {
        for key in parse_macro(keys)? {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key))))?;
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            self.app.event_loop_until_idle(&mut self.rx),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_millis(300),
            self.app.event_loop_until_idle(&mut self.rx),
        )
        .await;
        Ok(())
    }

    async fn focus_gained(&mut self) {
        self.event(FOCUS_GAINED).await;
    }

    fn text(&self) -> String {
        current_ref!(self.app.editor).1.text().to_string()
    }

    fn cursor_line(&self) -> usize {
        let (view, doc) = current_ref!(self.app.editor);
        let text = doc.text().slice(..);
        text.char_to_line(doc.selection(view.id).primary().cursor(text))
    }

    fn status(&self) -> Option<(String, Severity)> {
        self.app
            .editor
            .get_status()
            .map(|(message, severity)| (message.to_string(), *severity))
    }

    fn disk(&self, index: usize) -> String {
        std::fs::read_to_string(&self.files[index]).unwrap()
    }
}

/// Writes the file and moves its modification time two seconds ahead of what it was.
fn change_on_disk(path: &Path, content: &str) {
    let before = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::write(path, content).unwrap();
    set_mtime(path, before.max(SystemTime::now()) + Duration::from_secs(2));
}

fn set_mtime(path: &Path, time: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

const OLD: &str = "one\ntwo\nthree\nfour\n";
const NEW: &str = "one\ntwo\nthree\nfour\nfive\n";

#[tokio::test(flavor = "multi_thread")]
async fn unmodified_buffer_is_reloaded_on_focus_and_undoable() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    h.keys("jj").await?;
    assert_eq!(h.cursor_line(), 2);

    change_on_disk(&h.files[0], NEW);
    h.focus_gained().await;

    assert_eq!(h.text(), NEW);
    assert_eq!(h.cursor_line(), 2);
    assert!(
        !matches!(h.status(), Some((_, Severity::Warning | Severity::Error))),
        "{:?}",
        h.status()
    );

    h.keys("u").await?;
    assert_eq!(h.text(), OLD);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn modified_buffer_warns_once_and_blocks_write() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    h.keys("ix<esc>").await?;
    let mine = h.text();
    assert_eq!(mine, format!("x{OLD}"));

    change_on_disk(&h.files[0], NEW);
    h.focus_gained().await;
    let (message, severity) = h.status().expect("a warning");
    assert_eq!(severity, Severity::Warning);
    assert!(message.contains("changed on disk"), "{message}");
    assert_eq!(h.text(), mine);

    // the same disk content does not warn again
    h.app.editor.clear_status();
    h.focus_gained().await;
    assert_eq!(h.status(), None);

    h.keys(":w<ret>").await?;
    let (message, severity) = h.status().expect("an error");
    assert_eq!(severity, Severity::Error);
    assert!(message.contains("changed on disk"), "{message}");
    assert_eq!(h.disk(0), NEW);

    h.keys(":w!<ret>").await?;
    assert_eq!(h.disk(0), mine);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_clears_the_protection() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    h.keys("ix<esc>").await?;
    change_on_disk(&h.files[0], NEW);

    h.keys(":reload<ret>").await?;
    assert_eq!(h.text(), NEW);

    h.keys("iy<esc>:w<ret>").await?;
    assert_eq!(h.disk(0), format!("y{NEW}"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn touched_file_does_not_block_write() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    h.keys("ix<esc>").await?;
    let mine = h.text();

    change_on_disk(&h.files[0], OLD);
    h.focus_gained().await;
    assert_eq!(h.status(), None);
    assert_eq!(h.text(), mine);

    h.keys(":w<ret>").await?;
    assert_eq!(h.disk(0), mine);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_to_a_buffer_reloads_it() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD), ("b.txt", "b1\nb2\n")])?;
    change_on_disk(&h.files[1], "b1\nb2\nb3\n");
    h.keys(":buffer-next<ret>").await?;
    assert_eq!(h.text(), "b1\nb2\nb3\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn focus_checks_buffers_that_are_not_shown() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD), ("b.txt", "b1\nb2\n")])?;
    h.keys(":buffer-next<ret>:buffer-previous<ret>").await?;
    change_on_disk(&h.files[1], "b1\nb2\nb3\n");
    h.focus_gained().await;
    h.keys(":buffer-next<ret>").await?;
    assert_eq!(h.text(), "b1\nb2\nb3\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn writing_an_unmodified_buffer_reloads_it_instead() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    change_on_disk(&h.files[0], NEW);
    h.keys(":w<ret>").await?;
    assert_eq!(h.text(), NEW);
    assert_eq!(h.disk(0), NEW);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn deleted_file_is_left_alone() -> anyhow::Result<()> {
    let mut h = Harness::new(&[("a.txt", OLD)])?;
    std::fs::remove_file(&h.files[0])?;
    h.app.editor.clear_status();
    h.focus_gained().await;
    assert_eq!(h.text(), OLD);
    assert_eq!(h.status(), None);
    Ok(())
}
