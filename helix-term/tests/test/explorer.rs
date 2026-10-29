use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use helix_term::application::Application;
use helix_view::input::parse_macro;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[cfg(windows)]
use crossterm::event::{Event, KeyEvent};
#[cfg(not(windows))]
use termina::event::{Event, KeyEvent};

use super::*;

/// A directory `a` holding `x.txt` and `y.txt`, inside the workspace the tests run in (the tree is
/// rooted at the workspace of the working directory, which the tests can't change). It has a
/// `.gitignore` for `secret.txt` and `build/`, which exist.
fn fixture() -> anyhow::Result<(tempfile::TempDir, PathBuf, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("explorer-test-")
        .tempdir_in(env!("CARGO_MANIFEST_DIR"))?;
    let a = dir.path().join("a");
    std::fs::create_dir(&a)?;
    let x = a.join("x.txt");
    let y = a.join("y.txt");
    std::fs::write(&x, "one\ntwo\nthree\n")?;
    std::fs::write(&y, "")?;
    std::fs::write(dir.path().join(".gitignore"), "secret.txt\nbuild/\n")?;
    std::fs::write(a.join("secret.txt"), "")?;
    std::fs::create_dir(a.join("build"))?;
    std::fs::write(a.join("build/out.txt"), "")?;
    Ok((dir, x, y))
}

/// The name of the fixture directory, which a filter can use to find the fixture only.
fn name_of(dir: &tempfile::TempDir) -> String {
    dir.path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

fn current_path(app: &Application) -> PathBuf {
    let path = helix_view::doc!(app.editor)
        .path()
        .expect("buffer has a path");
    canonical(path)
}

/// Drives an application with keys, and waits for the results of background tasks.
struct Session {
    app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    input: UnboundedReceiverStream<std::io::Result<Event>>,
}

impl Session {
    fn new(app: Application) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app,
            tx,
            input: UnboundedReceiverStream::new(rx),
        }
    }

    /// A session with `file` open.
    fn on(file: &Path) -> Self {
        Self::new(
            helpers::AppBuilder::new()
                .with_file(file, None)
                .build()
                .unwrap(),
        )
    }

    async fn keys(&mut self, keys: &str) {
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// A terminal of `width` by `height` cells.
    async fn resize(&mut self, width: u16, height: u16) {
        self.app.resize_screen(width, height);
        #[cfg(windows)]
        let event = Event::Resize(width, height);
        #[cfg(not(windows))]
        let event = Event::WindowResized(termina::WindowSize {
            cols: width,
            rows: height,
            pixel_width: None,
            pixel_height: None,
        });
        self.tx.send(Ok(event)).unwrap();
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Run until `condition` is true of the screen.
    async fn wait_for_screen(&mut self, what: &str, condition: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !condition(&self.rows()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}:\n{:#?}",
                self.rows()
            );
            // (the idle timer is off after the last time it ran out)
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    fn rows(&self) -> Vec<String> {
        let screen = self.app.screen();
        (0..screen.area.height)
            .map(|y| {
                (0..screen.area.width)
                    .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                    .collect()
            })
            .collect()
    }

    fn status(&self) -> String {
        self.app
            .editor
            .get_status()
            .map(|(message, _)| message.to_string())
            .unwrap_or_default()
    }

    /// Whether the tree is open: the frame of the overlay is on the first row.
    fn tree_is_open(&self) -> bool {
        self.rows()[0].starts_with('┌')
    }

    async fn close(mut self) {
        for key in parse_macro("<esc><esc><esc>:qa!<ret>").unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        // (this one ends the application)
        while self.app.event_loop_until_idle(&mut self.input).await {}
        let errors = self.app.close().await;
        assert!(errors.is_empty(), "{errors:?}");
    }
}

/// The rows of the entries on the screen: not the prompt of the overlay at the top, nor the status
/// line and the command line at the bottom.
fn overlay(rows: &[String]) -> &[String] {
    &rows[3.min(rows.len())..rows.len().saturating_sub(2).max(3.min(rows.len()))]
}

/// The columns of the tree: the left half of the overlay.
fn tree_side(row: &str) -> String {
    row.chars().take(60).collect()
}

/// Whether the tree shows `text`. Not the preview on the right.
fn on_screen(rows: &[String], text: &str) -> bool {
    overlay(rows)
        .iter()
        .any(|row| tree_side(row).contains(text))
}

/// Whether the preview shows `text`.
fn in_preview(rows: &[String], text: &str) -> bool {
    overlay(rows)
        .iter()
        .any(|row| row.chars().skip(60).collect::<String>().contains(text))
}

fn row_with<'a>(rows: &'a [String], text: &str) -> Option<&'a str> {
    overlay(rows)
        .iter()
        .map(String::as_str)
        .find(|row| tree_side(row).contains(text))
}

/// The row of the selected entry: the picker marks it with `>`. Only the left half, where the
/// tree is, since the preview changes with the selection.
fn selected(rows: &[String]) -> String {
    overlay(rows)
        .iter()
        .find(|row| row.contains(" > "))
        .map(|row| tree_side(row))
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn space_e_opens_the_tree_at_the_root_with_nothing_expanded() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>e").await;
    helpers::assert_status_not_error(&session.app.editor);
    let rows = session.rows();
    // The directories of the repository, collapsed; the file of the buffer is not revealed.
    assert!(on_screen(&rows, "+ helix-term/"), "{rows:#?}");
    assert!(on_screen(&rows, "+ book/"), "{rows:#?}");
    assert!(!on_screen(&rows, "x.txt"), "{rows:#?}");
    // Files have a blank where the sign of a directory is.
    let cargo = row_with(&rows, "Cargo.toml").expect("Cargo.toml is listed");
    assert!(
        cargo.contains("   Cargo.toml") && !cargo.contains("- Cargo") && !cargo.contains("+ Cargo"),
        "{cargo}"
    );
    assert!(
        session.status().contains("s filter"),
        "{}",
        session.status()
    );
    // The buffer is as it was.
    assert_eq!(current_path(&session.app), canonical(&x));
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_lists_directories_first_and_never_git() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>e").await;
    let rows = session.rows();
    let at = |text: &str| rows.iter().position(|row| row.contains(text)).unwrap();
    // Directories first, then files, in the order of the names regardless of case.
    assert!(at("+ book/") < at("+ helix-core/"));
    assert!(at("+ helix-core/") < at("+ xtask/"));
    assert!(at("+ xtask/") < at("Cargo.toml"));
    assert!(at("Cargo.toml") < at("README.md"));
    assert!(!on_screen(&rows, ".git/"), "{rows:#?}");
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn space_shift_e_reveals_the_current_file() -> anyhow::Result<()> {
    let (dir, x, y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>E").await;
    let rows = session.rows();
    assert!(
        on_screen(&rows, &format!("- {}/", name_of(&dir))),
        "{rows:#?}"
    );
    let a = row_with(&rows, "- a/").expect("a is expanded");
    let x_row = row_with(&rows, "x.txt").expect("x.txt is shown");
    assert!(on_screen(&rows, "y.txt"));
    // Two columns of indentation per level: `x.txt` is below `a`.
    assert!(a.find("- a/").unwrap() + 2 <= x_row.find("x.txt").unwrap() - 2);
    // The cursor is on the current file: `j` goes to the next, enter opens it.
    assert!(selected(&rows).contains("x.txt"), "{rows:#?}");
    session.keys("j<ret>").await;
    helpers::assert_status_not_error(&session.app.editor);
    assert_eq!(current_path(&session.app), canonical(&y));
    assert!(!session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn l_h_and_enter_expand_and_collapse() -> anyhow::Result<()> {
    let (_dir, x, y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>E").await;
    // `h` goes from the file to its directory, then collapses it.
    session.keys("h").await;
    assert!(selected(&session.rows()).contains("- a/"));
    session.keys("h").await;
    let rows = session.rows();
    assert!(selected(&rows).contains("+ a/"), "{rows:#?}");
    assert!(!on_screen(&rows, "x.txt"));
    // `l` expands, and moves into the directory once it is expanded.
    session.keys("l").await;
    let rows = session.rows();
    assert!(selected(&rows).contains("- a/"), "{rows:#?}");
    assert!(on_screen(&rows, "x.txt"));
    session.keys("l").await;
    assert!(!selected(&session.rows()).contains("a/"));
    // Enter on a directory toggles it.
    session.keys("h<ret>").await;
    assert!(!on_screen(&session.rows(), "x.txt"));
    session.keys("<ret>").await;
    assert!(on_screen(&session.rows(), "x.txt"));
    // The arrows do the same.
    session.keys("<left>").await;
    assert!(!on_screen(&session.rows(), "x.txt"));
    session.keys("<right>").await;
    assert!(on_screen(&session.rows(), "x.txt"));
    // Nothing of that opened a buffer or closed the tree.
    assert_eq!(current_path(&session.app), canonical(&x));
    assert!(session.tree_is_open());

    // `l` on a file opens it and closes the tree.
    session.keys("jjjjl").await;
    assert_eq!(current_path(&session.app), canonical(&y));
    assert!(!session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn movement_keys_move_the_selection() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>e").await;
    let top = selected(&session.rows());
    for (keys, moved) in [
        ("j", true),
        ("jk", false),
        ("<down>", true),
        ("<down><up>", false),
        ("<C-n>", true),
        ("<C-n><C-p>", false),
        ("<tab>", true),
        ("<tab><S-tab>", false),
        ("<C-d><C-u>", false),
        ("<pagedown><pageup>", false),
        ("G", true),
        ("Gk", true),
        ("gg", false),
        ("ge", true),
        ("<end>", true),
        ("<end><home>", false),
    ] {
        session.keys("<home>").await;
        assert_eq!(selected(&session.rows()), top, "at the top for {keys}");
        session.keys(keys).await;
        assert_eq!(selected(&session.rows()) != top, moved, "{keys}");
    }
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn files_open_in_the_current_view_or_in_splits() -> anyhow::Result<()> {
    let (_dir, x, y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>E").await;
    session.keys("j<ret>").await;
    assert_eq!(current_path(&session.app), canonical(&y));
    assert!(!session.tree_is_open());
    // The editor has the keys again: `i` types in the buffer.
    session.keys("ihi<esc>").await;
    assert_eq!(
        helix_view::doc!(session.app.editor).text().to_string(),
        "hi"
    );

    // `C-v` and `C-s` open the file in a split.
    session.keys("<space>E").await;
    session.keys("k<C-v>").await;
    assert_eq!(session.app.editor.tree.views().count(), 2);
    assert_eq!(current_path(&session.app), canonical(&x));
    assert!(!session.tree_is_open());
    session.keys("<space>E").await;
    session.keys("j<C-s>").await;
    assert_eq!(session.app.editor.tree.views().count(), 3);
    assert_eq!(current_path(&session.app), canonical(&y));
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn s_filters_the_workspace_and_keeps_the_parents() -> anyhow::Result<()> {
    let (dir, x, y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>es").await;
    session.keys(&format!("'{} 'y.txt", name_of(&dir))).await;
    // The walk of the workspace is in the background.
    session
        .wait_for_screen("the filtered tree", |rows| {
            on_screen(rows, "y.txt") && !on_screen(rows, "+ book/")
        })
        .await;
    let rows = session.rows();
    // The match with the directories above it, expanded, and nothing else.
    for expected in [
        "- helix-term/".to_string(),
        format!("- {}/", name_of(&dir)),
        "- a/".to_string(),
        "y.txt".to_string(),
    ] {
        assert!(on_screen(&rows, &expected), "{expected}: {rows:#?}");
    }
    assert!(!on_screen(&rows, "x.txt"), "{rows:#?}");
    assert!(!on_screen(&rows, "helix-core/"), "{rows:#?}");
    // The cursor is on the best match, and the prompt shows the query with the cursor in it.
    assert!(selected(&rows).contains("y.txt"), "{rows:#?}");
    assert!(on_screen(&rows, "y.txt"));

    // Enter on the match opens it and closes the tree.
    session.keys("<ret>").await;
    assert_eq!(current_path(&session.app), canonical(&y));
    assert!(!session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_is_not_the_filter_key_any_more() -> anyhow::Result<()> {
    let (dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>E").await;
    let before = session.rows();
    // Neither `/` nor the letters after it do anything in tree mode: they don't reach the prompt.
    session.keys("/zz").await;
    assert_eq!(session.rows(), before);
    assert!(session.tree_is_open());

    // `s` does, and the letters are typed from then on.
    session.keys(&format!("s'{} 'y.txt", name_of(&dir))).await;
    session
        .wait_for_screen("the filtered tree", |rows| !on_screen(rows, "+ book/"))
        .await;
    assert!(on_screen(&session.rows(), "y.txt"));
    // A `/` in filter mode is a character of the query.
    session.keys("/").await;
    assert!(session.rows()[1].contains("y.txt/"), "{:#?}", session.rows());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_a_directory_without_hits_in_the_filter_shows_it_in_the_tree() -> anyhow::Result<()>
{
    let (dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    // `/a$` matches the directory `a` but nothing in it.
    session.keys("<space>es").await;
    session.keys(&format!("'{} /a$", name_of(&dir))).await;
    session
        .wait_for_screen("the filtered tree", |rows| {
            on_screen(rows, "+ a/") && !on_screen(rows, "+ book/")
        })
        .await;
    assert!(selected(&session.rows()).contains("+ a/"));

    session.keys("<ret>").await;
    let rows = session.rows();
    // The filter is gone and the directory is expanded in the tree, with the cursor on it.
    assert!(selected(&rows).contains("- a/"), "{rows:#?}");
    assert!(on_screen(&rows, "x.txt"), "{rows:#?}");
    assert!(on_screen(&rows, "+ book/"), "{rows:#?}");
    assert!(!rows[1].contains('$'), "{rows:#?}");
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_filter_skips_ignored_files_and_git() -> anyhow::Result<()> {
    let (dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    // In the tree, ignored entries are listed.
    session.keys("<space>E").await;
    assert!(on_screen(&session.rows(), "secret.txt"));
    assert!(on_screen(&session.rows(), "build/"));
    session.keys("s").await;
    session.keys(&format!("'{} 'txt", name_of(&dir))).await;
    session
        .wait_for_screen("the filtered tree", |rows| !on_screen(rows, "+ book/"))
        .await;
    let rows = session.rows();
    assert!(
        on_screen(&rows, "x.txt") && on_screen(&rows, "y.txt"),
        "{rows:#?}"
    );
    // ... but not in the filter, which does not walk what git ignores.
    assert!(!on_screen(&rows, "secret.txt"), "{rows:#?}");
    assert!(!on_screen(&rows, "out.txt"), "{rows:#?}");

    // Nothing of `.git` either.
    session
        .keys(&"<backspace>".repeat(name_of(&dir).len() + 10))
        .await;
    session.keys(".git/HEAD").await;
    let rows = session.rows();
    assert!(!on_screen(&rows, "HEAD"), "{rows:#?}");
    assert!(!on_screen(&rows, "x.txt"), "{rows:#?}");
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn hide_gitignored_hides_what_git_ignores() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut config = helpers::test_config();
    config.editor.explorer.hide_gitignored = true;
    let mut session = Session::new(
        helpers::AppBuilder::new()
            .with_config(config)
            .with_file(&x, None)
            .build()?,
    );

    session.keys("<space>E").await;
    let rows = session.rows();
    assert!(on_screen(&rows, "x.txt"), "{rows:#?}");
    assert!(!on_screen(&rows, "secret.txt"), "{rows:#?}");
    assert!(!on_screen(&rows, "build/"), "{rows:#?}");
    // `H` shows them for this session.
    session.keys("H").await;
    let rows = session.rows();
    assert!(
        on_screen(&rows, "secret.txt") && on_screen(&rows, "build/"),
        "{rows:#?}"
    );
    assert!(selected(&rows).contains("x.txt"));
    session.keys("H").await;
    assert!(!on_screen(&session.rows(), "secret.txt"));
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn escape_leaves_the_filter_mode_then_clears_the_filter_then_closes() -> anyhow::Result<()> {
    let (dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>es").await;
    session.keys(&format!("'{} 'y.txt", name_of(&dir))).await;
    session
        .wait_for_screen("the filtered tree", |rows| {
            on_screen(rows, "y.txt") && !on_screen(rows, "+ book/")
        })
        .await;
    assert!(selected(&session.rows()).contains("y.txt"));

    // Esc leaves the filter mode and keeps the filter: the letters are keys again, and they
    // work on the filtered tree.
    session.keys("<esc>").await;
    assert!(on_screen(&session.rows(), "y.txt"));
    assert!(!on_screen(&session.rows(), "+ book/"));
    session.keys("k").await;
    assert!(
        selected(&session.rows()).contains("- a/"),
        "{:#?}",
        session.rows()
    );
    // Collapsing a directory hides its matches.
    session.keys("h").await;
    assert!(
        !on_screen(&session.rows(), "y.txt"),
        "{:#?}",
        session.rows()
    );
    session.keys("l").await;
    assert!(on_screen(&session.rows(), "y.txt"));
    // `/` resumes the editing of the filter, here a looser one.
    session
        .keys("s<backspace><backspace><backspace><backspace><backspace><backspace>")
        .await;
    session.keys("<esc>").await;
    assert!(session.tree_is_open());
    assert!(on_screen(&session.rows(), "x.txt"), "{:#?}", session.rows());

    // Esc in tree mode clears the filter: the tree is back, with the entry selected in it.
    session.keys("<esc>").await;
    let rows = session.rows();
    assert!(on_screen(&rows, "+ book/"), "{rows:#?}");
    assert!(selected(&rows).contains("explorer-test-"), "{rows:#?}");
    // Esc again closes it.
    session.keys("<esc>").await;
    assert!(!session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn q_closes_the_tree_but_is_a_letter_in_the_filter() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<space>e").await;
    assert!(session.tree_is_open());
    session.keys("q").await;
    assert!(!session.tree_is_open());
    session.keys("<space>esq").await;
    assert!(session.tree_is_open());
    session.keys("<C-c>").await;
    assert!(!session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_created_renamed_moved_and_deleted() -> anyhow::Result<()> {
    let (dir, x, y) = fixture()?;
    let a = x.parent().unwrap().to_path_buf();
    let mut session = Session::on(&x);

    // New file next to the selected one, the cursor lands on it.
    session.keys("<space>Ea").await;
    session.keys("new.txt<ret>").await;
    session
        .wait_for_screen("the new file", |rows| on_screen(rows, "new.txt"))
        .await;
    assert!(a.join("new.txt").is_file());
    assert!(selected(&session.rows()).contains("new.txt"));
    assert!(session.tree_is_open());

    // A name ending in `/` makes a directory, and the missing parents too.
    session.keys("a").await;
    session.keys("b/c/<ret>").await;
    session
        .wait_for_screen("the new directory", |rows| on_screen(rows, "+ c/"))
        .await;
    assert!(a.join("b/c").is_dir());
    assert!(selected(&session.rows()).contains("+ c/"));

    // An existing name is refused, nothing is overwritten.
    session.keys("a").await;
    session.keys("../../new.txt<ret>").await;
    assert!(
        session.status().contains("already exists"),
        "{}",
        session.status()
    );
    assert!(session.tree_is_open());

    // Rename the file of the buffer, which follows it.
    session.keys("<esc><space>E").await;
    assert!(selected(&session.rows()).contains("x.txt"));
    session.keys("r").await;
    session.keys("<C-u>z.txt<ret>").await;
    session
        .wait_for_screen("the renamed file", |rows| on_screen(rows, "z.txt"))
        .await;
    let z = a.join("z.txt");
    assert!(z.is_file() && !x.exists());
    assert_eq!(std::fs::read_to_string(&z)?, "one\ntwo\nthree\n");
    assert_eq!(current_path(&session.app), canonical(&z));
    assert!(selected(&session.rows()).contains("z.txt"));

    // Cut it, then paste it into the directory `b`.
    session.keys("x").await;
    assert!(
        session.status().contains("Cut z.txt"),
        "{}",
        session.status()
    );
    for _ in 0..10 {
        if selected(&session.rows()).contains("b/") {
            break;
        }
        session.keys("k").await;
    }
    assert!(
        selected(&session.rows()).contains("b/"),
        "{:#?}",
        session.rows()
    );
    session.keys("p").await;
    session
        .wait_for_screen("the moved file", |rows| selected(rows).contains("z.txt"))
        .await;
    assert!(!z.exists());
    assert!(a.join("b/z.txt").is_file());
    assert_eq!(current_path(&session.app), canonical(&a.join("b/z.txt")));

    // Delete needs a `y`, anything else cancels.
    session.keys("s").await;
    session.keys(&format!("'{} 'y.txt", name_of(&dir))).await;
    session
        .wait_for_screen("y.txt", |rows| selected(rows).contains("y.txt"))
        .await;
    session.keys("<esc>d").await;
    assert!(session.status().contains("Delete"), "{}", session.status());
    session.keys("n").await;
    assert!(y.exists());
    assert!(
        session.status().contains("cancelled"),
        "{}",
        session.status()
    );
    session.keys("dy").await;
    assert!(!y.exists(), "{} {:#?}", session.status(), session.rows());
    assert!(session.status().contains("Deleted"), "{}", session.status());
    assert!(session.tree_is_open());
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_with_unsaved_changes_is_not_deleted() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("ix<esc><space>Edy").await;
    assert!(x.exists());
    assert!(
        session.status().contains("unsaved changes"),
        "{}",
        session.status()
    );
    // The tree is still there, with the file.
    assert!(on_screen(&session.rows(), "x.txt"));
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn git_status_marks_the_names() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    // The fixture is new to the repository: untracked files and the directories holding them.
    session.keys("<space>E").await;
    session
        .wait_for_screen("the git marks", |rows| {
            on_screen(rows, "x.txt U") && on_screen(rows, "a/ ●")
        })
        .await;
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_screen_has_no_icons_or_arrows_and_the_editor_is_not_shrunk() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    session.keys("<esc>").await;
    let before = session.rows();
    session.keys("<space>E").await;
    for row in session.rows() {
        for ch in row.chars() {
            let private_use = ('\u{e000}'..='\u{f8ff}').contains(&ch) || ch >= '\u{f0000}';
            assert!(!private_use, "an icon {ch:?} in {row:?}");
            assert!(!"▸▾▶▼".contains(ch), "an arrow {ch:?} in {row:?}");
        }
    }
    // The tree is an overlay over the editor: closing it leaves the screen as it was, but for
    // the status line.
    session.keys("<esc>").await;
    let after = session.rows();
    let last = before.len() - 1;
    assert_eq!(before[..last], after[..last]);
    // No panel: the document starts at the left edge, after its line number.
    assert!(
        after[0].trim_start().starts_with("1  one"),
        "{:?}",
        after[0]
    );
    session.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_preview_is_on_wide_screens_and_off_on_narrow_ones() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut session = Session::on(&x);

    // 120 columns: the rows on the left, the file on the right.
    session.keys("<space>E").await;
    let rows = session.rows();
    assert!(on_screen(&rows, "x.txt"));
    assert!(in_preview(&rows, "three"), "{rows:#?}");
    // Directories are previewed too, with their entries.
    session.keys("hh").await;
    let rows = session.rows();
    assert!(selected(&rows).contains("+ a/"), "{rows:#?}");
    assert!(!on_screen(&rows, "x.txt"));
    assert!(
        in_preview(&rows, "x.txt") && in_preview(&rows, "y.txt"),
        "{rows:#?}"
    );
    // `C-t` hides it, and shows it again.
    session.keys("<C-t>").await;
    assert!(!in_preview(&session.rows(), "x.txt"));
    session.keys("<C-t>").await;
    assert!(in_preview(&session.rows(), "x.txt"));

    // 50 columns: no room for it.
    session.keys("lljj").await;
    assert!(selected(&session.rows()).contains("x.txt"));
    assert!(in_preview(&session.rows(), "three"));
    session.resize(50, 20).await;
    let rows = session.rows();
    assert_eq!((rows.len(), rows[0].chars().count()), (20, 50));
    assert!(on_screen(&rows, "x.txt"), "{rows:#?}");
    assert!(rows.iter().all(|row| !row.contains("three")), "{rows:#?}");
    // The rows have the whole width.
    assert!(
        rows[1].starts_with('│') && rows[1].chars().count() == 50,
        "{rows:#?}"
    );
    // Back to a wide one.
    session.resize(120, 40).await;
    assert!(
        in_preview(&session.rows(), "three"),
        "{:#?}",
        session.rows()
    );
    session.close().await;
    Ok(())
}
