//! The lists of changed files of the commit log: the files of commits, and the uncommitted
//! changes, which update while the working tree changes. Lockfiles are collapsed to one quiet
//! row with their line counts, after the other files; Enter still opens their diff.

use std::path::{Path, PathBuf};

use vortex_vcs::FileChange;
use vortex_view::{editor::ChangesConfig, graphics::Style, Editor};
use imara_diff::{Algorithm, Diff, InternedInput};
use tui::text::Span;

use super::FileChangeData;
use crate::compositor::{Compositor, Context};
use crate::job::Callback;
use crate::ui::{overlay::Overlay, picker, Picker, PickerColumn};
use crate::{ctrl, key};

/// A row of a list of changed files.
pub(crate) struct FileRow {
    pub(crate) change: FileChange,
    /// The lines a lockfile adds and removes; `None` for the other files.
    lockfile: Option<LockfileLines>,
}

/// The files of a list for `n`/`N` in their diffs: each with whether they skip it (lockfiles).
pub(crate) type NavFiles = Vec<(FileChange, bool)>;

/// The files of `rows` for `n`/`N`.
pub(crate) fn nav_files(rows: &[FileRow]) -> NavFiles {
    rows.iter()
        .map(|row| (row.change.clone(), row.lockfile.is_some()))
        .collect()
}

/// Puts the cursor of `picker` on the row of `path`, if it is matched.
pub(crate) fn select_path(picker: &mut Picker<FileRow, FileRowData>, path: &Path) {
    // the matched rows are ready (unless there are many)
    let mut i = 0;
    while let Some(row) = picker.matched_item(i) {
        if row.change.path() == path {
            picker.set_cursor_index(i);
            return;
        }
        i += 1;
    }
}

/// Puts the cursor of the topmost list of files of a commit or range on `change`.
pub(crate) fn select_in_commit_files(compositor: &mut Compositor, change: &FileChange) {
    if let Some(overlay) = compositor.find::<Overlay<Picker<FileRow, FileRowData>>>() {
        select_path(&mut overlay.content, change.path());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockfileLines {
    Counts { added: u32, removed: u32 },
    Binary,
}

pub(crate) struct FileRowData {
    files: FileChangeData,
    /// The style of the rows of lockfiles.
    quiet: Style,
}

/// The rows for `changes`, sorted by path with the lockfiles last. `contents` gives the two
/// versions of a lockfile (empty for a missing side), for its line counts.
pub(crate) fn rows(
    changes: Vec<FileChange>,
    config: &ChangesConfig,
    contents: impl Fn(&FileChange) -> Option<(Vec<u8>, Vec<u8>)>,
) -> Vec<FileRow> {
    let mut rows: Vec<_> = changes
        .into_iter()
        .map(|change| {
            let lockfile = config.is_lockfile(change.path()).then(|| {
                contents(&change).map_or(LockfileLines::Binary, |(before, after)| {
                    line_counts(&before, &after)
                })
            });
            FileRow { change, lockfile }
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.lockfile.is_some(), a.change.path()).cmp(&(b.lockfile.is_some(), b.change.path()))
    });
    rows
}

/// The lines added and removed between `before` and `after`, as the diff view counts them.
pub(crate) fn line_counts(before: &[u8], after: &[u8]) -> LockfileLines {
    // `bun.lockb`, or a lockfile that git stores as binary
    if before.contains(&0) || after.contains(&0) {
        return LockfileLines::Binary;
    }
    let (before, after) = (
        String::from_utf8_lossy(before),
        String::from_utf8_lossy(after),
    );
    let input = InternedInput::new(before.as_ref(), after.as_ref());
    let diff = Diff::compute(Algorithm::Histogram, &input);
    LockfileLines::Counts {
        added: diff.count_additions(),
        removed: diff.count_removals(),
    }
}

/// A picker of changed files, newest change of each file. Enter and `C-g` run `show_diff` on top
/// of it; `C-s`/`C-v`... open the file as it is now, closing the pickers of the commit log.
pub(crate) fn files_picker(
    editor: &Editor,
    cwd: PathBuf,
    rows: Vec<FileRow>,
    show_diff: impl Fn(&mut Context, &FileChange) + Clone + 'static,
) -> Picker<FileRow, FileRowData> {
    let columns = [
        PickerColumn::new("change", |row: &FileRow, data: &FileRowData| {
            let (label, style) = data.files.label(&row.change);
            let style = if row.lockfile.is_some() {
                data.quiet
            } else {
                style
            };
            Span::styled(label, style).into()
        }),
        PickerColumn::new("path", |row: &FileRow, data: &FileRowData| {
            let path = data.files.path(&row.change);
            match row.lockfile {
                None => path.into(),
                Some(LockfileLines::Counts { added, removed }) => {
                    Span::styled(format!("{path} (+{added} -{removed})"), data.quiet).into()
                }
                Some(LockfileLines::Binary) => {
                    Span::styled(format!("{path} (binary)"), data.quiet).into()
                }
            }
        }),
    ];
    let data = FileRowData {
        files: FileChangeData::new(editor, cwd),
        quiet: editor
            .theme
            .try_get("ui.text.inactive")
            .unwrap_or_else(|| editor.theme.get("comment")),
    };
    let diff = move |cx: &mut Context, row: &FileRow| show_diff(cx, &row.change);
    Picker::new(columns, 1, rows, data, |cx, row: &FileRow, action| {
        // the file as it is now, closing the commit log below too
        let path = row.change.path().to_path_buf();
        if let Err(err) = cx.editor.open(&path, action) {
            cx.editor
                .set_error(format!("unable to open \"{}\": {err}", path.display()));
        }
        cx.jobs.callback(async move {
            let call = |_: &mut Editor, compositor: &mut Compositor| {
                compositor.remove(picker::ID);
            };
            Ok(Callback::EditorCompositor(Box::new(call)))
        });
    })
    .with_stacked_key_handler(key!(Enter), diff.clone())
    .with_stacked_key_handler(ctrl!('g'), diff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn change(path: &str) -> FileChange {
        FileChange::Modified {
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn lockfiles_are_last_with_their_line_counts() {
        let config = ChangesConfig::default();
        let changes = vec![
            change("/r/Cargo.lock"),
            change("/r/src/main.rs"),
            change("/r/web/package-lock.json"),
            change("/r/README.md"),
        ];
        let rows = rows(changes, &config, |change| {
            match change.path().file_name()?.to_str()? {
                "Cargo.lock" => Some((b"a\nb\nc\n".to_vec(), b"a\nB\nc\nd\n".to_vec())),
                _ => Some((Vec::new(), b"x\n".to_vec())),
            }
        });
        let paths: Vec<_> = rows.iter().map(|row| row.change.path()).collect();
        assert_eq!(
            paths,
            [
                Path::new("/r/README.md"),
                Path::new("/r/src/main.rs"),
                Path::new("/r/Cargo.lock"),
                Path::new("/r/web/package-lock.json"),
            ]
        );
        assert_eq!(rows[0].lockfile, None);
        assert_eq!(
            rows[2].lockfile,
            Some(LockfileLines::Counts {
                added: 2,
                removed: 1
            })
        );
        assert_eq!(
            rows[3].lockfile,
            Some(LockfileLines::Counts {
                added: 1,
                removed: 0
            })
        );
    }

    #[test]
    fn binary_lockfiles() {
        assert_eq!(line_counts(b"\0\x01", b"\0\x02"), LockfileLines::Binary);
        let config = ChangesConfig::default();
        let rows = rows(vec![change("/r/bun.lockb")], &config, |_| None);
        assert_eq!(rows[0].lockfile, Some(LockfileLines::Binary));
    }

    #[test]
    fn configured_lockfiles() {
        let config = ChangesConfig {
            lockfiles: vec!["deps.lock".into()],
        };
        let rows = rows(
            vec![change("/r/Cargo.lock"), change("/r/deps.lock")],
            &config,
            |_| Some((Vec::new(), Vec::new())),
        );
        assert_eq!(rows[0].change.path(), Path::new("/r/Cargo.lock"));
        assert_eq!(rows[0].lockfile, None);
        assert!(rows[1].lockfile.is_some());
    }
}
