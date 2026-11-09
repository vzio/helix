//! The lists of changed files of the commit log: the files of commits, and the uncommitted
//! changes, which update while the working tree changes. Lockfiles are collapsed to one quiet
//! row with their line counts, after the other files; Enter still opens their diff.

use std::path::PathBuf;
use std::sync::Arc;

use helix_vcs::{DiffProviderRegistry, FileChange};
use helix_view::{editor::ChangesConfig, graphics::Style, Editor};
use imara_diff::{Algorithm, Diff, InternedInput};
use tui::text::Span;

use super::{diff_view, FileChangeData};
use crate::compositor::{self, Component, Compositor, Context, Event, EventResult};
use crate::job::Callback;
use crate::live::{Live, Pending, RepoWatch};
use crate::ui::{overlay::overlaid, picker, Picker, PickerColumn};
use crate::{ctrl, key};

/// A row of a list of changed files.
pub(crate) struct FileRow {
    pub(crate) change: FileChange,
    /// The lines a lockfile adds and removes; `None` for the other files.
    lockfile: Option<LockfileLines>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockfileLines {
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
fn line_counts(before: &[u8], after: &[u8]) -> LockfileLines {
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

/// What the uncommitted changes need from the editor, for listing them off the main thread.
#[derive(Clone)]
struct Source {
    cwd: PathBuf,
    providers: DiffProviderRegistry,
    trust_full: bool,
    config: ChangesConfig,
}

impl Source {
    fn new(editor: &Editor, cwd: PathBuf) -> Self {
        Self {
            trust_full: super::git_log::trust_full(editor, &cwd),
            providers: editor.diff_providers.clone(),
            config: editor.config().changes.clone(),
            cwd,
        }
    }

    /// The changes of the working tree (and of the index) since HEAD, untracked files included.
    fn list(&self) -> anyhow::Result<Vec<FileRow>> {
        let changes = self.providers.changed_files(&self.cwd, self.trust_full)?;
        Ok(rows(changes, &self.config, |change| {
            let (base_path, path) = diff_view::paths_of(change);
            let before = self
                .providers
                .get_diff_base(&base_path, self.trust_full)
                .unwrap_or_default();
            let after = std::fs::read(path).unwrap_or_default();
            Some((before, after))
        }))
    }
}

/// The picker of the uncommitted changes, which follows the working tree while `live` sees it
/// change: the list is read again and the cursor stays on the same file (or at the same place
/// when that file is gone).
pub(crate) struct UncommittedFiles {
    picker: Picker<FileRow, FileRowData>,
    source: Source,
    live: Option<Live>,
    pending: Pending<anyhow::Result<Vec<FileRow>>>,
}

impl UncommittedFiles {
    fn refresh(&mut self) {
        if self.live.as_mut().is_some_and(Live::changed) {
            let source = self.source.clone();
            self.pending.run(move || source.list());
        }
        match self.pending.take() {
            Some(Ok(rows)) => self.replace(rows),
            Some(Err(err)) => log::debug!("uncommitted changes: {err}"),
            None => {}
        }
    }

    fn replace(&mut self, rows: Vec<FileRow>) {
        let selected = self
            .picker
            .selection()
            .map(|row| row.change.path().to_path_buf());
        let index = self.picker.cursor_index();
        self.picker.replace_items(rows, index);
        let Some(selected) = selected else {
            return;
        };
        // the matched rows are ready (unless there are many): find the file that was selected
        let mut i = 0;
        while let Some(row) = self.picker.matched_item(i) {
            if row.change.path() == selected {
                self.picker.set_cursor_index(i);
                return;
            }
            i += 1;
        }
    }
}

impl Component for UncommittedFiles {
    fn render(
        &mut self,
        area: helix_view::graphics::Rect,
        surface: &mut tui::buffer::Buffer,
        cx: &mut Context,
    ) {
        self.refresh();
        self.picker.render(area, surface, cx);
    }

    fn handle_event(&mut self, event: &Event, cx: &mut Context) -> EventResult {
        self.picker.handle_event(event, cx)
    }

    fn cursor(
        &self,
        area: helix_view::graphics::Rect,
        editor: &Editor,
    ) -> (
        Option<helix_core::Position>,
        helix_view::graphics::CursorKind,
    ) {
        self.picker.cursor(area, editor)
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        self.picker.required_size(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        self.picker.id()
    }
}

/// Pushes the picker of the uncommitted changes on top of the commit log. With a `watch`, it and
/// the diffs opened from it update while the working tree changes.
pub(crate) fn open_uncommitted(
    cx: &mut compositor::Context,
    cwd: PathBuf,
    watch: Option<Arc<RepoWatch>>,
) {
    cx.jobs.callback(async move {
        let call = move |editor: &mut Editor, compositor: &mut Compositor| {
            let source = Source::new(editor, cwd.clone());
            let rows = match source.list() {
                Ok(rows) => rows,
                Err(err) => {
                    editor.set_error(format!("{err}"));
                    return;
                }
            };
            editor.set_status(match rows.len() {
                0 => "No uncommitted changes".to_string(),
                1 => "Uncommitted changes: 1 file".to_string(),
                files => format!("Uncommitted changes: {files} files"),
            });
            let diff_watch = watch.clone();
            let show_diff = move |cx: &mut Context, change: &FileChange| {
                let change = change.clone();
                let live = diff_watch.clone().map(Live::new);
                cx.jobs.callback(async move {
                    let call = move |editor: &mut Editor, compositor: &mut Compositor| {
                        diff_view::open_uncommitted(editor, compositor, &change, live)
                    };
                    Ok(Callback::EditorCompositor(Box::new(call)))
                });
            };
            let picker = files_picker(editor, cwd, rows, show_diff);
            let files = UncommittedFiles {
                picker,
                source,
                live: watch.map(Live::new),
                pending: Pending::new(),
            };
            compositor.push(Box::new(overlaid(files)));
        };
        Ok(Callback::EditorCompositor(Box::new(call)))
    });
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
