# Working on this fork

This is [vzio/helix](https://github.com/vzio/helix), upstream
[Helix](https://github.com/helix-editor/helix) plus the features in [FORK.md](FORK.md). Read
FORK.md first: it lists every fork feature, where its code lives and which core files it hooks
into.

## The rule

**Every feature, original or ported, ships with at least one `:practice` exercise.**

A PR that adds or ports a feature adds its exercise(s) under `runtime/practice/` in the same PR
(the fork's own features go in a chapter of their own, see below). The replay test must pass with them.

## Writing a `:practice` exercise

`:practice` is the fork's built-in trainer (see FORK.md, "Practice"). Exercises are plain TOML
data in `runtime/practice/`:

```
runtime/practice/index.toml                    # the chapters, in order: [[chapters]] dir, title, keys
runtime/practice/<chapter>/<nn>-<name>.toml    # one exercise
```

- A new chapter goes into `index.toml` (every directory is listed there, nothing else).
- `<nn>` is two digits, from `01` in each chapter, without gaps; `<name>` is kebab-case. The order of
  the files is the order of the exercises. The id is `<chapter>/<nn>-<name>`; the user's progress is
  keyed by it, so don't rename or renumber existing exercises without need.

```toml
title = "Word by word"
chapter = "motions"                     # the directory name
kind = "learn"                          # learn | apply | combine | challenge
feature = "Word motions: w, b, e"       # the feature taught, one line
lesson = """
`w` selects to the start of the next word, `b` to the start of the previous
one, `e` to the end. A motion leaves what it passed over selected, so `d`
right after deletes it.
"""
start = """
The #[q|]#uick quick brown fox
"""
goal = """
The quick brown fox
"""
# language = "rust"                     # optional language id, for tree-sitter features
hints = [
  "One word is there twice.",                                # vague
  "Select a whole word with a word motion, then delete it.", # names the feature
  "`w` selects `quick ` from the cursor, then `d`.",         # names the keys
]
# before = ["older text"]               # optional: texts the buffer had before `start` (undo history)
teach = "wd"                            # solution that uses the taught feature
naive = "dwwwd"                         # what a beginner types without the feature
# gold = "..."                          # optional: shortest known, strictly fewer keys than teach
```

- **Lesson**: which keys and why, at most 6 lines of at most 80 characters. It's read on a phone:
  the panel joins the lines and wraps them to its width, so a line break is a space; a blank line
  (a new paragraph), an indented line or a list item (`-`, `*`, `1.`) starts a new line.
- **title** and **feature**: one line each.
- **start / goal**: realistic text (code, config, prose), lines of at most 50 characters, at most
  16 lines. `start` may mark the selection with Helix's test markup (`#[|x]#` primary, `#(|y)#`
  secondary); without it the cursor is on the first character. Neither may otherwise contain `#[`
  or `#(`. The goal is compared exactly (text only, not selections), so both usually end with a
  newline.
- **Solved** means the text equals `goal` in normal mode, checked after every key: a solution that
  types text ends with `<esc>`, and it must not reach the goal before its last key.
- **Keys** use Helix's macro notation (`<esc>`, `<ret>`, `<space>`, `<C-w>`, `<A-o>`, ...). Every
  key event counts as one keystroke.
- **Par** is the number of keys of `teach`. It is computed, never written in the file (unknown
  fields are an error).
- **Hints**: exactly three, from vague to naming the keys.
- **gold**: only when strictly shorter than `teach` and worth seeing; it's shown once the user is
  at or under par.
- Shell commands (`|`, `!`, ...) only in the `shell` chapter, with `sort`, `tr`, `rev` or `uniq`
  (the replay test skips that chapter on Windows).
- Typing quotes or brackets in a solution adds the closing one too (auto-pairs), so avoid them.
- An exercise doesn't rely on state it doesn't set up itself (registers, the jumplist, the last
  search).
- Use the keys of this fork's keymap (`helix-term/src/keymap/default.rs`), which differs from
  upstream in places (for example `M`).

### Solutions are verified

The replay test (`helix-term/tests/test/practice.rs`) loads every exercise, checks the rules above,
and replays `teach` and `gold` through the integration test harness: each must reach the goal in
normal mode exactly at its last key, in a fresh editor. It also prints the par of each exercise. So exercises can't
rot when the keymap or a command changes: the test fails and names them.

```sh
cargo integration-test -- practice --nocapture
```

## Fork conventions

- **Own modules.** A feature lives in its own modules (see the code layout table in FORK.md). Core
  files (`commands.rs`, `typed.rs`, `keymap/default.rs`, `ui/editor.rs`, `ui/picker.rs`,
  `application.rs`, `helix-view/src/editor.rs`, ...) get the smallest hooks possible, and any
  generic addition to them (a picker option, a keymap macro option) must make sense on its own.
  This keeps rebasing on upstream cheap. List the hooks in FORK.md's code layout.
- **FORK.md.** Every feature gets a section (usage, behaviour, limitations) and a row in the code
  layout table; ported work gets a credit with links to the original PR and its author.
- **PRs go to the fork only:** `vzio/helix`, base `master`. Never open PRs against upstream
  (`helix-editor/helix`) or any other fork. The PR title and description are the squash commit
  message, so write them as one. One feature or fix per PR.
- **Checks** before pushing, all clean:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo check -p helix-term --no-default-features
  cargo test --workspace
  cargo integration-test
  cargo xtask docgen   # after changing commands, keys or options; commit book/src/generated
  ```

  (Four upstream `write` tests fail when run as root; that's not the fork's.)
- New behaviour comes with tests: unit tests next to the code, integration tests in
  `helix-term/tests/test/`, and the `:practice` exercise.
