//! Tests of the menus in groups (this fork): the `Space` and `g` menus list their keys under dim
//! separators with a label, a key a configuration adds comes last, and `Space ?` is still the
//! command palette.

use vortex_view::graphics::Color;

use super::git_log::{on_screen, Session};
use super::helpers::test_config;

const INACTIVE: Color = Color::Rgb(0x11, 0x22, 0x33);
const INFO: Color = Color::Rgb(0xaa, 0xbb, 0xcc);

fn session_with(config: vortex_term::config::Config) -> Session {
    let mut session = Session::with_config(config);
    session.app.editor.theme = vortex_view::Theme::from(
        toml::from_str::<toml::Value>(&format!(
            r##"
"ui.text.inactive" = {{ fg = "#112233" }}
"ui.text.info" = {{ fg = "#aabbcc" }}
"ui.popup.info" = {{ bg = "#000000" }}
"ui.selection" = {{ bg = "#333333" }}
"##
        ))
        .unwrap(),
    );
    session
}

fn session() -> Session {
    session_with(test_config())
}

/// The row of the screen with `text` in it.
fn row_with(session: &Session, text: &str) -> usize {
    session
        .rows()
        .iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("no row with {text:?}:\n{:#?}", session.rows()))
}

/// The color of the first character of `text` on the screen.
fn fg_of(session: &Session, text: &str) -> Color {
    let y = row_with(session, text);
    let row = &session.rows()[y];
    let x = row[..row.find(text).unwrap()].chars().count();
    session.app.screen().get(x as u16, y as u16).unwrap().fg
}

/// The rows of the screen with `labels` in them, in order: panics unless they are all there and
/// each is below the one before.
fn assert_in_order(session: &Session, labels: &[&str]) {
    let mut last = None;
    for label in labels {
        let y = row_with(session, label);
        if let Some(last) = last {
            assert!(y > last, "{label:?} is not below the one before it");
        }
        last = Some(y);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn space_shows_its_groups_under_separators_in_order() {
    let mut session = session();
    session.keys("<space>").await;
    assert_in_order(
        &session,
        &["── files", "── git", "── commands"],
    );
    // the keys are under them
    assert!(on_screen(&session.rows(), "Open file picker"));
    let files = row_with(&session, "── files");
    let picker = row_with(&session, "Open file picker");
    let git = row_with(&session, "── git");
    assert!(files < picker && picker < git);
}

#[tokio::test(flavor = "multi_thread")]
async fn g_shows_its_groups() {
    let mut session = session();
    session.keys("g").await;
    assert_in_order(&session, &["── LSP", "── history"]);
    // the first group comes first
    let separators: Vec<_> = session
        .rows()
        .iter()
        .filter(|row| row.contains("── "))
        .cloned()
        .collect();
    assert!(separators.len() >= 3, "{separators:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remap_keeps_the_groups_and_a_new_key_comes_after_the_last_separator() {
    let toml = "[keys.normal.space]\nf = \"buffer_picker\"\nZ = \"select_all\"\n";
    let loaded = vortex_term::config::Config::load(
        Ok(&toml.to_string()),
        Err(vortex_term::config::ConfigLoadError::default()),
    )
    .unwrap();
    let mut config = test_config();
    config.keys = loaded.keys;
    let mut session = session_with(config);
    session.keys("<space>").await;
    assert_in_order(&session, &["── files", "── git", "── commands"]);
    // f is the buffer picker now, still in its group
    let buffers = row_with(&session, "Open buffer picker");
    assert!(buffers > row_with(&session, "── files"));
    assert!(buffers < row_with(&session, "── git"));
    // the new key is after the last separator
    let last = session
        .rows()
        .iter()
        .rposition(|row| row.contains("── "))
        .unwrap();
    let new = row_with(&session, "Select all");
    assert!(new > last, "{:#?}", session.rows());

    // and it runs
    session.keys("Z").await;
    let doc = vortex_view::doc!(session.app.editor);
    assert_eq!(
        doc.selection(vortex_view::view!(session.app.editor).id)
            .primary()
            .len(),
        doc.text().len_chars()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn space_question_mark_is_the_command_palette() {
    let mut session = session();
    session.keys("<space>?").await;
    assert!(
        session.rows().iter().any(|row| row.starts_with('┌')),
        "{:#?}",
        session.rows()
    );
    session.keys("file_picker").await;
    assert!(
        session.frame().iter().any(|row| row.contains("file_picker")),
        "{:#?}",
        session.frame()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn separators_are_dimmer_than_the_entries() {
    let mut session = session();
    session.keys("<space>").await;
    assert_eq!(fg_of(&session, "── files"), INACTIVE);
    assert_eq!(fg_of(&session, "Open file picker"), INFO);
    assert_ne!(fg_of(&session, "── git"), fg_of(&session, "Open file picker"));
}
