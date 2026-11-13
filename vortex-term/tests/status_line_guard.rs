//! The status line (the message area at the bottom) is for errors and the short results of the
//! commands you run. Keys and help go in the UI they belong to: the mode bar, a picker's or
//! view's `?` help, its prompt row. This test keeps key hints out of the status line: it reads
//! every message written with `set_status`, `set_error`, `set_warning` or `save_messages` in the
//! sources and fails on text that tells which key to press (`n/N go through...`, `press p...`,
//! `:reload to load it`, `s filter, enter cd`): the string literals of the call, and the `const`
//! strings of the file it names. A message built elsewhere and passed in isn't seen: the rule in
//! CLAUDE.md covers those. The allowlist below explains each exception; keep it short: move the
//! hint into the UI instead.
use std::path::Path;
use std::process::Command;

use regex::Regex;

/// Messages that may look like key hints (a substring of the message), with the reason.
const ALLOWED: &[(&str, &str)] = &[
    (
        "(y/n)",
        "a confirmation question: the answer is typed right there, and it says which",
    ),
    (
        "Press <tab> to ",
        "upstream's: the code action menu",
    ),
];

/// What a key hint looks like: a key followed by what it does, a typed command and what it does,
/// "press", or a list of keys and their actions.
const HINTS: &[&str] = &[
    // `press p on the destination`, `Press Enter`
    r"(?i)\bpress(es)?\b",
    // `n/N go through`, `u unstages it`, `Space plays`, `C-s sets one`, `L goes back`
    r"(^|[\s(,:;])([a-zA-Z]|[a-zA-Z]/[a-zA-Z]|[CAS]-\w+|Space|Enter|Esc|Tab|<\w+>)\s+(to|go|goes|shows|starts|stops|opens|plays|pauses|deletes|keeps|unstages|reverts|brings|forgets|sets|toggles|searches|overwrites|cd|filter|extend|help|add|rename|move|del|section|files)\b",
    // `:reload to load it`, `:watch starts one`, `:watch-clear forgets them`
    r":[a-z][\w-]*!?\s+(to|starts|forgets|loads|opens|ends|writes)\b",
    // `(f) stops when`, `(running: Space pauses it)`
    r"\(([a-zA-Z]|Space|Enter)\)\s+\w+s\b",
    r"\((\w+: )?(Space|Enter|Esc)\s",
];

/// The calls that write to the message area.
const WRITERS: &[&str] = &[
    "set_status(",
    "set_error(",
    "set_warning(",
    "save_messages.insert(",
    "set_status_line(",
];

/// The string literals of the call starting at `lines[start]`: up to its closing `);`.
fn call_text(lines: &[&str], start: usize, writer: &str) -> String {
    let first = lines[start];
    let mut text = first[first.find(writer).unwrap()..].to_string();
    let mut i = start;
    while !text.contains(");") && !text.contains(")\n") && i + 1 < lines.len() && i < start + 8 {
        i += 1;
        text.push('\n');
        text.push_str(lines[i]);
    }
    text
}

/// The string literals in `code` (plain `"..."` ones, enough for messages).
fn literals(code: &str) -> Vec<String> {
    let literal = Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap();
    literal
        .captures_iter(code)
        .map(|captures| captures[1].to_string())
        .collect()
}

#[test]
fn no_key_hints_in_the_status_line() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let files = Command::new("git")
        .args(["ls-files", "--", "*.rs"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    assert!(files.status.success());
    let hints: Vec<Regex> = HINTS.iter().map(|hint| Regex::new(hint).unwrap()).collect();
    let mut found = Vec::new();
    let mut messages = 0;
    for file in String::from_utf8(files.stdout).unwrap().lines() {
        // the sources, not the tests (which check what is shown) nor this guard
        if !file.contains("/src/") || file.ends_with("tests.rs") || file.contains("/tests/") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let lines: Vec<&str> = source.lines().collect();
        // the `const NAME: &str = "..."` strings of the file, for `set_status(NAME)`
        let consts: Vec<(String, String)> = Regex::new(
            r#"(?s)const ([A-Z_][A-Z0-9_]*): &str =\s*"((?:[^"\\]|\\.)*)""#,
        )
        .unwrap()
        .captures_iter(&source)
        .map(|captures| (captures[1].to_string(), captures[2].to_string()))
        .collect();
        for (number, line) in lines.iter().enumerate() {
            let Some(writer) = WRITERS.iter().find(|writer| line.contains(*writer)) else {
                continue;
            };
            if line.trim_start().starts_with("//") || line.contains("fn ") {
                continue;
            }
            let call = call_text(&lines, number, writer);
            let named = consts
                .iter()
                .filter(|(name, _)| {
                    Regex::new(&format!(r"\b{name}\b"))
                        .unwrap()
                        .is_match(&call)
                })
                .map(|(_, text)| text.clone());
            for message in literals(&call).into_iter().chain(named) {
                messages += 1;
                if ALLOWED.iter().any(|(allowed, _)| message.contains(allowed)) {
                    continue;
                }
                if hints.iter().any(|hint| hint.is_match(&message)) {
                    found.push(format!("{file}:{}: {message}", number + 1));
                }
            }
        }
    }
    // the guard reads something: if the writers were renamed, it must follow
    assert!(messages > 100, "only {messages} messages found");
    assert!(
        found.is_empty(),
        "key hints in the status line (keys and help go in the UI: the mode bar, `?` help, a \
         picker's prompt row; see CLAUDE.md):\n{}",
        found.join("\n")
    );
}

/// The patterns catch the hints this fork used to print, and leave errors and results alone.
#[test]
fn what_a_key_hint_looks_like() {
    let hints: Vec<Regex> = HINTS.iter().map(|hint| Regex::new(hint).unwrap()).collect();
    let is_hint = |text: &str| hints.iter().any(|hint| hint.is_match(text));
    for hint in [
        "Watch ended: n/N go through its 3 changes, :watch-clear forgets them",
        "No watch session: :watch starts one",
        "s filter, v extend, w/b section, enter files",
        "s filter, enter cd",
        "Cut main.rs, press p on the destination to move it",
        "The hunk is staged: u unstages it, then r reverts it",
        "No bookmarks: C-s sets one on the cursor's line",
        "main.rs changed on disk: :reload to load it, or :write! to overwrite it",
        "Stopped at the first line of your code that ran: Space plays",
        "Marked: fast forward (f) stops when it changes",
        "Spectating thread 2 worker (running: Space pauses it and plays)",
        "REWIND: memory as recorded, L goes back to live",
    ] {
        assert!(is_hint(hint), "not caught: {hint}");
    }
    for fine in [
        "Watch ended: 3 changes",
        "no watch session",
        "The hunk is staged: unstage it first",
        "Cut main.rs",
        "No bookmarks",
        "main.rs changed on disk",
        "unable to open a.rs: not found",
        "'main.rs' written",
        "Spectating thread 2 worker (running)",
        "a picker is open",
        "No changes in src/main.rs",
    ] {
        assert!(!is_hint(fine), "caught: {fine}");
    }
}
