# Working on this fork

This is [vzio/helix](https://github.com/vzio/helix), upstream
[Helix](https://github.com/helix-editor/helix) plus the features in [FORK.md](FORK.md). Read
FORK.md first: it lists every fork feature, where its code lives and which core files it hooks
into.

## The rule

**Every feature, original or ported, ships with at least one `:practice` exercise.**

A PR that adds or ports a feature adds its exercise(s) under `runtime/practice/` in the same PR
(in the chapter where the feature fits, see below). The replay test must pass with them.

## Writing a `:practice` exercise

`:practice` is the fork's built-in trainer (see FORK.md, "Practice"). Exercises are plain TOML
data in `runtime/practice/`:

```
runtime/practice/index.toml                    # the chapters, in order: [[chapters]] dir, title, keys
runtime/practice/CURRICULUM.md                 # the curriculum and the writing guide; agrees with index.toml
runtime/practice/<chapter>/<nn>-<name>.toml    # one exercise
```

Read **`runtime/practice/CURRICULUM.md` first**: it has the 12 chapters from modes to shell
commands, what each teaches and assumes, its keys, and the guide for lessons and hints that every
exercise follows.

### The curriculum

The chapters run from fundamentals to advanced: modes and moving, lines and characters (`d` `x` `J`
`>`...), words and counts, yank, paste and move, undo and repeat, search and jump, many selections,
text objects, the syntax tree, surround, macros, shell and commands. Each chapter lists in
`index.toml` the keys it introduces, and **an exercise may only use the keys of its chapter and of
the chapters before it** (the replay test records the keys of `teach` and `naive` as commands and
checks them). A feature of this fork goes in the chapter where it fits, with its keys, like move
mode `M` in yank, paste and move; its lesson says it is this fork's.

Inside a chapter the exercises go `learn` (the new keys alone) → `apply` (realistic text) →
`combine` (with earlier chapters) → `challenge` (one or two at the end, where the shortest solution
takes thought). The `kind` field says which; the first is a learn, the last a challenge.

### The format

```toml
title = "The end of the line"           # at most 40 characters
chapter = "basics"                      # the directory name
kind = "learn"                          # learn | apply | combine | challenge
feature = "A appends at the end of the line"
lesson = """
`A` starts insert mode at the end of the line, wherever the cursor is in it.
`I` is its twin for the start of the line (after the indentation). Use them
instead of walking to either end.
"""
start = '''
const path = process.argv[2]
const text = await readFile(path, "utf8")
'''
goal = '''
const path = process.argv[2];
const text = await readFile(path, "utf8");
'''
# language = "rust"                     # optional language id, for tree-sitter features and indentation
# before = ["older text"]               # optional: texts the buffer had before `start` (its undo history)
hints = [
  "Every line is missing the same thing at its very end.",
  "`A` jumps to the end of the line and starts insert mode, from anywhere in the line.",
  "`A`, `;`, `<esc>`, then `j` to the next line; `A;<esc>` again on the last.",
]
teach = "A;<esc>jA;<esc>"               # the solution with the feature: par is its number of keys
naive = "llll...a;<esc>..."             # brute force without the feature, earlier chapters' keys only
# gold = "..."                          # optional: shorter than teach, shown once the user is at par
```

- Use literal strings (`'''`) for code, so backslashes stay as they are.
- **start / goal**: realistic text (code in several languages, config files, logs, prose), at most
  16 lines of at most 50 characters. `start` may mark the selection with Helix's test markup
  (`#[|x]#` primary, `#(|y)#` secondary); without it the cursor is on the first character. The goal
  is compared exactly (text only, not selections), so both usually end with a newline.
- **Solved** means the text equals `goal` in normal mode, checked after every key: a solution that
  types text ends with `<esc>`, and it must not reach the goal before its last key.
- **Keys** use Helix's macro notation (`<esc>`, `<ret>`, `<space>`, `<C-w>`, `<A-o>`); a literal
  `<` or `>` is `<lt>` or `<gt>`. Every key event is one keystroke. Par is computed, never written.
- Shell commands (`|`) only in the `shell` chapter, with POSIX tools (`sort`, `uniq`, `tr`, `cut`,
  `nl`, `rev`); the replay test skips that chapter on Windows.
- An exercise doesn't rely on state it doesn't set up itself (registers, the jumplist, the last
  search); `before` is how it gets an undo history.
- Use this fork's keymap (`helix-term/src/keymap/default.rs`), which differs from upstream in places.

### Quality rules (checked by the replay test)

- **The feature must win.** `naive` (brute force with earlier chapters' keys: moving with
  `hjkl`, deleting with `d`, typing; or the same keys repeated where a feature would repeat them)
  takes at least **2 x par** keys. So does retyping the goal (`%c`, the goal, `<esc>`). Make it so
  with repetition, several edit sites, and text where the feature pays off; not with toy one-liners.
- Every key a chapter lists is used by the `teach` or `gold` of one of its exercises.
- CURRICULUM.md's `Keys:` line of each chapter equals the chapter's keys in `index.toml`.

### Lessons and hints

- **Lesson**: 2-4 short sentences, at most 6 lines of 80 characters. The *why* before the *how*;
  name the keys; don't restate the goal (the panel shows it).
- **Hints**, exactly three, specific to *this* exercise, plain and friendly, proofread:
  1. the idea, as what to look at ("Every line is missing the same thing at its very end.");
  2. the feature by name and what it does here;
  3. the exact keys, step by step, matching `teach` (or another solution at par).
  No generic advice like "try using selections".

### Solutions are verified

The replay test (`helix-term/tests/test/practice.rs`) loads every exercise, checks the rules above,
and replays `teach`, `gold` and `naive` through the integration test harness, each in a fresh
editor: each must reach the goal in normal mode exactly at its last key. It prints a table of par,
naive and retype per exercise. So exercises can't rot when the keymap or a command changes.

```sh
cargo integration-test -- the_solutions_of_all --nocapture       # all of them (about 2 minutes)
PRACTICE_FILTER=search/ cargo integration-test -- the_solutions_of_all --nocapture
PRACTICE_DIR=path/to/drafts cargo integration-test -- the_solutions_of_all --nocapture
```

While writing an exercise, `practice_try` replays keys one at a time and prints the buffer with
its selections and the recorded commands after each key (prompts like `/` need the replay test):

```sh
PRACTICE_TRY=runtime/practice/basics/03-end-of-the-line.toml PRACTICE_KEYS='A;<esc>j' \
  cargo integration-test -- practice_try --ignored --nocapture
```

Auto-pairs and auto-indent apply while replaying: typing `(` or a quote may insert the closing
one, and a new line is indented like the last. Replay rather than reason.

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
