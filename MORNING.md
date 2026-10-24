# Morning report: inline git blame

Inline git blame is done, tested and merged into **`fork-features`**. It was developed on
**`inline-blame`**, which is also pushed. FORK.md has the user docs ("Inline git blame") and
credits nik-rev's helix PR #13133. The report on the diff viewer and file tree port is in git
history (`git log -p -- MORNING.md`).

## Status

| | State |
| --- | --- |
| Blame at the end of the line (cursor line or all visible lines), inlay-hint style | Works |
| `[editor.inline-blame]`: `enable`, `show`, `format`, `delay` | Works, off by default |
| `space B`, `:toggle-inline-blame`, `:set inline-blame.*` | Works |
| "Not committed yet" on new, edited and unsaved lines | Works |
| Background computation, cache per document, refresh on save/`:reload`/focus | Works |
| Hidden in insert mode, with the file tree focused, in the diff viewer | Works |
| Diff viewer and file tree | Still work (tests and a real run) |
| `cargo test --workspace` | 548 pass |
| `cargo integration-test` | 193 pass, 4 fail: the same root-only upstream `write` tests as last time |
| `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check` | Clean |
| `cargo check -p helix-term --no-default-features` (no git) | Builds |

Nothing is broken or partial that I know of.

## Try it

```toml
# ~/.config/helix/config.toml
[editor.inline-blame]
enable = true             # or leave it off and press space B when you want it
show = "cursor-line"      # "all-lines" = every visible line
format = "{author}, {time-ago} • {title}"   # also {date} {commit} {email} {body}
delay = 300               # ms on a line before it shows, 0 = at once
```

```
  5  line 5 changed by bob    Bob Smith, 3 days ago • Change line five
 27  brand new line    Not committed yet
```

Keys: `space B` toggles it (`space b` is still the buffer picker). `blame_line` in the command
palette (`space ?`) shows the current line's blame in the statusline, even with inline blame off.

Theme: `ui.virtual.inline-blame`, falling back to `ui.virtual.inlay-hint` when a theme doesn't
set it. To restyle it: `"ui.virtual.inline-blame" = { fg = "gray", modifiers = ["italic"] }`.

On Termux: `git pull` on `fork-features`, `cargo build`, then the same `HELIX_RUNTIME` setup as
before. A file with a long history (e.g. `helix-term/src/commands.rs`) takes ~9 s in a debug
build on this machine before its blame appears (a phone will be slower; release builds are ~6x
faster). The editor stays responsive meanwhile.

## How it was built

Upstream master still has no blame feature (checked first), so this is a port of PR #13133,
adapted to current master and to the spec. Three phases, each delegated to a subagent and
reviewed: the git core in helix-vcs, the editor integration, then rendering tests and a real run
in tmux.

## Testing done

- Unit tests:
  - helix-vcs (the PR's tests plus renames, untracked files, outside a repo, a repo without
    commits, a HEAD change);
  - helix-view (line resolution with hunks, the request state machine, config parsing);
  - helix-term (placement, truncation, soft wrap).
- 17 integration tests in `helix-term/tests/test/inline_blame.rs`. Two of them assert on the
  rendered screen of the whole app (TestBackend):
  - text, gap and style; hidden in insert mode;
  - "Not committed yet"; all-lines mode;
  - gone while the tree is focused; absent from `space =`;
  - plus toggles, saves, focus, reload and the delay.
- Real run, a debug build in tmux:
  - **Display:** delay and hide/reshow on line moves, insert mode, unsaved new lines, all-lines
    mode, a non-git file, a scratch buffer, and a 40x12 terminal with soft wrap.
  - **Refreshing:** `:w`, then an external `git commit` followed by focus or `:reload`.
  - **Toggles:** `space B`, `:toggle-inline-blame` and `:set`.
  - **Big file:** blame on `commands.rs` appeared after 8.6 s. Meanwhile, cursor moves took a
    median of 14 ms and at most 38 ms, and the picker and typing in another buffer worked.
  - **Many files:** 8 big files at once all got their blame, two at a time.
  - **Diff viewer and file tree:** `space =`, `space g` + `C-g`, `space l`, `space e`/`space E`
    all still work, with no blame inside them.
  - **Held `j`:** 500 presses took the same time with blame on as off, with no flicker.

Benchmark (`cargo bench -p helix-vcs --features git --bench blame`, release):

| | Time |
| --- | --- |
| Blame of `helix-term/src/commands.rs` (7,273 lines, 1,141 commits touching it) | 1.5 s |
| Blame of small files | 3-160 ms (it depends on history, not size) |
| Checking whether HEAD moved | ~0.3 ms |
| Blame text of a line, when drawing | ~0.4 µs |

## Decisions I made for you

1. **`fork-features` didn't exist on GitHub**, so I created it from `master` (identical to what
   you merged), branched `inline-blame` off it, and merged back with a merge commit when green.
2. **Config follows your spec, not the PR:** `enable`/`show`/`format`/`delay`. The PR's
   `auto-fetch` and `show = "never"` are gone (`enable` covers it). Default format
   `"{author}, {time-ago} • {title}"` (the PR also appended `• {commit}`; add `{commit}` if you
   want it). Default delay 300 ms; it only applies to `cursor-line`.
3. **Theme scope name** `ui.virtual.inline-blame` (the PR's name), with the inlay-hint fallback.
4. **Key**: `space B` (free). The PR bound `blame_line` to `space B`; here `blame_line` is
   unbound and in the command palette.
5. **All the git work happens in the background.** The PR looked up commits from the
   repository while drawing; here the blame is fully resolved up front, so drawing is a lookup.
   At most one computation per file and two in total, so opening many files can't start dozens
   of history walks. Turning it off drops all cached blame, and nothing runs while it's off.
6. **Refresh points**: open, save, `:reload`/`:reload-all`, terminal focus, turning it on.
   Each one first checks whether HEAD moved (~0.3 ms) and only recomputes if it did. There is no
   file watcher, so a commit made in another terminal shows up on the next focus/save/reload.
7. **The diff base follows HEAD.** Helix only read the diff base on open/reload, so after an
   external commit blame and diff disagreed (wrong "Not committed yet" lines). When blame moves
   to a new HEAD it now re-reads the diff base too. Side effect: the git gutter also updates
   after external commits.
8. **Untracked or uncommitted files** in a repo show "Not committed yet" on every non-empty
   line. Outside any repo: nothing.
9. **Empty lines show no blame** (as in the PR). Easy to change if you want it on empty lines.
10. **Hiding in panels:** `cursor-line` only draws in the focused view, which covers the tree
    having focus. With `all-lines` the document next to an open tree keeps its blame (it's
    still visible text, the tree just sits beside it).
11. **Fixed a PR quirk:** when the diff wasn't ready or was being updated, the PR showed "Not
    committed yet". Here it shows nothing for that frame.
12. **Renames are followed** (the PR blamed renamed files on the rename commit), and commit
    titles are trimmed.
13. **Debug builds:** blame was ~120 s for commands.rs unoptimized, so the crates it spends its
    time in (gix-*, zlib-rs, hashing) are optimized even in debug builds (`[profile.dev.package]`
    in `Cargo.toml`, one commit, `82ea1ec79`): now ~9 s. Only those crates; the first debug build
    takes a bit longer, rebuilds of helix itself don't.
14. **Repository trust:** blame opens the repository with reduced trust (it only reads objects).
    The diff base refresh uses the workspace trust setting like the rest of helix.
15. **Tests that need background jobs** each build one app (the job queue is per runtime), with
    a small helper in the test file instead of `test_key_sequences`.
