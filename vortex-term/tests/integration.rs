#[cfg(feature = "integration")]
mod test {
    mod helpers;

    use vortex_core::{syntax::config::AutoPairConfig, Selection};
    use vortex_term::config::Config;

    use indoc::indoc;

    use self::helpers::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn hello_world() -> anyhow::Result<()> {
        test(("#[\n|]#", "ihello world<esc>", "hello world#[|\n]#")).await?;
        Ok(())
    }

    mod auto_pairs;
    mod change_lists;
    mod change_lists_live;
    mod command_line;
    mod commands;
    mod conflicts;
    mod dashboard;
    mod debug;
    mod explorer;
    mod git_log;
    mod hex_view;
    mod hunks;
    mod inline_blame;
    mod move_lines;
    mod movement;
    mod picker_modes;
    mod practice;
    mod practice_panel;
    mod splits;
    mod stale_buffers;
    mod terminal;
    mod uncommitted;
    mod watch;
    mod worktrees;
    mod x_empty_line;
}
