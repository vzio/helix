//! Every command of the default keymap is taught somewhere: practised in a `:practice` chapter
//! (its key is in the `keys` of a chapter of `index.toml`), or described in `:tutor` (its key is
//! in one of the tutor's key lists), or named in [`NOT_TAUGHT`] with the reason.
//!
//! A command counts as taught when any of its keys is, in normal or in insert mode: `h` teaches
//! `move_char_left`, and `<left>` needs no lesson of its own. Select mode isn't checked: it has
//! the keys of normal mode, and `v` is taught. Typed commands (`:sort`) aren't either: `:`
//! completes and documents them.

use std::collections::{BTreeMap, BTreeSet};

use vortex_view::{
    document::Mode,
    input::KeyEvent,
    keyboard::{KeyCode, KeyModifiers},
};

use crate::keymap::KeyTrie;

use super::session::key_name;

/// Commands that are bound but taught nowhere, each with the reason.
pub const NOT_TAUGHT: &[(&str, &str)] = &[];

/// The key lists of the tutor, for normal mode and for insert mode. A key list is a line that
/// starts with two spaces and then a key, like `  C-w v  C-w s   split vertically, horizontally`;
/// its words are compared with the keys. The lists after a line that ends in `insert mode:` are
/// for insert mode, up to the end of the section (a line of `=`).
fn tutor_key_lines(tutor: &str) -> [Vec<Vec<&str>>; 2] {
    let (mut normal, mut insert) = (Vec::new(), Vec::new());
    let mut in_insert = false;
    for line in tutor.lines() {
        if line.starts_with('=') {
            in_insert = false;
        } else if line.trim_end().to_lowercase().ends_with("insert mode:") {
            in_insert = true;
        } else if line.starts_with("  ") && !line[2..].starts_with(' ') {
            let words = line.split_whitespace().collect();
            if in_insert {
                insert.push(words);
            } else {
                normal.push(words);
            }
        }
    }
    [normal, insert]
}

/// How the tutor writes a key: `A-;`, `C-w`, `A--`, `space`, `Enter`, `Esc`, `S-Tab`, `Home`, or
/// the character itself. Names longer than a character compare without case.
fn tutor_name(key: &KeyEvent) -> String {
    match key.code {
        KeyCode::Char(' ') if key.modifiers.is_empty() => "space".to_owned(),
        KeyCode::Char(c) if (key.modifiers - KeyModifiers::SHIFT).is_empty() => c.to_string(),
        KeyCode::Enter if key.modifiers.is_empty() => "enter".to_owned(),
        _ => key.to_string().replace("minus", "-"),
    }
}

/// Whether a word of a key list is the key `name`, also with a count before it (`12g|`), or in
/// the punctuation of a sentence (`(space`, `r:`, `f,`).
fn same_key(word: &str, name: &str) -> bool {
    let same = |word: &str| {
        if name.chars().count() == 1 {
            word == name
        } else {
            word.eq_ignore_ascii_case(name)
        }
    };
    let bare = word
        .trim_start_matches(|c: char| c == '(' || c.is_ascii_digit())
        .trim_end_matches([':', ',', ')']);
    same(word) || (!bare.is_empty() && same(bare))
}

/// Whether one of the tutor's key lists has `keys`: written together (`gn`, `]p`, `]space`), or
/// as separate words in order on one line (`C-w v`, `space f`, `C-w h j k l`).
fn in_tutor(keys: &[KeyEvent], lines: &[Vec<&str>]) -> bool {
    let names: Vec<String> = keys.iter().map(tutor_name).collect();
    let together = names.concat();
    lines.iter().any(|words| {
        if words.iter().any(|word| same_key(word, &together)) {
            return true;
        }
        let mut words = words.iter();
        names
            .iter()
            .all(|name| words.any(|word| same_key(word, name)))
    })
}

/// A key sequence that runs a command, and the names `:practice` records for it: one name for
/// the whole sequence (`gg`, `<space>c`), but in a sticky mode one for the key that enters it and
/// one for each key after (`M`, `j`).
struct Sequence {
    keys: Vec<KeyEvent>,
    recorded: Vec<String>,
}

/// The bound commands of a mode, each with the key sequences that run it.
fn commands(trie: &KeyTrie) -> BTreeMap<String, Vec<Sequence>> {
    fn walk(
        trie: &KeyTrie,
        keys: &mut Vec<KeyEvent>,
        sticky_at: Option<usize>,
        out: &mut BTreeMap<String, Vec<Sequence>>,
    ) {
        match trie {
            KeyTrie::MappableCommand(command) => {
                let name = command.name();
                if name == "no_op" {
                    return;
                }
                let joined = |keys: &[KeyEvent]| keys.iter().map(key_name).collect::<String>();
                let recorded = match sticky_at {
                    Some(at) => vec![joined(&keys[..at]), joined(&keys[at..])],
                    None => vec![joined(keys)],
                };
                out.entry(name.to_owned()).or_default().push(Sequence {
                    keys: keys.clone(),
                    recorded,
                });
            }
            KeyTrie::Node(node) => {
                let sticky_at = sticky_at.or(node.is_sticky.then_some(keys.len()));
                for (key, trie) in node.iter() {
                    keys.push(*key);
                    walk(trie, keys, sticky_at, out);
                    keys.pop();
                }
            }
            KeyTrie::Sequence(_) => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(trie, &mut Vec::new(), None, &mut out);
    out
}

/// The commands of normal and insert mode that are neither practised, nor in the tutor, nor in
/// [`NOT_TAUGHT`], as `command: keys`.
pub fn untaught(
    keymap: &std::collections::HashMap<Mode, KeyTrie>,
    practised: &BTreeSet<String>,
    tutor: &str,
) -> Vec<String> {
    let [normal_lines, insert_lines] = tutor_key_lines(tutor);
    // how `index.toml` prefixes the keys of the mode, and the tutor's key lists for it
    let modes = [("", normal_lines), ("insert:", insert_lines)];
    // every command, with its key sequences in both modes (by their index in `modes`)
    let mut all: BTreeMap<String, Vec<(usize, Sequence)>> = BTreeMap::new();
    for (index, mode) in [Mode::Normal, Mode::Insert].into_iter().enumerate() {
        for (command, sequences) in commands(&keymap[&mode]) {
            all.entry(command)
                .or_default()
                .extend(sequences.into_iter().map(|sequence| (index, sequence)));
        }
    }
    let mut untaught = Vec::new();
    for (command, sequences) in all {
        if NOT_TAUGHT.iter().any(|(name, _)| *name == command) {
            continue;
        }
        let taught = sequences.iter().any(|(index, sequence)| {
            let (prefix, lines) = &modes[*index];
            let practised = sequence
                .recorded
                .iter()
                .all(|name| practised.contains(&format!("{prefix}{name}")));
            practised || in_tutor(&sequence.keys, lines)
        });
        if !taught {
            let keys: Vec<String> = sequences
                .iter()
                .map(|(index, sequence)| {
                    let keys: String = sequence.keys.iter().map(key_name).collect();
                    format!("{}{keys}", modes[*index].0)
                })
                .collect();
            untaught.push(format!("{command}: {}", keys.join(" ")));
        }
    }
    untaught
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn keys(keys: &str) -> Vec<KeyEvent> {
        vortex_view::input::parse_macro(keys).unwrap()
    }

    #[test]
    fn keys_are_found_in_key_lists_only_and_by_mode() {
        let tutor = "\
 Prose names space e, which doesn't count.

  C-w h j k l    go to the split on the left, below, ...
  ]space [space  add an empty line below, above
  12g|           column 12
  space G s t    switch thread; space G s f, stack frame
  A--  A-;       merge, flip

 In insert mode:

  C-u            delete to the start of the line
=====
  gn             next buffer
";
        let [normal, insert] = tutor_key_lines(tutor);
        for found in [
            "<C-w>j",
            "]<space>",
            "[<space>",
            "g|",
            "<space>Gsf",
            "<A-minus>",
            "<A-;>",
            "gn",
        ] {
            assert!(in_tutor(&keys(found), &normal), "{found}");
        }
        for missing in ["<space>e", "<C-w>x", "<C-u>", "gp", "g"] {
            assert!(!in_tutor(&keys(missing), &normal), "{missing}");
        }
        assert!(in_tutor(&keys("<C-u>"), &insert));
        assert!(!in_tutor(&keys("gn"), &insert));
    }

    #[test]
    fn a_command_is_taught_by_any_of_its_keys_and_sticky_keys_record_apart() {
        let keymap = crate::keymap::default::default();
        let practised: BTreeSet<String> = ["h", "M", "j"].map(String::from).into();
        let untaught = untaught(&keymap, &practised, "");
        let listed = |command: &str| {
            untaught
                .iter()
                .any(|line| line.starts_with(&format!("{command}:")))
        };
        // `h` teaches it, so `<left>` needs nothing
        assert!(!listed("move_char_left"));
        // `M` then `j`
        assert!(!listed("move_lines_down"));
        assert!(listed("move_lines_up"));
        assert!(listed("move_char_right"));
    }

    #[test]
    fn every_command_of_the_default_keymap_is_taught() {
        let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../runtime");
        let chapters = super::super::exercise::load_chapters(&runtime.join("practice")).unwrap();
        let practised: BTreeSet<String> = chapters
            .iter()
            .flat_map(|chapter| chapter.keys.iter().cloned())
            .collect();
        let tutor = std::fs::read_to_string(runtime.join("tutor")).unwrap();
        let untaught = untaught(&crate::keymap::default::default(), &practised, &tutor);
        assert!(
            untaught.is_empty(),
            "{} commands are neither in a chapter of :practice, nor in a key list of :tutor, \
             nor in NOT_TAUGHT:\n{}",
            untaught.len(),
            untaught.join("\n")
        );
    }
}
