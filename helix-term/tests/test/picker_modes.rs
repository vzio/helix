//! Pickers without modes (every picker but the file tree and the commit log) send the letters to
//! their prompt, and Esc closes them, as upstream's do.

use helix_view::input::parse_macro;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[cfg(windows)]
use crossterm::event::{Event, KeyEvent};
#[cfg(not(windows))]
use termina::event::{Event, KeyEvent};

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn pickers_without_modes_type_the_letters_in_the_prompt() -> anyhow::Result<()> {
    use helix_view::graphics::CursorKind;

    let mut app = helpers::AppBuilder::new().build()?;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut input = UnboundedReceiverStream::new(rx);
    let keys = |keys: &'static str| {
        for key in parse_macro(keys).unwrap() {
            tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
    };
    let rows = |app: &helix_term::application::Application| -> Vec<String> {
        let screen = app.screen();
        (0..screen.area.height)
            .map(|y| {
                (0..screen.area.width)
                    .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                    .collect()
            })
            .collect()
    };

    // `s`, `j`, `q` and `gg` are letters of the query of the buffer picker, whose cursor shows
    keys("<space>bsjqgg");
    assert!(app.event_loop_until_idle(&mut input).await);
    let rows = rows(&app);
    let prompt = rows.iter().find(|row| row.contains("│ sjqgg")).cloned();
    assert!(prompt.is_some(), "{rows:#?}");
    assert_ne!(app.cursor_kind(), CursorKind::Hidden);

    // Esc closes it
    keys("<esc>");
    assert!(app.event_loop_until_idle(&mut input).await);
    assert!(!rows_contain(&app, "│ sjqgg"));

    keys(":qa!<ret>");
    while app.event_loop_until_idle(&mut input).await {}
    let errors = app.close().await;
    assert!(errors.is_empty(), "{errors:?}");
    Ok(())
}

fn rows_contain(app: &helix_term::application::Application, text: &str) -> bool {
    let screen = app.screen();
    (0..screen.area.height).any(|y| {
        (0..screen.area.width)
            .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
            .collect::<String>()
            .contains(text)
    })
}
