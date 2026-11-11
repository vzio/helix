# Fork features

This fork ([vzio/helix](https://github.com/vzio/helix)) is upstream
[Helix](https://github.com/helix-editor/helix) plus the features below. Everything else works and
is documented exactly as in the [Helix documentation](https://docs.helix-editor.com/).

Every feature, original or ported, ships with at least one `:practice` exercise (see
[Practice](#practice), and [CLAUDE.md](CLAUDE.md) for how to write one).

## Credits

The git diff viewer and the file explorer are ported from
[marcos-venicius/helix](https://github.com/marcos-venicius/helix) by Marcos Sousa:

- [PR #2](https://github.com/marcos-venicius/helix/pull/2): side by side git diff view, staged
  files in the changed files picker
- [PR #3](https://github.com/marcos-venicius/helix/pull/3): file explorer side panel (in this
  fork it has become an overlay built on Helix's picker, see [File tree overlay](#file-tree-overlay))
- [PR #4](https://github.com/marcos-venicius/helix/pull/4): git commit log picker with per-commit
  diffs, `C-g` in the changed files picker
- Later fixes from his `marcos` branch: diff view soft wrap (`9ccc1e16a`, `f1d06509a`), explorer
  git status markers and sideways scrolling (`cde8ec5d1`, `673ba0d33`; the scrolling went with
  the side panel)

His commits are cherry-picked with their authorship kept. The design changes in this fork: the
explorer is on `space e` (at the workspace root) and `space E` (revealing the current file),
replacing upstream's file explorer picker on those keys, it is rooted at the workspace root rather
than the working directory, and it is a modal overlay built on Helix's picker rather than a side
panel.

Inline git blame is ported from Helix
[PR #13133](https://github.com/helix-editor/helix/pull/13133) by Nik Revenco (nik-rev), which
hadn't landed upstream when it was ported. It is adapted to current master and reworked so that
blame is computed entirely in the background (see [Inline git blame](#inline-git-blame)).

Moving lines up and down is a rewrite, not a port, but it started from two earlier attempts: Helix
[PR #4545](https://github.com/helix-editor/helix/pull/4545) by sireliah (never merged) and the
bug-fixed version of it in omentic's [helix-ext](https://github.com/omentic/helix-ext) fork. See
[Move lines](#move-lines).

## Contents

- [Side by side git diff](#side-by-side-git-diff)
- [Git commit log](#git-commit-log)
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
- [Practice](#practice)
- [Tutor](#tutor)

## Code layout

Each feature lives in its own modules, to keep rebasing on upstream cheap:

| Feature | Modules |
| --- | --- |
| Diff view | `vortex-term/src/ui/diff_view.rs`, `vortex-term/src/commands/diff_view.rs` |
| Hunk actions, `n`/`N` across files | `vortex-term/src/commands/hunks.rs` (stage, unstage, revert), `vortex-vcs/src/staging.rs` (the texts of the index and of the working tree after an action), `vortex-vcs/src/git/stage.rs` (reading and writing one entry of the index), the `Actions` and `FileNav` of the diff view, `base` of `:practice` exercises, `vortex-term/tests/test/hunks.rs` |
| Commit log | `vortex-term/src/commands/git_log.rs` (also the background reading of a commit's changes), `vortex-vcs/src/commit.rs`, `vortex-vcs/src/git/log.rs`, `range_changes` and `for_each_range_file` in `vortex-vcs/src/git.rs` (the files of a commit or range, read with one repository open) |
| Uncommitted changes, live views | `vortex-term/src/commands/uncommitted.rs` (the list of uncommitted hunks, their marks, unsaved edits), `vortex-term/src/commands/changes.rs` (the lists of changes of the uncommitted changes and of commits: hunk rows, file dividers, `+`/`-` marks, lockfiles), `with_groups` and `with_section_keys` in `vortex-term/src/ui/picker.rs` (dividers that head groups, `w`/`b` between them), `with_drawn_preview` in `vortex-term/src/ui/picker.rs` (a preview drawn by its owner), `vortex-term/src/live.rs` (the shared watch, background refreshes), `vortex-view/src/watcher.rs` (the reusable watcher), `RepoState` and `log_entries` in `vortex-vcs/src/lib.rs`, `[editor.changes]` in `vortex-view/src/editor.rs`, `vortex-term/tests/test/uncommitted.rs`, `vortex-term/tests/test/change_lists.rs` |
| Files changed on disk | `vortex-term/src/handlers/disk.rs`, the disk state of `Document` in `vortex-view/src/document.rs`, `vortex-term/tests/test/stale_buffers.rs` |
| Conflict diagnostics | `vortex-vcs/src/conflict.rs` (finding the blocks), `conflicted_files` in `vortex-vcs/src/git.rs` (the unmerged files), `vortex-term/src/handlers/conflicts.rs` (when to look, the documents' state), `vortex-term/tests/test/conflicts.rs` |
| Staged files, commit diffs | `vortex-vcs/src/git.rs`, `vortex-vcs/src/lib.rs`, `vortex-vcs/src/status.rs` |
| File tree | `vortex-term/src/ui/explorer.rs`, `vortex-term/src/ui/explorer/`, `vortex-term/benches/explorer.rs` |
| Inline blame | `vortex-vcs/src/git/blame.rs`, `vortex-vcs/src/blame_stub.rs`, `vortex-vcs/benches/blame.rs`, `vortex-stdx/src/time.rs`, `vortex-view/src/blame.rs`, `vortex-term/src/handlers/blame.rs`, `vortex-term/src/commands/blame.rs`, `vortex-term/src/ui/text_decorations/blame.rs` |
| `x` on an empty line | `vortex-term/src/handlers/line_select.rs` |
| Move lines | `vortex-term/src/commands/move_lines.rs` |
| Terminal panel | `vortex-terminal/` (the shell: pty, emulator, OSC 133, bash/zsh integration), `vortex-view/src/terminal.rs` (panel state, `[editor.terminal]`), `vortex-term/src/terminal/` (drawing, focus, input encoding, the `hx` shim), `vortex-term/tests/test/terminal.rs` |
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
`Editor::terminal` field and `close_document` (an awaited buffer closing answers `hx` and focuses
the panel) in `vortex-view/src/editor.rs`, where `[editor.terminal]` also absorbed the debugger's
`command`/`args`; the layout, the cursor and `Esc` in `ui/editor.rs`; the event routing in
`Application::handle_terminal_events`; `toggle_terminal` in `commands.rs` and `keymap/default.rs`;
`:sh`, `:!` (and `:!cmd` without a space) and `quit` on an awaited buffer in `typed.rs`; the `shell`
element in `ui/statusline.rs`; the client mode at the top of `main.rs`.

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

The hunk actions add `Editor::save_messages` (`vortex-view/src/editor.rs`), a message shown
instead of `'file' written` when a document's save completes (`handle_document_write` in
`application.rs`), so that a revert says how to undo it.

To build: `cargo install --path vortex-term --locked`.

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
in a diff opened from a list of files (`file 1/3`) and the keys; the line numbers of the current
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
| `space l`, `:git-log` | Open the commit picker, in normal mode, with the cursor on its first row, the [uncommitted changes](#uncommitted-changes-live). The status line shows `s filter, v extend, w/b section, enter files`. |
| `s` | Filter mode: typing filters by summary, and by hash: a query of 4 hex digits or more (`a1b2c3`) lists the commits whose hash starts with it first, then the commits whose summary matches it. `%commit`, `%author`, `%date` filter by the other columns. `Esc` goes back to normal mode and keeps the query. |
| `j`/`k`, `Down`/`Up`, `C-n`/`C-p`, `Tab`/`S-Tab`, `C-d`/`C-u`/`PageDown`/`PageUp`, `gg`/`ge`/`G`, `Home`/`End` | Move. The keys that aren't letters work in filter mode too. |
| `Enter` in the commit picker | List the changes of the commit, or of the selected commits, or the uncommitted changes, on top of the commit picker: one row per hunk, under a divider per file ([Lists of changes](#lists-of-changes)). The status line shows the commit or the range and the totals: `a1b2c3d: 12 changes in 4 files, +80 -31 · Fix the parser`. The changes of a commit or range are read in the background: the log stays usable meanwhile, and the status line says `Loading changes of a1b2c3d…` if it takes more than a tenth of a second. Moving on and pressing `Enter` on another row, or closing the log, drops the list being read. |
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
cursor keeps the style of the cursor), and in the status line, oldest to newest, with `[extend]`
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

- List the commits that changed the current file.
- Show the commit message and stats in a preview.
- Pick another branch.

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

The status line says how much there is: `Uncommitted changes: 6 changes in 5 files, +80 -31, 2
unsaved` (a file without hunk rows is one change). The totals follow the list as it changes, but
never hide another message: while the status line shows something else (a hunk action's message
in a diff, an error), they wait until the diff is closed and the list is shown again.

While the commit log, or anything opened from it, is open, the working tree is watched, and what
is on screen follows it:

- the count of the first row;
- the list of uncommitted changes, also while buffers are edited: the cursor stays on the same
  hunk (the same file and line), or on the first change of the same file, or at the same place
  when the file is gone; and its totals in the status line;
- the diff of an uncommitted file: the line at the top of the screen stays there, and so does the
  horizontal scroll. A diff with no changes left stays open, empty, with `no changes` in the
  footer;
- the log itself when HEAD moves (a commit, a checkout, a reset), with the cursor on the same
  commit;
- the open buffers of the files that changed, with the rule of
  [files changed on disk](#files-changed-on-disk).

The watch covers what git doesn't ignore (`.gitignore` files at any depth, `.git/info/exclude`, the
global excludes file), with one watch per directory, so `target/` or `node_modules/` cost nothing,
plus `HEAD`, the index and `logs/HEAD` in the git dir. Changes are applied 200ms after the last one
(at most a second after the first, when they never stop). The watch stops when the last of these
views closes.

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
- The totals of the uncommitted changes wait while the status line shows another message: when
  something else clears it (a hunk action's message is replaced by nothing), they come back at
  the next change of the list or when a diff opened from it is closed.
- The cursor never rests on the divider of a file that has hunk rows, as in the commit log: the
  file's first hunk is where `w`/`b` go, and Enter there opens the diff at it.
- A buffer with unsaved edits is listed by its path: a buffer without a file (a scratch buffer)
  isn't.
- Only the commit log's views update live. The changed files picker (`space g`) and `space =` don't,
  and the changes of a commit don't need to.
- A directory that a `.gitignore` edit stops ignoring isn't watched until it is created again.
- Symlinked directories are not followed.

## Stage, unstage and revert hunks

In the diff of a file of the working tree (the [uncommitted changes](#uncommitted-changes-live) of
the commit log, `space =`, `C-g` in `space g`), the current hunk, or the whole file, can be staged,
unstaged and reverted:

| Keys | Action |
| --- | --- |
| `s`, `S` | Stage the hunk, the file (`git add`; a deleted file is staged as deleted) |
| `u`, `U` | Unstage the hunk, the file (back to its version in HEAD; a new file becomes untracked) |
| `r`, `R` | Revert the hunk, the file, to its version in the index |

The footer shows them, and the state of the current hunk: `hunk 2/5 staged`. On the separator
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
  nothing to revert (`u` unstages it first).
- An **untracked file** has no version to go back to: reverting it deletes it, after asking
  (`y` deletes it, any other key keeps it), since git can't bring it back.
- A buffer with unsaved changes can't be staged or reverted (write it first): the diff shows the
  buffer, which is not what is on disk.
- After an action the diff is built again at once; in the commit log it also follows changes made
  from elsewhere, like `git add` in a terminal.

`n`/`N` go through the hunks of every file of the entry the diff was opened from: the uncommitted
changes, a commit, or a range of commits. Past the last hunk of a file, `n` opens the diff of the
next file at its first hunk (`N`: the previous file at its last hunk), and the cursor of the list of
files follows. At the end of the list they go around to the first (last) file, with `Wrapped
around to the first file` in the statusline. Lockfiles and files that can't be shown (binary) are
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
  [terminal panel](#terminal-panel) (`Ctrl-t`, a click, `hx`);
- right before writing (`:w`, `:wq`, `:x`, `:wa`, auto-save);
- while the [uncommitted changes](#uncommitted-changes-live) are watched, on every change of a
  file.

Every open buffer is checked on focus; the other cases check the buffers concerned. What happens
depends on the buffer:

| The buffer | What happens |
| --- | --- |
| has no unsaved changes | It is reloaded silently, as one undoable step (`u` brings the old text back). Cursors and selections are mapped through the change, so they stay on the same text when it is still there |
| has unsaved changes | The statusline warns once, `<file> changed on disk: :reload to load it, or :write! to overwrite it`. `:write` (and `:wq`, `:wa`...) refuses with the same message until `:reload` (the disk wins, undoable) or `:write!` (the buffer wins) |

A change is a change of content: the modification time and size are compared first, and when they
differ the file is read and hashed, so `touch` or a checkout that rewrites the same bytes changes
nothing. A file that was deleted is left alone (writing creates it again), and so is a buffer
whose file didn't exist when it was opened.

### Limitations

- Nothing is checked while Helix is focused and idle, unless the uncommitted changes are watched:
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

No bundled theme sets them, so conflicts look like errors with their own gutter symbol.

### Limitations

- A block missing a marker (half resolved, with the `=======` or a `>>>>>>>` line left) isn't
  reported: only complete blocks are.
- A conflict without markers (binary files, a file modified on one side and deleted on the
  other) has no diagnostic and no row in `space D`.
- While Helix is focused and idle, with no live view open, a merge started from another terminal
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
right. It is built on Helix's own picker, so the overlay, the prompt and the preview are the
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

Helix pickers send the letters typed to their prompt, but the tree needs them as keys, so it uses
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
| `?` | Show the keys in the status line. |
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
The names for `a` and `r` are asked in Helix's usual prompt on the command line. After an operation
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

Off by default. Full reference: the
[`[editor.inline-blame]` section](book/src/editor.md#editorinline-blame-section) of the book.

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
- Cost (release build): `vortex-term/src/commands.rs`, 7,273 lines touched by 1,141 commits, takes
  1.5 s; small files 3-160 ms. It depends on the file's history, not its size. Showing a line
  is a lookup (~0.4 µs). `cargo bench -p vortex-vcs --features git --bench blame`.

### Limitations

- In a debug build blame takes ~6x longer than release (commands.rs: ~9 s; the crates it spends
  its time in are optimized even in debug builds via `[profile.dev.package]`, otherwise it would
  be ~2 minutes). The editor stays responsive meanwhile.
- After a commit made outside the editor, blame updates on the next save, `:reload` or terminal
  focus, not by itself.
- Blame is for the committed file at `HEAD`; staged-but-uncommitted lines show "Not committed yet".

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

### `hx` in the panel

The shell gets an `hx` first on its `PATH`, and `EDITOR`, `VISUAL` and `GIT_EDITOR` pointing to
it, which never starts a nested Helix. It is the `hx` binary itself in a client mode, talking to
this editor over a private Unix socket (`HELIX_TERMINAL_SOCKET`):

- `hx` alone focuses the editor (the panel stays shown).
- `hx file[:line[:col]] ...` opens the files in the editor and focuses it.
- As `$EDITOR` (`git commit`, `git rebase -i`, `crontab -e`...) it opens the file and waits until
  that buffer is closed. `:wq` or `:q` on it closes the buffer (not the split, nor the editor,
  even when it is the last one) and focuses the panel again; `:q!` closes it unsaved, so git
  aborts as usual on an empty message.
- `hx --help`, `--version`, `--health` and `--grammar` run normally.

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
`ui.background`).

`[editor.terminal]` was already the debugger's external terminal (`command`, `args`); those keys
still work there.

### Behaviour

- Typed input waits for the shell to be ready: `:sh cmd` on a new shell holds `cmd` until the
  first prompt (bash and zsh drop what is typed while they start), 3 seconds at most.
- Typing in the panel scrolls the view back down to the prompt.
- Quitting Helix kills the shell (and what runs in it).

### Practice

The panel, `:sh` and `:!` don't change text, so they have no `:practice` exercise; they are in
the tutor's "THE TERMINAL PANEL" section. The keys that put shell output into the text are
upstream's: `|` is practised in the shell chapter, and `!` and `A-!` stay in the tutor, since
with the POSIX tools the exercises may use no short text makes them beat typing the output.

### Limitations

- Unix first: the `hx` shim (a Unix socket) and the status of shells without integration (the
  foreground process group) aren't available on Windows, where the panel itself should work
  (ConPTY) but is untested.
- No keyboard scrolling of the scrollback (every key goes to the shell): use the mouse wheel.
- No text selection or copy inside the panel with the mouse; use the outer terminal's own
  selection (usually with Shift held) or the shell's tools.
- One shell, one panel: no tabs, no splits of the panel, no resizing it with the mouse.
- The debugger's `runInTerminal` still uses the external terminal, not the panel.
- `hx` in a shell started inside the panel over `ssh` or in a container doesn't reach this editor.
- A command typed with `:sh` while a full screen program runs goes to that program.

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

Its styles come from theme scopes that fall back to existing ones, so every theme works:
`ui.practice` (the panel; falls back to `ui.popup.info`), `ui.practice.lesson`,
`ui.practice.goal` (falls back to `markup.raw.block`, `markup.raw`), `ui.practice.hint`,
`ui.practice.label`, `ui.practice.key` (keys in backticks, falls back to `markup.raw.inline`),
`ui.practice.status`, `ui.practice.solved` and `ui.practice.dim`; `book/src/themes.md` lists all
fallbacks.

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

Your best count per exercise is kept in `practice.toml` in Helix's data directory
(`~/.local/share/helix/practice.toml` on Linux, `~/Library/Application Support/helix/` on macOS,
`%AppData%\helix\` on Windows), never in the repository. An exercise is unsolved (`·`), solved
(`✓`) or at or under par (`★`). The file is read when needed and written on every solve, so two
Helix instances don't overwrite each other's results; if it can't be read, nothing is written to
it and the statusline says why.

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
a buffer: opening, saving and quitting, buffers, pickers, the file tree, the jumplist, going
places (`gw`, `[`/`]`), the view, splits, the system clipboard, language servers, git (with this
fork's diff view, commit log and blame), the shell commands other than `|`, `:earlier`, settings,
the debugger, this fork's terminal panel, and a last section with the editing keys that have no exercise. Upstream's lessons
on editing are exercises now (the case keys, `<A-C>`, `n` in select mode, rotating and removing
selections were added for them).

Between them, `:practice` and `:tutor` teach every command of the default keymap. A unit test
(`every_command_of_the_default_keymap_is_taught`, in `vortex-term/src/practice/coverage.rs`) checks
it: each command of normal and insert mode must have a key in the `keys` of a chapter, or in one of
the tutor's key lists (a line that starts with two spaces and then the key, as the tutor writes it:
`space f`, `C-w v`, `A-;`). A command counts as taught when any of its keys is. A new binding
fails the test until it gets an exercise or a line in the tutor.

When a key gets an exercise, it leaves the last section of `runtime/tutor`.
