# The `:practice` curriculum

`:practice` teaches Helix from the first keystroke to the advanced features, one chapter at a
time. Every chapter builds on the ones before it: an exercise may only use the keys of its own
chapter and of earlier chapters, so you never meet a key the course hasn't taught yet. The
chapters and their keys live in `index.toml`. This file explains them, and the replay test
checks that both agree.

Each chapter runs in the same order:

1. **learn**: the new keys on their own, in text where they clearly pay off.
2. **apply**: the same keys on realistic text, like code, config files, logs and prose.
3. **combine**: the new keys together with earlier chapters.
4. **challenge**: one or two exercises at the end, where the shortest solution takes some
   thought.

Every exercise is built so that the taught feature wins. Its `naive` solution (what someone
would type without the feature) takes at least twice as many keys as par, and so does retyping
the whole goal.

The key names below are explained in [Key names](#key-names) at the end.

## 1. Modes and moving

Keys: `h` `j` `k` `l` `i` `a` `I` `A` `o` `O` `<esc>`

Helix is modal. In normal mode, keys are commands. In insert mode, keys type text. `<esc>` goes
back to normal mode. You move with `h` `j` `k` `l`. You start typing with `i` (before the
cursor), `a` (after it), `I` and `A` (at the start and end of the line), `o` and `O` (on a new
line below or above). The chapter is about getting to the right place to type in as few keys as
possible.

Assumes: nothing.

## 2. Lines and characters

Keys: `d` `x` `~` `J` `<gt>` `<lt>` `<C-c>` `insert:<C-w>`

Helix selects first and acts second. So far the selection was just the cursor, one character, and
`d` deletes it. `x` selects the whole line (press it again to add the next one). On an empty line
`x` selects just that line, so a selection can start on the blank line above a block (upstream
Helix also takes the next line there; this fork doesn't). The commands of this chapter act on
whatever is selected:

- `xd` deletes lines;
- `~` switches the case;
- `J` joins lines;
- `>` and `<` indent and unindent;
- `<C-c>` comments lines out or back in.

In insert mode, `<C-w>` deletes the word before the cursor.

Assumes: chapter 1. This chapter comes before motions on purpose. In Helix a motion selects text,
and the motions chapter needs commands that act on a selection.

## 3. Words, lines and counts

Keys: `w` `b` `e` `W` `B` `E` `f` `t` `F` `T` `<A-.>` `c` `r` `gh` `gl` `gs` `v` `;` `count` `<C-a>` `<C-x>`

Bigger selections, and what to do with them:

- words: `w` `b` `e`, and `W` `B` `E` for WORDs (everything between spaces, like a URL);
- a character in the line: `f` `t` `F` `T`, repeated with `<A-.>`;
- `c` changes the selection (deletes it and starts insert mode), and `r` replaces every selected
  character with one character, so `er*` blanks out a word;
- the line: `gh` `gl` `gs` go to its start, its end and its first non-blank;
- `v` starts select mode, where motions extend the selection instead of replacing it, and `;`
  collapses the selection back to the cursor;
- counts: `3w` or `5j` instead of pressing a key several times. `<C-a>` and `<C-x>` add to and
  subtract from a number, so `5<C-a>` adds 5.

Assumes: chapters 1-2.

## 4. Yank, paste and move

Keys: `y` `p` `P` `R` `"` `<A-d>` `<A-c>` `insert:<C-r>` `M`

`y` yanks (copies) the selection. `p` and `P` paste it after or before. `R` replaces the
selection with what was yanked. `d` and `c` yank what they delete; `<A-d>` and `<A-c>` don't, so
your yank survives. `"a` before a command uses register `a` instead of the default one, so you
can keep several texts at once. `<C-r>` followed by a register name pastes in insert mode. The
system clipboard works the same way with `<space>y` and `<space>p`. The lessons mention it, but
no exercise touches your clipboard.

To move whole lines a short way, this fork has a move mode: `M` starts it, `j` and `k` carry the
selected lines down and up (with a count, further), and `<esc>` leaves it. Unlike `d` and `P`, it
leaves the registers alone.

Assumes: chapters 1-3.

## 5. Undo, redo and repeat

Keys: `u` `U` `.`

`u` undoes and `U` redoes. Some exercises start with an undo history (`before`), so undoing is
the fastest way back. `.` repeats the last insert, including the command that started it
(`A;<esc>` once, then `.` everywhere else). This is the first chapter where doing a change
once and repeating it beats doing it by hand.

Assumes: chapters 1-4.

## 6. Search and jump

Keys: `/` `?` `n` `N` `*` `<C-o>` `gg` `ge`

How to get somewhere without counting lines or columns:

- searching: `/` forward and `?` backward, with a regex; `n` and `N` for the next and previous
  match; `*` to search for the selection as a whole word. A match is a selection, so `c`
  changes it, and `n.` repeats that on the next one;
- coming back: a search is a jump, and `<C-o>` returns to where you were before it;
- the ends of the file: `gg` and `ge`.

The lessons also mention the related keys that no exercise needs: `12G` for line 12, `<C-s>`
to save a position in the jumplist yourself, `<C-i>` to go forward again, and `g.` for the
last change.

Assumes: chapters 1-5.

## 7. Many selections

Keys: `%` `s` `S` `C` `,` `<A-s>` `K` `<A-K>` `&`

Helix's multiple selections. One edit applies to every selection at once:

- `%` selects everything;
- `s` selects the matches of a regex inside the selection, and `S` splits the selection on a
  regex;
- `<A-s>` splits it into lines;
- `C` copies the cursor to the next line;
- `K` and `<A-K>` keep or drop the selections that match a regex;
- `,` keeps only the primary selection;
- `&` aligns the selections in columns.

From here on, many earlier exercises have a much shorter answer.

Assumes: chapters 1-6 (regexes as in chapter 6).

## 8. Text objects

Keys: `mi` `ma` `mm`

`mi` selects inside a text object and `ma` selects around it: a word (`w`), a paragraph
(`p`), quotes (`"` `'`), brackets (`(` `[` `{` `<`), or the nearest pair of any of these (`m`).
Text objects work wherever the cursor is inside the object, so they don't need precise
motions. `mm` jumps to the matching bracket.

Assumes: chapters 1-7.

## 9. The syntax tree

Keys: `<A-o>` `<A-n>` `<A-p>` `<A-a>` `]f`

Helix parses code with tree-sitter, and these keys select along the syntax tree:

- `<A-o>` expands the selection to the enclosing node (and `<A-i>` shrinks it back);
- `<A-n>` and `<A-p>` go to the next and previous sibling node;
- `<A-a>` selects all the siblings;
- `]f` goes to the next function.

The text objects of chapter 8 understand syntax too: `maf` selects a function, `mia` an argument
and `mac` a comment.

Assumes: chapters 1-8.

## 10. Surround

Keys: `ms` `mr` `md`

`ms` adds a pair of characters around each selection. `mr` replaces the nearest pair with
another one, and `md` deletes it. Use quotes, brackets or any other character.

Assumes: chapters 1-9 (text objects make the selections to surround).

## 11. Macros

Keys: `Q` `q`

`Q` starts recording a macro and stops it. `q` replays it, and `3q` replays it three times. A
macro is any edit you can describe once and repeat on text that isn't regular enough for
multiple selections.

Assumes: chapters 1-10.

## 12. Shell and commands

Keys: `|` `:sort` `:reflow`

`|` pipes every selection through a shell command and replaces it with the output: `sort`,
`uniq`, `tr`, `rev`, `cut`, `nl`. `:sort` sorts the selections without a shell, and `:reflow`
rewraps text to a width.

Assumes: chapters 1-11.

## Key names

The replay test records the keys of every solution as commands, so it can check them against
this list:

- Keys typed in a prompt (`:`, `/`, `s`, `|`, ...) are text. The exception is `<ret>` on the
  `:` command line, which records `:` and the command's name, such as `:sort`.
- The character after `f`, `t`, `r`, `"`, `mi`, `ms`, or `<C-r>` in insert mode is an argument,
  not a key of its own.
- In insert mode, typing and deleting characters is text. `<esc>` is `<esc>`, and other keys are
  `insert:<key>`, such as `insert:<C-w>`.
- In normal and select mode, a count is `count`, and a key sequence is its keys joined: `gg`,
  `mi`, `<A-o>`.

## Writing exercises

### An exercise

- **One clear point:** the feature in the `feature` line.
- **Realistic text** in varied forms, such as Rust, Python, JavaScript, Go, SQL, shell, TOML,
  YAML, JSON, Markdown, logs, CSV or prose. Not lorem ipsum, and not a one-line toy.
- **The feature has to win.** The `naive` solution is the brute force a learner falls back on
  without the feature, using only keys of earlier chapters. Mostly that is moving with `h` `j`
  `k` `l`, deleting with `d`, and typing; for a feature that replaces a sequence of keys, it is
  that sequence repeated (a macro's keys typed out for every record). It takes at least twice
  as many keys as par, and so does retyping the goal. Get there with repetition, several places
  to edit, or more text where the feature pays off.
- **Mind auto-pairs and auto-indent.** Typing `(`, `[`, `{` or a quote can insert the closing
  one too, and a new line is indented like the one before. Replay every solution; don't
  reason about it.
- **`teach`** uses the feature honestly; it is not a golf trick. A shorter `gold` is shown once
  the learner is at or under par, so it should teach something too.
- **Size:** `start` and `goal` have at most 16 lines of 50 characters. The lesson has at most 6
  lines of 80 characters.

### The lesson

Two to four short sentences. Say why the feature exists before how it works. Name the keys.
Don't restate the exercise, since the goal is shown next to it.

### The hints

Each hint is specific to *this* exercise:

1. **The idea.** Point at what to look at in the text: "All five lines end the same way. Can you
   reach the end without crossing the line?" Don't name keys.
2. **The feature.** Name it and say what it does here: "`A` starts insert mode at the end of the
   line, wherever the cursor is in it."
3. **The exact keys,** step by step and explained: "`A` then `;` then `<esc>`, then `j` to go
   down a line. Repeat for each line."

Write plain, friendly sentences. Avoid generic advice like "try using selections". Proofread every
hint, and check that the keys in hint 3 match `teach` (or another solution at par).
