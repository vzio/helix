# Fork features

This fork ([vzio/helix](https://github.com/vzio/helix)) is upstream
[Helix](https://github.com/helix-editor/helix) plus the features below. Everything else works and
is documented exactly as in the [Helix documentation](https://docs.helix-editor.com/).

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

## Contents

- [Side by side git diff](#side-by-side-git-diff)
- [Git commit log](#git-commit-log)
- [Staged files in the changed files picker](#staged-files-in-the-changed-files-picker)
- [File tree overlay](#file-tree-overlay)
- [Inline git blame](#inline-git-blame)

## Code layout

Each feature lives in its own modules, to keep rebasing on upstream cheap:

| Feature | Modules |
| --- | --- |
| Diff view | `helix-term/src/ui/diff_view.rs`, `helix-term/src/commands/diff_view.rs` |
| Commit log | `helix-term/src/commands/git_log.rs`, `helix-vcs/src/commit.rs` |
| Staged files, commit diffs | `helix-vcs/src/git.rs`, `helix-vcs/src/lib.rs`, `helix-vcs/src/status.rs` |
| File tree | `helix-term/src/ui/explorer.rs`, `helix-term/src/ui/explorer/`, `helix-term/benches/explorer.rs` |
| Inline blame | `helix-vcs/src/git/blame.rs`, `helix-vcs/src/blame_stub.rs`, `helix-vcs/benches/blame.rs`, `helix-stdx/src/time.rs`, `helix-view/src/blame.rs`, `helix-term/src/handlers/blame.rs`, `helix-term/src/commands/blame.rs`, `helix-term/src/ui/text_decorations/blame.rs` |

Hooks into core files: commands and keys in `helix-term/src/commands.rs`,
`helix-term/src/commands/typed.rs` and `helix-term/src/keymap/default.rs`; `C-g` and the hooks the
file tree needs (an external filter, replacing the items, the query and the selected index) in
`helix-term/src/ui/picker.rs`;
`[editor.explorer]` and `[editor.inline-blame]` in `helix-view/src/editor.rs`. Inline blame also
has one field on `Document`, one decoration in `render_view`, `is_last_visual_line` in
`helix-term/src/ui/document.rs`, and refresh calls on save (`application.rs`), on focus
(`ui/editor.rs`) and in `:reload`/`:reload-all`.

To build: `cargo install --path helix-term --locked`.

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
| `n` / `N`, `]c` / `[c` | Next / previous hunk |
| `j` / `k`, `C-d` / `C-u`, `C-f` / `C-b`, `PageDown` / `PageUp`, mouse wheel | Scroll |
| `gg` / `ge`, `G` | Top / bottom |
| `h` / `l`, `0` | Scroll horizontally, back to the start of the line |
| `gh` / `gl` | Scroll horizontally all the way to the start / to the end of the longest line |
| `w` | Wrap long lines, or scroll them horizontally again |
| `Enter` | Close the diff (and the picker it was opened from) and open the file at the current hunk |
| `q`, `Esc` | Close |

The footer shows the current hunk (e.g. `hunk 2/5`), and its line numbers are bold. New files
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
- Stage or revert a hunk from the diff.

## Git commit log

A picker of the commits reachable from HEAD, newest first, with their short id, date, author and
summary. Choosing a commit lists the files it changed, and choosing a file opens the
[side by side diff](#side-by-side-git-diff) of that file in the commit: its version in the
commit's parent on the left, in the commit on the right.

### Usage

| Keys | Action |
| --- | --- |
| `space l`, `:git-log` | Open the commit picker. Typing filters by summary; `%commit`, `%author`, `%date` filter by the other columns. |
| `Enter` in the commit picker | List the files changed by the commit, on top of the commit picker. The status line shows the commit. |
| `Enter`, `C-g` in the files picker | Open the diff of the file, on top of the files picker. |
| `C-s` / `C-v` in the files picker | Open the file as it is now in a split, closing both pickers. |
| `q`, `Esc` in the diff | Back to the files picker. |
| `Esc` in the files picker | Back to the commit picker. |
| `Enter` in the diff | Open the file as it is now, at the current hunk, closing both pickers. |

The diff keys are the same as in the [side by side diff](#side-by-side-git-diff). Renames are
detected, so a renamed file is compared with its old path.

### Limitations

- Merge commits are compared with their first parent only.
- `Enter` in the diff opens the file as it is now in the working tree, so the line it jumps to may
  have moved since the commit. Files deleted since then can't be opened.
- Only the history of HEAD is listed, sorted by commit time; there's no graph.
- In a shallow clone, the oldest commit can't be shown: its parent isn't in the repository.

### Ideas

- List the commits that changed the current file.
- Show the commit message and stats in a preview.
- Pick another branch or a range.

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

Helix pickers send the letters typed to their prompt, but the tree needs them as keys, so it has two
modes. In tree mode (the one it opens in) the keys move in the tree and act on its entries; `/`
enters filter mode, where the keys go to the prompt.

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
| `/` | Filter mode: see below. |
| `a` | Create a file in the selected directory (or next to the selected file). Missing parent directories are created, and a name ending in `/` creates a directory. |
| `r` | Rename. The new name may contain `/` to move the entry to a subdirectory. Changing only the case of a name works on case-insensitive filesystems too. |
| `x`, then `p` | Cut the selected entry, then move it into the selected directory (or next to the selected file). |
| `d`, then `y` | Delete the selected file or directory (directories are deleted recursively). Any other key cancels. |
| `R` | Read the whole tree from disk again. |
| `H` | Show/hide git ignored files for this session. |
| `?` | Show the keys in the status line. |
| `Esc` | Clear the filter if there is one, otherwise close the tree. |
| `q`, `C-c` | Close the tree. |

Filter mode, entered with `/`, is the prompt of the file picker: type to filter, edit with the usual
prompt keys, and move with `C-n`/`C-p`/`Up`/`Down`/`Tab`/`S-Tab`. The query is matched fuzzily
against the paths relative to the workspace root, with the same smart case and path scoring as the
file picker, over the whole workspace and not only over the expanded directories. The tree then
shows the matching files and directories with all the directories above them, expanded, in tree
order, with the matched characters of the names highlighted and the cursor on the best match.
`Enter` expands/collapses a directory or opens a file as in tree mode. `Esc` goes back to tree mode
and keeps the filter, so that the keys above work on the filtered tree (collapsing a directory hides
its matches); `/` resumes editing the query; `Esc` in tree mode clears it. With the filter cleared,
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
  `cargo bench -p helix-term --features bench --bench explorer`), so only a directory with tens of
  thousands of entries, or a slow network filesystem, pauses the editor noticeably. Filtering
  matches every path of the workspace on each key typed, which the same benchmark measures too.
- Copying files isn't supported, only moving.
- The mouse does nothing in the tree, like in other pickers.

### Ideas

- Copy and paste (`c`/`p`), and a multi-selection for batch moves and deletions.

## Inline git blame

Greyed-out text at the end of a line about the commit that last changed it, GitLens style:

```
  5  line 5 changed by bob    Bob Smith, 3 days ago • Change line five
 27  brand new line    Not committed yet
```

Off by default. Full reference: the
[`[editor.inline-blame]` section](book/src/editor.md#editorinline-blame-section) of the book.

### Usage

| Keys | Action |
| --- | --- |
| `space B`, `:toggle-inline-blame` | Turn inline blame on or off |
| `blame_line` (command palette, unbound) | Show the current line's blame in the statusline, also when inline blame is off |

### Configuration

```toml
[editor.inline-blame]
enable = true              # default false
show = "cursor-line"       # or "all-lines" (every visible line)
format = "{author}, {time-ago} • {title}"   # also {date} {commit} {email} {body}
delay = 300                # ms the cursor rests on a line before it shows; 0 = at once
```

Theme scope `ui.virtual.inline-blame`; themes that don't define it use `ui.virtual.inlay-hint`,
so it looks like inlay hints everywhere.

### Behaviour

- Shown in normal and select mode only, hidden in insert mode. `cursor-line` shows only in the
  focused view, so it disappears while the file tree has focus; the diff viewer never shows it.
- New and edited lines, unsaved ones included, show "Not committed yet", and so does every line of
  a file that is in a repository but not committed. Files outside a repository, scratch buffers
  and empty lines show nothing.
- The blame of a file is computed in the background (at most one computation per file and two
  at once) and cached per document. It's refreshed on save, `:reload`, terminal focus and when
  turned on; a refresh only reads `HEAD` (~0.3 ms) unless a commit was made, and then it also
  updates the diff base so the git gutter follows external commits.
- Cost (release build): `helix-term/src/commands.rs`, 7,273 lines touched by 1,141 commits, takes
  1.5 s; small files 3-160 ms. It depends on the file's history, not its size. Showing a line
  is a lookup (~0.4 µs). `cargo bench -p helix-vcs --features git --bench blame`.

### Limitations

- In a debug build blame takes ~6x longer than release (commands.rs: ~9 s; the crates it spends
  its time in are optimized even in debug builds via `[profile.dev.package]`, otherwise it would
  be ~2 minutes). The editor stays responsive meanwhile.
- After a commit made outside the editor, blame updates on the next save, `:reload` or terminal
  focus, not by itself.
- Blame is for the committed file at `HEAD`; staged-but-uncommitted lines show "Not committed yet".

