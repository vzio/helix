use std::path::{Path, PathBuf};

use helix_term::application::Application;

use super::*;

/// A directory `a` holding `x.txt` and `y.txt`, inside the workspace the tests run in (the tree is
/// rooted at the workspace of the working directory, which the tests can't change).
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
    Ok((dir, x, y))
}

fn current_path(app: &Application) -> PathBuf {
    let path = helix_view::doc!(app.editor)
        .path()
        .expect("buffer has a path");
    std::fs::canonicalize(path).unwrap()
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn space_shift_e_reveals_the_current_file() -> anyhow::Result<()> {
    let (_dir, x, y) = fixture()?;
    let mut app = helpers::AppBuilder::new().with_file(&x, None).build()?;

    // The tree opens with `x.txt` selected: `j` goes to `y.txt`, enter opens it.
    test_key_sequence(
        &mut app,
        Some("<space>Ej<ret>"),
        Some(&|app| {
            helpers::assert_status_not_error(&app.editor);
            assert_eq!(current_path(app), canonical(&y));
        }),
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn space_e_opens_the_tree_at_the_root() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut app = helpers::AppBuilder::new().with_file(&x, None).build()?;

    // The tree opens at the root, focused, with nothing revealed: `j` and enter move and expand in
    // the tree, the buffer stays as it was. Not the picker explorer, which would take `j` as its
    // query and open what it matches.
    test_key_sequence(
        &mut app,
        Some("<space>ej<ret>"),
        Some(&|app| {
            helpers::assert_status_not_error(&app.editor);
            assert_eq!(current_path(app), canonical(&x));
            let (view, doc) = helix_view::current_ref!(app.editor);
            assert_eq!(
                doc.selection(view.id)
                    .primary()
                    .cursor(doc.text().slice(..)),
                0
            );
        }),
        false,
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn space_e_again_closes_the_focused_tree() -> anyhow::Result<()> {
    let (_dir, x, _y) = fixture()?;
    let mut app = helpers::AppBuilder::new().with_file(&x, None).build()?;

    // Closed again, `j` moves the cursor in the buffer.
    let on_second_line = |app: &Application| {
        helpers::assert_status_not_error(&app.editor);
        assert_eq!(current_path(app), canonical(&x));
        let (view, doc) = helix_view::current_ref!(app.editor);
        let text = doc.text().slice(..);
        let line = text.char_to_line(doc.selection(view.id).primary().cursor(text));
        assert_eq!(line, 1);
    };
    test_key_sequences(
        &mut app,
        vec![
            (Some("<space>e<space>ej"), Some(&on_second_line)),
            (Some("gg<space>E<space>Ej"), Some(&on_second_line)),
        ],
        false,
    )
    .await
}
