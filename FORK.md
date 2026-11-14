# Vortex

Vortex ([vzio/vortex](https://github.com/vzio/vortex)) is a personal fork of
[Helix](https://github.com/helix-editor/helix): upstream Helix as of
[ba40e5474](https://github.com/helix-editor/helix/commit/ba40e547426b0f9896c8bdc699a4ab11f2b37dbc)
(25.07.1, see [History](#history)) plus the features below. It doesn't sync with upstream anymore.
Everything else works as described in the [Helix documentation](https://docs.helix-editor.com/),
under Vortex's own names: the binary is `vx`, the config is in `~/.config/vortex/` (a workspace's
in `.vortex/`), the log is `~/.cache/vortex/vortex.log` and the variables are `VORTEX_*`
(`VORTEX_RUNTIME`, ...). Vortex never reads upstream Helix's directories; when it has no config of its
own but upstream's exists, it says once at startup how to copy it. Vortex runs on Linux and macOS
only.

README.md is the owner's (agents don't edit it). Vortex issues and PRs stay in this repository:
never send them upstream.

Every feature, original or ported, ships with at least one `:practice` exercise (see
[Practice](#practice), and [CLAUDE.md](CLAUDE.md) for how to write one).

## Credits

The git diff viewer and the file explorer are ported from
[marcos-venicius/helix](https://github.com/marcos-venicius/helix) by Marcos Sousa:

- [PR #2](https://github.com/marcos-venicius/helix/pull/2): side by side git diff view, staged
  files in the changed files picker
- [PR #3](https://github.com/marcos-venicius/helix/pull/3): file explorer side panel (in this
  fork it has become an overlay built on Vortex's picker, see [File tree overlay](#file-tree-overlay))
- [PR #4](https://github.com/marcos-venicius/helix/pull/4): git commit log picker with per-commit
  diffs, `C-g` in the changed files picker
- Later fixes from his `marcos` branch: diff view soft wrap (`9ccc1e16a`, `f1d06509a`), explorer
  git status markers and sideways scrolling (`cde8ec5d1`, `673ba0d33`; the scrolling went with
  the side panel)

His commits are cherry-picked with their authorship kept. The design changes in this fork: the
explorer is on `space e` (at the workspace root) and `space E` (revealing the current file),
replacing upstream's file explorer picker on those keys, it is rooted at the workspace root rather
than the working directory, and it is a modal overlay built on Vortex's picker rather than a side
panel.

Inline git blame is ported from upstream Helix
[PR #13133](https://github.com/helix-editor/helix/pull/13133) by Nik Revenco (nik-rev), which
hadn't landed upstream when it was ported. It is adapted to current master and reworked so that
blame is computed entirely in the background (see [Inline git blame](#inline-git-blame)).

Moving lines up and down is a rewrite, not a port, but it started from two earlier attempts:
upstream Helix [PR #4545](https://github.com/helix-editor/helix/pull/4545) by sireliah (never merged) and the
bug-fixed version of it in omentic's [helix-ext](https://github.com/omentic/helix-ext) fork. See
[Move lines](#move-lines).

## History

The history starts with one root commit, "Branched from upstream Helix ba40e5474 (25.7.1)":
upstream's tree at that commit, with the mechanical part of the rename already applied (the
`helix-<crate>/` directories and crate names became `vortex-<crate>`, Rust paths `helix_<crate>`
became `vortex_<crate>`). Upstream's history isn't included (it is in
[helix-editor/helix](https://github.com/helix-editor/helix), and CHANGELOG.md is upstream's up to
the fork point). Every later commit is this fork's own, rewritten with the same mechanical rename;
the history before the rewrite is kept under the tag `pre-vortex-master`. The rest of the rename
(the `vx` binary, directories, variables, prose) is the single commit
"feat!: rename Helix to Vortex (#24)", listed in [`.git-blame-ignore-revs`](.git-blame-ignore-revs) so that blame skips it: git with
`git config blame.ignoreRevsFile .git-blame-ignore-revs` (once per clone), GitHub's blame view,
and Vortex's own inline blame (which reads that file, or git's `blame.ignoreRevsFile`).

So a file this fork never changed has only the root commit in its history, once the ignored revs
are left out. To check a file:

```sh
# the commits that changed it, except those in .git-blame-ignore-revs:
# only the root commit means untouched upstream code
git log --format='%H %s' -- FILE | grep -vFf <(grep -oE '^[0-9a-f]{40}' .git-blame-ignore-revs)

# line by line (after the blame.ignoreRevsFile config above)
git blame FILE
```

## Contents

- [The status line](#the-status-line)
- [The mode bar](#the-mode-bar)
- [Menus and key help](#menus-and-key-help)
- [The command palette](#the-command-palette)
- [Side by side git diff](#side-by-side-git-diff)
- [Git commit log](#git-commit-log)
- [Line history](#line-history)
- [Uncommitted changes, live](#uncommitted-changes-live)
- [Stage, unstage and revert hunks](#stage-unstage-and-revert-hunks)
- [Files changed on disk](#files-changed-on-disk)
- [Conflict diagnostics](#conflict-diagnostics)
- [Staged files in the changed files picker](#staged-files-in-the-changed-files-picker)
- [File tree overlay](#file-tree-overlay)
- [Inline git blame](#inline-git-blame)
- [`x` on an empty line](#x-on-an-empty-line)
- [Move lines](#move-lines)
- [Terminal panel](#terminal-panel)
- [The debugger](#the-debugger)
- [Hex view](#hex-view)
- [Dashboard](#dashboard)
- [Worktrees](#worktrees)
- [Watch mode](#watch-mode)
- [History and bookmarks](#history-and-bookmarks)
- [The vortex theme](#the-vortex-theme)
- [Theme effects](#theme-effects)
- [The Matrix theme](#the-matrix-theme)
- [Practice](#practice)
- [Tutor](#tutor)

## Code layout

Each feature lives in its own modules, to keep rebasing on upstream cheap:

| Feature | Modules |
| --- | --- |
| The status line rule | `vortex-term/tests/status_line_guard.rs` (the test that keeps key hints out of it) |
| The mode bar | `vortex-term/src/ui/modebar.rs` (segments, the layout and its truncation, badges, the mouse), `Layer`, `Keymaps::layer_get` and `layer_reverse_map` in `vortex-term/src/keymap.rs`, `layers()` in `vortex-term/src/keymap/default.rs`, `RawKeys` in `vortex-term/src/config.rs` (`[keys.watch]`, `[keys.debug]`), debug mode's bar in `vortex-term/src/debug/bar.rs` |
| Menus and key help | the `["group"]` form of `keymap!` in `vortex-term/src/keymap/macros.rs`, `KeyTrieNode::groups` and `grouped_infobox` in `vortex-term/src/keymap.rs`, `Info::grouped` in `vortex-view/src/info.rs`, `vortex-term/src/ui/keyhelp.rs` (the `?` overlay), `with_help`, `key_help`, `without_header` and `with_info` in `vortex-term/src/ui/picker.rs`, `vortex-term/tests/test/menus.rs` |
| The command palette's recent commands | `Editor::recent_commands` and `remember_command` in `vortex-view/src/editor.rs`, `with_tie_rank` in `vortex-term/src/ui/picker.rs`, `palette_key` and `command_palette` in `vortex-term/src/commands.rs`, `vortex-term/tests/test/palette.rs` |
| Diff view | `vortex-term/src/ui/diff_view.rs`, `vortex-term/src/commands/diff_view.rs` |
| Hunk actions, `n`/`N` across files | `vortex-term/src/commands/hunks.rs` (stage, unstage, revert), `vortex-vcs/src/staging.rs` (the texts of the index and of the working tree after an action), `vortex-vcs/src/git/stage.rs` (reading and writing one entry of the index), the `Actions` and `FileNav` of the diff view, `base` of `:practice` exercises, `vortex-term/tests/test/hunks.rs` |
| Commit log | `vortex-term/src/commands/git_log.rs` (also the background reading of a commit's changes), `vortex-vcs/src/commit.rs`, `vortex-vcs/src/git/log.rs`, `range_changes` and `for_each_range_file` in `vortex-vcs/src/git.rs` (the files of a commit or range, read with one repository open) |
| Line history | `vortex-vcs/src/line_history.rs` (mapping lines through a diff, the merge rule), `vortex-vcs/src/git/line_history.rs` (the walk through the repository), `vortex-term/src/commands/line_history.rs` (from the buffer to HEAD, the search on its thread and its status line, the rows and their preview, `Enter`), `vortex-vcs/src/git/line_history_test.rs`, `vortex-term/tests/test/line_history.rs`, `vortex-term/tests/test/repo_gen.rs` (big repositories for performance tests) |
| Uncommitted changes, live views | `vortex-term/src/commands/uncommitted.rs` (the list of uncommitted hunks, their marks, unsaved edits), `vortex-term/src/commands/changes.rs` (the lists of changes of the uncommitted changes and of commits: hunk rows, file dividers, `+`/`-` marks, lockfiles), `with_groups` and `with_section_keys` in `vortex-term/src/ui/picker.rs` (dividers that head groups, `w`/`b` between them), `with_drawn_preview` in `vortex-term/src/ui/picker.rs` (a preview drawn by its owner), `vortex-term/src/live.rs` (the shared watch, background refreshes), `vortex-view/src/watcher.rs` (the reusable watcher), `RepoState` and `log_entries` in `vortex-vcs/src/lib.rs`, `[editor.changes]` in `vortex-view/src/editor.rs`, `vortex-term/tests/test/uncommitted.rs`, `vortex-term/tests/test/change_lists.rs` |
| Files changed on disk | `vortex-term/src/handlers/disk.rs`, the disk state of `Document` in `vortex-view/src/document.rs`, `vortex-term/tests/test/stale_buffers.rs` |
| Conflict diagnostics | `vortex-vcs/src/conflict.rs` (finding the blocks), `conflicted_files` in `vortex-vcs/src/git.rs` (the unmerged files), `vortex-term/src/handlers/conflicts.rs` (when to look, the documents' state), `vortex-term/tests/test/conflicts.rs` |
| Staged files, commit diffs | `vortex-vcs/src/git.rs`, `vortex-vcs/src/lib.rs`, `vortex-vcs/src/status.rs` |
| File tree | `vortex-term/src/ui/explorer.rs`, `vortex-term/src/ui/explorer/`, `vortex-term/benches/explorer.rs` |
| Inline blame | `vortex-vcs/src/git/blame.rs`, `vortex-vcs/src/git/blame/ignore_revs.rs`, `vortex-vcs/src/blame_stub.rs`, `vortex-vcs/benches/blame.rs`, `vortex-stdx/src/time.rs`, `vortex-view/src/blame.rs`, `vortex-term/src/handlers/blame.rs`, `vortex-term/src/commands/blame.rs`, `vortex-term/src/ui/text_decorations/blame.rs` |
| `x` on an empty line | `vortex-term/src/handlers/line_select.rs` |
| Move lines | `vortex-term/src/commands/move_lines.rs` |
| Terminal panel | `vortex-terminal/` (the shell: pty, emulator, OSC 133, bash/zsh integration), `vortex-view/src/terminal.rs` (panel state, `[editor.terminal]`), `vortex-term/src/terminal/` (drawing, focus, input encoding, the `vx` shim), `vortex-term/tests/test/terminal.rs` |
| The debugger | `vortex-term/src/debug/` (`session.rs` the sessions, the adapter's events and reverse requests, `load.rs` what the panel loads at a stop, `breakpoints.rs` breakpoints per session (yours and the engine's hidden ones in one request), `engine.rs` the per-line breakpoint engine: finding and installing hidden breakpoints, full-speed runs, the hit decision, `panel.rs` the keys and the mouse, `render.rs` the drawing; spectator mode: `playback.rs` automatic stepping, fast forward and run to cursor, `threads.rs` spectating, freezing and running threads alone, `record.rs` what each stop records, `rewind.rs` scrubbing, `cpu.rs` a thread's CPU time from the OS; `memory.rs` a debugged program's memory in the hex view: opening, reading, refreshing, writing, recording for rewind), `vortex-view/src/debug/` (`mod.rs` the panel's state and rows; `spectator.rs`, `history.rs`, `timing.rs`, `config.rs` spectator mode's state, `[editor.debugger]`; `engine.rs` the engine's state and decisions, `lineline.rs` the record of every hit), `vortex-dap/src/hits.rs` (the engine's fast path: hits of unwatched threads continued by the client), `vortex-term/src/ui/text_decorations/step_times.rs` (the time per line), `vortex-dap/src/requester.rs` (requests that borrow nothing, for background tasks), child sessions in `vortex-dap/src/registry.rs`, `vortex-term/src/commands/dap.rs` (the debug commands, the templates); debug mode: `vortex-term/src/debug/mode.rs` (entering and leaving, view mode and following, the camera, the lineline's seeks, the held keys and the notes), `vortex-term/src/debug/bar.rs` (the three rows: the transport, the lineline's layout and cells, the keys; clicks), `vortex-term/src/debug/launch.rs` (the template of `vx --debug`, the template picker), `vortex-term/tests/test/debug/` (a scripted fake adapter and the tests) |
| Hex view | `vortex-view/src/hex/mod.rs` (the bytes: windowed reads, the overwritten bytes, undo, cursors, search, the row layout), `vortex-view/src/hex/memory.rs` (a debugged program's memory as the bytes: the page cache, what each byte is as drawn), `vortex-term/src/ui/hex_view.rs` (drawing, the statusline texts), `vortex-term/src/commands/hex.rs` (keys, mouse, prompts, `:hex`), `vortex-term/tests/test/hex_view.rs` |
| Dashboard | `vortex-term/src/dashboard.rs` (when it is up, its keys), `vortex-term/src/dashboard/` (`stats.rs` the background reading, `languages.rs` the line counts, `render.rs` the layout and the panels, `timeline.rs` when things move, `title.rs` the title, `vortex.rs` the vortex, `charset.rs` blocks or ASCII, `wrap.rs` long values on two rows, `snapshots/` the drawings the tests compare with), `vortex-vcs/src/git/summary.rs` (the git summary), `vortex-term/tests/test/dashboard.rs` |
| Worktrees | `vortex-term/src/commands/worktrees.rs` (the `:worktrees` picker: its rows, columns, background jobs and `Enter`), `vortex-vcs/src/git/summary.rs` (the worktrees, missing ones flagged; `Repo::status`, the last modified time; `Repo::dated_worktrees`; `Worktree::details`), `vortex-stdx/src/time.rs` (`format_ago`), the dashboard's worktree rows and `t` in `vortex-term/src/dashboard/`, `vortex-term/tests/test/worktrees.rs` |
| Watch mode | `vortex-view/src/watch.rs` (the session: the baseline, the files and hunks numbered in the order they changed, following, `n`/`N`, the new count), `vortex-term/src/watch.rs` (`:watch` and the other commands, the keys of the mode, reading the changed files on the watcher's thread, following, the flash, which commands are held, the mode bar's rows, the statusline element, the `:watch-changes` picker), `RepoWatch::start_tapped` in `vortex-term/src/live.rs`, the session's gutter markers in `vortex-view/src/gutter.rs`, `vortex-term/tests/test/watch.rs` |
| History and bookmarks | `vortex-view/src/history.rs` (what records a jump, `Editor::record_jump`, the places of a view, walking), `vortex-view/src/bookmarks.rs` (the bookmarks of a buffer, mapped through its changes; the closed files' bookmarks; the ring), `vortex-term/src/history.rs` (`ga`, `gA` and its sticky state, showing a screen again, the bookmark commands and picker), the `bookmarks` gutter in `vortex-view/src/gutter.rs`, `vortex-term/tests/test/history.rs` |
| The vortex theme | `theme.toml` (the theme), `helix_theme.toml` (upstream's default theme), the built-in themes in `vortex-view/src/theme.rs` |
| Theme effects | `vortex-view/src/effects.rs` (the `[effects]` table: parsing, validation, inheritance), `vortex-term/src/ui/glitch.rs` (the glitch and its wake-ups), `vortex-term/src/dashboard/charset.rs` (blocks or the measured ASCII ramp), `vortex-term/src/terminal/phosphor.rs` (the phosphor filter), the glitch over error diagnostics in `vortex-term/src/ui/text_decorations/diagnostics.rs` (where the ghost text of errors is) and `glitch_diagnostics` in `vortex-term/src/ui/editor.rs` |
| The Matrix theme | `matrix_theme.toml` (the theme and its effects), built in by `vortex-view/src/theme.rs`, `vortex-view/src/contrast_tests.rs` (the contrast test) |
| Practice | `vortex-term/src/practice.rs`, `vortex-term/src/practice/`, `runtime/practice/`, `vortex-term/tests/test/practice.rs` |

Hooks into core files: commands and keys in `vortex-term/src/commands.rs`,
`vortex-term/src/commands/typed.rs` and `vortex-term/src/keymap/default.rs`; `C-g`, the hooks the
file tree needs (an external filter, replacing the items, the query), the ones of the commit log
(dividers, marked rows), the ones of the lists of changes (dividers that head groups and
filter with them, `w`/`b` between groups), the opt-in modal mode both use (`Picker::with_modes`: normal mode where the
letters are keys, filter mode where they are typed) and the cursor accessors in
`vortex-term/src/ui/picker.rs`;
`[editor.explorer]` and `[editor.inline-blame]` in `vortex-view/src/editor.rs`. Inline blame also
has one field on `Document`, one decoration in `render_view`, `is_last_visual_line` in
`vortex-term/src/ui/document.rs`, and refresh calls on save (`application.rs`), on focus
(`ui/editor.rs`) and in `:reload`/`:reload-all`.
The move mode label (`MOV`) is a generic `mode="..."` option of sticky nodes in the `keymap!` macro
(`mode_label` on `KeyTrieNode`, `vortex-term/src/keymap.rs` and `keymap/macros.rs`), which
`render_view` passes to the statusline's mode element (`RenderContext::mode_label` in
`vortex-term/src/ui/statusline.rs`).
`:practice` counts keys from two calls around the dispatch of terminal events in
`Application::handle_terminal_events`, draws its panel from one call in `EditorView::render`, has
one entry in `typed.rs`, and opens its buffers with `Editor::new_file_from_document`, made public.

The terminal panel changes core files directly (the fork no longer syncs with upstream): the
`Editor::terminal` field and `close_document` (an awaited buffer closing answers `vx` and focuses
the panel) in `vortex-view/src/editor.rs`, where `[editor.terminal]` also absorbed the debugger's
`command`/`args`; the layout, the cursor and `Esc` in `ui/editor.rs`; the event routing in
`Application::handle_terminal_events`; `toggle_terminal` in `commands.rs` and `keymap/default.rs`;
`:sh`, `:!` (and `:!cmd` without a space) and `quit` on an awaited buffer in `typed.rs`; the `shell`
element in `ui/statusline.rs`; the client mode at the top of `main.rs`.

Line history shares the commit log's picker: `CommitLog::new` and the line history's rows,
preview, `o` and range `Enter` in `commands/git_log.rs`; `sections` of `vortex-vcs/src/git/log.rs`,
for the walk to sort commits as the log does; `Injector::is_shut_down` in `ui/picker.rs`, which the
walk checks to stop; `go_to_lines` in `ui/diff_view.rs`; `open_lines_in_commits`,
`open_lines_uncommitted` and how many pickers a commit's diff closes in `commands/diff_view.rs`;
`draw_hunk`, `place` and `PreviewStyles`, shared previews of hunks, in `commands/changes.rs`;
`line_history` in `commands.rs`, `typed.rs` (`:line-history`) and `keymap/default.rs` (`space L`).

The uncommitted changes and files changed on disk also change core files: the disk state of
`Document` (`disk_change`, and `save_impl`, whose external-change check now compares content
instead of the modification time) in `vortex-view/src/document.rs`; the checks before writing in
`write_impl` and `write_all_impl` (`typed.rs`), on focus (`ui/editor.rs`) and when leaving the
terminal panel (`terminal/panel.rs`); the `notify` watcher in `vortex-view`; `[editor.changes]` in
`vortex-view/src/editor.rs`.

The conflict diagnostics change core files: `Severity::Conflict` and `DiagnosticProvider::Conflict`
in `vortex-core/src/diagnostic.rs`, and every match on them (gutter, statusline, the inline and popup
diagnostics in `ui/editor.rs` and `ui/text_decorations/diagnostics.rs`, the pickers in
`commands/lsp.rs`, `DiagnosticFilter`); `Document::set_conflicted` and
`update_conflict_diagnostics`, and `replace_diagnostics` without a provider keeping the editor's own
diagnostics, in `vortex-view/src/document.rs`; `Editor::conflicts` and `workspace_conflict_count` in
`vortex-view/src/editor.rs`; `Theme::get_or`; refresh calls in `disk::check_all`, `live.rs` and
`Application::new`.

The debugger changes core files directly: the `Editor::debug` field and the per-session state of
`Breakpoint` (`disabled`, `sessions`) in `vortex-view/src/editor.rs`, where the old event handler
of `vortex-view/src/handlers/dap.rs` moved to `vortex-term/src/debug/session.rs` (the module keeps
its small helpers); the gutter's breakpoint and `▶` markers in `vortex-view/src/gutter.rs`; the
layout (the panel takes the terminal panel's place), its keys first in `handle_event`, `Esc`, the
cursor, the frame line limited to its file, and a gutter click in `ui/editor.rs`; the mouse in
`Application::handle_terminal_events` and the adapters' messages in `Application`; the `debug`
statusline element; `:debug-eval` (raw input) in `typed.rs`; `space G` (`debug_mode`) and `C-g` in
`keymap/default.rs`; the terminal panel hiding the debug panel in `terminal/panel.rs`; in
`vortex-dap`, the client's `Requester`, child sessions, bodyless responses, the TCP connect retry,
and `supportsStartDebuggingRequest`; `SetVariable`/`SetExpression` and signed exit codes in
`vortex-dap-types`. Spectator mode adds: `Editor::current_stack_frame` showing the recorded frame in
rewind (and the last one while a step runs) and `[editor.debugger]` in `vortex-view/src/editor.rs`;
not opening a frame's missing file in `handlers/dap.rs`; the time-per-line decoration in
`ui/editor.rs`; playback and REWIND in the `debug` statusline element; `std-paths`, `lib-paths` and
`capabilities` in `DebugAdapterConfig` (`vortex-core`); the step requests' `singleThread` and
granularity, paged `variables`, the configured capabilities and the arrival time of `stopped`
events in `vortex-dap`; `supportsSingleThreadExecutionRequests` and `SteppingGranularity` in
`vortex-dap-types`. Memory in the hex view adds: `Document::open_memory`, the memory's name in
`display_name` and `save_impl` refusing it in `vortex-view/src/document.rs`; `in_source_view` in
`handlers/dap.rs` (a stop moves the source in another view than a memory view); reading memory
after each frame, `Esc` going first to a hex view that overwrites or selects, and the hex view
leaving its keys to an open sticky menu, in `ui/editor.rs`; `:debug-memory` and `:w` writing memory
in `typed.rs`; a name for buffers without a path and the memory note in `ui/statusline.rs`;
`readMemory`/`writeMemory`, `supportsMemoryReferences` and `supportsMemoryEvent` in `vortex-dap`
and `vortex-dap-types`; a `capabilities` event merged into the capabilities instead of replacing
them, in `vortex-term/src/debug/session.rs`. The engine adds: `stopped` events going through the
engine first, the configuration installing hidden breakpoints before the rest, `continued` and
`breakpoint` events redrawing only when something shown changed, in `debug/session.rs`; every send
of your breakpoints carrying the hidden ones, in `debug/breakpoints.rs`; `L` running at full
speed in `commands/dap.rs`; the loading animation in the `debug` statusline element; the
`own-paths` of `DebugAdapterConfig` (`vortex-core`); `[editor.debugger]` applied at startup and on
reload (`DebugState::apply_config`, called from `Editor::new` and `refresh_config`); in `vortex-dap`
the client's hit filter and `Event::taken`, `breakpointLocations` and `loadedSources`; `BreakpointLocations` and `LoadedSources` in `vortex-dap-types`; the vortex
theme's loading scopes in `theme.toml`.

Debug mode changes core files: `Layer::Debug` and `Keymaps::layer_infobox` in
`vortex-term/src/keymap.rs`, the debug layer in `layers()` and `space G` in
`vortex-term/src/keymap/default.rs`, `[keys.debug]` in `vortex-term/src/config.rs`; the layer's keys
first, the gutter click (a breakpoint only in debug mode) and the view mode cursor style in
`ui/editor.rs`; `Content::Spans`, `Line`, `Drawn::spots`, `click_spot` and the three rows in
`ui/modebar.rs`; the new commands (`debug_mode`, `debug_view`, `debug_play_pause`,
`debug_line_forward`, `debug_line_back`, `debug_breakpoint_forward`, `debug_breakpoint_back`,
`debug_breakpoint_kind`, `debug_help`, `debug_leave`, `debug_panel_focus`, `debug_panel`) in
`commands.rs` and `commands/dap.rs` (a breakpoint command outside debug mode is an error);
`:debug-mode` in `commands/typed.rs`; `--debug` in `args.rs` and `application.rs`; the gutter rule
(breakpoints in debug mode, bookmarks outside it) in `vortex-view/src/gutter.rs`; `BreakpointKind`
in `vortex-view/src/editor.rs` and `breakpoint_pauses` in `vortex-view/src/debug/engine.rs`;
`DebugState::mode` in `vortex-view/src/debug/mod.rs`; `History::seek` and `nearest` in
`vortex-view/src/debug/history.rs`; `takes` of `DebugTemplate` in `vortex-core/src/syntax/config.rs`
and `languages.toml`; the debug mode scopes in `theme.toml` and `matrix_theme.toml`.

The hex view changes core files: the `hex` field of `Document` and what it changes there
(`open_hex`, `enter_hex`/`leave_hex`, `apply` refusing transactions, `is_modified`, `save_impl`
writing in place, `disk_change` and `reload`, `remove_view`) and `DocumentSavedEvent::hex_written`
in `vortex-view/src/document.rs`; binary files opening with `open_hex` in `Editor::open`
(`vortex-view/src/editor.rs`); `is_binary`, moved from `vortex-term/src/lib.rs` to
`vortex_view::hex`; the key, mouse, drawing and cursor hooks in `ui/editor.rs`
(`render_view_frame` is the border and statusline the hex view shares); the mode, position and
other elements in `ui/statusline.rs`; `:hex`, and `:goto`/`:0x…` going to an offset, in
`typed.rs`; the write message in `application.rs`; the hex preview of binary files in
`ui/picker.rs`; the blame request skipping it in `handlers/blame.rs`; `ui.hex.*` in `theme.toml`.

The dashboard changes core files: the `dashboard` field of `EditorView`, its keys first in
`handle_event`, its drawing in `render` (over the views, under the infobox and the command line)
and no cursor in `cursor`, in `ui/editor.rs`; the startup decision in `Application::new` (and
no file picker for `vx <folder>` when it shows); `:dashboard` and `:vortex` in `typed.rs`; `dashboard` and
`dashboard-background` (`DashboardBackground`) in `vortex-view/src/editor.rs`; `Args::initial_working_dir` in `args.rs`, used by `main.rs`.

Worktrees change core files: `:worktrees` (`:wt`) in `typed.rs`, where `apply_directory_change`
(what `:cd` does) is now `pub(crate)` for the picker's `Enter`; `EditorView::live_dashboard` in
`ui/editor.rs` calls `Dashboard::follow_cwd`, so the dashboard reloads when the working directory
changes.

Watch mode changes core files: the `watch` field of `Editor` and the `watch` statusline element
(no longer in the default statusline: the mode bar shows watch mode) in `vortex-view/src/editor.rs`; the session's diff of
`Document` (`watch_diff`, `set_watch_base`, kept up to date on every edit like git's) in
`vortex-view/src/document.rs`; the `diff` gutter in `vortex-view/src/gutter.rs`, which draws the
session's hunks over git's; `MappableCommand::execute` in `commands.rs` and `execute_command` in
`typed.rs`, which tell watch mode which command runs; `search_next_or_prev_impl` in `commands.rs`
(`n`/`N`); the mouse (looks around), the flash, the select cursor while looking around and the
keys of `[keys.watch]` first (`EditorView::layer_key`) in `ui/editor.rs`; `MappableCommand::execute`
not running a command watch mode holds, and the `watch_*` static commands, in `commands.rs`; `RepoWatch` in `live.rs`
(a tap that sees each batch of paths on the watcher's thread); `--watch` in `args.rs` and
`Application::new`; `Diff` exported by `vortex-vcs`; `blend` of `ui/diff_view.rs`, now
`pub(crate)`.

The vortex theme changes `vortex-view/src/theme.rs` (the built-in themes and their names, the
`default` alias), the `:theme` completion in `vortex-term/src/ui/mod.rs` and the theme check in
`xtask`; the Matrix theme is one more built-in theme there.

Theme effects change core files: the `effects` of `Theme` and `Theme::effects`, the `effects` key
taken out of the styles in `build_theme_values` and merged key by key in `Loader::merge_themes`,
in `vortex-view/src/theme.rs`; `InlineDiagnostics::new` taking the list of cells to keep, in
`ui/text_decorations/diagnostics.rs`; the `diagnostic_glitch` and `diagnostic_cells` of
`EditorView` and the glitch after the views in `render`, in `ui/editor.rs`; the panel's colors
through the filter in `terminal/render.rs`; `DashboardBackground` (`settle` with its `load`
alias, `drain`, `grow`, `grow-on` the default) in `vortex-view/src/editor.rs`.

The hunk actions add `Editor::save_messages` (`vortex-view/src/editor.rs`), a message shown
instead of `'file' written` when a document's save completes (`handle_document_write` in
`application.rs`), so that a revert says how to undo it.

History and bookmarks change core files: the jumplist in `vortex-view/src/view.rs` became
`View::history` (`View::push_jump` is gone); `Editor::switch` records a buffer switch and the old
`Editor::jump_forward`/`jump_backward` are gone, `jump_to` shows a history place, `new_document` and
`close_document` hand a file's bookmarks over (`vortex-view/src/editor.rs`, with the fields
`screen_cover`, `history_paused`, `history_menu`, `bookmark_state`); `Document::bookmarks`, mapped in
`apply_impl` (`vortex-view/src/document.rs`); the recording calls in `commands.rs` (`jump_from` and
`record_jump` where `push_jump` was, `n`/`N`), `commands/lsp.rs`, `commands/typed.rs` (`:<line>`),
`ui/mod.rs` (a search prompt; `regex_prompt` says what its prompt records); `Picker::pick` and
`without_jumps` (the command palette) in `ui/picker.rs`; the sticky state and the dashboard as a
screen in `ui/editor.rs`; `ga`/`gA`, leaving and coming back in `ui/diff_view.rs`;
`Compositor::pop_to_editor`; `ga`/`gA` in the hex view (`commands/hex.rs`); entering the debug panel
(`debug/panel.rs`) and the debugger's stops not recording (`handlers/dap.rs`); the opening of each
diff (`commands/diff_view.rs`); the keys in `keymap/default.rs`; `ui.gutter.bookmark` in both
themes. With watch mode: its command lists (`vortex-term/src/watch.rs`) know the new commands, and
its following and `n`/`N` run without recording.

The mode bar changes core files: `EditorView::render` takes its rows from the bottom of the editor's
area, above the message line, and keeps where its badges were drawn for a click
(`EditorView::click_modebar`), and `handle_event` runs the keys of the layer of the mode that is on
first (`EditorView::layer_key`), in `ui/editor.rs`; `Keymaps` has the layers (`with_layers`, given
`Config::layers` by `Application::new`); `Config` reads `[keys.watch]` beside the modes' tables
(`RawKeys`); the infobox moves up over the bar (`ui/info.rs`).

Menus and key help change core files: `KeyTrieNode` has the groups of its keys and
`KeyTrieNode::infobox` draws them; `Info` has `separators`, drawn dim by `ui/info.rs`;
`keymap/default.rs` groups the menus; `Picker` (`ui/picker.rs`) has `header`, `info`, `help` and
`tie_rank`, and opens the key help on `?` in normal mode; `Picker::render_picker` draws the info
before the count.

The status line rule changes the fork's messages in the files that print them (the commit log,
line history, the lists of changes, hunks, the diff view, the file tree, the hex view, the
worktrees, bookmarks, files changed on disk, the debugger, practice); `:watch-stop` and `:watch`
ending watch mode print `Watch ended: N changes`.

The dashboard's startup changes core files: `dashboard` is a `DashboardStartup` (`"auto"`, `true`,
`false`) in `vortex-view/src/editor.rs`, read by `Application::new` with `Args::plain` (`--plain`,
`-p`, in `args.rs` and the help text of `main.rs`).

The command palette's recent commands change core files: `Editor::recent_commands` and
`remember_command` (`vortex-view/src/editor.rs`), called by `command_palette` (`commands.rs`) and
`execute_command` (`typed.rs`).

To build: `cargo install --path vortex-term --locked`.

## The status line

The message area at the bottom, under the statuslines, is for errors (an LSP, I/O or git failure,
a command that failed) and the short result of a command you ran (`:w`, `:cd`, `:reload`, a staged
hunk, `Watch ended: 12 changes`). It is never for key hints, "how to use this" text, progress
chatter or notes from the fork's features: those are in the UI they belong to:

- a mode's state and keys on the [mode bar](#the-mode-bar) (watch mode);
- a modal picker's or view's keys in its `?` help ([below](#menus-and-key-help));
- a list's totals, a range, what is loading, on the picker's prompt row, before the count
  (`ui.picker.info`, falling back to `ui.text.inactive`);
- the rest in the panel the feature already has, or nowhere.

A confirmation question that waits for its answer (`Delete src/old.rs? (y/n)`) stays there: the
answer is typed right there. The test `vortex-term/tests/status_line_guard.rs` reads every message
the sources write there (`set_status`, `set_error`, `set_warning`, `save_messages`) and fails on
text that names a key and what it does (`n/N go through...`, `press p...`, `:reload to load it`,
`s filter, enter cd`); its allowlist explains the exceptions. A message built elsewhere and passed
in isn't seen by it: the rule in [CLAUDE.md](CLAUDE.md) covers those.

## The mode bar

A bar of one to three rows above the message area, shown only while a mode that uses it is on, and
costing nothing otherwise: no row is taken, nothing is laid out. [Watch mode](#watch-mode) uses two
rows; [debug mode](#debug-mode) uses three.

```text
 WATCH ● following      src/main.rs · 3/12 changes · 2 files      followed src/main.rs:30
  v  look   n  next   N  previous   c  changes   Esc  leave
```

**Layout.** A mode supplies rows of segments, each placed left, middle or right; the bar draws
them, a column of padding on each side, the middle centered (moved aside when it would touch the
others). A segment is the mode's **state**, **information**, a passing **note** (what the last key
did: debug mode's), or a **key badge**; a row can also be a **line** of cells as wide as the bar
(debug mode's lineline). When a row is too narrow, a note goes first (cut with `…`, then gone), then
the keys collapse (all their labels go, leaving the keys, then the last badges go), then the
information is cut with `…` (the widest first), then the state. The bar takes its place
under the splits and the panels (the terminal and debug panels stay above it), and the infobox
moves up over it.

**Key badges** are drawn like the dashboard's, the key then its label (` v  look`). A badge names a
command, not a key: it shows the key the mode's keymap has for that command, so a remapped key
shows as remapped and a command without a key has no badge. A click on a badge (on its key or its
label) runs the command. A label may be a glyph (`▶`) rather than a word.

**Theme.**

| Scope | What | Falls back to |
| --- | --- | --- |
| `ui.modebar` | the bar's background | `ui.statusline` |
| `ui.modebar.state` | the state (a mode adds its own, like `ui.modebar.state.following`) | `ui.statusline.normal` |
| `ui.modebar.info` | the information | `ui.statusline` |
| `ui.modebar.key` | a badge's key | `ui.dashboard.key` |
| `ui.modebar.label` | a badge's label | `ui.dashboard.action` |

**How debug mode uses it.** Debug mode is one more mode of the bar, the same way as watch mode:

1. a layer `Layer::Debug` in `vortex-term/src/keymap.rs` with its default keys in `layers()`
   (`[keys.debug]` in a config), looked up before the normal mode's while debug mode is on (one
   line in `EditorView::layer_key`);
2. `debug::bar::bar(editor, &keys, width)` in `vortex-term/src/debug/bar.rs`, returning three rows:
   the transport, the lineline, the keys; `modebar::current` and `modebar::height` call it while
   debug mode is on (`height` is 3);
3. the transport's badges are plain glyphs (never emoji: no variation selector), each naming its
   command, so each shows its key and a click runs it: `|◀` is `debug_breakpoint_back`, `◀◀`
   `debug_line_back`, `▶` or `❚❚` `debug_play_pause`, `▶▶` `debug_line_forward` and `▶|`
   `debug_breakpoint_forward`; the speed is information on the right;
4. the lineline is a `Line`: a full-width row of cells, not segments. A cell has a style and may carry
   a click `spot`, a number the mode decides the meaning of; the bar records the cells that have one
   in `Drawn::spots`, and the mouse calls `modebar::click_spot`, which hands the spot to the mode
   that is on (`debug::bar::click` seeks);
5. the state is styled: `Content::Spans` is text with a style for each piece, which is how the
   loading animation (a spinner, the bar filling, the glow) is drawn in the state's place, under
   `ui.modebar.state.loading`;
6. the third row is badges for the other keys, and the note on its right: what the last key did,
   or why it waited (`DebugState::note`).

The test `three_rows_like_debug_mode` in `vortex-term/src/ui/modebar.rs` stays as the layout test:
it lays out three rows like debug mode's with a fake mode. The real bar's tests (the lineline's
layout, its cells and glyphs, the clicks, the states) are in `vortex-term/src/debug/bar.rs`.

## Menus and key help

**Categories.** The `space` menu, the `g` menu, `[` and `]`, `z` and `Z`, `C-w` (`space w`) group their entries under short labels, drawn in the infobox as dim
separator lines (`ui.infobox.separator`, falling back to `ui.text.inactive`):

```text
┌Space──────────────────────────────────────┐
│── files ──────────────────────────────────│
│f  Open file picker                        │
│F  Open file picker at current working dir │
│...                                        │
│── git ────────────────────────────────────│
│g  Open changed file picker                │
│...                                        │
│── commands ───────────────────────────────│
│?  Open command palette                    │
└───────────────────────────────────────────┘
```

| Menu | Groups |
| --- | --- |
| `space` | files, search, git, LSP, clipboard, comments, windows and debug, commands |
| `g` | in the file, on the screen, LSP, files, history |
| `[`, `]` | diagnostics and changes, syntax, lines |
| `z`, `Z` | align, scroll, search |
| `C-w`, `space w` | splits, focus, move |

The groups are part of the default keymap: in `keymap!`, a `["label"]` before keys puts them under
that label (`keymap/default.rs`); a menu without labels looks as before. A key a config adds to a
menu has no group: it comes last, under a separator without a label. A key a config remaps keeps its
place. The `space` and `g` menus have no `?` and no expanded help: only the separators.

**`?` help.** In the fork's modal pickers and views, `?` in their normal mode opens a box over them
listing their keys, in groups; any key closes it: the commit log and line history, the lists of
changes (of commits, the uncommitted changes, watch mode's changes), the worktrees, the bookmarks,
the file tree, the side by side diff, the hex view, the debug panel and [debug mode](#debug-mode)
(its groups: playback, stepping, threads, view and breakpoints, session, inspect, panel, mode; the box is
built from the keymap, so a remapped key shows as remapped). A box taller than the screen (debug
mode's, on a small terminal) is laid out in two columns, split between groups. Where `?` is typed text
(a picker's filter mode, after `s`), it stays text. Their keys are no longer printed to the status
line, nor in the diff's footer. The hex view's `?` was a backward search: `/` and then `N` searches
backward.

**No table headers.** The fork's pickers and lists (the commit log, line history, the lists of
changes, the worktrees, the bookmarks) have no header row with the names of their columns: what
each column shows is clear from it. Upstream's pickers keep theirs.

## The command palette

The command palette (`space ?`) lists the commands you ran this session first, the most recent
first, then the rest as before. A command counts when you run it from the palette or the command
line (`:`), not from its key. With a query, matches are ranked as before and a recent command wins
a tie (same score). The list lives in memory only: nothing is saved, it is gone when `vx` quits.

## Side by side git diff

Shows a file's HEAD version (left) next to its current contents (right), read-only. Changed lines
are aligned, with `╱` filler where one side has no line. Both sides get syntax highlighting, and
within a changed line, only the words that changed are highlighted. When the file is open with
unsaved changes, the right side shows the buffer and its title says `[modified]`.

### Usage

| Keys | Action |
| --- | --- |
| `space =` | Open the diff of the current file. |
| `C-g` in the changed files picker (`space g`) | Open the diff of the selected file on top of the picker: closing the diff goes back to the picker. `Enter` still opens the file. |

Inside the diff:

| Keys | Action |
| --- | --- |
| `n` / `N`, `]c` / `[c` | Next / previous hunk; past the last (first) one, around to the first (last), or, in a diff opened from the [commit log](#git-commit-log), on to the next (previous) file |
| `s` `u` `r`, `S` `U` `R` | Stage, unstage, revert the hunk, or the whole file, see [hunks](#stage-unstage-and-revert-hunks) |
| `j` / `k`, `C-d` / `C-u`, `C-f` / `C-b`, `PageDown` / `PageUp`, mouse wheel | Scroll |
| `gg` / `ge`, `G` | Top / bottom |
| `h` / `l`, `0` | Scroll horizontally, back to the start of the line |
| `gh` / `gl` | Scroll horizontally all the way to the start / to the end of the longest line |
| `w` | Wrap long lines, or scroll them horizontally again |
| `Enter` | Close the diff (and the picker it was opened from) and open the file at the current hunk |
| `q`, `Esc` | Close |

The footer shows the current hunk (e.g. `hunk 2/5`), whether it is staged, the number of the file
in a diff opened from a list of files (`file 1/3`); `?` lists the keys. The line numbers of the current
hunk are bold. The last line of the screen stays the editor's, for its messages. New files
(untracked or staged) only have a right side, and deleted files only have a left side. Renamed files
are compared with their old path in HEAD.

Long lines wrap when soft wrap is on for the file, as in the editor: the setting of its buffer when it
is open, otherwise `soft-wrap.enable` of its language or of `[editor.soft-wrap]`. Continuation lines
have no line number, and scrolling goes one screen line at a time, so a line taller than the screen
can be read whole. `w` switches between wrapping and horizontal scrolling.

### Limitations

- Binary files can't be shown.
- Lines wrap at the last column, not at word boundaries, and without the `wrap-indicator`.
- A line ending change (CRLF ↔ LF) marks every line as changed.
- The background tints need a theme with true colors. With other themes, only the changed words are
  marked (reversed).

### Ideas

- Search inside the diff.
- Fold long runs of unchanged lines.
- Stage part of a hunk (a selection of its lines).

## Git commit log

A picker of the commits reachable from HEAD, with their short hash, date, author and summary. The
commits are in sections by branch, newest first in each, each section under a divider with the
name of the branch. Choosing a commit lists its changes, one row per hunk under a divider per
file (see [Lists of changes](#lists-of-changes)), and choosing a hunk opens the
[side by side diff](#side-by-side-git-diff) of that file in the commit at that hunk: its version
in the commit's parent on the left, in the commit on the right. Several commits can be selected, to
list and diff their combined change.

### Sections

The sections are made of the commits reachable from HEAD (`B` is the current branch):

| Section | Commits | Divider |
| --- | --- | --- |
| 1 | not on the upstream of `B`, nor on the base branch | `B (unpushed)`; `B` if `B` has no upstream; `HEAD (detached)` for a detached HEAD |
| 2 | on the upstream of `B`, not on the base branch | `B` |
| 3 | on the base branch | the name of the base branch as git shows it: `origin/main`, or `main` |

- The upstream is the remote tracking branch of `B` (`branch.<B>.remote` and `.merge`) if it is
  configured and exists.
- The base branch is the first that exists of: the branch `refs/remotes/origin/HEAD` points to
  (`<remote>/HEAD` if the upstream is on another remote), `<remote>/main`, `<remote>/master`, the
  local `main` and `master`. A local branch is never the base of itself.
- Sections without commits have no divider, and there is no section 3 without a base branch. If
  there is no base branch, the commits that aren't in section 1 are in section 2.
- On `main` with `origin/main` as its upstream (and base), section 1 is `main (unpushed)`, section 2
  is empty and section 3 is `origin/main`. Without a remote or an upstream there's only the
  section with the name of the branch, plus the base branch if there's a local `main` or `master`
  that isn't the current branch.
- Commits are put in a section by reachability, not by their parent: a merge commit of `main` into
  the feature branch is in the section of the feature branch, and the commits it brought in that are
  on the base branch are in the section of the base branch. Every commit is listed once.
- Sections 1 and 2 are found first, with the commits of the current branch only, then the rest of
  the history streams into section 3 as before.

The short hash is abbreviated as git abbreviates it (`core.abbrev`, 7 characters or more, unique
in the repository, the same as `git log --oneline`), and drawn subtly, with the style of
`ui.text.inactive` (else `comment`): it is there to be found, not read.

Dividers are not entries: the cursor skips them (`j`, `k`, `Up`, `Down`, `Tab`, `C-n`, `C-p`,
`PageUp`, `PageDown`, `Home`, `End`, wrapping around the ends), they are not counted in the numbers
above the list, and a query hides them. They are drawn as a line across the picker, with the style of the
theme scope `ui.picker.divider` if the theme has it, else `ui.text.inactive`, else `comment`.

### Usage

| Keys | Action |
| --- | --- |
| `space l`, `:git-log` | Open the commit picker, in normal mode, with the cursor on its first row, the [uncommitted changes](#uncommitted-changes-live). `?` lists its keys. |
| `s` | Filter mode: typing filters by summary, and by hash: a query of 4 hex digits or more (`a1b2c3`) lists the commits whose hash starts with it first, then the commits whose summary matches it. `%commit`, `%author`, `%date` filter by the other columns. `Esc` goes back to normal mode and keeps the query. |
| `j`/`k`, `Down`/`Up`, `C-n`/`C-p`, `Tab`/`S-Tab`, `C-d`/`C-u`/`PageDown`/`PageUp`, `gg`/`ge`/`G`, `Home`/`End` | Move. The keys that aren't letters work in filter mode too. |
| `Enter` in the commit picker | List the changes of the commit, or of the selected commits, or the uncommitted changes, on top of the commit picker: one row per hunk, under a divider per file ([Lists of changes](#lists-of-changes)). Its prompt row shows the commit or the range and the totals: `a1b2c3d: 12 changes in 4 files, +80 -31 · Fix the parser`. The changes of a commit or range are read in the background: the log stays usable meanwhile, and its prompt row says `Loading changes of a1b2c3d…` if it takes more than a tenth of a second. Moving on and pressing `Enter` on another row, or closing the log, drops the list being read. |
| `v`, `A-v` | Toggle extend mode: while it's on, every movement extends the selection from the commit where it started to the commit under the cursor. `A-v` works in filter mode too. |
| `w`, `e` | Extend the selection down (older) to the oldest commit of the section the cursor is in, and put the cursor on it. Pressed again they do nothing, they never go past the divider. |
| `b` | Extend the selection up (newer) to the newest commit of the section the cursor is in, and put the cursor on it. |
| `S-Down`, `S-Up` | Extend the selection by one commit, in or out of extend mode. It doesn't wrap around the ends of the log. |
| `A-s` | Select all the commits of the section the cursor is in ("diff the entire branch"). |
| `Esc` | In normal mode: if several commits are selected or extend mode is on, select the commit under the cursor alone and leave extend mode; else clear the query if there is one; else close the picker. In filter mode: back to normal mode. |
| `q`, `C-c` | Close the picker. |
| `Enter`, `C-g` in the list of changes | Open the diff of the file at the hunk, on top of the list. |
| `C-s` / `C-v` in the list of changes | Open the file as it is now in a split, closing both pickers. |
| `q`, `Esc` in the diff | Back to the list of changes. |
| `Esc` in the list of changes | Back to the commit picker. |
| `Enter` in the diff | Open the file as it is now, at the current hunk, closing both pickers. |

The diff keys are the same as in the [side by side diff](#side-by-side-git-diff). Renames are
detected, so a renamed file is compared with its old path. The picker opens in normal mode, where
the letters are keys (the other letters do nothing, and pasting is ignored), the way the
[file tree](#file-tree-overlay) works; the prompt shows its cursor in filter mode only. `w`, `e`,
`b` and `v` are for Termux, where `Shift` and the arrows don't make `S-Down`. `w`, `e` and `b`
don't turn extend mode on or off: from normal mode without a selection they start one at the
commit under the cursor, and with a selection or in extend mode they keep the commit it started
from. With a query, they go to the last commit of the section that is shown in their direction,
and the selection has every commit between (see below).

### Ranges

The selected commits are shown in the rows with the style of `ui.selection` (the commit under the
cursor keeps the style of the cursor), and on the prompt row, oldest to newest, with `[extend]`
in front when extend mode is on:

```
[extend] a1b2c3d4e..d4e5f6a7b (5 commits)
```

- A selection is always contiguous in the log, dividers excluded: all the commits between the
  commit where it started and the commit under the cursor, in the order of the log without a
  query. With a query, the cursor moves among the commits that match, but the selection still has
  every commit between the two ends, matching or not (so does the count).
- A selection across sections takes the commits of both, in the order they are listed in.
- The uncommitted changes are never part of a range: extending from their row selects from the
  first commit.
- Any movement that isn't extending (and typing a query that moves the cursor) goes back to the
  commit under the cursor alone, unless extend mode is on. `A-v` itself keeps the selection.
  After `A-s`, extending goes on from the oldest commit of the section.
- `Enter` on several commits lists the combined change: the files that differ between the first
  parent of the oldest selected commit (nothing if it is a root commit) and the newest selected
  commit. A file added and deleted in the range isn't there, a file changed twice is there once.
  The diff of a file has the first parent of the oldest commit on the left (`a1b2c3d4e^: path`) and
  the newest commit on the right (`d4e5f6a7b: path`).
- One commit, selected alone, is listed and diffed as it always was: against its first parent, the
  root commit against nothing.

### Lists of changes

The changes of a commit, of a range, and the [uncommitted changes](#uncommitted-changes-live) are
listed in the same shape: one row per hunk, grouped under a divider per file.

```
   edit change
── gone.txt (deleted)  1 hunk  -1 ───────────────────
   --   gone.txt:1
── logo.png (binary) ────────────────────────────────
── src/lib.rs (new)  1 hunk  +2 ─────────────────────
   ++   src/lib.rs:1-2
── src/main.rs  3 hunks  +3 -3 ──────────────────────
   -+   src/main.rs:3
   -    src/main.rs:9
   +    src/main.rs:20
── src/old.rs → src/new.rs  1 hunk  +1 ──────────────
   +    src/new.rs:4
── Cargo.lock  +2 -1 ────────────────────────────────
```

- **Dividers** are drawn like the commit log's (`ui.picker.divider`, else `ui.text.inactive`, else
  `comment`), with the file's counts: hunks, lines added and removed and, in the uncommitted
  changes, how many hunks are staged and unsaved. Zero counts are left out. A renamed file shows
  `old → new`; a new, deleted or untracked file says so: `(new)`, `(deleted)`, `(untracked)`.
- **Files without hunk rows** are a divider alone: [lockfiles](#lockfiles), with their counts,
  last; binary files, `(binary)`; files without changed lines (an empty new file, a change of
  mode), `(new, empty)` or `(no lines changed)`. The cursor stops on these dividers, which are
  drawn with its `>` marker when it is on them, and Enter opens their diff.
- **The `edit` column** says what a hunk does, `-` in the color of `diff.minus` and `+` in the
  color of `diff.plus`, as the signs of the diff view:

  | Mark | The hunk |
  | --- | --- |
  | `+` | only adds lines |
  | `-` | only removes lines |
  | `-+` | removes and adds lines |
  | `++` | is in a file that is created (new or untracked) |
  | `--` | is in a file that is deleted |

  In the uncommitted changes it comes after the `S`/`M`/`•` marks of where the change is.
- **Rows** say where the hunk is, `src/main.rs:12-14` (removed lines: the line before them).
  Files are sorted by path, lockfiles last, and hunks by line.

| Keys | Action |
| --- | --- |
| `j`/`k` (and the other movements) | Move from hunk to hunk, skipping the dividers of files that have hunk rows. |
| `w`, `b` | The first hunk of the next file; the first hunk of the file the cursor is in, or of the previous file when it is there already. They don't wrap around. |
| `s` | Filter: a hunk matches by its row (`src/main.rs:12-14`), so a query matching a path lists all the hunks of that file. A divider is listed while any of its hunks match, and hidden otherwise; the rows stay in their order. `%edit ++`, and `%marks S` in the uncommitted changes, filter by the other columns. |
| `Enter`, `C-g` | The diff of the file at the hunk, where `n`/`N` go through the hunks of every file of the list. On a divider without hunk rows: its diff. |

The preview shows the hunk with a few lines of context, under its place, its counts and, in the
uncommitted changes, its marks: `src/main.rs:5 · +1 -1 · staged`. A divider's preview shows the
first hunk of its file.

### Limitations

- Merge commits are compared with their first parent only. In a range, the same goes for the
  parent of the oldest commit; the changes a merge brought in are part of the combined change,
  because they are in the newest commit.
- `Enter` in the diff opens the file as it is now in the working tree, so the line it jumps to may
  have moved since the commit. Files deleted since then can't be opened.
- Only the history of HEAD is listed, sorted by commit time within each section; there's no graph.
- The upstream is found with the default fetch refspec of its remote
  (`refs/heads/*:refs/remotes/<remote>/*`) when the configuration of the repository can't be
  trusted (workspace trust).
- The base branch is guessed by name, not by looking for the branch the current branch was
  created from.
- Queries with a negation (`!word`) match dividers: they are listed, but the cursor still skips
  them.
- In a shallow clone, the oldest commit can't be shown: its parent isn't in the repository.

### Ideas

- ~~List the commits that changed the current file~~: `%` then `space L`, the
  [line history](#line-history) of the whole file.
- Show the commit message and stats in a preview.
- Pick another branch.

## Line history

"Who changed this line, and why": the [commit log](#git-commit-log), showing only the commits
that changed the cursor's line, or the selected lines, like `git log -L`. It looks and works like
the commit log: the same columns, branch sections and dividers, modal keys and side by side diff.

```
   commit    date       author    summary
 > ...                            Uncommitted changes
── main (unpushed) ─────────────────────────────────
   d4e5f6a7b 2026-11-12 Ana       Handle empty input
── origin/main ─────────────────────────────────────
   a1b2c3d4e 2026-10-02 Bruno     Parse the header once
   9f8e7d6c5 2026-09-14 Ana       Add the parser (added)
```

### Usage

| Keys | Action |
| --- | --- |
| `space L`, `:line-history` | Open the line history of the cursor's line in normal mode, or of the lines every selection covers (several selections make several ranges). `%` then `space L` is the history of the whole file. |
| `Enter` | The side by side diff of the file in that commit, at the hunk that changed the lines (the first one, when several did). `n`/`N` go through that commit's hunks of this file. `q`/`Esc` come back to the line history. On the uncommitted changes: the diff of the file against HEAD (the buffer, with its unsaved edits), at the lines. |
| `o` | The whole commit: its list of changes, as `Enter` in the commit log. On a range, the list of the range; on the uncommitted changes, their list. |
| `Enter` on a range (`v`, `w`/`b`, `A-s`...) | The combined change of the file over the range (the [range rules](#ranges) of the commit log), at the first hunk that touches the lines as they are in the newest commit of the range. |
| `Enter` in the diff | Open the file as it is now, at the hunk, closing the line history. |
| `s`, `j`/`k`..., `v`, `A-v`, `w`/`e`/`b`, `S-Down`/`S-Up`, `A-s`, `Esc`, `q` | As in the [commit log](#usage-1). |

- **Rows**: the commits, newest first in the sections of the commit log (`B (unpushed)`, `B`, the
  base branch, with the same rules); the last one, which added the lines, is marked `(added)`.
  The first row is the uncommitted changes when any of the lines has uncommitted changes, staged
  or not, saved or not.
- **Prompt row**: what's followed and the count: `src/main.rs:12-14 · 7 commits`. While it
  searches it says how far it is: `searching… 1,240 commits` (the commits looked at). Several
  ranges read `src/main.rs:3,12-14`. `?` lists the keys.
- **Preview**: the hunks of the commit that touched the lines, each under its place and counts
  (`src/main.rs:12-14 · +3 -1`) with a few lines of context, as the lists of changes preview
  hunks.
- **No picker**, but a message, for a buffer without a file, a file outside a git repository, or
  one git doesn't track (or that has no commit yet). When none of the lines is committed (they were
  all added since HEAD), the prompt row says `these lines aren't committed yet` and the only row
  is the uncommitted changes.

### From the buffer to HEAD

The lines are those of the buffer, which may differ from HEAD. They are mapped to their place in
HEAD through the buffer's diff against HEAD, the one the gutter shows, the same way they are
mapped from a commit to its parent (below). A line edited since HEAD is followed into the line it
replaced; lines only in the buffer have no history and are left out (the uncommitted changes row
still shows them).

### Following the lines back

From HEAD, each commit's diff of the file against its parent maps the lines to the parent, as
`git log -L` does:

- a line the diff doesn't touch keeps its place, shifted by the lines added and removed above it;
- a range that starts or ends inside a hunk starts or ends with the hunk's old lines, so lines a
  commit rewrote (edited, split, joined) are followed into the lines they replaced;
- lines a commit added (a hunk without old lines) stop there: that commit is where they come from,
  marked `(added)` when a whole range ends there;
- a commit is listed when its diff touches the lines: replaces or adds some of them, or removes
  lines between two of them (not lines right above or below them);
- ranges that meet are merged, as git merges them.

The walk ends when every line has reached the commit that added it, without looking at the older
history. **Renames** are followed: when a commit's parent doesn't have the file, the parent's
version is found by rename detection (the one of the lists of changes, 50% similarity), and the
lines are followed under the old path from then on. A commit whose rename didn't change the lines
isn't listed.

**Merges** follow the rule of `git log -L` without `--first-parent`, which simplifies the history
of the lines as `git log <path>` simplifies a file's: when the lines are the same in one of the
parents (the first such parent, in order), the merge isn't listed and only that parent is followed;
when every parent's diff touches them, the merge is listed and each parent is followed with the
lines mapped to it. A listed merge is previewed and diffed against its first parent, like the commit
log does.

### Performance

`git log -L` walks the whole history before printing anything, so line history has its own walk
with gix, which lists commits as it finds them:

- **The picker opens at once**, with the uncommitted changes when they're there, and fills as
  commits are found, newest first. The walk runs on a thread of its own; the prompt row shows
  its progress.
- **A commit costs a lookup of the file in its tree**: when the file is the same blob as in the
  parent, the lines pass on without reading anything. Only the commits that changed the file are
  read and diffed.
- **It stops at the origin**, once every line reached the commit that added it.
- **Closing the picker stops it**: the walk checks after each commit whether its picker is still
  open, so nothing goes on in the background after it closes (with `q`, `Esc`, or opening a file
  from a diff on top of it).
- Commits are taken by section (unpushed, pushed, base branch), then newest first, so that a commit
  comes after its children and the lines a commit gets from all its children are known when it's
  looked at. Finding the commits of the first sections takes a walk of the branch's own commits,
  as in the commit log; the base branch's section needs none.
- Everything is in memory and goes when the picker closes: no cache, no file written.

Measured in a debug build, on repositories made by the tests' generator (`repo_gen.rs`), following
a whole file that one commit in 25 changes: with 20,000 commits, the first commit is listed after
20-35 ms and the walk ends after about 1 s (800 commits listed); with 50,000 commits, after about
25 ms and 2.3 s (2,000 listed). Closing the picker stops the walk within about 15 ms.

### Limitations

- The lines followed are a snapshot of the buffer when the picker opens: edits after that don't
  change the list.
- A commit dated before one of its children (clock skew) can be looked at before that child's
  lines reach it; those lines are then not followed further.
- When two sides of a merge renamed the file differently, the lines are followed under the path
  of the first side that reaches the common ancestor.
- In a shallow clone, the oldest commit is marked `(added)`: its parent isn't in the repository.
- A listed merge's preview and diff are against its first parent, which may not show the change
  that came from another side (the preview then says so).
- Resuming the picker (`space '`) after closing it shows what was found until then: the search
  isn't resumed.
- `Enter` in the diff opens the file at its path in that commit; if the file was renamed since,
  it opens the old path, which no longer exists.

## Uncommitted changes, live

The [commit log](#git-commit-log) starts with a row for what isn't committed yet, above the first
divider: `Uncommitted changes (3 files, 1 unsaved)`. Enter on it lists every change that isn't
committed, in the shape of the [lists of changes](#lists-of-changes), one row per hunk under a
divider per file, with where the change is:

```
   marks edit change
── notes.md (untracked)  1 hunk  +20 ────────────────
    M    ++   notes.md:1-20
── src/lib.rs  1 hunk  +1 -1  1 staged ──────────────
   SM    -+   src/lib.rs:3
── src/main.rs  2 hunks  +4 -1  1 staged  1 unsaved ─
   S     +    src/main.rs:12-14
    M•   -+   src/main.rs:40
── src/new_idea.rs (untracked)  1 hunk  +6  1 unsaved
     •   ++   src/new_idea.rs:1-6
── Cargo.lock  +340 -12 ─────────────────────────────
```

- `S`: the change is in the index (staged).
- `M`: it is in the file on disk and not in the index (unstaged).
- `•`: it is in the buffer and not on disk yet (unsaved).

A hunk has any combination of them: staged, changed again on disk since, and edited again in its
buffer is `SM•`. The hunks are those of the [side by side diff](#side-by-side-git-diff): HEAD
against the buffer of the file when it is open, with its unsaved edits, else against the file on
disk. So it doesn't matter who made a change: you, a formatter or an agent, written or not. A
buffer whose file has no other change (or isn't on disk yet, or is ignored by git) lists its
unsaved edits against the file on disk. Lines are those of the buffer when it is open, as in the
right side of the diff.

The list is a modal picker like the log, with the keys of the
[lists of changes](#lists-of-changes): `j`/`k` move, `w`/`b` jump between files, `s` filters (by
the `change` column; `%marks S` by the marks). The preview draws the hunk with a few lines of
context. Rows are sorted by path, then by line, with lockfiles last.

- **Enter** opens the diff of the file at that hunk (on a divider without hunk rows, its diff), where `s`/`u`/`r` stage, unstage and revert it
  ([hunk actions](#stage-unstage-and-revert-hunks)) and `n`/`N` go through the hunks of every file
  of the list.
- On a change that is only in the buffer (`•` alone), Enter opens the buffer at its first line
  instead, closing the log.
- `C-s`, `C-v` and the like open the file at the change, closing the log.

The prompt row says how much there is: `Uncommitted changes: 6 changes in 5 files, +80 -31, 2
unsaved` (a file without hunk rows is one change). The totals follow the list as it changes.

While the commit log, or anything opened from it, is open, the working tree is watched, and what
is on screen follows it:

- the count of the first row;
- the list of uncommitted changes, also while buffers are edited: the cursor stays on the same
  hunk (the same file and line), or on the first change of the same file, or at the same place
  when the file is gone; and its totals on the prompt row;
- the diff of an uncommitted file: the line at the top of the screen stays there, and so does the
  horizontal scroll. A diff with no changes left stays open, empty, with `no changes` in the
  footer;
- the log itself when HEAD moves (a commit, a checkout, a reset), with the cursor on the same
  commit;
- the open buffers of the files that changed, with the rule of
  [files changed on disk](#files-changed-on-disk).

The watch covers what git doesn't ignore (`.gitignore` files at any depth, `.git/info/exclude`, the
global excludes file), plus `HEAD`, the index and `logs/HEAD` in the git dir. A nested repository
(a directory holding a `.git` file or directory: a submodule, a repository cloned inside this one)
is left out, as git leaves its files out of this repository's status: it isn't walked, and changes
inside it are dropped; its own entry (created, removed, renamed) still counts. How the tree is
watched depends on what the platform's file watching can do:

- **One watch per directory** (inotify on Linux, and every backend but FSEvents), so `target/` or
  `node_modules/` cost nothing. The walk that places them runs in the background, on the
  watcher's thread: opening the log never waits for it (in a release build on Linux, about
  75 ms for 400 directories, 1.3 s for 15,000). A file changed in a directory the walk hasn't reached yet
  isn't missed: the walk reports what was modified since the watch started, and the views
  refresh once with it.
- **One recursive watch on the root** (FSEvents on macOS), which watches a whole tree for the cost
  of one watch, so there is no walk at all; events from ignored directories are dropped by the
  same rules. A watch per directory would restart FSEvents' stream for every directory watched.

Changes are applied 200ms after the last one (at most a second after the first, when they never
stop). The watch stops when the last of these views closes.

### Lockfiles

In the lists of changes, of a commit or uncommitted, lockfiles are never hidden, but each is
a divider alone, without hunk rows, with the lines it adds and removes, after the other files:

```
── src/main.rs  1 hunk  +2 -1 ───────────────────────
   -+   src/main.rs:12-13
── Cargo.lock  +340 -12 ─────────────────────────────
```

The cursor stops on it, and Enter opens its diff like any file. The other files are sorted by path. Which files are
lockfiles is set by their name in `[editor.changes]`:

```toml
[editor.changes]
lockfiles = ["Cargo.lock", "package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock",
  "bun.lockb", "poetry.lock", "uv.lock", "go.sum", "Gemfile.lock", "composer.lock", "flake.lock"]
```

A binary lockfile (`bun.lockb`) shows `(binary)` instead of counts.

### Limitations

- The uncommitted changes are one row per hunk, but a hunk is still staged whole, from its diff.
- The changes of a commit or range are read when the list opens, all of them before it shows,
  in the background: 2,000 changed files take about a quarter of a second (in a release build),
  a range of tens of thousands a few seconds, during which the log stays usable. They don't
  stream in as they are read. The uncommitted changes are still read on the main thread when
  their list opens (they are refreshed in the background).
- The cursor never rests on the divider of a file that has hunk rows, as in the commit log: the
  file's first hunk is where `w`/`b` go, and Enter there opens the diff at it.
- A buffer with unsaved edits is listed by its path: a buffer without a file (a scratch buffer)
  isn't.
- Only the commit log's views update live. The changed files picker (`space g`) and `space =` don't,
  and the changes of a commit don't need to.
- A directory that a `.gitignore` edit stops ignoring isn't watched until it is created again
  (with a watch per directory).
- Symlinked directories are not followed.
- A change inside a submodule or another nested repository isn't seen live. Its files aren't part
  of this repository's status either, but a submodule as a whole is (new commits, a dirty
  checkout): that shows at the next refresh another change brings, or when the log opens again.
  With a watch per directory, a directory whose `.git` is removed is watched again only when the
  watch restarts.

## Stage, unstage and revert hunks

In the diff of a file of the working tree (the [uncommitted changes](#uncommitted-changes-live) of
the commit log, `space =`, `C-g` in `space g`), the current hunk, or the whole file, can be staged,
unstaged and reverted:

| Keys | Action |
| --- | --- |
| `s`, `S` | Stage the hunk, the file (`git add`; a deleted file is staged as deleted) |
| `u`, `U` | Unstage the hunk, the file (back to its version in HEAD; a new file becomes untracked) |
| `r`, `R` | Revert the hunk, the file, to its version in the index |

The footer shows the state of the current hunk, `hunk 2/5 staged`, and `?` lists the keys. On the separator
between the two sides, a staged hunk is marked `┃` and a partly staged one `╏` (the colors of
`diff.plus` and `diff.delta`), and the background of a staged hunk is quieter.

- **Staging** changes no text, so it is not part of undo: `u` is its inverse. Each action writes
  one entry of the index, with the git CLI (`git hash-object`, `git update-index`), from the texts
  the diff shows: the hunk is applied to the index version, never as a patch, so it can't fail on
  context. Git doesn't run the filters or the fsmonitor of the repository's configuration: the
  conversion to what is stored (like `core.autocrlf`) is done by the same code as the reads, with
  the same workspace trust rules (`:workspace-trust`).
- **Reverting** is an edit: the file's buffer (opened in the background if it isn't open) gets one
  transaction, and is written. `u` in that buffer undoes it, and a whole file is one undo step.
  It goes back to the index, like `git restore`: what is staged is kept, so a staged hunk has
  nothing to revert (`The hunk is staged: unstage it first`).
- An **untracked file** has no version to go back to: reverting it deletes it, after asking
  (`Delete untracked x.rs? (y/n)`: `y` deletes it, any other key keeps it), since git can't bring
  it back.
- A buffer with unsaved changes can't be staged or reverted (write it first): the diff shows the
  buffer, which is not what is on disk.
- After an action the diff is built again at once; in the commit log it also follows changes made
  from elsewhere, like `git add` in a terminal.

`n`/`N` go through the hunks of every file of the entry the diff was opened from: the uncommitted
changes, a commit, or a range of commits. Past the last hunk of a file, `n` opens the diff of the
next file at its first hunk (`N`: the previous file at its last hunk), and the cursor of the list of
files follows. At the end of the list they go around to the first (last) file. Lockfiles and files that can't be shown (binary) are
skipped.

### Limitations

- A hunk is staged whole. When the index has part of a hunk's change, the hunk is partly staged,
  and `s` stages the rest.
- The diff is HEAD against the working tree, so a change that is staged and then undone in the
  working tree has no hunk (`U` unstages it).
- Reverting a renamed file reverts its new path only.
- Symlinks and submodules can't be staged.

## Files changed on disk

Open buffers are compared with their file when it matters, without a background watcher:

- after switching to another buffer (`ga`, the buffer picker, `:bn`, a jump, a split that shows
  another buffer...);
- when the terminal regains focus, and when going back to the editor from the
  [terminal panel](#terminal-panel) (`Ctrl-t`, a click, `vx`);
- right before writing (`:w`, `:wq`, `:x`, `:wa`, auto-save);
- while the [uncommitted changes](#uncommitted-changes-live) are watched, on every change of a
  file.

Every open buffer is checked on focus; the other cases check the buffers concerned. What happens
depends on the buffer:

| The buffer | What happens |
| --- | --- |
| has no unsaved changes | It is reloaded silently, as one undoable step (`u` brings the old text back). Cursors and selections are mapped through the change, so they stay on the same text when it is still there |
| has unsaved changes | The statusline warns once, `<file> changed on disk`. `:write` (and `:wq`, `:wa`...) refuses with the same message until `:reload` (the disk wins, undoable) or `:write!` (the buffer wins) |

A change is a change of content: the modification time and size are compared first, and when they
differ the file is read and hashed, so `touch` or a checkout that rewrites the same bytes changes
nothing. A file that was deleted is left alone (writing creates it again), and so is a buffer
whose file didn't exist when it was opened.

### Limitations

- Nothing is checked while Vortex is focused and idle, unless the uncommitted changes are watched:
  the check comes with the next buffer switch, focus or write.
- Terminals that don't report focus (or tmux without `focus-events on`) only get the checks on
  switching and writing.
- Writing a buffer to another path (`:w other.txt`) doesn't check that path.

## Conflict diagnostics

The conflict blocks git writes into a file it couldn't merge are diagnostics, of their own
severity, `conflict`:

```
fn main() {
◆   <<<<<<< HEAD                  └─ Conflict: HEAD vs feature-x
        let a = 100;
    ||||||| 1a2b3c4               (with merge.conflictStyle diff3 or zdiff3)
        let a = 1;
    =======
        let a = 10;
    >>>>>>> feature-x
}
```

- **Only in unmerged files**: those git reports as unmerged (with conflict stages in the index,
  as `git status` lists them under "Unmerged paths"). Docs and test fixtures with marker-like
  lines aren't flagged, and neither is a file once it is `git add`ed.
- **One diagnostic per block**, from the `<<<<<<<` line to the `>>>>>>>` line, with the labels of
  the markers: `Conflict: HEAD vs feature-x` in a merge, `Conflict: HEAD vs 1a2b3c4 (Add the
  cache)` in a rebase or cherry-pick (`ours`/`theirs` when a marker has no label). Markers are at
  the start of a line, at least 7 characters long (longer with the `conflict-marker-size`
  attribute), and only complete blocks count.
- **Above errors**:
  - The gutter shows `◆` on the block's first line.
  - `space d` and `space D` list conflicts first (severity `CONFLICT`, source `git`).
  - `]d` and `[d` go to them, selecting the whole block.
  - The diagnostic shows inline like the others, and `conflict` passes every minimum-severity
    filter (`[editor.inline-diagnostics]`, `end-of-line-diagnostics`).
- **`space D` also lists the conflicts of the unmerged files that aren't open**, read from the
  file on disk. Enter opens the file with the block selected.
- **Statusline**: `conflict` is a severity of the `diagnostics` and `workspace-diagnostics`
  elements (`◆ 2`), and is in their default lists: `["warning", "error", "conflict"]`. The
  workspace count includes the unmerged files that aren't open.

### Live

- **On every edit** of an unmerged file, its blocks are found again in the text, so deleting the
  markers of a block clears its diagnostic at once (and `u` brings it back).
- **Which files are unmerged** is read from git's index:
  - when a file is opened, and after switching buffers;
  - when the terminal regains focus, and when going back to the editor from the
    [terminal panel](#terminal-panel), with the [disk checks](#files-changed-on-disk);
  - while the [uncommitted changes](#uncommitted-changes-live) are watched (the commit log or a
    diff view is open), on every change of the repository: a merge, rebase or cherry-pick that
    starts or ends shows up at once;
  - when `space D` opens.

The repositories looked at are those of the working directory and of the open files. There are
no commands to take one side: a block is resolved by editing it (`:practice multi-cursor` has an
exercise).

### Theme

| Scope | Falls back to | Used for |
| --- | --- | --- |
| `conflict` | `error` | the gutter symbol, the statusline count, the pickers' severity column, the popup at the cursor |
| `diagnostic.conflict` | `diagnostic.error` | the text of the block |
| `conflict.diagnostic.inline` | `error.diagnostic.inline` | the inline message |

The default theme, `vortex`, underlines the text of a block twice (`diagnostic.conflict`); with
the other bundled themes conflicts look like errors with their own gutter symbol.

### Limitations

- A block missing a marker (half resolved, with the `=======` or a `>>>>>>>` line left) isn't
  reported: only complete blocks are.
- A conflict without markers (binary files, a file modified on one side and deleted on the
  other) has no diagnostic and no row in `space D`.
- While Vortex is focused and idle, with no live view open, a merge started from another terminal
  shows up with the next buffer switch or focus. A merge run in the [terminal panel](#terminal-panel)
  shows up when going back to the editor.
- Nested repositories (a submodule inside the working directory) are only looked at for the files
  open in them, when no file of the outer repository is.

## Staged files in the changed files picker

Upstream's changed files picker (`space g`) only lists unstaged changes, so files disappear from it
as soon as they are `git add`ed. In this fork it lists every change compared to HEAD, like
`git status`, staged or not:

- new files that are staged are listed as `+ added` (unstaged ones stay `+ untracked`);
- staged renames (`git mv`) are listed as renamed;
- a file that is staged and then changed again is listed once;
- a file that is added and then deleted from disk is not listed, since it matches HEAD again.

## File tree overlay

A file tree that looks and works like the file picker (`space f`): the same overlay, the prompt line
on top, the rows of the tree on the left and a preview of the selected file or directory on the
right. It is built on Vortex's own picker, so the overlay, the prompt and the preview are the
picker's. Like the file picker it hides the preview on narrow screens (below 73 columns), where the
rows get the whole width; `C-t` hides and shows the preview.

The tree is rooted at the workspace root (the git repository holding the working directory, like
upstream's file explorer picker). Directories come first, then files, in the order of their names
regardless of case. Each row shows the indentation (two spaces per level), `+` for a collapsed
directory and `-` for an expanded one (a blank for a file), and the name, with a `/` after the name
of a directory. The `.git` directory is never listed. Files and directories ignored by git
(`.gitignore` files, including nested ones and the ones of parent directories,
`.git/info/exclude` and the global excludes file) are shown dimmed instead of hidden, or are left
out with `hide-gitignored`. The file of the current buffer is shown in bold. There are no icons.

Files changed in git are marked like in VS Code's explorer: the name takes the color of the change
and a letter follows it, `M` modified, `A` added (staged), `U` untracked, `R` renamed and `!` in
conflict. Directories holding changes, deleted files included, show a `●` in the color of their
most important change (conflict, then modified, deleted, renamed, added, untracked).

In this fork the tree replaces upstream's file explorer picker on `space e` (and takes `space E`).
The picker is still on `space .` (current buffer's directory) and in the command palette
(`file_explorer`).

### Usage

Vortex pickers send the letters typed to their prompt, but the tree needs them as keys, so it uses
the modes of the picker (`Picker::with_modes`, which the [commit log](#git-commit-log) uses too).
In normal mode (the one it opens in) the keys move in the tree and act on its entries, and the
other letters do nothing; `s` enters filter mode, where the keys go to the prompt. The prompt shows its cursor in filter mode only.

| Keys | Action |
| --- | --- |
| `space e` | Open the tree at the workspace root, with nothing expanded. |
| `space E` | Open the tree expanded down to the file of the current buffer, with the cursor on it. |
| `j`/`k`, arrows, `C-n`/`C-p`, `Tab`/`S-Tab` | Move. `C-d`/`C-u`/`PageDown`/`PageUp` move a page, `gg`/`ge` (or `G`), `Home`/`End` go to the top/bottom. |
| `l`, `Right` | Expand a directory (or move into an expanded one), open a file and close the tree. |
| `h`, `Left` | Collapse a directory, or go to the parent directory. |
| `Enter` | Expand/collapse a directory, open a file and close the tree. |
| `A-Enter` | Open a file and keep the tree open. |
| `C-s`, `C-v` | Open a file in a horizontal/vertical split and close the tree. |
| `C-t` | Show/hide the preview. |
| `s` | Filter mode: see below. |
| `a` | Create a file in the selected directory (or next to the selected file). Missing parent directories are created, and a name ending in `/` creates a directory. |
| `r` | Rename. The new name may contain `/` to move the entry to a subdirectory. Changing only the case of a name works on case-insensitive filesystems too. |
| `x`, then `p` | Cut the selected entry, then move it into the selected directory (or next to the selected file). |
| `d`, then `y` | Delete the selected file or directory (directories are deleted recursively). Any other key cancels. |
| `R` | Read the whole tree from disk again. |
| `H` | Show/hide git ignored files for this session. |
| `?` | List the keys in a box over the tree; any key closes it. |
| `Esc` | Clear the filter if there is one, otherwise close the tree. |
| `q`, `C-c` | Close the tree. |

Filter mode, entered with `s`, is the prompt of the file picker: type to filter, edit with the usual
prompt keys, and move with `C-n`/`C-p`/`Up`/`Down`/`Tab`/`S-Tab`. The query is matched fuzzily
against the paths relative to the workspace root, with the same smart case and path scoring as the
file picker, over the whole workspace and not only over the expanded directories. The tree then
shows the matching files and directories with all the directories above them, expanded, in tree
order, with the matched characters of the names highlighted and the cursor on the best match.
`Enter` expands/collapses a directory or opens a file as in normal mode; on a directory that
matched by its name only (nothing in it matches), it clears the filter and opens the directory in
the tree. `Esc` goes back to normal mode
and keeps the filter, so that the keys above work on the filtered tree (collapsing a directory hides
its matches); `s` resumes editing the query; `Esc` in normal mode clears it. With the filter cleared,
the tree is back as it was, with the entry that was selected shown in it.

Files that git ignores are not part of the filter (they are not walked, which keeps `target` and
`node_modules` out of it), hidden files are. The walk of the workspace runs in the background the
first time a filter is typed and is kept until the tree is closed or read again (`R`); until it is
done the tree is shown unfiltered.

Creating, renaming, moving and deleting go through the same code as `:move`, so language servers
are notified (`willRename`, `didCreate`, ...). Open buffers follow renamed and moved files,
including files inside a moved directory. Deleting a file closes its buffers, and deleting is
refused when one of them has unsaved changes. Moves and renames never overwrite an existing file.
The names for `a` and `r` are asked in Vortex's usual prompt on the command line. After an operation
the tree stays open and shows the affected entry with the cursor on it (the filter is cleared if
there was one, so that it can't hide the entry).

The expanded directories are kept while the tree is open, and not between two openings. Directory
listings are kept too: after an operation only the directories whose modification time changed (or
their `.gitignore`'s, with everything below it) are read again. `R` reads everything again. The git
status is read in the background (the same as `space g`) when the tree opens, after each operation
and when a buffer is saved.

### Configuration

```toml
[editor.explorer]
hide-gitignored = false  # hide git ignored files instead of dimming them
git-status = true        # mark files changed in git and the directories holding them
```

The options of the side panel this tree used to be (`width`, `icons`, `auto-reveal`) are still
accepted in a config file and ignored, so that existing configs keep loading.

The commands are `toggle_explorer` (`space E`) and `toggle_explorer_root` (`space e`); they keep the
names they had as the toggle of the side panel, so that existing keymaps keep working.

Theme scopes: `ui.explorer.ignored` (ignored entries, defaults to `ui.text.inactive`, or dim text),
`ui.text.directory` (directories), `special` (the matched characters of a filter), and the scopes
of the file picker for the rest (`ui.text.focus` for the selected row, `ui.background`,
`ui.background.separator`, ...). The git markers use the scopes of the changed files picker:
`diff.plus` (added, untracked), `diff.delta` (modified), `diff.delta.moved` (renamed), `diff.minus`
(deleted) and `diff.delta.conflict` (conflict).

### Limitations

- The tree doesn't watch the disk: files created outside the editor show up when the tree is
  opened again or on `R`. The same goes for the git status after a git command run outside the
  editor (`git commit`, `git checkout`).
- Changes to the global git excludes file or `.git/info/exclude`, and to a `.gitignore` of a
  directory above the workspace root, are only picked up by `R`.
- One entry at a time: there is no multi-selection for moving or deleting several files.
- Deleting is permanent, there is no trash.
- Directories are read on the UI thread. That takes about 1 ms per 1,000 entries (see
  `cargo bench -p vortex-term --features bench --bench explorer`), so only a directory with tens of
  thousands of entries, or a slow network filesystem, pauses the editor noticeably. Filtering
  matches every path of the workspace on each key typed, which the same benchmark measures too.
- Copying files isn't supported, only moving.
- The mouse does nothing in the tree, like in other pickers.

### Ideas

- Copy and paste (`c`/`p`), and a multi-selection for batch moves and deletions.

## Inline git blame

Greyed-out text at the end of a line about the commit that last changed it, GitLens style:

```
  5  line 5 changed by bob    a1b2c3d Bob Smith, 3 days ago • Change line five
 27  brand new line    Not committed yet
```

The commit's short hash comes first, abbreviated as git does (`core.abbrev`, unique in the
repository); lines that aren't committed have none.

Off by default; turn it on with `space B` or in `[editor.inline-blame]` (below).

### Usage

| Keys | Action |
| --- | --- |
| `space B`, `:toggle-inline-blame` | Turn inline blame on or off |
| `blame_line` (command palette, unbound) | Show the current line's blame in the statusline, also when inline blame is off |
| `space i`, `:yank-diagnostic` | Yank the diagnostic under the cursor to the clipboard; with none, and inline blame on, yank the full hash of the line's commit instead (see below) |

### Configuration

```toml
[editor.inline-blame]
enable = true              # default false
show = "cursor-line"       # or "all-lines" (every visible line)
format = "{commit} {author}, {time-ago} • {title}"   # also {date} {email} {body}
delay = 300                # ms the cursor rests on a line before it shows; 0 = at once
```

The variables of `format`: `{commit}` (the short hash), `{author}`, `{email}`, `{date}`
(`YYYY-MM-DD`), `{time-ago}`, `{title}` (the first line of the message) and `{body}` (the rest).
The text before a variable that has no value (`{body}` of a one-line message) is left out with it.

Theme scope `ui.virtual.inline-blame`; themes that don't define it use `ui.virtual.inlay-hint`,
so it looks like inlay hints everywhere. The hash (`{commit}`) has its own scope,
`ui.virtual.inline-blame.hash`, patched over the blame's style (so without it, it looks like the
rest of the blame).

### Yanking the commit hash

`:yank-diagnostic` (upstream's, unbound there) is on `space i` in this fork, "the info under the
cursor". It yanks the messages of the diagnostics under the primary selection, to the clipboard
(`+`) by default, to a register given as its argument (`:yank-diagnostic a`) or typed before the
key (`"a space i`). When there is no diagnostic there and inline blame is on, it yanks the full
hash of the commit of the cursor's line instead, and says so:
`No diagnostic here: yanked the commit a1b2c3d4e5f6… of line 12 to register +`. Diagnostics
(git [conflicts](#conflict-diagnostics) included) always win. On a line that isn't committed,
with blame off or not computed yet, it yanks nothing and the error says why.

### Behaviour

- Shown in normal and select mode only, hidden in insert mode. `cursor-line` shows only in the
  focused view; the diff viewer never shows it.
- New and edited lines, unsaved ones included, show "Not committed yet", and so does every line of
  a file that is in a repository but not committed. Files outside a repository, scratch buffers
  and empty lines show nothing.
- The blame of a file is computed in the background (at most one computation per file and two
  at once) and cached per document. It's refreshed on save, `:reload`, terminal focus and when
  turned on; a refresh only reads `HEAD` (~0.3 ms) unless a commit was made, and then it also
  updates the diff base so the git gutter follows external commits.
- Commits listed in [`.git-blame-ignore-revs`](.git-blame-ignore-revs) at the root of the
  working tree, or in a file named by git's `blame.ignoreRevsFile` setting, are passed over like
  `git blame --ignore-revs-file` does: their lines show the commit their parent blamed them on
  (so the rename commit never shows, see [History](#history)). A line is matched to the parent's
  line at the same offset within its hunk; lines a listed commit added beyond what it replaced
  stay on it (`vortex-vcs/src/git/blame/ignore_revs.rs`).
- Cost (release build, measured on upstream's full history): `vortex-term/src/commands.rs`,
  7,273 lines touched by 1,141 commits, takes 1.5 s; small files 3-160 ms. It depends on the
  file's history, not its size. Showing a line is a lookup (~0.4 µs). `cargo bench -p vortex-vcs --features git --bench blame`.

### Limitations

- In a debug build blame takes ~6x longer than release (commands.rs: ~9 s; the crates it spends
  its time in are optimized even in debug builds via `[profile.dev.package]`, otherwise it would
  be ~2 minutes). The editor stays responsive meanwhile.
- After a commit made outside the editor, blame updates on the next save, `:reload` or terminal
  focus, not by itself.
- Blame is for the committed file at `HEAD`; staged-but-uncommitted lines show "Not committed yet".
- A change to the ignored revisions applies to blames computed after it: the next commit, or a
  file opened afterwards.

## `x` on an empty line

Upstream's `x` (`extend_line_below`) on an empty line selects that line and the next one
([helix-editor/helix#3077](https://github.com/helix-editor/helix/issues/3077)): the cursor on an
empty line already covers the whole line (its line ending), so `x` takes the line as selected and
extends. That breaks `xd` and `<count>x` (with relative line numbers, `3x` should select 3 lines).
In this fork `x` selects only the empty line, `3x` selects exactly 3 lines starting at it, and
pressing `x` again extends by one line as usual (`xx` selects 2 lines, `xxx` 3).

- It works for each cursor, so multiple cursors can be on empty and text lines at once. Lines with
  only whitespace are text lines, and `X`, `extend_line_above` and `extend_line` are unchanged.
- An empty line selected by `x` can't be told from a cursor on it, so `x` remembers the selection
  it set (`vortex-term/src/handlers/line_select.rs`) and forgets it on any other selection change or
  edit: `x`, `k`, `j`, `x` on an empty line selects just that line, but a selection that moved and
  came back is a cursor again. An empty line selected by another command (`X`, for example) is a
  cursor to `x` too.

## Move lines

Moves the lines touched by the selections up or down by one line, or by a count of lines, and keeps
the selections on the moved text. Upstream has no such command
([#2245](https://github.com/helix-editor/helix/issues/2245) was rejected); the usual macro
workaround overwrites registers, misbehaves at the end of the file, loses text with several
selections and drops the column.

### Usage

`M` in normal or select mode enters the sticky *move mode* (like `Z` is for the view), where:

| Keys | Action |
| --- | --- |
| `j`, `down` | `move_lines_down` |
| `k`, `up` | `move_lines_up` |

`3j` moves the lines 3 lines down, as one undo step. The mode stays until `Esc` (in select mode
that `Esc` also leaves select mode, as it does in the view mode), so `Mjjj` moves three times, as
three undo steps. Other keys do nothing in the mode. The which-key popup shows the keys and the
statusline shows `MOV` in place of `NOR`/`SEL`, in the colors of the mode that it was entered from.

There are no Alt or Ctrl keys, they are taken by window managers on some systems. `A-up` and
`A-down` are upstream's `expand_selection` and `shrink_selection` as before. The commands are
static commands, so they can be bound anywhere, for example in `config.toml`:

```toml
[keys.normal]
"C-j" = "move_lines_down"
"C-k" = "move_lines_up"
```

They are also in the command palette, and as the typable commands `:move-lines-down [count]` and
`:move-lines-up [count]` (one undo step too).

### Behaviour

- Every line touched by a selection (`Range::line_range`, so a selection that ends at the start of
  a line doesn't touch that line) moves. Each selection stays on the moved text, with the same anchor
  and head, so the direction and the column are kept.
- Line ranges that overlap or are adjacent are one block, which moves as a whole. Moving a block
  down swaps it with the single line below it. With several blocks every block moves by the same
  number of lines, and the lines between them stay between them. A count of `n` is `n` of these
  moves, in one transaction.
- At the top (bottom) of the file, everything moves by the room the nearest block has, and by
  nothing if there is none: then nothing changes, not even the undo history. The empty line after
  the final line ending is not a line, a cursor on it doesn't move.
- A last line without a line ending stays the last line without one: the line ending is passed on to
  the line that takes its place. `\r\n` files keep `\r\n`.
- No register or the clipboard is used. The lines are moved by a transaction that only deletes and
  inserts the lines that the blocks jump over, so the blocks themselves are not edited and the
  selections of other views on the same file follow them.

### Limitations

- The indentation is not changed to fit the new place of a line.
- If the file has no final line ending, a selection that ends with the line ending of the line that
  becomes the last one is shortened by it, as that line ending is gone.

## Terminal panel

A persistent shell in a panel below all the splits (not a split itself), with a statusline
indicator that tells what the shell is doing while the panel is hidden.

### Usage

| Key or command | What it does |
| --- | --- |
| `Ctrl-t` | From the editor (normal, select or insert mode): shows and focuses the panel, starting a shell when none runs. From the panel: back to the editor, the panel stays shown |
| `Esc` | In normal mode, with nothing pending and no popup: hides the panel. The shell keeps running |
| `:sh cmd`, `:run-shell-command cmd` | Shows and focuses the panel and runs `cmd` in its shell, as if typed and Enter pressed |
| `:sh` | Shows and focuses the panel, starting a shell only if none runs |
| `:! cmd`, `:shell-popup cmd` | Runs `cmd` on its own and shows its output in a popup (what `:sh` used to do). `:!cmd` without a space works too |
| `exit`, `Ctrl-d` in the shell | Closes the panel for real; the next `Ctrl-t` or `:sh` starts a fresh shell |

While the panel is focused, every key goes to the shell except `Ctrl-t`: `Esc`, `Ctrl-c`, `Ctrl-w`,
arrows, function keys... are translated to what an xterm sends, in the modes the program asked for
(application cursor keys, bracketed paste). Full screen programs work: colors (indexed colors stay
indexed, so your terminal's palette applies; RGB colors are passed through), the cursor and its
shape, the alternate screen, mouse reporting, resizing. The mouse wheel over the panel scrolls the
scrollback (or goes to the program when it asked for the mouse, or becomes arrow keys on the
alternate screen); a click in the panel focuses it, a click in a split focuses the editor. The
panel has no border or title of its own: it starts right below the statusline of the split above,
which shows as inactive while the panel has the keys.

### `vx` in the panel

The shell gets a `vx` first on its `PATH` (the only program the panel adds), and `EDITOR`,
`VISUAL` and `GIT_EDITOR` set to `vx --wait` (its absolute path), which never starts a nested
Vortex. It is the `vx` binary itself in a client mode, talking to
this editor over a private Unix socket (`VORTEX_TERMINAL_SOCKET`):

- `vx` alone focuses the editor (the panel stays shown).
- `vx file[:line[:col]] ...` opens the files in the editor and focuses it.
- As `$EDITOR` (`git commit`, `git rebase -i`, `crontab -e`...) it opens the file and waits until
  that buffer is closed. `:wq` or `:q` on it closes the buffer (not the split, nor the editor,
  even when it is the last one) and focuses the panel again; `:q!` closes it unsaved, so git
  aborts as usual on an empty message.
- `vx --help`, `--version`, `--health` and `--grammar` run normally.

### Shell indicator

The statusline element `shell` (in the default layout, before the selections) shows `SH` while a
shell runs, in a color for its state, also while the panel is hidden:

| State | Theme scope | Fallback |
| --- | --- | --- |
| at the prompt | `ui.statusline.shell` | the statusline's colors |
| a command runs | `ui.statusline.shell.running` | yellow |
| the last command exited 0 | `ui.statusline.shell.success` | green |
| the last command failed | `ui.statusline.shell.failure` | red |

Success and failure stay until the panel is focused, then it is back to idle. The states come from
OSC 133 markers (prompt, command, exit code), injected into bash and zsh when the shell starts,
the way VS Code does it, without touching your rc files: bash runs with `--init-file` pointing to a
script that sources your `~/.bashrc` and then adds its hooks (`PS0`, or a `DEBUG` trap before bash
4.4, and the front of `PROMPT_COMMAND`, keeping `$?` for your own prompt); zsh runs with a
`ZDOTDIR` whose startup files source yours and then add `precmd`/`preexec` hooks, and restore your
`ZDOTDIR`. Shells that send OSC 133 themselves work too. For other shells, a command is running
when the terminal's foreground process group isn't the shell's; there is no exit code then, so the
indicator goes back to idle.

### Configuration

```toml
[editor.terminal]
height = 30              # percent of the editor's height (10 to 90)
shell = []               # the shell and its arguments; empty: $SHELL, else /bin/sh
shell-integration = true # inject the OSC 133 markers into bash and zsh
scrollback = 10000       # lines

[editor.statusline]
right = ["diagnostics", "shell", "selections", "register", "position", "file-encoding"]
```

The panel's colors are `ui.terminal` (background and default text; falls back to
`ui.background`). A theme can draw the whole panel in one color at each color's own brightness,
like an old monochrome monitor, with `terminal-phosphor` in its
[effects](#the-phosphor-filter).

`[editor.terminal]` was already the debugger's external terminal (`command`, `args`); those keys
still work there.

### Behaviour

- Typed input waits for the shell to be ready: `:sh cmd` on a new shell holds `cmd` until the
  first prompt (bash and zsh drop what is typed while they start), 3 seconds at most.
- Typing in the panel scrolls the view back down to the prompt.
- Quitting Vortex kills the shell (and what runs in it).

### Theme effects

Themes are data, and so are their effects: an optional `[effects]` table in a theme file says how
the dashboard draws its background and title, what glitches, and whether the terminal panel goes
phosphor. Every effect is off when a theme doesn't set it, and an effect that is off costs
nothing: a theme without the table draws and idles exactly as one with no effects at all.

```toml
[effects]
dashboard-charset = "ascii"     # "blocks" (default) or "ascii"
dashboard-logo = "classic"      # "runes" (default) or "classic"
glitch-logo = 0.6               # chances per cell per second, 0 (default, off) to 10
glitch-background = 0.004
glitch-diagnostics = 0.01
terminal-phosphor = "phosphor"  # a color: a palette name, "#rrggbb" or 0-255; default off
```

| Key | Values | Default | What it does |
| --- | --- | --- | --- |
| `dashboard-charset` | `"blocks"`, `"ascii"` | `"blocks"` | The dashboard's vortex and the title's opening drawn with shaded blocks, `·░▒▓█`, or with an ASCII ramp, `.:=#&@` |
| `dashboard-logo` | `"runes"`, `"classic"` | `"runes"` | The large title: the runic wordmark, or the block letters with a shadow the dashboard had first |
| `glitch-logo` | 0 to 10 | 0 | The glitch rate of the title's letters (not their shadow) |
| `glitch-background` | 0 to 10 | 0 | The glitch rate of the vortex's cells (never in a panel) |
| `glitch-diagnostics` | 0 to 10 | 0 | The glitch rate of the ghost text of error diagnostics |
| `terminal-phosphor` | a color | off | The terminal panel drawn through a phosphor filter of that color |

The rates are chances per cell per second: `glitch-diagnostics = 0.01` gives each character of an
error's ghost text a 1% chance a second to glitch.

The table is read once, when the theme loads, and inherited through `inherits` key by key, like
the palette: a theme that inherits from `matrix` and sets only `dashboard-logo = "runes"` keeps the
Matrix's other effects. A wrong value (a name that isn't one of the choices, a rate that isn't a
number from 0 to 10, something that isn't a color, a key that isn't an effect) is a warning like
the theme's other ones (in the log, and in `cargo xtask themecheck`), and that effect keeps its
default: the theme still loads.

### The glitch

A glitched cell shows a random character for 100 ms, then goes back to what it was. One
component does it (`vortex-term/src/ui/glitch.rs`), for any set of cells on screen: the title,
the vortex and error diagnostics use it now, each with its own rate and duration.

- **When.** Glitches are a Poisson process over the cells on screen: with `n` cells at a rate `r`,
  the time to the next glitch is drawn from an exponential distribution of rate `n × r`, and it
  lands on one of the cells at random. At any moment about `r × 0.1 s` of the cells are glitched:
  the Matrix's `glitch-logo = 0.6` keeps about 7 of the classic title's 126 letter cells changing
  at once, its `glitch-background = 0.004` about 2 of a 160 × 50 vortex's 5,000 cells.
- **What.** ASCII letters, digits and `@#$%&*+=-:;/\|<>?!`, one column each, in the cell's own
  color: no other Unicode, nothing double-width, so every terminal and font shows it.
- **No constant redraws.** After a frame, the glitch asks for one more frame at its next change,
  a glitch starting or one ending, and nothing else: it schedules a single wake-up, kept until a
  sooner one is needed. Only cells drawn in the frame count, so a vortex or a diagnostic that is
  off screen (or under a panel) asks for nothing, and with no glitch to come the editor stays
  idle. Measured in a pseudo-terminal (release build), over a
  minute at rest with an error diagnostic on screen, its message 44 characters long: with
  `glitch-diagnostics = 0.01`, 0.7 frames a second (one to start each glitch and one to end it),
  50 bytes a second written to the terminal, 0.08% of a CPU; the same file in a theme without
  effects, 0 frames and 0%.
  The Matrix's fast logo glitch is the one that keeps frames coming: on its dashboard (with a
  `static` vortex) about 75 glitches a second over the classic title's 126 letters draw 24 frames
  a second, writing 4 KiB/s and taking 3.5% of a CPU. Each of those frames draws the whole
  dashboard, but a still vortex reuses the light of its last frame.
- **Error diagnostics.** Only the ghost text Vortex draws for diagnostics of severity error, at
  the end of the line and inline under it, over its characters (not the spaces between words);
  never warnings, info or hints, never their `┘├─` connectors, never the buffer's own text.
- The random source is seeded once per run; nothing is saved.

### The ASCII charset

`dashboard-charset = "ascii"` draws the vortex with `.:=#&@` and opens the title through `:=#`,
with the same geometry, the same math and the same frame cost as the blocks. The ramp was
measured rather than guessed: the ink of each character, the share of its cell it covers, averaged
over DejaVu Sans Mono, JetBrains Mono and Liberation Mono:

| | `.` | `:` | `=` | `#` | `&` | `@` |
| --- | --- | --- | --- | --- | --- | --- |
| ink | 2.6% | 5.0% | 10.1% | 19.8% | 22.2% | 27.0% |
| relative to `@` | 9% | 18% | 37% | 74% | 82% | 100% |
| light it needs | 0.10 | 0.22 | 0.41 | 0.76 | 0.89 | 1.15 |

`.` takes the place of `·` and `@` that of `█`, at the same thresholds of light, so the same cells
are lit. The others sit where the blocks' ink (a quarter for `░`, a half for `▒`, three quarters
for `▓`) reaches their ink relative to `@`, so the gradient reads the same, one step finer. The
other symbols the glitch uses are no denser than `@` or fall between these with nothing to add
(`-` and `:`, `*`, `+` and `=`, `#` and `%` measure within a few tenths of a percent).

### The phosphor filter

With `terminal-phosphor` set, every color in the terminal panel is drawn as that color at the
original's brightness: `ls`, `htop` and the prompt all turn phosphor and stay readable, since what
was bright stays bright and what was dark stays dark. Indexed colors are resolved to RGB first
(with xterm's default palette, as the outer terminal's own can't be read), RGB colors are taken as
they are, and the default colors are the panel's (`ui.terminal`, else `ui.background`; xterm's
light grey on black when the theme leaves them to the terminal). Brightness is the perceived one
(HSP, the square root of the weighted squares of the channels), which ranks a pure blue as dim
but visible rather than black. Up to the phosphor's own brightness the color is the phosphor
scaled down; brighter colors go on from it towards white, so white stays white.

The filter is off by default, and then costs one check per frame of the panel.

## The Matrix theme

`matrix`, built in like `vortex`: phosphor green on pure black. A bit of a joke, and genuinely
usable: with one hue, contrast and style carry the meaning.

```toml
theme = "matrix"
```

- **Greens by brightness**: bright (`#7dff9b`) for keywords, functions and what matters, normal
  (`#33dd66`) for text, dim (`#2fa84f`) for comments, line numbers and inactive things. The one
  other hue is **red for errors** (`#ff3b3b`): error diagnostics, their gutter and statusline marks,
  a failed shell or debug session.
- **Style does the rest**: bold for keywords, definitions and what matters; italics for comments,
  types, parameters and hints; tinted green backgrounds for strings, selections, search matches,
  the current line, diagnostics and diff lines.
- Warnings, info and hints differ by brightness and weight only (bold with a curly underline,
  normal with a dotted one, dim italic). Diffs: additions bright, deletions dim, with the `+`/`-`
  marks; conflicts bright on a tinted background.
- Every scope the vortex theme sets is set too (selections and cursors, the dashboard, the debug
  panel, the hex view, the commit log, watch mode's markers, bookmarks, line history,
  `:practice`...), so nothing falls back to an unreadable default.
- Its effects: the ASCII charset and the classic logo on the dashboard (runes and the Matrix
  don't mix), a fast glitch on the logo (0.6), a slow one on the vortex (0.004), 0.01 on error
  diagnostics, and the terminal panel through a phosphor filter of its text green.

Its contrast is tested (WCAG ratios, `vortex-view/src/contrast_tests.rs`): text on the background at least 7:1,
comments and inactive text at least 4.5:1, text on the tinted backgrounds (the current line, the
selections, strings) at least 4.5:1, the selection and the cursor distinguishable from the text
and the background. Measured: text 11.7:1 on the background, comments and
inactive text 6.8:1, text on the primary selection 5.1:1 (its lowest pair), errors 5.9:1, the
selection 1.7:1 against the background and the cursor 16.7:1. The same rows run on the vortex
theme too, printed only (`cargo test -p vortex-view contrast -- --nocapture`).

## Practice

The panel, `:sh` and `:!` don't change text, so they have no `:practice` exercise; they are in
the tutor's "THE TERMINAL PANEL" section. The keys that put shell output into the text are
upstream's: `|` is practised in the shell chapter, and `!` and `A-!` stay in the tutor, since
with the POSIX tools the exercises may use no short text makes them beat typing the output.

### Limitations

- No keyboard scrolling of the scrollback (every key goes to the shell): use the mouse wheel.
- No text selection or copy inside the panel with the mouse; use the outer terminal's own
  selection (usually with Shift held) or the shell's tools.
- One shell, one panel: no tabs, no splits of the panel, no resizing it with the mouse.
- The debugger's `runInTerminal` still uses the external terminal, not the panel.
- `vx` in a shell started inside the panel over `ssh` or in a container doesn't reach this editor.
- A command typed with `:sh` while a full screen program runs goes to that program.

## The debugger

Upstream's experimental debugger (DAP) with a real panel: variables, watch expressions, the call
stack, the breakpoints and a console below the splits, the line the program stopped at, and its
state in the statusline. Tested against `lldb-dap` (Rust, C) and `js-debug` (JavaScript,
TypeScript).

Vortex ships no debug adapter: like language servers, you install them, and `languages.toml`
says how to start them (see [Adapters](#adapters)).

The debugger is driven from [debug mode](#debug-mode), entered with `space G`: a mode with its own
keys and a three-row bar, whose spectator mode plays the program like a video.

### Debug mode

The debugger is driven from a mode, like [watch mode](#watch-mode): while it is on, its keys come
first, one key per command, and its state, the transport, the record of the run and the keys are on
the [mode bar](#the-mode-bar), in three rows. Playing, stepping, rewinding, breakpoints and
threads are all keys of the mode, so the debugger works like a video player: play, pause, one line
forward or back, jump to the next breakpoint, click on the timeline to seek.

**Entering and leaving.**

| Key or command | What it does |
| --- | --- |
| `space G`, `:debug-mode` | Enter debug mode (`:debug-mode` again leaves it) |
| `vx --debug [program] [-- args]` | Start in debug mode and launch the program (see [`vx --debug`](#vx---debug)) |
| `Esc` | Leave debug mode. The session keeps running, paused or playing, and the panel stays as it is |

One mode at a time: entering debug mode ends [watch mode](#watch-mode) (the changes it recorded
stay), and `:watch` or `vx --watch` leaves debug mode. Without a session debug mode starts in view
mode (there is nothing to follow): set breakpoints, then `s` launches. When a session starts, the
mode follows it.

The keys are the layer `[keys.debug]` (see [Watch mode's keys](#watch-modes-keys) for how a layer
is remapped); `?` shows them as configured. A command that has no key is still in the command
palette (`space ?`).

#### Keys

| Key | Live | In rewind |
| --- | --- | --- |
| **Playback** | | |
| `Space` | Play or pause. A program running at full speed is paused (DAP `pause`) | Plays on, back to live |
| `l` | The next line: one playback step | One recorded stop forward; past the last, live |
| `h` | The previous line: into rewind, one recorded stop back | One more stop back |
| `L` | The next breakpoint: [continue](#engine) at full speed to your next breakpoint | The next breakpoint mark of the record, else live |
| `H` | The previous breakpoint: into rewind, at the stop nearest the previous breakpoint mark | The same, from where you are |
| `k`, `j` | Play faster, slower | The same |
| `p` | Nothing (only in rewind) | Back to live: the present |
| `f` | Fast forward to the first change of a watched value (`f` again cancels) | Goes back to live first |
| `r` | Run (or play) to the cursor's line | Goes back to live first |
| `e` | What playback follows: own, lib, std | The same |
| **Stepping** | | |
| `.`, `i`, `o` | Step over, into, out | Go back to live first |
| **Threads** | | |
| `>`, `<` | Spectate the next, previous thread | The same |
| `t`, `F` | Pick a thread, a stack frame | The same |
| `z` | Freeze or thaw the spectated thread | The same |
| `a` | Only the spectated thread runs during steps, or all do | The same |
| **View and breakpoints** | | |
| `v` | View mode, or follow again (see below) | The same |
| `b` | Add or remove a breakpoint on the cursor's line | The same |
| `B` | The type of the breakpoint on the line: any thread, main thread only, sub-threads only | The same |
| `C`, `M` | The condition, the log message (a logpoint) of the breakpoint on the line | The same |
| `E` | Stop on exceptions (the adapter's filters), or don't | The same |
| **Session** | | |
| `s` | Start: pick a template of the language, answer its prompts (the picker of every template when the file's language has no debugger) | |
| `S` | End the session: terminate a launched program, detach from an attached one | The same |
| `R` | Restart | The same |
| **Inspect** | | |
| `x` | Evaluate an expression in the selected frame; the console shows it and its value (`:debug-eval expr`) | Refused |
| `w` | Add a watch expression | The same |
| `V` | The panel's variables, focused | The same |
| `m` | The [memory](#memory) of the value under the panel's cursor (`:debug-memory [expr\|0xaddr]` for an expression or an address) | The same |
| `c` | The capture settings | The same |
| `X` | Clear the time labels | The same |
| **Panel** | | |
| `Tab`, `Ctrl-g` | Focus the panel; in the panel, `Tab` and `Ctrl-g` go back to the editor | The same |
| `P` | Show or hide the panel | The same |
| **Mode** | | |
| `?` | The key help box: every key above, in these groups, built from the keymap | The same |
| `Esc` | Leave debug mode | The same |

Keys that change a value (`x`, setting a variable in the panel) are refused in rewind, which is
history, never re-execution.

Where the keys of the old `space G` menu went (the menu is gone: `space G` enters the mode):

| Old | New | Old | New |
| --- | --- | --- | --- |
| `space G l` | `s` | `space G space`, `h` | `Space` |
| `space G r` | `R` | `space G n`, `i`, `o` | `.`, `i`, `o` |
| `space G t` | `S` | `space G v` | `V` |
| `space G p` | `P` | `space G M` | `m` |
| `space G b` | `b` | `space G s t`, `s f` | `t`, `F` |
| `space G C-c`, `C-l` | `C`, `M` | `space G .` | `l` |
| `space G e`, `E` | `E` (a toggle) | `space G +`, `-` | `k`, `j` |
| `space G c` | `L` | `space G g` | `r` |
| `space G m` | `e` | `space G [`, `]` | `h`, `l` |
| `space G L` | `p` | `space G R` | `c` |

`w`, `x`, `X`, `f`, `z`, `a` and `< >` are where they were, after `space G`. Every command is
still in the command palette.

#### The bar

Three rows, above the message area:

```text
 DEBUG ❚❚ paused · breakpoint   H |◀  h ◀◀  Space ▶  l ▶▶  L ▶|     j −  2 lines/s · own  k +
 ·░▒▓█▓▒░·░░▒▒▓███▒░  ✕  ┄full speed┄  ░▒▓█▓▒░░  ●  ░░▒▓▒·  ‖ ┃
  v view   b breakpoint   e own   f change   r cursor   > thread   S stop   main · src/main.rs:30
```

**Row 1, the transport.** On the left the state, then the badges `|◀` (`H`), `◀◀` (`h`), `▶` or
`❚❚` while playing (`Space`), `▶▶` (`l`) and `▶|` (`L`), in the middle; in rewind `p present` too.
On the right the speed and what playback follows (`8 lines/s · own`), between the badges of its
keys: `j −` slower, `k +` faster.

| State | When |
| --- | --- |
| `DEBUG ○ no session` | nothing is running |
| `DEBUG ◌ starting` | the session is starting |
| `DEBUG ▶ playing` | playback is on |
| `DEBUG ❚❚ paused` | the program is stopped, with `· <reason>` when a breakpoint, an exception or a pause stopped it |
| `DEBUG ▶ running at full speed` | a continue on the engine |
| `DEBUG ▶▶ fast forward · 12` | fast forward, with its count |
| `DEBUG ▶ blocked in f 1.2 s` | a step that doesn't stop |
| `DEBUG ◀◀ rewind 128/2,048` | rewinding: the stop and how many are kept |
| `DEBUG ○ exited 3`, `DEBUG ○ ended` | the program ended |
| `DEBUG ◜ ━━━─── installing breakpoints · 12/48 files` | the [engine](#engine)'s loading animation |

**Row 3, the keys.** `s start` without a session; `v view` or `v follow`; `b breakpoint`; in view
mode `B type`; with a session `e own` (what playback follows), `f change` (fast forward to a
change), `r cursor` (run to the cursor), `> thread`, `S stop`; then `Tab panel`, `? keys`,
`Esc leave`: every label fits a 120-column window. Each is a badge: a click runs it. On the right of
the row is the note, what the last key did or why it waited (`following: v looks around to move`),
else the spectated thread and its place: `main · src/main.rs:30`. The note gives way first (cut, then
gone) so that it never hides a label; then a narrow window drops the labels, then the last badges.

#### The lineline

Row 2 is the [record](#the-record) of the run drawn as one row of cells, as wide as the bar. It
reads like a video's timeline: the left end is the oldest hit kept, the last column is now.

- **Scale.** The range from the oldest hit kept to now is mapped evenly onto the columns, apart from
  the full-speed gaps. The last column is **now**: the live playhead `┃` (ASCII `|`); in rewind
  `▕` (`]`), the end of the record; `■` once the program ended. Marks recorded after the last hit
  go in the last column.
- **Density.** A span's cell shows how often the spectated thread ran in it, normalized to the densest
  cell, with the [dashboard](#dashboard)'s shades: ` ·░▒▓█` (ASCII ` .:=#&@`, as the theme's
  `charset` says).
- **The playhead.** In rewind, `┃` is drawn on the cell of the stop you are at, over everything.
- **Marks.** When several share a cell, the first of this list shows:

  | Mark | Glyph | ASCII |
  | --- | --- | --- |
  | an exception | `✕` | `!` |
  | a breakpoint hit (also one that didn't pause: a type that didn't match) | `●` | `*` |
  | a switch to another thread (the first thread followed is no switch) | `⇄` | `/` |
  | the program's end | `■` | `$` |
  | a pause | `‖` | `"` |
  | every covered file has its hidden breakpoints from here | `✓` | `+` |
  | the selected variable written, read here (reserved: not recorded yet) | `▾`, `▿` | `w`, `r` |

  Below the marks come the dead stretch and the density.
- **Dead stretches.** Cells before the spectated thread started and after it exited are `─` (ASCII
  `_`) in the error color: the thread wasn't there.
- **Full-speed gaps.** A run at full speed isn't counted line by line, so it takes columns of its own:
  `┄full speed┄` (12 columns) when every gap fits in a third of the width, else `┄┄┄`, else one `┄`
  (ASCII `~`).
- **Click** a cell to seek: the last column goes to the present; a span, to the recorded stop of
  the spectated thread nearest the middle of the cell; a gap, to the stop nearest its counter.

#### Following and view mode

With a session the mode **follows**: each stop moves the view to the spectated thread's line, and
rewind follows the recorded stop the same way. The line is kept in the middle third of the view: no
scroll while it stays there, centered when it leaves it; a stop in another file opens it centered.

While following, the movement and search keys of the editor do nothing (the bar's note says `v looks
around to move`), and neither do the keys that edit (`Esc leaves debug mode, then edit`).

`v` is **view mode**: your own cursor, drawn as in select mode (`ui.cursor.select`), moves and
searches freely, and the stops don't move the view (playback goes on). `v` again follows, jumping to
the current stop. A click or a scroll with the mouse in the text enters view mode too. In view mode
the keys of the mode stay its own (`h j k l w b e f x`...): move with the arrows, `Ctrl-d` and
`Ctrl-u`, the page keys, the `g` menu, `/` and `n`/`N`.

`b`, `B`, `C`, `M` and `r` act on the cursor's line: while following, the line of the stop; in view
mode, where your cursor is.

Outside debug mode, with a session running, playback steps don't move the view (you are editing); a
pause (a breakpoint, an exception) still jumps there.

#### Breakpoints

Breakpoints are set in debug mode only: `b`, `C`, `M`, `B`, and a click in the gutter. Outside it
the commands say `breakpoints are set in debug mode`. They stay in memory across sessions and are
sent to the adapter whether or not debug mode is on.

A breakpoint has a type (`B` cycles it): **any thread** (the default), **main thread only**, or
**sub-threads only**. The main thread is the first one the adapter reported. Verified and
unverified glyphs:

| Type | Verified | Not verified |
| --- | --- | --- |
| any thread | `●` | `◯` |
| main thread only | `▲` | `△` |
| sub-threads only | `▼` | `▽` |

A turned-off breakpoint is `○` whatever its type. The panel's list shows the type (`main thread
only`, `sub-threads only`) after the line.

A hit on a thread the type doesn't match isn't a stop: it is recorded as a breakpoint hit (a `●`
on the lineline), continued at once, and the camera stays where it is.

**The gutter.** In debug mode the gutter's breakpoint column shows breakpoints only; outside it,
the bookmarks only (and breakpoints aren't drawn). The two never share the column.

#### `vx --debug`

`vx --debug [program] [-- args]` starts in debug mode and launches the program, without the
dashboard. A source file (a file of a language that has a debugger) is launched with that
language's templates; anything else is an executable, launched with the templates of every language.
A template fits when:

- it launches (`request = "launch"`) and one of its prompts is a filename prompt that fills
  `program`;
- its `takes` agrees with the program: `takes = "executable"` or `takes = "source"` in the debugger
  template in `languages.toml`; unset means either;
- its prompts are exactly the program, plus one that fills a list argument whole (`args = ["{1}"]`)
  when arguments were given;
- its adapter's command is installed (on `PATH`).

Of the templates of one adapter that fit, the first in the config's order is kept (`binary`, not
`binary (terminal)`). One template left: the program launches at once, with its arguments filled in
(quoted where they have spaces). Several: a picker of `adapter: template`, with the language. None:
an error, such as `no installed debugger of rust launches a source file`. `vx --debug` alone opens
the template picker.

#### The status line

While debug mode is on, the debugger's notes (what a key did, an expression's result, a step's
outcome) go to the bar's note, on the right of the third row, not the status line. Errors still
go to the status line, and outside debug mode the debugger's messages do too.

### The panel

Five sections: **Variables** (the scopes of the selected frame, a tree), **Watch** (your
expressions), **Call stack** (the threads; the current one open on its frames), **Breakpoints**
(all of them, by file) and **Console** (the program's output, the adapter's messages, and what you
evaluated). From 120 columns the console shows beside the data section; narrower, it is the fifth
tab. The tab bar shows the session's status and name, and the search of the section.

The panel opens when a session starts (the editor keeps the keys) and hides when it ends; `P` in
debug mode shows it again with the last console. `Esc` in normal mode hides it, like the terminal
panel, with which it shares its place: showing one hides the other. `Tab` or `Ctrl-g` in debug
mode, or `Ctrl-g` outside it, focuses it.

Its keys, when focused, are modal like the fork's pickers. The keys it doesn't take go to debug
mode while debug mode is on, so `Space`, `H`, `L`, `.` and the rest of the mode still work
with the panel focused:

| Key | What it does |
| --- | --- |
| `j` `k`, arrows, `gg` `G`, `Ctrl-d` `Ctrl-u` | Move |
| `Tab` `Shift-Tab`, `w` `b`, `1`-`5` | The next, previous, or a given section; in debug mode `Tab` goes back to the editor instead (`Shift-Tab`, `w`, `b` and `1`-`5` still switch) |
| `Enter` | Open or close a node; select a frame (or a thread: its top frame), which jumps to its code and shows its variables; go to a breakpoint; in the console, evaluate |
| `l` `h` | Open, close (on a closed node: to its parent) |
| `s` | Search the section as you type (`Esc` in the panel clears it) |
| `c` | Change: a variable's value (DAP `setVariable`, or `setExpression`), a watch expression, a breakpoint's condition |
| `m` | A breakpoint's log message |
| `a` | Add a watch expression; on a variable, watch it |
| `d` | Delete the watch or the breakpoint; in the console, clear it |
| `t` | Turn a breakpoint off or on (off: kept in the list and the gutter as `○`, not sent to the adapter) |
| `y` | Yank the value (the line, in the console) |
| `i` | Evaluate an expression (history in the `=` register) |
| `z`, `o`, `p` | Freeze or thaw the thread under the cursor (the spectated one outside the call stack); only the spectated thread runs; continue or pause the thread under the cursor |
| `u` | The step's unit (line, statement, instruction), when the adapter offers it |
| `*` | Mark a variable: fast forward stops when it changes |
| `{` `}` | Rewind ten stops back or forward |
| `M` | The [memory](#memory) of the value under the cursor (`▦` after it), in the hex view |
| `:`, `Ctrl-t` | The command line; the terminal panel |
| `q`, `Esc`, `Ctrl-g` | Back to the editor |

The panel has no row of hints: the keys are in debug mode's `?` box and on the bar. The loading
animation (a spinner, `installing breakpoints · 12/48 files`) shows in the panel's top row outside
debug mode, and in the bar's state in debug mode.

The mouse: a click focuses the panel and puts the cursor on the row (a second click acts like
`Enter`, a click on a tab switches to it), the wheel moves, a click elsewhere gives the keys back.

### At a stop

At every stop the panel loads, in the background: the threads, the stack of the thread that
stopped (and the editor jumps to its top frame), the scopes of that frame with the first cheap
scope open, every node you had opened (by its path of names: the adapter's references change at
each stop), and the watches. A value that differs from the previous stop in the same function is
marked. A node loads its children when you open it. A load that a newer stop or resume overtook is
dropped, so stepping fast never shows old values.

While debugging the editor marks the line the program stopped at (`ui.highlight.frameline`, and `▶`
in the gutter) in every split of its file, and the breakpoints in the gutter: `●` verified by the
adapter, `◯` not yet (or not at all: the console or the breakpoint's row says why), `○` turned off.

The statusline element `debug` (in the default layout, before `shell`) shows `DBG running`, `DBG
stopped: breakpoint`, then `DBG exited 3` (or `DBG ended`, when the adapter gave no exit code) until
the next session.

### Spectator mode

Watch the program like a video, or a game's spectator mode: it plays line by line at the speed you
choose, you follow one thread and cut to others as they come, pause anywhere to look or change a
value, and scrub back through what you already watched.

Spectator mode is driven from [debug mode](#debug-mode): `Space` plays and pauses, `l` and `h` are
the next and previous line, `L` and `H` the next and previous breakpoint, `k` and `j` the speed,
and the bar's [lineline](#the-lineline) is its timeline. Its old keys in the panel moved there:
`Space`, `+ -`, `n`, `< >`, `e`, `r`, `f`, `L`, `[ ]`, `R` and `x` became `Space`, `k j`, `l`,
`< >`, `e`, `r`, `f`, `p`, `h l`, `c` and `X` of debug mode (the panel keeps `{ }`, `z o p u * M`
and the data keys), and they work with the panel focused, since debug mode takes the keys the
panel doesn't.

**Playback** runs on the [engine](#engine): a hidden breakpoint on every line of your code, so
the program continues and stops at its next line, wherever it is; Vortex waits out the speed's
interval there, and continues it again. With `engine = "stepping"` it steps instead: step, wait for
the stop, draw it, wait out the interval, repeat. Speeds go from 0.5 to 25 lines per second (0.5,
1, 2, 3, 5, 8, 12, 18, 25; 2 by default). Your breakpoints, exceptions, a pause and the program's
end pause it; so does a stop at one of your breakpoints on another thread, and the camera cuts to
that thread. Values load at every stop like at any stop
(only the open nodes), changes marked; while playing the view scrolls only when the line leaves
it, instead of centering at every step. With `supportsSteppingGranularity` the step's unit can be
a statement or an instruction (`u`); line by default.

**What playback follows**, cycled with `e` (shown on the bar, `8 lines/s · own`): `own` (the default)
stays in your code, `lib` also goes into libraries, `std` into everything, the language's runtime
included. On the engine, that is which files get hidden breakpoints (`lib` and `std` are a work in
progress: see [Engine](#engine)). With `engine = "stepping"`, a playback step is a `stepIn`; when
the frame it lands in isn't one the mode follows, `stepOut`s until it is, all counted as one
step. Frames are classified by their source path:
**std** when they have no source, or their path (or source name) contains one of the runtime's
pieces: `/rustc/` and `/lib/rustlib/src/` (Rust's sysroot as rustc records it, and as rustup
installs it), `/usr/include/`, `/usr/lib/`, `/usr/src/` (C headers and system sources), `node:` and
`<node_internals>` (Node's internals); **lib** when it contains a library's piece
(`/.cargo/registry/`, `/.cargo/git/`, `/node_modules/`, `/vendor/`) or is outside your folders;
**own** inside the workspace, or the launch's `cwd` or program's folder when they are elsewhere
(or inside the folders `own-paths` names, when it is set). The lists can be set per language, in
its debugger:

```toml
[language.debugger]
std-paths = ["/rustc/", "/lib/rustlib/src/"]
lib-paths = ["/.cargo/registry/", "/third_party/"]
own-paths = ["/home/me/work/app/server"]   # instead of the workspace and the launch's folders
```

A step that doesn't stop (blocking I/O, a lock, a sleep) shows `blocked in <function>` and a
running timer after half a second, in the panel, the bar's state and the statusline; `Space` pauses it there (DAP
`pause`), so you see where it waits.

**Fast forward** (`f`) plays as fast as the adapter goes, without moving the view, and compares at
each of the spectated thread's stops the watch expressions and the variables you marked (`*`, `◆`
in the tree) with their values when it started; it stops at the first change and says `old -> new`
(status line and console). Your breakpoints and the program's end stop it too; `f` or `Space`
cancels it.

**Run to cursor** (`r`): paused, the program runs until the spectated thread reaches the cursor's
line: on the engine, its hits are checked against that line by their position alone (no frames are
loaded on the way); where the line has no hidden breakpoint (or with `engine = "stepping"`),
through a temporary breakpoint, removed at the next stop, whatever it is. Playing, playback goes on
until that line.

**Threads.** Playback follows one thread, `◉ spectating` in the call stack (where every thread of
every session is, under its session's name when there are several: js-debug runs each worker in a
session of its own) and `T2 name` in the statusline. `<` and `>` cut to the previous and next
thread; `Enter` on a thread spectates it. A thread starting or exiting shows in the call stack
(and the log), not in the status line. To force a suspected race by hand, schedule the threads
yourself: `z` freezes a thread (`❄ frozen`: it never runs during steps, continues or playback
until thawed, and a running thread is paused as it freezes), `a` makes steps and continues run only
the spectated thread (`o` in the panel), and `p` in the panel continues or pauses the thread under the cursor. For example: step
A to the check, freeze A, run B through, thaw A and continue it. This is DAP `singleThread`: the
adapter must say it supports it (`supportsSingleThreadExecutionRequests`); without it `z` and `a`
say that the adapter runs every thread on each step, and aren't offered (except between sessions,
which never run each other's threads). Adapters that stop every thread at a stop (lldb's
all-stop) can run one thread or all of them: with a thread frozen, steps of the others run them
alone. Adapters whose threads stop one by one (`allThreadsStopped: false`) also get a continue for
each thread that isn't frozen.

An adapter that honours `singleThread` without saying so can be told in its debugger config,
which assumes capabilities over the adapter's `initialize` answer:

```toml
[language.debugger]
capabilities = { supportsSingleThreadExecutionRequests = true }
```

lldb-dap (18, 20 and 21) doesn't advertise it: lldb-dap 18 ignores `singleThread`; lldb-dap 20
honours it for `next` and `stepIn` but not for `stepOut`, which runs every thread (so with it,
playback's step outs of library code let frozen threads run: force interleavings with
`.`); lldb-dap 21 answers a `continue` with `singleThread` (`allThreadsContinued: false`)
and then runs nothing at all, so with the capability forced on, playback on the engine (which
continues) stands still while a thread is frozen. An adapter that stops every thread may also report each of them in a `stopped` event
of its own (lldb-dap does, with the reason of their last stop); those marked `preserveFocusHint`
only update their thread's state, so they don't move the camera or pause playback.

**Time per line.** After each playback step, the line the program left gets a greyed label at its
end, like inline blame (`ui.virtual.step-time`): how long the step took, minus the cost of stepping
itself, e.g. `12 ms`, or `12 ms (3 cpu)` when the CPU time can be read, and `×4` when several steps
counted. On the engine a step is the time from the `continue` to the spectated thread's next hit,
minus the overhead of a hit (calibrated the same way, on plain continues: the next hit in the same
function at the same depth, no other thread's hit between them) and, with an adapter that stops
every thread at a hit, the overhead of each other thread's hit on the way. Each time is also kept
on the [lineline](#the-record) with the thread and the frame (function and depth) it ran in. Labels stay on the lines visited this session, adding up, so the code turns into a rough
flame graph; `X` clears them. The method:

- each request of a step (the step in, then any step out of library code) is timed from when it is
  sent to when the adapter's `stopped` event arrives, stamped as it comes off the connection, not
  when the editor gets to it (the editor may be drawing or loading values): that includes the round
  trip to the adapter and the debugger's own work;
- that overhead is measured continuously, per kind of request: for a step in, the median of the
  faster half of the last 32 plain steps (one request, the same function before and after); for a
  step out, the same over the last 32 step outs (most leave a cheap library call; lldb's step out
  costs several times its step in). A step's time is the time of its requests minus their
  overheads;
- a step under 1 ms after that (`[editor.debugger] time-threshold`), or under its own overhead (the
  overhead varies about as much as it is), counts as nothing, so cheap lines stay clean and the
  correction's noise doesn't pile up on them; nothing is labelled until 5 plain steps calibrated it;
- when the adapter says the debuggee is a local process (the DAP `process` event, its
  `systemProcessId`), on Linux the CPU time is the thread's (`/proc/<pid>/task/<tid>/stat`, when
  the adapter's thread ids are the OS's, as lldb's are), else the process's (`/proc/<pid>/stat`);
  on macOS the process's (`proc_pidinfo`); elsewhere wall time only. Busy is the CPU part, the rest
  waited.

It is a debugger's stopwatch, not a profiler: a step runs the line under the debugger (single
stepping and breakpoints cost far more than the code), so only times well above the overhead mean
something, and the CPU time of the process includes the threads that run during the step. Measured
with lldb-dap 20 on a debug build: plain steps cost 2-5 ms and step outs 2-25 ms; a line that sleeps
300 ms read `314 ms (0 cpu)`, a line summing 20 million numbers through an iterator `472 ms (260
cpu)`; a line calling a trivial library function (`total += n` through `AddAssign`) mostly stays
clean, with an occasional 5-10 ms of the debugger's jitter.

**Rewind.** Every stop is recorded whole, because a value missed at a stop can never be fetched
later: the thread, where it is, its call stack, and the variables of every frame of the thread (not
only what was open), and the watches. Caps keep it affordable: 3 levels of variables below each
scope, the first 100 children of a variable (and how many more there were), 2,000 stops (the oldest
go). Where a level costs much and says little, less is recorded: frames playback doesn't follow
(the runtime's, in `own` mode: a Rust stack has a dozen of them under `main`) keep their own
variables, not what is inside them; registers keep their groups; an expensive scope (js-debug's
global object) and a scope without a hint that every frame repeats (lldb's globals) are recorded in
the top frame only. Frames that can't have changed since the stop before aren't fetched again:
playing line by line, the frames under the top one are mostly the same calls, waiting at the same
lines, and those carry the variables recorded then. A frame is the same when it is on the same
thread, at the same depth counted from the bottom of the stack (the top changes, the bottom
doesn't), in the same function and source, at the same line (adapters give frames new ids at every
stop, so ids can't tell). The top frame is always fetched, and so is a frame whose line moved,
which is how a return into a carried frame fetches it again. Nothing is carried at the first stop
of a session, the first after a thread switch, after the capture settings or the follow mode
changed, after the thread ran lines that weren't recorded (a full-speed run, a run to the cursor),
or while hidden breakpoints are still going in. In rewind a carried frame says so above its
variables, `carried from step 128` (`ui.debug.carried`): the step that fetched them. A callee can
change a caller's locals through a pointer or a reference while the caller isn't on top, and a
frame that runs again at the same line between two stops (a loop on one line; library code that
isn't recorded calling back into yours) keeps the values it had: those changes show when the frame
is fetched again (its line moves, or it is the top). The requests of a stop go out together, a
level of the tree at a time, at most 16 at once. With lldb-dap and rustc's formatters a stop of the
example program costs about 15 requests (74 when every frame is fetched) and 50 KB. Recording runs
in the background, so the screen never waits on it; playback waits for it before its next step (a
stop's references are only valid until the program moves), which may lower the top speed. `c` opens the capture settings in the panel: depth, collection size and history length
(`unlimited` allowed, with a warning), with the cost measured over the last stops (requests per stop,
memory per stop and in all) or estimated before any. `h` and `l` (`{` `}` by ten, in the panel) scrub back and
forward: the editor and the panel show that stop, read-only, under an unmissable marker,
`REWIND · step 132/480 · nothing is executing` in the panel and `REWIND 132/480` in the statusline
(`ui.debug.rewind`), values greyed; `Enter` on a frame shows that frame's variables. Changing values
and evaluating are refused in rewind. `p`, `l` past the last stop, playing or stepping go back to
live.

The defaults are settings (read at startup and when the config is reloaded):

```toml
[editor.debugger]
speed = 2                  # lines per second (the nearest of the speeds)
follow = "own"             # std, lib or own
capture-depth = 3          # or "unlimited"
capture-collection = 100   # or "unlimited"
history = 2000             # stops; 0 records nothing; or "unlimited"
capture-memory = 4096      # bytes per memory view; 0 records none; or "unlimited"
time-labels = true
time-threshold = 1         # milliseconds
engine = "breakpoints"     # or "stepping"
line-record = 1000000      # hits of every thread kept on the lineline; or "unlimited"
```

### Engine

Playback's engine: **hidden breakpoints on every executable line** of the code the follow mode
covers. The program runs (`continue`), stops at each line, and Vortex decides at each hit whether
to pause there or to continue, and when. It replaces stepping (`stepIn`, then `stepOut`s) as the
way playback advances; `engine = "stepping"` keeps the old way, to compare both on the same program
or for an adapter that can't do breakpoints on lines (a session whose adapter verifies no line
breakpoint at all falls back to it by itself, and says so).

**Which files.** In `own` mode: every source file of the session's languages (the languages whose
debugger is the session's adapter; for an adapter no language names, the languages of the open
documents) in the folders of your code: the workspace, and the launch's `cwd` and program folder
when they are elsewhere, or `own-paths` when set (see [Spectator mode](#spectator-mode)). That is
git-tracked and untracked files that aren't ignored, at most 5,000 files. `lib` and `std` are a
**work in progress**: they add the sources the adapter lists (`loadedSources`, where it offers it:
js-debug does, lldb-dap doesn't) and, as the spectated thread's frames show them, the files of the
frames the mode follows; files that can't be enumerated upfront (all of lldb's libraries and
runtime until a stack reaches them; code without debug info, which can't have breakpoints) are
covered by stepping for the spectated thread, as before the engine. Switching to `lib` or `std`
warns first, in the status line and the panel's console: absurdly slow, and programs with timeouts
may fail.

**Which lines.** DAP `breakpointLocations` where the adapter offers it and answers with lines.
Otherwise (or when it answers nothing for a file) a breakpoint on every line, keeping only the ones
the adapter verifies on the line asked for: adapters move a breakpoint on a line without code to the
next line with code, and those duplicates are dropped by sending the file again with the kept lines.
A breakpoint the adapter can't resolve yet (verified false without a line: js-debug before the
script loads) is kept, and its `breakpoint` event decides later: moved, it becomes a stray (its hits
are ignored, and the file is sent again without it). What the adapters do:

| Adapter | `breakpointLocations` | Without it |
| --- | --- | --- |
| lldb-dap 21 (Rust) | offered, but answers no location for Rust sources (it looks the file up by compile unit, and rustc's units aren't named after files) | verify and drop: answers at once, moved lines verified on the next line with code, lines outside any function unverified |
| js-debug | offered; it answers only once the script is loaded, so before the launch it times out (after 1 s for all the files at once) | every breakpoint unverified, no line; `breakpoint` events verify them as the script loads, moved ones to their real line |

lldb-dap 18 and 20 weren't available to test. lldb-dap 21 ends the session on a request bigger than
64 KiB, so no `setBreakpoints` carries more than 4,000 breakpoints: a bigger file is probed in
chunks, and a file with more executable lines than that gets hidden breakpoints on its first 4,000
(the console says so). The lines found are remembered for the editor's session (in memory, by file
and modification time), so a restart sends them without finding them again.

**Merged with yours.** `setBreakpoints` replaces every breakpoint of a source, so each request for
a covered file carries your breakpoints first and the hidden ones after, and the answer is split the
same way: your breakpoints keep their ids, verification, conditions, hit counts and log messages
exactly as before. A line with one of your breakpoints gets no hidden one (lldb-dap merges two
breakpoints on a line into one). The engine knows its breakpoints by the ids the adapter gave them;
they never show in the gutter or the panel's breakpoints, and don't follow edits made while
debugging (like yours).

**Installing.** At the session's configuration, before `configurationDone`: the files are found on
a background thread, then sent four requests at a time, the files likely to run first ahead of the
rest: the open buffers, the launch's program when it is a source file, and the files with your
breakpoints. Once those are in, your breakpoints in other files and the exception filters follow,
then `configurationDone`, and the program starts while the rest go in behind it, with the loading
animation saying so (`installing breakpoints · 120/486 files: lines in the rest aren't counted
yet`): until a file is in, its lines aren't hit, so playback passes through them unseen and the
lineline doesn't count them (the spectated thread, when it stops in such a file, steps through it
as without the engine). When the last file is in, the lineline gets a `covered` mark: before it,
coverage was partial. When none of the files is one of those (nothing open, no breakpoint), the
program waits for all of them, as before. The program stops at the first line of your code that
runs, on whichever thread: that thread is spectated, playback paused (`Stopped at the first line of
your code that ran`). Switching the follow mode (`e`) sends the whole set again for the
new mode, and the restore after a full-speed run puts it back, both in the same order (the
spectated thread's file first) and both letting playback go on once those are in. `L` while
a set goes in (right after a stop, while the hidden breakpoints are put back) runs at full speed once
it is complete (the note says so). While breakpoints are installed, lifted or restored, the loading animation
shows in the bar's state (the panel's top row outside debug mode) (a spinner, a bar that fills as files are done with a glow running along
it, `installing breakpoints · 12/48 files · 3,410 lines`) and the statusline `DBG … · ◜ 12/48`; the
editor stays usable.

**At each hit.** Where a hit is comes from its breakpoint id, not from a `stackTrace`, so a hit of a
thread nobody watches costs the `stopped` event, a lookup and a `continue`: the DAP client itself
continues those as they come off the connection (`vortex-dap/src/hits.rs`), without waiting for the
editor, which only records them. Then:

- the spectated thread's hit is a stop, shown and recorded whole as rewind does; playing, it waits
  out the speed's interval and continues; paused, it stays;
- another thread's hit is recorded and continued at once, unpaced. With an adapter that stops every
  thread at a hit (lldb's all-stop) that continue runs them all, so while a thread is frozen (`z`)
  every continue goes alone, with `singleThread`, the other threads' hits too: the frozen thread
  never moves, and in an all-stop session only the thread you continue runs (`o` as before);
- a hit of one of your breakpoints goes through one decision (`breakpoint_pauses` in
  `vortex-view/src/debug/engine.rs`: the breakpoint, the thread, the main thread), which pauses for
  now; a stop at yours on another thread cuts the camera to it, as before. Exceptions, a pause and
  the program's end pause playback as before;
- `.`, `i`, `o` continue the thread and end at its next hit where the step would have: the
  next line anywhere for `i`, the next line not deeper for `.`, the next line above for `o`.

**Full-speed runs.** `L` lifts every hidden breakpoint (each covered file sent again with
yours alone), continues at full speed, and puts them back at the next stop, whatever it is,
behind the loading animation. The lines run at full speed aren't counted: the lineline gets one
`full speed` mark with how long it took.

#### The record

The **lineline** (`vortex-view/src/debug/lineline.rs`) is one counter of lines executed, shared by
every thread of every session: each hit (of any thread, and every other stop) advances it by one,
and everything is recorded against it:

- every hit, compactly: its counter is its place, its file and thread are indexes into small tables
  (`u16`), its line a `u32`, and why it stopped is a line unless a sparse list of reasons says
  otherwise. **8 bytes a hit: 800 KB per 100,000 hits** (measured in a unit test); capped at
  `line-record` hits (1,000,000 by default, about 8 MB), the oldest go first, with what was recorded
  against them;
- marks: your breakpoints, exceptions, pauses, switches of the spectated thread, full-speed gaps,
  threads starting and exiting, the program's end, the point where every covered file had its
  hidden breakpoints after the program ran on a partial set (`covered`) (room is left for the selected variable's reads
  and writes);
- each thread's lifetime: the counter when it started, its first and last hit, when it exited; and
  the main thread, the first one the adapter reported;
- the time of each line of the spectated thread, with its thread and frame (function and depth);
- rewind's snapshots carry the counter of their stop.

`where_at(thread, counter)` says where a thread was at any point.

#### Measured

Measured in a slow shared container (numbers are lower bounds), in a release build (a debug build
spends about 0.7 s drawing each stop, which hides everything else), with lldb-dap 21 on a small
Rust program and js-debug on a small TypeScript one, in `own`, rewind recording on. *Before* fetches
every frame at every stop; *after* carries the frames that can't have changed (see rewind in
[Spectator mode](#spectator-mode)). Requests and time are per stop, medians, from the `stopped` event to the last
answer:

| | Engine, before | Engine, after | Stepping, before | Stepping, after |
| --- | --- | --- | --- | --- |
| Rust, asked 2 / 8 / 25 lines/s | 1.9 / 1.9 / 1.9 | 2.0 / 7.6 / 12.8 | 1.8 / 2.1 / 2.2 | 2.0 / 7.8 / 11.1 |
| Rust, a stop | 74 requests, 460-560 ms | 14-16 requests, 72-81 ms | 67-71 requests, 360-380 ms | 14-16 requests, 71-75 ms |
| TypeScript, asked 2 / 8 / 25 lines/s | 0.53 / 0.53 / 0.53 | 0.66 / 0.66 / 0.66 | 0.53 / 0.53 / 0.53 | 0.73 / 0.66 / 0.66 |
| TypeScript, a stop | 184 requests, 1.7 s | 119 requests, 1.35-1.45 s | 183 requests, 1.4-1.5 s | 116 requests, 1.3-1.4 s |

- **Recording each stop sets the top speed.** lldb-dap answers one request at a time (about 6 ms
  each, however many are in flight), so what counts is how many a stop sends: carrying the
  unchanged frames takes a Rust stop from 74 to 14, and playback from under 2 lines/s to about 13
  (the engine) or 11 (stepping). With js-debug most of a stop is the top frame itself: its scopes
  (Local, the module, Global) and the objects in them, about 95 `variables` requests, which nothing
  can carry since that frame is the one that moved.
- **A line of another thread** (lldb-dap): 15-17 ms, about 460 hits/s, one thread or several,
  against about 25 ms a step for stepping; js-debug: 55-100 ms, about 22 hits/s.
- **Installing**: 41 hidden breakpoints in one Rust file in 44 ms; 32 in one TypeScript file (in
  both js-debug sessions). Debugging Vortex itself (`vx --health rust` with `vortex-term/src/main.rs`
  open; 487 files, about 180,000 lines): `configurationDone` went out 4.5 s after the launch, once
  that file was in (it was 13 minutes when every file went in first), and the program stopped at
  its first line 50 s after the launch, lldb-dap still indexing the debug info. The rest went in
  behind it in 20 minutes: 51,219 hidden breakpoints in the 283 files that have code in the
  program, lldb-dap growing to 2.1 GB. For those 20 minutes the lines of the files not yet in run
  unseen: playback passes through them and the lineline doesn't count them. Set in a running
  program, some files took lldb-dap over 20 s, so the install waits up to 5 minutes for each
  answer. On a program that big, set `own-paths` to the part you care about.
- **Memory**: 8 bytes a hit, 800 KB per 100,000 hits.

### Memory

A debugged program's memory shows in the [hex view](#hex-view), with its colors, hex and bin modes,
ASCII column, search, `v`/`y`, `G` to an address and statusline. Whether memory is offered depends
only on the adapter's capabilities and on each value, never on the language:

- An adapter that reads memory (`supportsReadMemoryRequest`) gives the values that live in memory a
  `memoryReference`: those get a `▦` after them in Variables and Watch (`ui.debug.memory`), and `M`
  in the panel (`m` in debug mode) opens their memory, in a split, the cursor on their address in the
  middle. `M` on another value says `<name> has no memory to show`. Without the capability there
  is no `▦` anywhere, and `M` or `:debug-memory` say `this debugger (<name>) doesn't support viewing
  memory`. Messages only: no popup, nothing greyed out.
- `:debug-memory 0x7ffee3b4a010` opens at an address (read through the reference of a memory view
  of the session already open, else through the address itself, which is what lldb-dap's references
  are); `:debug-memory <expression>` evaluates it in the selected frame and opens the memory its
  value points to, or says `<expression> doesn't point to memory`. Without an argument it is `M`.
- The view's offsets are the addresses, `0x00007ffee3b4a010` with the grey `0x`; its name is
  `memory: <value>`, and the position element shows the address, the byte and where it is from the
  value (`buf+0x10`). The address space is huge: the view scrolls freely and reads only what it
  draws, a 1 KiB page at a time (`readMemory`, through the value's reference with an offset), and
  `gg`/`ge` say that memory has no first or last byte. Unreadable bytes (unmapped or freed memory,
  a read that failed) show as `??` (`ui.hex.unreadable`), never as an error.
- At every stop the pages on screen are read again, and the bytes the program changed since the
  stop before stay highlighted (`ui.hex.changed`) until the next one. An adapter's `memory` event
  reads them again too. While the program runs, the view keeps its last stop's bytes and the
  statusline says `running · the last stop's bytes`; after the session, `session ended · its last
  bytes`. A stop never replaces a memory view: the source moves in another split.
- During playback and fast forward the views are read at most at the screen's rate (about 30 times a
  second), not at every step; the highlight is then against the refresh before.
- **Overwrite**: `i` as in a file, only where the adapter writes memory (`supportsWriteMemoryRequest`;
  else `this debugger (<name>) can't write memory`), and only while the program is paused: refused,
  with a message, while it runs, plays, fast forwards, or in REWIND, and on `??`. `:w` sends the
  overwritten bytes with `writeMemory`, a request per run of bytes, and says
  `wrote 4 bytes to memory: the debugger wrote 4`, then reloads the panel's values. `u` after writing
  writes the old bytes back at once (`U` writes them again), so memory always holds what the view
  shows. Memory is never a file: `:w <path>` is refused.
- **Rewind**: at each recorded stop, every open memory view of the session records the bytes it
  shows and 512 bytes on each side, at most `capture-memory` bytes (4 KiB by default) around the
  middle of what it shows; the capture settings (`c`) have a `memory window` row and count the views
  in the cost estimate. In REWIND a memory view shows those bytes, the ones that changed since the
  recorded stop before highlighted, and anything outside them as `··` (`ui.hex.unrecorded`): never
  live memory. A memory view opened after a stop wasn't recorded at it.

The memory of a value stays the same document: `M` on it again focuses its view. A search (`/`)
looks through the bytes read so far (the pages around where the view has been), since the address
space can't be read through.

### Sessions

Apart from starting the adapter (and its `initialize` handshake), nothing waits on it: launches,
configuration, loads and edits run in the background and come back through the job queue. The
configuration (breakpoints, exception filters, `configurationDone`) is sent once per session, when
the adapter says it is ready, even if it says so twice. Every session gets every file's
breakpoints, and keeps its own ids and verification.

An adapter can ask for a child session (`startDebugging`), as js-debug does for the program it
runs: the child connects to the same socket (or starts the adapter again, for one on stdio), its
launch goes in the background, and the panel follows the session that last stopped. A child's end
gives the panel back to its parent; the root's end ends it all. `S` ends a launched
program (`terminate`, else `disconnect` with it) and detaches from an attached one, which keeps
running. When a session ends, the adapter process Vortex started is stopped if it doesn't exit by
itself (js-debug's server doesn't).

Templates fill `{0}`, `{1}`... with the answers to their prompts and `${workspaceFolder}` with the
workspace's root; in a list, an item that is exactly `{N}` becomes the answer's words (quotes group
words), so `args = ["{1}"]` takes a command line of arguments. A template's `cwd` wins over the
editor's working directory. As in a launch configuration of VS Code, the arguments also carry the
template's `request` and `name` (js-debug attaches only when `request` says so).

### Adapters

Install them from their official sources; Vortex finds them on `PATH` (`vx --health rust` says
whether it does).

**lldb-dap** (Rust, C, C++, Zig...): part of LLVM 18 and later (`lldb-dap`, called `lldb-vscode`
before 18). Linux: your distribution's `lldb` package (Debian and Ubuntu name it `lldb-dap-18`:
link it as `lldb-dap`, or set `command` in your `languages.toml`); macOS: `xcrun -f lldb-dap` with
the Xcode command line tools, or Homebrew's `llvm`; Windows: the LLVM installer. Templates:
`binary`, `binary with arguments`, `binary (terminal)` (`runInTerminal`, in the external terminal),
`attach` (a pid; on Linux, attaching to a process that isn't your child needs
`/proc/sys/kernel/yama/ptrace_scope` at 0, or root), `gdbserver attach`. Environment variables:
`env = ["NAME=value"]` in a template of your own.

For readable Rust values (`String`, `Vec`, `Option`...), load rustc's lldb formatters, which
`rust-lldb` loads, from `~/.lldbinit` (lldb-dap reads it):

```
command script import <sysroot>/lib/rustlib/etc/lldb_lookup.py
command source <sysroot>/lib/rustlib/etc/lldb_commands
```

with `<sysroot>` what `rustc --print sysroot` prints. Without them a `String` shows as a struct
you can open, down to its bytes.

**js-debug** (JavaScript, TypeScript, Node): VS Code's JavaScript debugger, as a standalone DAP
server: download `js-debug-dap-v<version>.tar.gz` from
[its releases](https://github.com/microsoft/vscode-js-debug/releases), extract it, and either put
a `js-debug-dap` script on your `PATH` that runs `node /path/to/js-debug/src/dapDebugServer.js "$@"`,
or set the command in your `languages.toml`:

```toml
[[language]]
name = "javascript"
[language.debugger]
name = "js-debug-dap"
transport = "tcp"
command = "node"
args = ["/path/to/js-debug/src/dapDebugServer.js"]
port-arg = "{} 127.0.0.1"
```

(the same for `typescript`; your `[language.debugger]` replaces the default one, templates
included, so copy them from the default `languages.toml`). Templates: `source` (a `.js` file; for
TypeScript the `.ts` file, which js-debug runs from the compiled file it finds for it, through its
source map, so `tsconfig.json` needs `"sourceMap": true`; without compiled files node 22.18 and
later runs the `.ts` itself) and `attach` (to `node --inspect`, port 9229 by default). The templates
set js-debug's `type = "pwa-node"` and its `__workspaceFolder` (without it js-debug drops its
default `outFiles`, and breakpoints in TypeScript sources land only after the code ran).

### Theme

| Scope | What | Fallback |
| --- | --- | --- |
| `ui.highlight.frameline` | the line the program stopped at | none |
| `ui.debug.active` | `▶` in the gutter, the current frame and thread in the panel | `ui.text.focus` |
| `ui.debug.breakpoint` | `●` | `error` |
| `ui.debug.breakpoint.unverified`, `.disabled` | `◯`, `○` | `ui.text.inactive` |
| `ui.debug.panel` | the panel | `ui.terminal`, then `ui.background` |
| `ui.debug.tab`, `ui.debug.tab.active` | the tab bar, the focused section's tab | `ui.statusline.inactive`, `ui.menu.selected` |
| `ui.debug.name`, `ui.debug.value`, `ui.debug.heading` | names, values, scopes | `variable`, `ui.text`, bold `ui.text` |
| `ui.debug.value.changed` | a value that changed since the last stop | bold `warning` |
| `ui.debug.console.input`, `.info`, `.error` | evaluated expressions, the adapter's messages, stderr and errors | `ui.text.focus`, `ui.text.info`, `error` |
| `ui.statusline.debug.running`, `.stopped`, `.exited`, `.failed`, `.playing` | `DBG ...` | yellow, red, green, red, cyan |
| `ui.debug.rewind` | the REWIND marker, in the panel and the statusline; `ui.modebar.state.rewind` falls back to it | bold reversed `warning`; black on yellow |
| `ui.debug.frozen` | `❄ frozen` | `info` |
| `ui.virtual.step-time` | the time per line | `ui.virtual.inline-blame`, then `ui.virtual.inlay-hint` |
| `ui.debug.memory` | `▦`, a value whose memory can be shown | `info` |
| `ui.debug.carried` | `carried from step 128`, a frame rewind reused from an earlier stop | `ui.text.inactive`, italic |
| `ui.debug.loading`, `.bar`, `.glow`, `.track` | the [engine](#engine)'s loading animation: its spinner and text, the bar's filled part, the glow running along it, the empty part | `ui.debug.active` (else `ui.text.focus`); its color; bold; `ui.text.inactive` |
| `ui.modebar.state.idle`, `.playing`, `.paused`, `.rewind`, `.loading` | debug mode's state on the [bar](#the-bar): no session, playing, paused, rewind, the loading animation | `ui.modebar.state` (`.rewind`: `ui.debug.rewind`) |
| `ui.modebar.lineline` | the lineline's density, and its base style | `ui.modebar.info` |
| `ui.modebar.lineline.playhead` | the rewind playhead `┃` | `ui.debug.active` |
| `ui.modebar.lineline.dead` | `─`, where the spectated thread didn't exist | `error` |
| `ui.modebar.lineline.gap` | a full-speed gap, `┄full speed┄` | `ui.text.inactive` |
| `ui.modebar.lineline.now` | the last column: the live playhead, the end of the record | `ui.text.inactive` |
| `ui.modebar.lineline.breakpoint` | `●` | `ui.debug.breakpoint`, then `error` |
| `ui.modebar.lineline.exception` | `✕` | `error` |
| `ui.modebar.lineline.pause` | `‖` | `ui.debug.active` |
| `ui.modebar.lineline.switch` | `⇄` | `info` |
| `ui.modebar.lineline.end` | `■` | `ui.text` |
| `ui.modebar.lineline.covered` | `✓` | `hint` |
| `ui.modebar.lineline.read`, `.write` | `▿`, `▾` | `info`, `warning` |
| `ui.hex.changed`, `ui.hex.unreadable`, `ui.hex.unrecorded` | in a [memory view](#memory): bytes the program changed since the stop before, `??`, `··` | bold `ui.debug.value.changed` (else `warning`); `ui.text.inactive`; `ui.virtual.whitespace` |

The vortex theme sets them all; the Matrix theme sets the debug mode ones.

### Practice

The debugger doesn't change text, so it has no `:practice` exercise; debug mode is in the tutor's
"THE DEBUGGER" section.

### Limitations

- No source without a path: frames whose code the adapter only has by `sourceReference` (js-debug's
  internal and evaluated code) show `no source` and don't open.
- js-debug reports no exit code (only a `Process exited with code N` line in the console), so the
  statusline says `ended`.
- One session (and its children) at a time, so one lineline.
- Debug mode: in view mode `h j k l w b e f x` are the mode's keys, not the editor's; move with the
  arrows, `Ctrl-d` and `Ctrl-u`, the `g` menu and search. While a session plays outside debug mode
  the camera doesn't follow (you are editing). The lineline's density is sampled (24 hits for each
  cell), so a cell is a good picture of a long run, not an exact count. The read and write marks
  are reserved for the variables' history and aren't recorded yet.
- Breakpoints don't follow edits made while debugging (upstream's behaviour).
- The console's results are text: an object evaluated there doesn't open (watch it to open it).
- Long values are cut at the panel's width (`y` yanks the whole value).
- `runInTerminal` uses the external terminal (`[editor.terminal] command`), not the terminal
  panel.
- No data breakpoints, function breakpoints, disassembly or modules list.
- Memory: a search looks through the bytes read so far only. Overwriting a variable's memory
  changes what the program reads only where its code reads that memory: an optimizer, and rustc
  even in debug builds, can keep a copy in another stack slot or a register (a `let limit = 3`
  compared through a copy kept its old value). Memory views are read through one reference each; an
  address opened by `:debug-memory` without a view of the session open needs an adapter whose
  references are addresses. lldb-dap writes memory from LLVM 21 (20 only reads it).
- Spectator mode: rewind is history, never re-execution (no `stepBack`); it records the spectated
  thread's frames, not the other threads' (the lineline records where every thread was). With
  `engine = "stepping"`, stepping out of library code skips any of your code it calls back (a
  closure given to a library function); the engine stops there. Fast forward compares values as
  text. An expensive scope (registers, globals, as the adapter marks them) records only its own
  variables.
- The engine: recording each stop whole sets the top speed of playback, not the hidden breakpoints
  (see [Measured](#measured)). A line with one of your breakpoints that doesn't stop (a condition
  that is false, a log message) isn't counted on the lineline. With an adapter that stops every
  thread at a hit, the spectated thread waits for every other thread's hit (each a round trip), and
  while it is paused at a line every thread is. `lib` and `std` are a work in progress. On a big
  program the files that aren't first in line take minutes to get their hidden breakpoints, and
  their lines aren't counted until then (they run at full speed, the lineline doesn't see them);
  the adapter grows by gigabytes (see [Measured](#measured)). Hidden breakpoints don't follow edits made while
  debugging. js-debug gets the hidden breakpoints in its
  parent session too (it can't be told upfront which session will run the program).

## Hex view

Binary files open in a hex view instead of as garbled text: offsets, the bytes in hex (or bits),
and the same bytes as ASCII. It is read-only until you ask to overwrite, it never inserts or
deletes a byte, and it never loads the file: a file of several GB opens at once.

```text
0x00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  .PNG........IHDR
0x00000010  00 00 00 10 00 00 00 10  08 02 00 00 00 90 91 68  ...............h
0x00000020  36 00 00 01 62 49 44 41  54 78 9c 63 60 60 e7 13  6...bIDATx.c``..
```

### When it appears

- Opening a file that looks binary (Vortex's check: a NUL byte in its first KiB, or the signature of
  a PDF or a PNG) shows it in the hex view, from any picker, `:open`, `gf` or the command line.
- `:hex` shows the current file in the hex view, text files too, and `:hex` again shows it as text.
  It shows the file on disk, so a buffer with unsaved changes must be written (or reloaded) first,
  and going back to text is refused while overwritten bytes aren't written (`:w`, or `:reload` to
  drop them).
- The file pickers preview binary files in hex too (instead of `<Binary file>`), whatever their
  size.

### Layout

- **Offsets** on the left, after a grey `0x`, with 8 hex digits, more for files past 4 GiB.
- **Hex mode** (the default): 16 bytes per row in two groups of 8, or 8 bytes per row when the
  view is narrower than a 16-byte row (78 columns with 8-digit offsets).
- **Bin mode** (`~`): each byte as 8 bits, 8 bytes per row in two groups of 4, or 4, 2 or 1 when
  the view is narrower.
- **ASCII** on the right: printable bytes as themselves, the others as `.`. A short last row keeps
  the ASCII column in place.
- **Colors** by kind of byte, as in hexyl, the same in both columns: null, printable ASCII, ASCII
  whitespace, other ASCII control bytes, and the bytes past ASCII. See [Theme](#hex-view-theme).
- **The cursor** is on a byte: highlighted in the column it is in (where typing goes), and
  underlined in the other one.

### Keys

| Key or command | What it does |
| --- | --- |
| `h` `l`, `←` `→` | Previous, next byte |
| `j` `k`, `↓` `↑` | Next, previous row |
| `w` `b` | Next, previous word: 4 bytes, from a multiple of 4 |
| `gg` `ge` | First, last byte |
| `gh` `gl`, `Home` `End` | Start, end of the row |
| `Ctrl-d` `Ctrl-u` | Half a page down, up; `Ctrl-f` `Ctrl-b`, `PageDown` `PageUp`: a page |
| Mouse wheel, click, drag | Scroll; put the cursor on a byte (in the column clicked); select |
| `Tab` | Go between the hex (or bin) and the ASCII column |
| `~` | Hex mode ↔ bin mode |
| `G` | Go to an offset: `0x1a4` in hex, `420` in decimal |
| `:goto 0x1a4`, `:goto 420`, `:0x1a4`, `:420` | The same, from the command line |
| `/` `?` | Search forward, backward: hex bytes (`de ad be ef`, spaces optional) or text in double quotes (`"IHDR"`) |
| `n` `N` | Next, previous match, wrapping around the file. The matches on screen are highlighted |
| `v` | Select bytes from the cursor; `;` or `Esc` stops selecting |
| `y` | Yank the selection (or the byte under the cursor) as hex text, `de ad be ef`, to the default register (`"ay` to register `a`) |
| `i` | Overwrite mode (`OVR`), until `Esc` |
| `u` `U` | Undo, redo (one step per stay in overwrite mode) |
| `:w` | Write the overwritten bytes, in place |

`:`, `space` and the keys with `Ctrl` or `Alt` the view doesn't use go to the keymap as in any
buffer: `:q`, `:w`, the pickers, the windows (`Ctrl-w`), the bookmarks (`Ctrl-o`/`Ctrl-i`)... Any
other character is refused in read-only mode with `read-only` (`?` lists the keys: `i` overwrites).

### Overwrite mode

`i` starts overwriting, `Esc` stops; the statusline says `OVR` instead of `RO`. Typing overwrites
the byte under the cursor, in the column the cursor is in:

- **hex column**: a hex digit overwrites a nibble, the high one first, then the cursor goes to the
  next byte;
- **bin column**: `0` or `1` overwrites the bit under the cursor, then the next bit;
- **ASCII column**: a character overwrites a whole byte (a character past ASCII, as many bytes as
  its UTF-8 takes, refused when they don't fit before the end of the file).

The arrows move by digit in the hex and bin columns, `Backspace` goes back one without deleting,
and other keys say what the column takes. Bytes are never inserted or deleted, so the size of the
file never changes: at the last byte, the cursor stays there.

Overwritten bytes are drawn in the `diff.delta` color until written, and the buffer shows as
modified (`[+]`, `:q` refuses). `u` and `U` undo and redo, also after writing. `:w` writes only
the bytes that changed, in place (`'file' written, 4 bytes changed in place`), never a copy of the
whole file, after the same check as text buffers: if the file changed on disk since it was opened
or last written, the write is refused until `:reload` (or `:w!`).

### Statusline

The mode element says `HEX RO`, `HEX OVR`, `BIN RO` or `BIN OVR` (in the colors of normal, insert
and select mode). The position element says where the cursor is and what it is on, and the size of
the file:

```text
 HEX RO   image.png                          0x1A4 (420)  26 38 00100110 '&'  of 4.0 GiB
```

That is the offset in hex and decimal, the byte in hex, decimal and bits, the character (or `\0`,
`\t`, `\n`, `\r`), `[N B sel]` while selecting, and the size. The selection length element counts
bytes; the elements about text (line ending, indentation, encoding, total lines) are hidden.

### How it lives in the editor

The hex view is a mode of the document, not a separate view or component: a `Document` in the hex
view has a `HexBuffer` (`vortex-view/src/hex.rs`) and draws its rows instead of its text. So it is a
buffer like any other: splits (each view with its own cursor and hex or bin mode, a new split
starting where the view it splits is), the buffer picker, `:q`, `:w`, `:bc`, history (`ga`,
`gA`) between buffers, the modified flag.

- **The bytes are read in windows**: a `HexBuffer` holds the open file and reads what it is asked
  for, through a 64 KiB window cache, with positioned reads. Drawing reads a screen of rows; a
  search reads 1 MiB chunks. Positioned reads rather than a memory map: a file that another program
  truncates can't crash the editor (reading past its new end only returns less).
- **Overwritten bytes** are a sparse map over the file (offset → byte), holding only the bytes that
  differ from the file, so the buffer is modified exactly when the map isn't empty. Undo steps
  record each byte's old and new values.
- **A debugged program's memory** is another source of bytes for the same buffer, with the
  addresses as offsets and pages read through the debug adapter as they are drawn: see the
  debugger's [Memory](#memory).
- **The text of the document** stays as it was (empty for a file opened in the hex view), and
  `Document::apply` refuses every transaction meanwhile: the keys that reach the keymap can't
  change it unseen. Going back to text reloads it from the file.
- **Files changed on disk**: the check compares the modification time and size only, since hashing
  the content (what text buffers do) would read the whole file.

### Hex view theme

| Scope | For | Falls back to |
| --- | --- | --- |
| `ui.hex.null` | `00` | `ui.text.inactive`, `comment` |
| `ui.hex.printable` | printable ASCII, `21`–`7e` | `function` |
| `ui.hex.whitespace` | tab, line feed, vertical tab, form feed, carriage return, space | `string` |
| `ui.hex.control` | the other ASCII control bytes, `01`–`1f` and `7f` | `constant` |
| `ui.hex.other` | `80`–`ff` | `ui.text` |
| `ui.hex.prefix` | the `0x` of the offsets | `ui.text.inactive`, `comment` |
| `ui.hex.offset` | the digits of the offsets | `ui.linenr` |
| `ui.hex.match` | search matches | `ui.highlight`, `ui.selection` |
| `ui.hex.changed` | in a [memory view](#memory), bytes the program changed since the stop before | `ui.debug.value.changed`, `warning` (bold) |
| `ui.hex.unreadable` | in a memory view, `??`: memory that can't be read | `ui.text.inactive`, `comment` |
| `ui.hex.unrecorded` | in a memory view in REWIND, `··`: bytes not recorded at that stop | `ui.virtual.whitespace`, `ui.text.inactive` |

Overwritten bytes use `diff.delta`, the selection `ui.selection` and the cursor `ui.cursor.primary`
(`ui.cursor`, else reversed colors). The default theme sets the `ui.hex.*` byte colors.

### Limitations

- The keys are built in, not in the keymap, so they can't be remapped, and there are no counts
  (`5j`): digits are refused like any other character in read-only mode.
- A search runs on the main thread: through a multi-GB file it can take seconds when the match is
  far or absent.
- History remembers buffers, not offsets: `ga` comes back to the hex view where its cursor is, not
  to the offset before a `G` or a search.
- `:w` writes to the file itself only: `:w other.bin` is refused.
- A file changed on disk counts as changed when its modification time does, even if `touch` left
  its bytes the same.

## Dashboard

`vx` and `vx <folder>` open on a dashboard instead of an empty buffer or the file picker: a main
menu with the title, the state of the project in a git repository, its languages, and a key for
each way to start, over a vortex that spins behind it. `vx <file>` opens the file as before, never
the dashboard.

```text
░░······░░░░·░░░░▒▒▒▓▓▓▓▓▓▓▓▓████▓▒▒▒▒▓▓▓████████▓▒▒░░░·········░░▒▓███▓▒░░░·░░░░▒█▓▓█▓▒░········░▒█
·······░░░░░░▒▒▒▓▓▓███▓▓▓▓▓▒▒▒▒░░░░░░░░░▒▒▒▓▒▓▒▓████▓▓▓▓▓░░········░▒▓████▓░░░░░░░▒▓▓████▒········░▒
·····░░░▒▒▒▓███▓████▓▓▒▒▒░░░░░░░░░░░░░░░░░░░░░░░▒▒▒▓▒▓▓▓▓▓▓▓░░·······░▒█████▒░░░░░░░▒▓███▒░········░
··░░░▒▓▓▓▓█████▓▓▒░░░░░░·░░░░░░░░░▒░▒▒▒░░░░░░░░░░░░░░░░░░▒▒▓▓▓▓▒░░·····░▒████▓░░░░·░░░▓▓█▓░░·······░
░░▒▓█▓█▓▓▓▓▓▒░░░░·····░░░░░░▒▒▒▓▓███████▓█▓▓█████▓▓▒░░░░░░░▒▒▒▓██▓▒░····░░▓██▓▓▒░░░·░░░▒▒▒▒░░·······
▒▓██▓█▓▓▒▒░░░··                                                                      ░░░▒▒▒▒░·······
██▓▓██▒░░░·····  ▀█      █▀     ▄██▄     ██▀▀▄▄       ▄██▄     ██▀▀▀▀▀▀  ▀█▄    ▄█▀
 ▀█    █▀    ▄█▀  ▀█▄   ██   ▄█▀   ▄█▀██▀█▄    ▀█▄        ▀█▄▄█▀
  ██  ██    ██      ██  ██▄█▀▀    ▀▀  ██  ▀▀     ██         ██
   █▄▄█      ▀█▄  ▄█▀   ██  ▀█▄       ██       ▄█▀        ▄█▀▀█▄
    ██         ▀██▀     ██    ▀█      ██      ██▄▄▄▄▄▄  ▄█▀    ▀█▄
```

### Usage

| Key or command | What it does |
| --- | --- |
| `i` | The empty buffer under the dashboard, in insert mode |
| `Esc` | The empty buffer, in normal mode |
| `f` | The file picker (`space f`) |
| `e` | The file tree (`space e`) |
| `l` | The commit log (`space l`), in a git repository |
| `t` | The [worktrees](#worktrees) (`:wt`), when the repository has other worktrees |
| `w` | [Watch mode](#watch-mode) (`:watch`), in a git repository: the dashboard closes and the mode bar opens |
| `q` | Quit (`:q`) |
| `:dashboard` | Show the dashboard again, over a new empty buffer (or over the current one when it is an empty scratch buffer), its animations from the start |
| `:vortex` | Only the vortex and the logo on its panel, centered (no credit line, no stats, no actions; no logo with `dashboard-title = false`), moving as `dashboard-background` says (`off` shows it still); it goes like the dashboard. Handy for a screenshot |

Every other key goes to the keymap as in any buffer: `:`, the `space` menu and its infobox,
`Ctrl-w`... A key that edits the buffer or leaves normal mode (`o`, `v`, a paste) drops into the
buffer like `i`. Opening a file from anywhere (a picker, the file tree, `:open`) replaces the
dashboard; closing a picker or the tree with `Esc` comes back to it. The keys on the right are
the shortest ones the keymap has for the same commands, so a remapped key shows as remapped.
Nothing waits for the animations: every key works from the first frame.

`vx <folder>` also makes the folder the working directory, as before. The dashboard always shows
the working directory: after `:cd` (or a pick in `:worktrees`) it reads its stats again for the
new folder, its animations from the start.

### Configuration

```toml
[editor]
dashboard = false               # start as before: an empty buffer, or the file picker for vx <folder>
                                # (true: always; "auto", the default: where it fits, below)
dashboard-background = "grow-on" # "grow-on", "grow", "drain", "settle", "on", "static" or "off"
dashboard-title = false         # no title and no credit line: the stats and the actions only
```

| `dashboard-background` | The vortex |
| --- | --- |
| `grow-on` (default) | starts empty; once loading ends, grows from the eye behind the panels until it reaches the corners, then keeps spinning like `on` |
| `grow` | grows like `grow-on`, slowing down as it grows, and stops once it reaches the corners; no more frames after that |
| `drain` | starts full and spinning; once loading ends, no new cells light at the rim: the edge falls into the eye, faster and faster like the flow, and the background ends empty; no more frames after that |
| `settle` | spins like `on` while the dashboard loads, then slows down to a stop over about three seconds once the title is open; no more frames after that |
| `on` | full from the first frame, spins while the dashboard is up, at about 20 frames a second |
| `static` | one still frame, never animated |
| `off` | no vortex and no panels: the title, the stats and the actions on the background |

`load`, the name `settle` had before, still works. "Once loading ends" is when the title starts to
open or a second after the dashboard opened, whichever is first (`MOTION_DELAY`): a big repository
whose stats take seconds doesn't hold the vortex back. Since the title never opens sooner than a
second, with fast stats both are the same instant. Growing and draining take 1.6 s from the eye to
the corners or back (`REACH` in `timeline.rs`, the one constant for both), in any window.

`:dashboard` works with `dashboard = false` too.

**Where it fits.** `dashboard = "auto"`, the default, decides by the situation, never by a list of
terminal names: the dashboard opens in a local terminal, and not under SSH (any of
`SSH_CONNECTION`, `SSH_TTY` or `SSH_CLIENT` set: its frames would cross the network) nor on the
Linux console (`TERM=linux`: few colors, no block shades). An explicit `dashboard = true` or
`false` always wins. `vx --plain` (`vx -p`) starts without the dashboard for that run, whatever the
config says.

The gap: a tmux (or screen) server started over SSH keeps the SSH variables, so a `vx` started in
it after attaching locally still counts as remote. It fails safe: no dashboard (`:dashboard`
shows it). The other way round, a shell opened in a local tmux server before an SSH client
attached has no SSH variables, so a `vx` started from it shows the dashboard over SSH (tmux's
`update-environment` gives the variables to the windows opened after the attach, not before).

**Narrow windows.** The actions take one row in a narrow window, and two when one is too narrow
for them, before the stats are dropped.

### The title

The title is a runic, brush-stroke wordmark: a diamond O, an up-arrow T, Σ for E, a sharp R. Its
diagonals and the Σ's bars are drawn with half blocks (`▀▄`), like the R's, so every stroke is as
thick:

```text
▀█      █▀     ▄██▄     ██▀▀▄▄        ██      ████████  ▀█▄    ▄█▀
 ▀█    █▀    ▄█▀  ▀█▄   ██   ▄█▀    ██████     ▀█▄        ▀█▄▄█▀
  ██  ██    ██      ██  ██▄█▀▀    ██  ██  ██     ██         ██
   █▄▄█      ▀█▄  ▄█▀   ██  ▀█▄       ██       ▄█▀        ▄█▀▀█▄
    ██         ▀██▀     ██    ▀█      ██      ████████  ▄█▀    ▀█▄
```

A theme can bring back the block letters the dashboard had first with `dashboard-logo =
"classic"` in its [effects](#theme-effects).

The title is also the loading indicator: it reads `VX` while the stats load, and opens to `VORTEX`
once they are all in, `V` and `X` sliding apart from the center while `O`, `R`, `T` and `E`
condense in the room they leave through three shades, `░▒▓` (or `:=#` with the ASCII charset; the
inner letters first), in about 0.7 s. It never opens sooner than a second after the dashboard did: rows shimmer until their stat
is in, then type themselves in, in a cascade from the top spread over that second, and the
language bar fills in over it, so fast stats don't flash in and slow ones appear when they
arrive. The title comes in three sizes for the room there is: the large wordmark (66 columns, or
51 for the classic letters), medium box-drawing letters, and plain `v x` opening to `v o r t e x`. The credit line under it reads "fork of
helix-editor/helix, by vzio".

`dashboard-title = false` leaves out the title and its credit line, panel and all: the stats and
the actions take the rows they took, so a short window keeps more of them. The loading shows in
the rows, which shimmer and type in as before.

### The vortex

A whirlpool fills the whole dashboard, every cell lit, from a faint `·` through `░▒▓` to `█` (or
from `.` through `:=#&` to `@` with the ASCII charset, see [Theme effects](#theme-effects)). It
is a flow, not a turning picture, in two layers. The current: eight arms of logarithmic spirals
with the golden pitch (the radius grows by φ every quarter turn), crossed by five finer ones of
another pitch; a log spiral that turns looks the same as one that flows inwards, so the arms pour
into the eye, and as the two sets drift at their own speeds their crossings shift like ripples.
On it, particles ride the arms from beyond the corners towards the eye, faster and faster as they
near it, each leaving a short bright streak. A cell's light picks its character and dims its
color into the background; its distance to the eye picks the color, in two tones: the title's
color at the eye, held through the middle, fading into the color of the text at the rim.
`ui.dashboard.vortex` changes the eye's color, and `ui.dashboard.vortex.1`, `.2`, ... replace
the two tones with a gradient of your own.

Growing and draining are a reach over the same flow, not another simulation: cells farther from
the eye than the reach (distances count a row as two columns, the corners are 1) aren't drawn,
and the last 8% of it fades out. `grow` and `grow-on` widen the reach from 0 to the corners,
fast first and gently at the end; `drain` narrows it from the corners to 0, slowly first and
faster and faster. Cells and particles out of reach aren't computed at all, so an empty vortex
costs nothing and a growing one less than a full one.

The title, the stats and the actions each sit on a panel of the theme's background
(`ui.dashboard.panel`) with a margin of a row and two columns around their text; the vortex is
never drawn in a panel. The panel of the title widens as it opens. A window smaller than 48 × 16
gets no vortex.

A frame is a function of the time (glitches aside): the same time draws the same frame, which the
tests use. What it costs, measured on an optimized build (the `integration` profile) in this
repository:

| | 200 × 60 | 160 × 50 | 100 × 44 |
| --- | --- | --- | --- |
| the vortex alone, a frame | 1.3 ms | 0.9 ms | 0.5 ms |
| the whole dashboard, a frame | 1.8 ms | 1.3 ms | 0.9 ms |

The geometry of each cell (its angle on the arms and the ripples, its distance to the eye) is
kept in memory for the window's size, so a frame needs no trigonometry per cell: the arms are
`cos(a − t)` expanded with the cell's `cos a` and `sin a`. The editor draws a frame at most 33 ms
after asking for one, which keeps it under 30 a second.

The first frame, and the cost at rest, of each mode, with `vx` on the dashboard of this repository
in a pseudo-terminal at 160 × 50 (release build; at rest is from 8 to 18 seconds after it opened;
the CPU is the whole editor's):

| `dashboard-background` | First frame | At rest: frames a second | Written to the terminal | CPU |
| --- | --- | --- | --- | --- |
| `grow-on` | 0.37 ms (empty) | 20 | 590 KiB/s | 4.3% |
| `grow` | 0.37 ms (empty) | 0 | 0 | 0% |
| `drain` | 2.4 ms | 0 (empty) | 0 | 0% |
| `settle` | 2.2 ms | 0 | 0 | 0% |
| `on` | 2.1 ms | 20 | 600 KiB/s | 4.9% |
| `static` | 2.1 ms | 0 | 0 | 0% |
| `off` | 0.27 ms | 0 | 0 | 0% |

First frames vary by a few tenths of a millisecond from run to run. The first frame of a full
vortex also computes its geometry for the window (the rest of the 2 ms
over a frame's 1.3); starting empty skips both, and the vortex costs only what it reaches while it
grows. A spinning vortex writes about 30 KiB a frame, mostly colors (the shades of the gradient are
few so that neighbor cells share them). The modes that stop write nothing and draw no frames once
they have.

### What it shows

Only in a git repository (a folder outside one gets the title, the credit line and the actions,
without `l`):

- **branch** and **commit**: the branch (`detached at 8b83cad` when detached) and HEAD's short id
  and summary (`no commits yet` before the first);
- **upstream**: the upstream branch of the current one and how many commits it doesn't have
  (`3 unpushed`, `all pushed`), or `no upstream`. There is no "behind": it would need a fetch;
- **remote**: the upstream's remote, else `origin`, else the first one, with its URL as
  configured, or `no remote`;
- **changes**: staged, unstaged and untracked files, like `git status`, or `clean`;
- an operation in progress (**merge**, **rebase**, **cherry-pick**, **revert**, **bisect**) and its
  conflicted files, when there is one (unmerged files without an operation, after a conflicting
  `git stash pop`, show as **conflicts**);
- **worktrees**: the other worktrees of the repository (the main one from a linked one), with
  their branch and, dimmed, when they were last modified (`2h ago`, see
  [Worktrees](#worktrees) for what counts), the most recently modified first, at most three rows.
  A worktree whose folder is gone says `missing`, last. `t` (`Worktrees`) opens the
  `:worktrees` picker; it is in the actions only when there are other worktrees (missing ones
  don't count), as soon as they are listed, before they are dated;
- **languages**: a thin bar of each language's share of the lines, the five largest named in a
  grid under it with their lines, and the total. Lines are newlines, counted in the files git
  tracks, skipping binary files (the hex view's check) and lockfiles (`[editor.changes]
  lockfiles`), grouped by the editor's own language detection (the file types of
  `languages.toml`). Files of no known language, and the languages too small for a segment (under
  a 200th of the lines, or past the fifth), are **other**, in grey, always last.

A value too long for its column goes on to a second row, under the value column, and ends with
`…` past two rows. Commit summaries and other values break between words, remote URLs also after
a `/` (or the `:` of an scp-style `git@host:path`).

Errors stay calm and in place: `no upstream`, `detached at 8b83cad`, `couldn't read` for a stat
git couldn't read, `3 unreadable` next to the total for files that couldn't be read. A
repository that can't be opened at all (a broken `.git`) shows no stats.

### Layout

Everything is centered. From 108 columns on, the project and its languages are side by side;
narrower, they are stacked. When the window is too short, the layout gives up room in steps:
smaller gaps, long values back on one row (with `…`), fewer rows of languages, the medium title,
one row of worktrees, the actions on one row, the plain-text title, no language names, no gaps,
and at last no stats (the actions back in a column when they fit). Long values give up their
second row first, before anything is dropped. The actions go on one row only when it fits the
width.

### Loading

The first frame draws at once: nothing is read before it but a look for `.git` in the folder and
its parents. Each stat is read in the background on its own. The language bar fills in as the
files are read (a few threads at once), with the counts so far, and the heading counts the files
read. Once nothing moves anymore (everything in, the title open, the vortex still or off), the
dashboard asks for no more frames and the editor is idle; in the `on` mode the vortex asks for
frames until the dashboard closes. Closing the dashboard stops the counting and the frames.

Nothing touches the network (`vortex-vcs` builds gix without any network feature) and nothing is
written: there is no cache and no list of recent files.

In a workspace that isn't trusted, gix can leave out the repository's own config; the remote and the
upstream are names and URLs, so they are read from `.git/config` directly as plain data.

### Dashboard theme

| Scope | For | Falls back to |
| --- | --- | --- |
| `ui.dashboard.logo` | the letters of the title, and its shades while it opens | `keyword` |
| `ui.dashboard.logo.shadow` | the shadow of the letters | `ui.text.inactive`, `comment` |
| `ui.dashboard.credit` | the line under the title | `ui.text.inactive`, `comment` |
| `ui.dashboard.heading` | `PROJECT`, `LANGUAGES` (bold) | `ui.text.focus`, `ui.text` |
| `ui.dashboard.label` | the labels of the rows, calm errors | `ui.text.inactive`, `comment` |
| `ui.dashboard.value` | the values | `ui.text` |
| `ui.dashboard.key` | the key badges of the actions | `ui.selection` with `ui.text.focus` (bold), else reversed |
| `ui.dashboard.action` | the actions | `ui.text` |
| `ui.dashboard.hint` | the keymap's keys next to the actions | `ui.text.inactive`, `comment` |
| `ui.dashboard.spinner` | the spinner of a heading while loading | `ui.dashboard.logo` |
| `ui.dashboard.panel` | the panels under the title, the stats and the actions | `ui.background` |
| `ui.dashboard.vortex` | the vortex's color at the eye, fading into the text's at the rim | `ui.dashboard.logo` |
| `ui.dashboard.vortex.1`, `.2`, ... | a gradient of your own for the vortex, from the eye out (as many as you like) | the two tones above |

Each scope is looked up exactly (a theme with only `ui.dashboard.logo` doesn't color the shadow
like the letters). `clean` and `all pushed` take the foreground of `diff.plus`, the unpushed
count, an operation in progress and conflicts that of `warning`. The language bar takes its
colors from the foregrounds of `function`, `string`, `keyword`, `type` and `constant`, in that
order (the largest language first), and `other` the foreground of `ui.text.inactive` (or
`comment`): scopes every theme has, so the bar works with any theme. The vortex mixes its
colors only when they are true colors (`#rrggbb`), and dims into the background only when that is
one; with named colors each band takes the nearest stop.

### Limitations

- The language counts read every tracked file, in the background; in a very large repository
  (hundreds of thousands of files) that takes a while and keeps a few threads busy until done or
  until the dashboard closes, and the title stays `VX` until then.
- Language detection is by file name only (no shebangs), like a file's language before its text
  is read.
- The stats aren't refreshed while the dashboard is up: `:dashboard` reads them again.
- `l` isn't offered when the repository couldn't be opened, even though the commit log might.
- The actions are keys only, not clickable.
- In the `on` mode the whole screen is redrawn about 20 times a second while the dashboard is up,
  under a picker too (and in `grow-on` once it has grown); over a slow connection (ssh) prefer
  `grow`, `settle` or `static`.
- The vortex's center is the window's, mostly under the panels; the eye shows only in windows
  much larger than the text.

## Worktrees

`:worktrees` (alias `:wt`, no key by default) opens a picker of the repository's worktrees:
the current one first, marked `●`, then the others by when they were last modified, the
most recently modified first. `Enter` goes to one the way `:cd <its path>` does. The dashboard
lists them in the same order and opens the picker with `t`.

```text
│     path       branch              changes                             unpushed    modified   │
│ > ● ~/main     main                clean                               all pushed  just now   │
│     ~/dirty    dirty               1 staged · 1 unstaged · 1 untracked no upstream just now   │
│     ~/ahead    ahead               clean                               2 unpushed  5h ago     │
│     ~/detached detached at fe847c0 clean                               no upstream 3d ago     │
│     ~/gone     missing                                                                        │
```

### Usage

| Key or command | What it does |
| --- | --- |
| `:worktrees`, `:wt` | Open the picker, in normal mode. Outside a git repository, or without other worktrees, there is no picker, only `no other worktrees` in the status line |
| `t` on the dashboard | The same, when the repository has other worktrees |
| `j`/`k`, `s`, `Esc`, `q` | The keys of the fork's other pickers: move, filter (by path and branch), back to normal mode or close |
| `Enter` | Exactly what `:cd <worktree>` does: the working directory changes, nothing else (no buffer is closed or opened). On the current worktree it only closes the picker; on a missing one the picker stays and the status line says it is missing |

The columns:

- **path**, from the home directory (`~/...`);
- **branch**, or `detached at <hash>`, or `missing`;
- **changes**: staged, unstaged and untracked files, like `git status`, or `clean`;
- **unpushed**: the commits the branch has that its upstream doesn't (`2 unpushed`, `all
  pushed`), or `no upstream` (also while detached). There is no "behind": it would need a fetch;
- **modified**: when it was last modified, `just now`, `5m ago`, `2h ago`, `3d ago`, `4mo ago`,
  `2y ago`.

### Last modified

A worktree was last modified at the newest of:

- the modification times of the files its status lists (changed, staged or untracked; a deleted
  file has none), from the same status that gives its counts;
- its index file (`git add`, `git reset`);
- its `HEAD` and `logs/HEAD` in its git dir (commits, checkouts, resets).

Every tracked file is never looked at: a file touched without being changed doesn't count, and a
worktree costs one status, like the dashboard's `changes`.

### Loading

The picker opens at once with the paths and branches (a few files read in `.git`), ordered by
the time of the git dir's files alone. The counts, upstreams and times fill in from a background
job per worktree, each row shimmering like the dashboard's while it loads; once they are all in,
the rows are ordered by their full times once (keeping the cursor on its row, and not while
filtering, so nothing moves under a query). The dashboard reads the statuses of the worktrees at
once, a thread each, and lists them when all are in. Only local git is read, never the network,
and nothing is written.

A worktree whose folder is gone (deleted without `git worktree remove`, what git calls prunable),
or that git lost track of (its `gitdir` file is gone, shown by its directory in `.git`), shows
`missing`, calmly, last, and can't be picked. `git worktree prune` forgets them.

### Limitations

- The list is read when the picker opens; it doesn't follow worktrees added or removed while it
  is open.
- A large repository with many worktrees costs one status per worktree, on the dashboard as in the
  picker.
- The current worktree is the one the working directory is in: picking it from a subfolder of
  it doesn't go to its root.

## Watch mode

Watch mode follows the changes made to the working tree as they happen, by an agent at work, a
formatter, a script or another editor: each change is opened, jumped to and flashed, its lines are
marked in the gutter, and `n`/`N` go through all of them in the order they happened. It is a mode:
while it is on, its keys come first, and its state and keys are on the [mode bar](#the-mode-bar).

```text
   28  line 28 of main
   29  line 29 of main
   30 ┃line 30 CHANGED by the agent        <- jumped to, centered, flashed
   31  line 31 of main
 NOR   src/main.rs                                                   1 sel  30:1
 WATCH ● following      src/main.rs · 3/12 changes · 2 files      followed src/main.rs:30
  v  look   n  next   N  previous   c  changes   Esc  leave
```

### Usage

| Key or command | What it does |
| --- | --- |
| `:watch`, `vx --watch [folder]` | Start watch mode in the repository of the working directory (with a folder, in it). `:watch` again ends it. `:watch` is in the command palette (`space ?`), and `w` on the [dashboard](#dashboard) starts it |
| `:watch-stop` | End watch mode. The session's changes stay: their markers, `n`/`N`, `:watch-changes` |
| `:watch-clear` | Forget the session: the markers go, `n`/`N` search again |
| `:watch-changes` | The picker of the session's changes (below) |

While watch mode is on, these keys come first (`[keys.watch]`, below):

| Key | What it does |
| --- | --- |
| `v` | Look around: your own cursor, drawn as in select mode (`ui.cursor.select`), moves and searches freely while new changes are only counted. `v` again follows again, from the newest change |
| `n`, `N` | The next, previous change of the session, across files, in the order they happened (newest last), around at the ends. Reaching the newest with `n` follows again; going to an older one looks around |
| `c` | The picker of the session's changes (`:watch-changes`) |
| `Esc` | Leave watch mode, to edit where you are. The changes stay |

**Following.** When a file changes on disk (changes are merged until 200ms pass without one, at
most a second, like the [live views](#uncommitted-changes-live)), its buffer is opened, or shown:
in the split that already shows it, else in the current one. The cursor goes to the first new
hunk of the file, which is centered and flashed (`ui.highlight.watch`, fading out over about a
second). When a batch touches several files, the most recently modified is followed; the others
are only marked.

**Keys don't stop it by accident.** While following, the movement and search keys (`hjkl`, words,
`gg`, `C-d`, `/`, ...) do nothing: the bar says that `v` looks around. While looking around they
work as usual, and `/`, `?`, `*` search (but `n`/`N` stay the changes'). Keys that would edit or
open something else (insert mode, `d`, `o`, a paste, a picker, `gf`, `gd`, a buffer switch,
[history](#history-and-bookmarks)'s `ga`/`gA`, the bookmarks' `C-i`/`C-o`/`space j`, a split) do
nothing in either state: the bar says that `Esc` leaves watch mode first. The command line (`:`),
the palette, the [terminal panel](#terminal-panel) (`C-t`: an agent can run in the panel while
its changes are followed above it) and setting a bookmark (`C-s`) work in both. A typed command
is meant: `:goto` looks around, the watch commands keep watch mode, any other command (`:w`,
`:open`, ...) leaves it first. Scrolling or clicking with the mouse looks around. The `space` and
other menus still open; a command picked in them is held like its key.

**Other modes.** One mode at a time. Starting watch mode (`:watch`, `vx --watch`) leaves
[debug mode](#debug-mode), and entering debug mode (`space G`) ends watch mode: the session's changes
stay, as after `Esc`. A [debugger](#the-debugger) session keeps running either way.

**Leaving.** `Esc` (or `:watch-stop`, `:watch`) leaves watch mode. The session's markers, `n`/`N`
and `:watch-changes` stay until the next `:watch` or `:watch-clear`; the working tree is no longer
watched, so they show the changes up to then.

**History and bookmarks.** Following and `n`/`N` through the session's changes are not
[history](#history-and-bookmarks)'s jumps: the session has its own order and its picker, and
following would otherwise fill the history with every change an agent makes. So `ga` after watch
mode goes back to where your own last jump left, not to the change followed before. A change
picked in `:watch-changes` is a jump, as in any picker. Bookmarks set while watching stay on their
lines while the files change under them (a reload maps them like any edit).

### Watch mode's keys

Watch mode's keys are a layer of their own, `[keys.watch]`, one key per command (no menus), looked
up before the keymap of normal mode while watch mode is on. Remap them there; the bar shows the
keys the layer has, and a command without a key has no badge:

```toml
[keys.watch]
l = "watch_look"        # look around on l (v still does too: add v = "no_op" to free it)
```

| Command | Default key |
| --- | --- |
| `watch_look` | `v` |
| `watch_next_change` | `n` |
| `watch_prev_change` | `N` |
| `watch_changes` | `c` |
| `watch_leave` | `Esc` |

The commands are also in the command palette.

### The bar

Watch mode's [mode bar](#the-mode-bar) has two rows:

- row 1: the state on the left, `WATCH ● following` (`ui.modebar.state.following`) or `WATCH ◌
  looking` (`ui.modebar.state.looking`, both falling back to `ui.modebar.state`); in the middle the
  current change, its number and the count (`src/main.rs · 3/12 changes · 2 files`, and `· 3 new`
  while looking around); on the right the latest activity: what was just followed, a file skipped
  for its unsaved edits, a lockfile or binary file that changed (not followed), a held key, where
  watch mode started and how many files were already uncommitted. Nothing of this goes to the
  status line.
- row 2: the key badges, from `[keys.watch]`, clickable: `v look` (`v follow` while looking around),
  `n next`, `N previous`, `c changes`, `Esc leave`.

### The statusline

The statusline's `watch` element is no longer in the default statusline: the bar shows the state.
A config with its own statusline may still list `"watch"`: it is a small `●` while following and
`◌` while looking around (`ui.statusline.watch.following` and `ui.statusline.watch.paused`, both
falling back to `ui.statusline.watch`), empty outside watch mode.

```toml
[editor.statusline]
left = ["mode", "watch", "spinner", "file-name"]
```

### What changed this session

A change of the session is a difference between a file now and its **baseline**, its content when
the watch started:

- a file clean in git: its committed version (the commit HEAD was then), read the first time the
  file changes, so starting costs nothing for clean files;
- a file already dirty (staged, unstaged or untracked, as `git status` lists them): its content at
  the start, read then; only those files are read. So uncommitted work from before never shows as
  a change of the session, and neither does a commit made during it;
- a file created during the session: all of it is a change (`++`). A deleted file is listed as
  deleted, and not followed.

Files git ignores are not watched. Lockfiles (`[editor.changes] lockfiles`) are listed, collapsed
to their line counts, but not followed nor counted nor gone through with `n`/`N`; binary files are
listed but not followed into. Everything is kept in memory, nothing is written: quitting drops the
session.

**Hunks are numbered as they appear.** A hunk that is still there with the same lines keeps its
place in the order when its file changes elsewhere; a new hunk, or one that grew, comes last. `n`
goes on from the hunk under the cursor, or else from the one followed or gone to last; with neither,
`n` goes to the oldest and `N` to the newest.

### Markers

The session's hunks have their own marker in the `diff` gutter, over git's: `┃` where lines were
added or changed, `▔` where they were removed (git's are `▍` and `▔`). Their colors are
`diff.watch.plus`, `diff.watch.delta` and `diff.watch.minus`, which fall back to `diff.watch`, then
to git's gutter colors. A buffer's markers follow its edits, also after watch mode ended, as git's
do. Lines changed before the watch keep git's marker.

### The picker

`:watch-changes` lists the session's changes in the shape of the [lists of
changes](#lists-of-changes): one row per hunk under a divider per file, with the `+`/`-` marks, the
most recently changed file first and, in a file, its newest hunk first. Lockfiles are a divider
alone, deleted files are `(deleted)`, created ones `(new)`. The prompt's row has the totals: `this
session: 6 changes in 4 files, +7 -4`.

```text
   edit change
── src/main.rs  3 hunks  +3 -3 ─────────────────────
 > -+   src/main.rs:12
   -+   src/main.rs:5
   -+   src/main.rs:30
── src/new.rs (new)  1 hunk  +2 ────────────────────
   ++   src/new.rs:1-2
── src/lib.rs  1 hunk  +1 -1 ───────────────────────
   -+   src/lib.rs:20
── Cargo.lock  +1 ──────────────────────────────────
```

Its keys are those of the lists of changes (`j`/`k`, `w`/`b` between files, `s` filters, `?` lists
them); `Enter` goes to the change (`n` goes on from there) and closes the picker, `C-s`/`C-v` open
it in a split. Opening the picker looks around but doesn't end watch mode.

### Unsaved buffers

A buffer with unsaved edits of your own is never overwritten: the rule of [files changed on
disk](#files-changed-on-disk) applies (a warning, `:reload` or `:write!`), and following skips
that file; the others are still followed. Its markers compare the buffer with the baseline.

### Limitations

- `n`/`N` mean the session's changes in watch mode, and after it until a search: once watch mode
  ended, a search (`/`, `?`, `*`) gives them back to the search, and `:watch-clear` too. While
  watch mode is on, a search moves to the first match, but `n` goes to changes.
- The hunks `n`/`N` and the picker go to are those of the files on disk at their last change: in a
  buffer edited since (after watch mode ended), the line may have moved. The markers follow the
  edits.
- A change made in the instant between the start of the watch and the snapshot of a dirty file is
  part of the baseline.
- A rename is a deleted file and a created one.
- Watch mode and the commit log's live views each have a watcher on the working tree when both are
  on.
- Following shows changes one batch at a time: in a burst over several files, only the newest is
  jumped to (the others are marked, and `n`/`N` reach them).

## History and bookmarks

Upstream's jumplist mixes two things: an automatic "where I was" and the places marked by hand
with `Ctrl-s`. It records so much that `Ctrl-o` can't be trusted. This fork splits it in two:
**history**, automatic and invisible, on `ga` and `gA`; and **bookmarks**, set by hand, on
`Ctrl-s`, `Ctrl-i`/`Ctrl-o` and `Space j`.

### History

`ga` goes back to where the last jump left, and pressing it again returns: `ga ga` is where you
were. `gA` steps one place back and leaves the `g` menu open in a small sticky state (its infobox
says `A back · a forward · Esc done`):

| Key | Does |
| --- | --- |
| `ga` | Back to the last place jumped from; again, back where you were. After a walk with `gA`, back to where the walk started, like `cd -` |
| `gA` | One place back, then the sticky state |
| `A` | (sticky) One more place back: `gAAA` is back three |
| `a` | (sticky) One place forward: `gAAAa` is back three, forward one |
| `Esc` | (sticky) Closes the state |
| any other key | Closes the state and does what it always does: `gAAAj` is back three, then down a line |

A count repeats `ga` and `gA` (`3gA`). Walking back records where the walk starts, the present,
so `a` and `ga` can return to it; making a new jump from somewhere in the middle of a walk drops
the places ahead, as an edit drops the redo history. Each split has its own history, as each had
its own jumplist (a new split of the same buffer starts with a copy): the places in one window
are what that window went through. A history keeps 30 places, the oldest go first; the place
of a closed buffer goes with it.

**What records a jump**: only teleports, through one function (`Editor::record_jump`, with the
rule in `vortex_view::history::records`):

- the goto family: `gd`, `gD`, `gy`, `gr`, `gi` and every location a language server gives (the
  pickers of several locations, symbols, diagnostics); `gf`;
- a search: `/` and `?` once validated, `n` and `N` (`*` and `Alt-*` only set the search: they
  don't move, the `n` after them does);
- `gg`, `ge`, a line by number (`12G`, `12gg`, `:12`);
- `g.`, the last modification;
- `[d`, `]d`, `[D`, `]D`, the diagnostics, and `[g`, `]g`, the changes: like `n`, they go
  anywhere in the file;
- the result picked in any picker (files, buffers, symbols, diagnostics, global search, the
  changed files, the commits and their changes, the file tree, the bookmarks...), but the command
  palette, whose commands record their own;
- another buffer, by any means: `gn`/`gp`, `gm`, `:open`, `:buffer`, `:new`, a picker, `vx file`
  from the terminal panel, `:tutor`, `:practice`...
- another screen. The screens, the full-screen views the editor has:
  - the dashboard (`:dashboard`, `:vortex`, and the one at startup), drawn over an empty buffer;
  - the side by side diff, every kind of it: `Space =` on a file, `Enter` or `Ctrl-g` in the
    changed files picker, a file of the uncommitted changes or of a commit from the commit log;
  - the debug panel, once focused (`Ctrl-g`, `Tab` or `V` in debug mode, a click in it);
  - the hex view and a debugged program's memory: they are buffers, so they are places like any
    buffer;
- going to a bookmark (`Ctrl-i`, `Ctrl-o`, `Space j`).

**What never records**: the motions through the text around the cursor (`hjkl`, `w`/`b`/`e`,
`f`/`t`, `}`/`{`, `%`, `gh`/`gl`/`gs`, `gt`/`gc`/`gb`, `g|`, the syntax tree's `]f`, `]t`, `]a`...
and `Alt-o`/`Alt-i`, `gw` and its labels on the screen), scrolling, a mouse click in the same
buffer, selections (`s`, `S`, `K`... even though they use a regex prompt like `/`), edits, the
terminal panel (a drawer, not a screen), history's own steps, the debugger's stops (stepping
through files would fill the history; spectator mode steps many times a second),
[watch mode](#watch-mode)'s following and its `n`/`N` (the session has its own order and picker)
and switching windows. A jump that lands on the line it left records nothing, and a place on the same buffer
and line as the last one recorded replaces it rather than adding a second one.

**Screens are places.** `ga` from a file opened from a diff (`Enter`) comes back to the diff, as
it was (its scroll, its hunk), read again in case the file changed; `ga` or `gA` in the diff
leaves it for the place before. `ga` from a file opened from the dashboard brings the dashboard
back (over a new empty buffer; the one left behind is closed). `ga` back to the debug panel
focuses it, while a session runs. Pickers are not places: the result picked is the jump, and a
picker, its query and its scroll are never brought back: leaving a diff by history also closes
the pickers it was opened from.

The hex view takes `ga` and `gA` from its own `g` keys. In the debug panel, which takes its keys
first, they do nothing: history goes on from wherever `Esc` (or `Ctrl-g`) drops you.

### Bookmarks

A bookmark is a line set by hand, a place to keep coming back to: a working set across files,
in the order it was built.

| Key | Does |
| --- | --- |
| `Ctrl-s` | Sets a bookmark on the cursor's line, or removes the one there; with several cursors, on each cursor's line. Normal and select mode (in insert mode it is still the undo checkpoint) |
| `Ctrl-i` (`Tab`) | The next bookmark, in the order they were set; past the last, the first |
| `Ctrl-o` | The previous bookmark |
| `Space j` | The bookmark picker: number, file and line, the line's text, in the order they were set. It opens in normal mode like the fork's other lists (`s` filters, `?` lists the keys); `Enter` goes to one, `Ctrl-x` removes it |

- **The order is the order they were set**, not file and line: the ring goes the way the working
  set was built, across files. A bookmark's number is its rank, so removing one closes the gap
  (remove 2, and 3 becomes 2). `Ctrl-i`/`Ctrl-o` go on from the bookmark on the cursor's line, or
  else from the last one gone to or set (even if it was removed since: from where it was).
- Going to a bookmark puts the cursor on the line's first non-blank character and is a history
  jump: `ga` comes back.
- **They move with the text.** A buffer maps its bookmarks through every change, as it does its
  selections: a bookmark stays on its line when lines are inserted or deleted above it (a line
  opened right above it with `O` goes above it), and goes when its line is deleted. A file
  reloaded from disk applies the difference as a change, so its bookmarks follow it as well as
  that difference allows.
- **They outlive their buffer**: closing a file's buffer keeps its bookmarks by path and line, and
  opening the file again brings them back; `Ctrl-i` opens it. A buffer without a file takes its
  bookmarks with it when it closes.
- They are global, not per split: every window shows the same ones. They live in memory and are
  gone when `vx` quits: no file, no cache.
- `Ctrl-i` and `Tab` are bound together: terminals send the same key for both unless the kitty
  keyboard protocol is on.

**The gutter** shows bookmarks in the column of diagnostics and breakpoints: the number, `1` to
`9`, then `◆` from the tenth on, so the order is visible at a glance. With a diagnostic on the
same line the bookmark is drawn in the diagnostic's color, so the warning isn't lost. In
[debug mode](#debug-mode) the column is the breakpoints' and bookmarks aren't drawn; outside it
breakpoints aren't drawn. Theme
scope: `ui.gutter.bookmark` (falls back to `ui.linenr.selected`).

### Removed

- `Ctrl-o`, `Ctrl-i` and `Tab` as jumplist keys, and the commands `jump_backward`, `jump_forward`;
- `Ctrl-s` `save_selection`, which put the selection on the jumplist;
- `Space j` `jumplist_picker`;
- `ga` as `goto_last_accessed_file`: a buffer switch is a jump, so `ga` goes back to the previous
  buffer anyway. The command is still there, in the command palette (`Space ?`), unbound.

`gk`/`gj` in the `g` menu are unchanged. The separate roadmap item "`ga` returns to the previous
screen" is this: screens are places.

### Decisions

- **`gA`'s sticky state is the editor's, not the keymap's.** A sticky keymap node (like `Z`)
  can't take a step as it opens, and an unknown key in it does nothing instead of running as
  usual. So `A`, `a` and `Esc` are fixed keys while the state is open, taken before anything
  else (the dashboard, the hex view, a diff on top).
- **The recording rule is a kind per call site** (`Jump::Goto`, `Search`, `FileEdge`...), and
  `records` decides which kinds record: the call sites of the old `push_jump` stayed where they
  were, each now says what it is, and the motions that used to record (`]f`, `gw`, `g|`) say so
  and are filtered in that one place.
- **A jump records after it lands**, so it can tell that it landed on the line it left: the place
  is taken before the move and recorded after, with the same-line check.
- **The diagnostics and changes (`]d`, `]g`) record**, like `n`: they go anywhere in the file. The
  syntax tree's objects (`]f`...) don't: they move to what is around the cursor.
- **The debugger's stops don't record**; the debug panel, once focused, is a place.
- **The terminal panel is not a screen**: a drawer toggled with one key; recording it would fill
  the history with it.
- **Bookmarks of a closed file are kept** by path and line (the prompt only said a buffer
  without a file loses them).
- **A bookmark is a line**, stored as the start of the line: text typed at the very start of the
  line, or a line opened above it, leaves it on its line. Of two bookmarks joined onto one line
  (`J`), the older stays.
- **In watch mode**, `ga`, `gA`, `C-i`, `C-o` and `space j` do nothing until `Esc` leaves watch
  mode, as opening a file yourself does (also from the keys that bypass the keymap: the sticky
  state, the hex view, a diff); `C-s` changes nothing.
- **`Ctrl-x` removes a bookmark** in the picker: no picker or prompt key uses it.
- **`◆`** marks the bookmarks past the ninth; numbers are drawn in `ui.gutter.bookmark`, bold
  purple in the vortex theme, bold honey in upstream's.

### Limitations

- Undo brings a deleted line back, not its bookmark.
- Moving a line with [move lines](#move-lines) (`j`/`k` in move mode, `M`; the `move_lines_down`
  and `move_lines_up` commands) deletes and inserts it: its bookmark goes.
- The hex view's history places are buffers, not offsets: `ga` comes back to the hex view where
  its cursor is now, not to the offset before a `G` or a search.
- A diff left by history is kept as it was and read again when it comes back; leaving it with
  `q` drops it, and history skips it from then on.
- A walk's places of a closed buffer are dropped; if it was where the walk started, `ga` says so.

## The vortex theme

The editor's default theme is the owner's own, `vortex`: near-black greys, reds for keywords,
strings and errors, purples for types, numbers and the modes, and colors for every screen of this
fork (the file tree, the terminal panel, the hex view, the dashboard and its vortex, `:practice`).
It is built into the binary, like the 16-color `base16_default`, so it works without a runtime
directory. Upstream's default theme is built in beside it as `helix`:

```toml
theme = "helix"
```

`default`, the name the built-in theme had before, still loads `vortex` (in `theme = "default"`
and in a theme's `inherits = "default"`), so older configs keep working; it is no longer listed by
`:theme`. A terminal without true color still starts with `base16_default`.

## Practice

A built-in trainer, Rustlings crossed with VimGolf: each exercise teaches a feature with a short
lesson, then counts your keystrokes, so you can come back later and solve it with fewer. There is
nothing else: no timers, no leaderboards, no extra modes.

### Usage

| Command | Action |
| --- | --- |
| `:practice` | Continue with the first exercise that is not solved yet |
| `:practice <chapter>` | The first unsolved exercise of a chapter (`Tab` completes them) |
| `:practice list` | Pick any exercise; shows its status, par and your best |
| `:practice hint` | Reveal the next hint (three per exercise, from vague to the keys) |
| `:practice reset` | Start the exercise again |
| `:practice next`, `:practice prev` | The next or previous exercise |
| `:practice lesson` | Hide or show the lesson panel |

The exercise opens in a scratch buffer in the current view. A panel shows the exercise's number
and chapter in its border (`3. 2/10 · Words, lines and counts`), its title, then labelled
sections: the lesson, the goal set apart as a block, the hints you revealed, and the count of keys
against the par. On a terminal 100 columns wide or more it is a column at the top right; on
narrower ones it is a band at the bottom, below the lines of the buffer. It gives way to the
which-key popup.

Its styles come from theme scopes that fall back to existing ones (in order), so every theme
works: `ui.practice` (the panel: `ui.popup.info`, `ui.popup`), `ui.practice.lesson`
(`ui.text.info`, `ui.text`), `ui.practice.goal` (`markup.raw.block`, `markup.raw`, `ui.text`),
`ui.practice.hint` (`hint`, `ui.text.info`), `ui.practice.label` (`markup.heading`,
`ui.text.focus`), `ui.practice.key` (keys in backticks: `markup.raw.inline`, `markup.raw`),
`ui.practice.status` (`ui.text.info`), `ui.practice.solved` (`diff.plus`, `info`) and
`ui.practice.dim` (`ui.text.inactive`, `comment`).

The buffer is checked after every key: once its text is the goal and you are in normal mode, the
exercise is solved and the count stops. The statusline and the panel show the keys you used, the
par and your best, then the `teach` solution, how many keys the same edit takes without the
feature, and, once your best is at or under par, the shortest known (`gold`) solution if there is
one. Solve it again (`:practice reset`) to beat your best.

### Counting

- Every key event counts: `Esc`, counts, mode switches, undo, the keys typed into prompts
  (`s`, `|`, `/`, ...) and other commands (`:sort`). A paste counts one per character.
- `:practice` commands don't count, from the command line or from a key binding, and neither do
  the keys of the `:practice list` picker. Keys replayed by a macro don't count, the `q` does.
- The par is the number of keys of the exercise's `teach` solution.

### Chapters

86 exercises in 12 chapters, from the first keystroke to shell commands, described in
[runtime/practice/CURRICULUM.md](runtime/practice/CURRICULUM.md):

| # | Chapter | Exercises |
| --- | --- | --- |
| 1 | Modes and moving | 6 |
| 2 | Lines and characters | 10 |
| 3 | Words, lines and counts | 10 |
| 4 | Yank, paste and move | 9 |
| 5 | Undo, redo and repeat | 6 |
| 6 | Search and jump | 8 |
| 7 | Many selections | 13 |
| 8 | Text objects | 6 |
| 9 | The syntax tree | 5 |
| 10 | Surround | 4 |
| 11 | Macros | 4 |
| 12 | Shell and commands | 4 |

Each chapter introduces some keys, and an exercise only uses the keys of its chapter and of the
chapters before it. A chapter goes from learning the keys alone, to applying them to realistic
code, config and prose, to combining them with earlier chapters, and ends with a challenge. Every
exercise is made so that the taught feature wins: without it, the same edit takes at least twice
as many keys.

### Progress

Your best count per exercise is kept in `practice.toml` in Vortex's data directory
(`~/.local/share/vortex/practice.toml` on Linux, `~/Library/Application Support/vortex/` on macOS),
never in the repository. An exercise is unsolved (`·`), solved (`✓`) or at or under par (`★`). The
file is read when needed and written on every solve, so two Vortex instances don't overwrite each
other's results; if it can't be read, nothing is written to it and the statusline says why.

### Exercises

The exercises are TOML files in `runtime/practice/<chapter>/`, in the order of
`runtime/practice/index.toml`. [CLAUDE.md](CLAUDE.md) describes the format and the rules. The
replay test (`cargo integration-test -- practice`) checks every file and replays every solution
(`teach`, `gold`, and the brute-force `naive`) through the editor, so an exercise that a keymap
change breaks fails the tests. It also checks that the brute force takes at least twice the par,
and that every solution only uses keys that its chapter or an earlier one teaches.

### Limitations

- The fork's git features (diff view, commit log, staged files, file tree, inline blame) have no
  exercise: they don't change text, and exercises are checked by the text of the buffer. The
  [tutor](#tutor) explains them.
- Registers, the last search and macros you recorded before stay available in an exercise, which
  can make a solution shorter than it would be from a fresh start (the replay test uses one).
- The selection is not part of the goal, only the text is.
- Some keys are explained in the [tutor](#tutor) instead, because in a short text they save no keys
  over what the exercises teach, so no exercise can make them win: `G`, `<C-s>`, `<C-i>`, `g.`,
  `X`, `<A-x>`, `<A-;>`, `<A-minus>`, `<A-i>`, `<A-I>`, `<A-e>`, `<A-b>`, `]<space>`, and in
  insert mode `<C-u>`, `<C-k>` and `<A-d>`. So is the system clipboard (`<space>y`, `<space>p`): an
  exercise never overwrites yours.
- On a short terminal the panel cuts the lesson first (the last line shown ends with `…`), then the
  title, the hints and the goal; the count is always shown. `:practice lesson` hides it.
- Renaming an exercise file loses its progress, which is keyed by `<chapter>/<file name>`.

## Tutor

`:tutor` is rewritten to go with [`:practice`](#practice): it keeps upstream's header and says to
use `:practice` for editing, then covers only what doesn't fit an exercise checked by the text of
a buffer: opening, saving and quitting, buffers, pickers, the file tree, history and bookmarks, going
places (`gw`, `[`/`]`), the view, splits, the system clipboard, language servers, git (with this
fork's diff view, commit log and blame), the shell commands other than `|`, `:earlier`, settings,
the debugger, this fork's terminal panel and hex view, and a last section with the editing keys that have no exercise. Upstream's lessons
on editing are exercises now (the case keys, `<A-C>`, `n` in select mode, rotating and removing
selections were added for them).

Between them, `:practice` and `:tutor` teach every command of the default keymap. A unit test
(`every_command_of_the_default_keymap_is_taught`, in `vortex-term/src/practice/coverage.rs`) checks
it: each command of normal and insert mode must have a key in the `keys` of a chapter, or in one of
the tutor's key lists (a line that starts with two spaces and then the key, as the tutor writes it:
`space f`, `C-w v`, `A-;`). A command counts as taught when any of its keys is. A new binding
fails the test until it gets an exercise or a line in the tutor.

When a key gets an exercise, it leaves the last section of `runtime/tutor`.
