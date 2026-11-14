//! Tests of the command palette's recent commands (this fork): the commands run this session
//! first, most recent first, winning ties; in memory only.

use super::git_log::Session;

/// The names in the rows of the palette, in order.
fn names(session: &Session) -> Vec<String> {
    session
        .frame()
        .iter()
        .map(|row| {
            row.trim_start_matches(" > ")
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_application_has_no_recent_commands() {
    let session = Session::new();
    assert!(session.app.editor.recent_commands.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn recent_commands_come_first_most_recent_first() {
    let mut session = Session::new();
    session.keys(":echo hi<ret>").await;
    session.keys("<space>?select_all<ret>").await;
    assert_eq!(
        session.app.editor.recent_commands[..2],
        ["select_all".to_string(), ":echo".to_string()]
    );

    session.keys("<space>?").await;
    let rows = names(&session);
    // (after the column header)
    let at = rows.iter().position(|name| name == "name").unwrap() + 1;
    assert_eq!(rows[at..at + 2], ["select_all", ":echo"], "{rows:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn running_one_again_moves_it_to_the_front_once() {
    let mut session = Session::new();
    session.keys(":echo a<ret>:echo b<ret>").await;
    session.keys("<space>?select_all<ret>:echo c<ret>").await;
    assert_eq!(
        session.app.editor.recent_commands,
        [":echo".to_string(), "select_all".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_recent_commands_live_on_the_editor_only() {
    // a new application starts empty whatever an earlier one ran: nothing is kept on disk
    let mut first = Session::new();
    first.keys(":echo hi<ret>").await;
    assert_eq!(first.app.editor.recent_commands, [":echo".to_string()]);
    let second = Session::new();
    assert!(second.app.editor.recent_commands.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recent_command_wins_a_tie_and_without_it_the_matchers_order_is_kept() {
    let rows_for = |session: &Session| -> Vec<String> {
        names(session)
            .into_iter()
            .skip_while(|name| name != "name")
            .skip(1)
            .filter(|name| !name.is_empty())
            .collect()
    };

    let mut session = Session::new();
    session.keys("<space>?move_line_").await;
    let rows = rows_for(&session);
    let up = rows.iter().position(|n| n == "move_line_up").unwrap();
    let down = rows.iter().position(|n| n == "move_line_down").unwrap();
    assert!(up < down, "{rows:#?}");
    session.keys("<esc>").await;

    // run the one that comes second: it comes first among the tied now
    session.keys("<space>?move_line_down<ret>").await;
    assert_eq!(session.app.editor.recent_commands[0], "move_line_down");
    session.keys("<space>?move_line_").await;
    let rows = rows_for(&session);
    let up = rows.iter().position(|n| n == "move_line_up").unwrap();
    let down = rows.iter().position(|n| n == "move_line_down").unwrap();
    assert!(down < up, "{rows:#?}");
}
