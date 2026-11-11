//! Vortex is a fork of Helix. This test keeps upstream's name and binary out of the repository,
//! except where the text is about upstream: it searches every tracked file and path,
//! case-insensitively, for "helix" and for the word "hx", and fails on every hit that the
//! allowlist below doesn't explain. Keep the allowlist short: rename instead of allowing.
use std::path::Path;
use std::process::Command;

use regex::Regex;

/// Files that may name upstream anywhere.
const ALLOWED_FILES: &[(&str, &str)] = &[
    ("README.md", "the owner's own (agents don't edit it)"),
    ("LICENSE", "the license text"),
    (
        "CHANGELOG.md",
        "upstream Helix's changelog, up to the fork point",
    ),
    (
        "vortex-loader/src/upstream.rs",
        "the version Vortex branched from, and the hint for people coming from Helix",
    ),
    ("vortex-term/tests/name_guard.rs", "this test"),
    (
        "tests/query/highlights/haxe/members.hx",
        "a Haxe file: .hx is Haxe's extension",
    ),
];

/// Text that may name upstream wherever it appears (case-sensitive, within a line).
const ALLOWED_TEXT: &[(&str, &str)] = &[
    (
        r"\[[^\]]*\]\([A-Za-z][A-Za-z0-9+.-]*://[^)\s]*\)",
        "markdown links to upstream and to other people's projects: their names and URLs",
    ),
    (
        r"[A-Za-z][A-Za-z0-9+.-]*://[^\s)\]>'`]+",
        "URLs: upstream's site, issues and PRs, other people's repositories",
    ),
    (
        r"\b(helix-editor/helix|marcos-venicius/helix)\b",
        "upstream's repository, and a fork credited for ported features",
    ),
    (
        r"\b[Uu]pstream Helix\b|\bfork of (the )?\[?Helix\b|\bbased on Helix\b|[Rr]ename Helix to Vortex",
        "statements about upstream and about the fork",
    ),
    (
        r"\[Helix[^\]]*\]|\bHelix \[?(PR|issue|Wiki|GitHub|mode|editing)\b",
        "credits and links to upstream: link texts, its PRs, issues, wiki and repository, and \
         other software's Helix modes",
    ),
    (
        r"\bhelix[-_]<crate>",
        "FORK.md's history of the crate rename",
    ),
];

/// Lines of some files that may name upstream: (path prefix, line pattern, why).
const ALLOWED_LINES: &[(&str, &str, &str)] = &[
    (
        "runtime/themes/",
        r"^#.*(\b(for|to) (the )?Helix\b|Helix('s \w+)? (theme|port)\b|team will update Helix|catppuccin/helix)",
        "who made or ported a theme for Helix",
    ),
    (
        "runtime/queries/",
        r"^;.*(\(Helix\)|Helix-specific|\bfor Helix\b)",
        "where a query file came from",
    ),
    (
        "languages.toml",
        r#""hx\|haxe"|\["hx"\]"#,
        "the Haxe language's file extension",
    ),
];

#[test]
fn upstream_name_only_where_allowed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let Ok(output) = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
    else {
        eprintln!("skipped: git is not available");
        return;
    };
    if !output.status.success() {
        eprintln!("skipped: not a git checkout");
        return;
    }

    let name = Regex::new(r"(?i)helix|\bhx\b").unwrap();
    let text: Vec<Regex> = ALLOWED_TEXT
        .iter()
        .map(|(pattern, _)| Regex::new(pattern).unwrap())
        .collect();
    let lines: Vec<(&str, Regex)> = ALLOWED_LINES
        .iter()
        .map(|(prefix, pattern, _)| (*prefix, Regex::new(pattern).unwrap()))
        .collect();

    let mut hits = Vec::new();
    for path in String::from_utf8(output.stdout).unwrap().split('\0') {
        if path.is_empty() || ALLOWED_FILES.iter().any(|(file, _)| *file == path) {
            continue;
        }
        if name.is_match(path) {
            hits.push(format!("{path}: the path itself"));
        }
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            continue; // a symlink to a directory, or deleted in the working tree
        };
        if bytes.contains(&0) {
            continue; // binary
        }
        for (number, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
            if !name.is_match(line) {
                continue;
            }
            if lines
                .iter()
                .any(|(prefix, allowed)| path.starts_with(prefix) && allowed.is_match(line))
            {
                continue;
            }
            let mut rest = line.to_owned();
            for allowed in &text {
                rest = allowed.replace_all(&rest, " ").into_owned();
            }
            if name.is_match(&rest) {
                hits.push(format!("{path}:{}: {}", number + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "upstream's name or binary in {} places; say Vortex and `vx` instead, or, if the text is \
         about upstream (a credit, a link), extend the allowlist in {}:\n{}",
        hits.len(),
        file!(),
        hits.join("\n")
    );
}
