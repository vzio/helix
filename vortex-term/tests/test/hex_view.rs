//! The hex view: binary files shown as bytes, which can be overwritten in place.
//!
//! The tests drive an `Application` with keys, read its state (`doc.hex()`, the statusline
//! message, the registers) and the test backend's screen, and check the files on disk.

use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use tokio_stream::wrappers::UnboundedReceiverStream;
use vortex_term::application::Application;
use vortex_view::{
    editor::Severity,
    graphics::Color,
    hex::{HexBuffer, Layout, Rows},
    input::parse_macro,
    Document,
};

use termina::event::{Event, KeyEvent};

use super::*;

/// A PNG, 64 bytes: the signature, the `IHDR` chunk at 12, `pHYs`, `IEND`; `de ad` twice, at
/// 50 and 62.
fn png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(b"\0\0\0\rIHDR");
    bytes.extend_from_slice(b"\0\0\x01\0\0\0\x01\0\x08\x06\0\0\0");
    bytes.extend_from_slice(b"\x5c\x72\xa8\x66");
    bytes.extend_from_slice(b"\0\0\0\tpHYs");
    bytes.extend_from_slice(b"\0\0\x0e\xc4\0\0\x0e\xc4\x01");
    bytes.extend_from_slice(b"\xde\xad\xbe\xef");
    bytes.extend_from_slice(b"IEND\xae\x42\x60\x82");
    bytes.extend_from_slice(b"\xde\xad");
    assert_eq!(bytes.len(), 64);
    bytes
}

/// A directory with `name` holding `bytes`.
fn file_with(name: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // paths are compared canonicalized
    let path = std::fs::canonicalize(dir.path()).unwrap().join(name);
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

/// Drives an application with keys and reads its screen.
struct Session {
    app: Application,
    tx: tokio::sync::mpsc::UnboundedSender<std::io::Result<Event>>,
    input: UnboundedReceiverStream<std::io::Result<Event>>,
}

impl Session {
    fn new(app: Application) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            app,
            tx,
            input: UnboundedReceiverStream::new(rx),
        }
    }

    /// An editor with `path` open, in a terminal of `cols` x 30.
    async fn open(path: &Path, cols: u16) -> Self {
        let app = helpers::AppBuilder::new()
            .with_file(path, None)
            .build()
            .unwrap();
        let mut session = Self::new(app);
        session.resize(cols, 30).await;
        session
    }

    /// Gives the screen another size, which the application picks up with the next event
    /// (an `<esc>`, which changes nothing in the hex view).
    async fn resize(&mut self, cols: u16, rows: u16) {
        self.app.resize_screen(cols, rows);
        self.keys("<esc>").await;
    }

    async fn keys(&mut self, keys: &str) {
        for key in parse_macro(keys).unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        assert!(self.app.event_loop_until_idle(&mut self.input).await);
    }

    /// Run until `condition` is true.
    async fn wait_for(&mut self, what: &str, condition: impl Fn(&Application) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition(&self.app) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: status {:?}",
                status(&self.app)
            );
            self.app.editor.reset_idle_timer();
            assert!(self.app.event_loop_until_idle(&mut self.input).await);
        }
    }

    /// Writes with `:w` (or what `command` is) and waits until the document is not modified.
    async fn write(&mut self, command: &str) {
        self.keys(&format!("{command}<ret>")).await;
        self.wait_for("the write", |app| !doc(app).is_modified())
            .await;
    }

    async fn close(mut self) {
        for key in parse_macro("<esc>:qa!<ret>").unwrap() {
            self.tx.send(Ok(Event::Key(KeyEvent::from(key)))).unwrap();
        }
        // (this one ends the application)
        while self.app.event_loop_until_idle(&mut self.input).await {}
        let errors = self.app.close().await;
        assert!(errors.is_empty(), "{errors:?}");
    }
}

fn doc(app: &Application) -> &Document {
    vortex_view::doc!(app.editor)
}

fn hex(app: &Application) -> &HexBuffer {
    doc(app).hex().expect("the document is in the hex view")
}

fn view_id(app: &Application) -> vortex_view::ViewId {
    vortex_view::view!(app.editor).id
}

/// The offset of the cursor of the focused view.
fn offset(app: &Application) -> u64 {
    hex(app).cursor(view_id(app)).offset
}

/// The bytes of the document as the view shows them (overwritten ones included).
fn bytes(app: &Application) -> Vec<u8> {
    hex(app).bytes(0..hex(app).len())
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(msg, _)| msg.to_string())
        .unwrap_or_default()
}

fn severity(app: &Application) -> Option<Severity> {
    app.editor.get_status().map(|(_, severity)| *severity)
}

fn assert_error_containing(app: &Application, text: &str) {
    assert_eq!(severity(app), Some(Severity::Error), "{:?}", status(app));
    assert!(status(app).contains(text), "{:?}", status(app));
}

/// The rows of the screen.
fn screen_rows(app: &Application) -> Vec<String> {
    let screen = app.screen();
    (0..screen.area.height)
        .map(|y| {
            (0..screen.area.width)
                .map(|x| screen.get(x, y).map_or(" ", |cell| cell.symbol.as_str()))
                .collect()
        })
        .collect()
}

/// The row of the (first) statusline: it shows the mode of the view.
fn statusline(app: &Application) -> String {
    screen_rows(app)
        .into_iter()
        .find(|row| row.contains("HEX ") || row.contains("BIN "))
        .expect("a statusline of the hex view")
}

/// The first `count` rows of the screen, without trailing spaces.
fn first_rows(app: &Application, count: usize) -> Vec<String> {
    screen_rows(app)
        .iter()
        .take(count)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

fn fg(app: &Application, x: usize, y: u16) -> Color {
    app.screen()[(x as u16, y)].fg
}

fn theme_of(toml: &str) -> vortex_view::Theme {
    vortex_view::Theme::from(toml::from_str::<toml::Value>(toml).unwrap())
}

/// Prints a snapshot of the screen, for the description of a pull request.
fn print_snapshot(title: &str, dir: &Path, rows: &[String], statusline: &str) {
    eprintln!("--- {title}");
    for row in rows {
        eprintln!("{row}");
    }
    let statusline = statusline.replace(dir.to_str().unwrap(), "<dir>");
    eprintln!("{}", statusline.trim_end());
}

// Opening

/// A binary file opens in the hex view, a text file does not.
#[tokio::test(flavor = "multi_thread")]
async fn binary_files_open_in_the_hex_view() {
    let (_dir, path) = file_with("image.png", &png());
    let session = Session::open(&path, 100).await;
    assert!(doc(&session.app).is_hex());
    assert_eq!(doc(&session.app).text().len_chars(), 0);
    assert_eq!(hex(&session.app).len(), 64);
    assert_eq!(offset(&session.app), 0);
    assert!(!doc(&session.app).is_modified());
    session.close().await;

    // a NUL in the first KiB is enough, and so are the signatures of a PDF
    let (_dir, path) = file_with("data.bin", b"text, then a NUL: \0 and more");
    let session = Session::open(&path, 100).await;
    assert!(doc(&session.app).is_hex());
    session.close().await;
    let (_dir, path) = file_with("doc.pdf", b"%PDF-1.7 no NUL in here");
    let session = Session::open(&path, 100).await;
    assert!(doc(&session.app).is_hex());
    session.close().await;

    let (_dir, path) = file_with("notes.txt", b"just text\nsecond line\n");
    let session = Session::open(&path, 100).await;
    assert!(!doc(&session.app).is_hex());
    assert_eq!(
        doc(&session.app).text().to_string(),
        "just text\nsecond line\n"
    );
    session.close().await;
}

// Rendering

const HEX_16: [&str; 4] = [
    "0x00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  .PNG........IHDR",
    "0x00000010  00 00 01 00 00 00 01 00  08 06 00 00 00 5c 72 a8  .............\\r.",
    "0x00000020  66 00 00 00 09 70 48 59  73 00 00 0e c4 00 00 0e  f....pHYs.......",
    "0x00000030  c4 01 de ad be ef 49 45  4e 44 ae 42 60 82 de ad  ......IEND.B`...",
];

const BIN_8: [&str; 4] = [
    "0x00000000  10001001 01010000 01001110 01000111  00001101 00001010 00011010 00001010  .PNG....",
    "0x00000008  00000000 00000000 00000000 00001101  01001001 01001000 01000100 01010010  ....IHDR",
    "0x00000010  00000000 00000000 00000001 00000000  00000000 00000000 00000001 00000000  ........",
    "0x00000018  00001000 00000110 00000000 00000000  00000000 01011100 01110010 10101000  .....\\r.",
];

/// The rows of a PNG in hex and in bin, in terminals of 160 and 100 columns, with the mode and
/// the position in the statusline.
#[tokio::test(flavor = "multi_thread")]
async fn rendering_in_hex_and_in_bin() {
    for cols in [160, 100] {
        let (dir, path) = file_with("image.png", &png());
        let mut session = Session::open(&path, cols).await;

        let rows = first_rows(&session.app, 5);
        let status_row = statusline(&session.app);
        print_snapshot(
            &format!("hex, {cols} columns"),
            dir.path(),
            &rows,
            &status_row,
        );
        assert_eq!(rows[..4], HEX_16);
        assert_eq!(rows[4], "", "no more rows than the file has");
        assert!(status_row.contains("HEX RO"), "{status_row}");
        assert!(status_row.contains("0x0 (0)"), "{status_row}");
        assert!(status_row.contains("of 64 B"), "{status_row}");

        session.keys("~").await;
        let rows = first_rows(&session.app, 5);
        let status_row = statusline(&session.app);
        print_snapshot(
            &format!("bin, {cols} columns"),
            dir.path(),
            &rows,
            &status_row,
        );
        // (8 bytes per row: 8 rows for the 64 bytes, the first 4 are checked)
        assert_eq!(rows[..4], BIN_8);
        assert_eq!(
            Rows::new(Layout::Bin, 64, cols).width(),
            94,
            "a row of bin is 94 wide"
        );
        assert!(status_row.contains("BIN RO"), "{status_row}");
        assert!(status_row.contains("0x0 (0)"), "{status_row}");

        // and back
        session.keys("~").await;
        assert_eq!(first_rows(&session.app, 4), HEX_16);
        assert!(statusline(&session.app).contains("HEX RO"));
        session.close().await;
    }
}

/// Two views side by side are about 49 wide: 8 bytes per row in hex, 2 in bin (4 need 53).
#[tokio::test(flavor = "multi_thread")]
async fn rendering_in_narrow_views() {
    let (dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("<C-w>v").await;
    let views: Vec<_> = session
        .app
        .editor
        .tree
        .views()
        .map(|(v, _)| v.area)
        .collect();
    assert_eq!(views.len(), 2);
    assert!(views.iter().all(|area| (49..=50).contains(&area.width)));

    // the left view, up to its right edge
    let left = |app: &Application, count: usize| -> Vec<String> {
        let width = views[0].width as usize;
        screen_rows(app)
            .iter()
            .take(count)
            .map(|row| {
                row.chars()
                    .take(width)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    };
    let rows = left(&session.app, 3);
    let status_row = statusline(&session.app);
    print_snapshot(
        "hex, two views of 100 columns",
        dir.path(),
        &rows,
        &status_row,
    );
    assert_eq!(
        rows,
        [
            "0x00000000  89 50 4e 47 0d 0a 1a 0a  .PNG....",
            "0x00000008  00 00 00 0d 49 48 44 52  ....IHDR",
            "0x00000010  00 00 01 00 00 00 01 00  ........",
        ]
    );

    // (the focus is on the right view, which `~` changes)
    session.keys("<C-w>h~").await;
    let rows = left(&session.app, 3);
    let status_row = statusline(&session.app);
    print_snapshot(
        "bin, two views of 100 columns",
        dir.path(),
        &rows,
        &status_row,
    );
    assert_eq!(
        rows,
        [
            "0x00000000  10001001 01010000  .P",
            "0x00000002  01001110 01000111  NG",
            "0x00000004  00001101 00001010  ..",
        ]
    );
    session.close().await;
}

const THEME_WITH_SCOPES: &str = r##"
"ui.hex.null" = "#000001"
"ui.hex.printable" = "#000002"
"ui.hex.whitespace" = "#000003"
"ui.hex.control" = "#000004"
"ui.hex.other" = "#000005"
"ui.text.inactive" = "#000009"
"##;

/// A theme without `ui.hex.*`: the kinds fall back to other scopes.
const THEME_WITHOUT_SCOPES: &str = r##"
"ui.text.inactive" = "#000009"
"ui.text" = "#00000a"
"function" = "#00000b"
"string" = "#00000c"
"constant" = "#00000d"
"comment" = "#00000e"
"##;

/// The bytes of the PNG by kind: 0x89 other (0), `P` printable (1), `\r` whitespace (4), 0x1a
/// control (6), NUL (8).
const KINDS: [(usize, &str); 5] = [
    (0, "other"),
    (1, "printable"),
    (4, "whitespace"),
    (6, "control"),
    (8, "null"),
];

async fn colors(theme: &str) -> (Session, Rows) {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 160).await;
    // (the cursor is on the last byte, not on the ones checked)
    session.keys("ge").await;
    session.app.editor.theme = theme_of(theme);
    session.keys("k").await;
    let rows = Rows::new(Layout::Hex, 64, 160);
    (session, rows)
}

/// The `0x` prefix is grey and the bytes have the color of their kind, in the bytes column and
/// in the ASCII column.
#[tokio::test(flavor = "multi_thread")]
async fn colors_of_the_kinds_of_bytes() {
    let (session, rows) = colors(THEME_WITH_SCOPES).await;
    let colors = [
        ("other", 5),
        ("printable", 2),
        ("whitespace", 3),
        ("control", 4),
        ("null", 1),
    ];
    for ((i, kind), (name, blue)) in KINDS.into_iter().zip(colors) {
        assert_eq!(kind, name);
        let color = Color::Rgb(0, 0, blue);
        assert_eq!(
            fg(&session.app, rows.byte_x(i), 0),
            color,
            "{kind}, first digit"
        );
        assert_eq!(
            fg(&session.app, rows.byte_x(i) + 1, 0),
            color,
            "{kind}, second digit"
        );
        assert_eq!(
            fg(&session.app, rows.ascii_x() + i, 0),
            color,
            "{kind}, ASCII"
        );
    }
    // the prefix, not the digits of the offset
    assert_eq!(fg(&session.app, 0, 0), Color::Rgb(0, 0, 9));
    assert_eq!(fg(&session.app, 1, 0), Color::Rgb(0, 0, 9));
    assert_ne!(fg(&session.app, 2, 0), Color::Rgb(0, 0, 9));
    session.close().await;
}

/// Without `ui.hex.*` in the theme: null is `ui.text.inactive`, other `ui.text`, printable
/// `function`, whitespace `string`, control `constant`.
#[tokio::test(flavor = "multi_thread")]
async fn colors_fall_back_to_other_scopes() {
    let (session, rows) = colors(THEME_WITHOUT_SCOPES).await;
    let colors = [
        ("other", 0x0a),
        ("printable", 0x0b),
        ("whitespace", 0x0c),
        ("control", 0x0d),
        ("null", 0x09),
    ];
    for ((i, kind), (name, blue)) in KINDS.into_iter().zip(colors) {
        assert_eq!(kind, name);
        let color = Color::Rgb(0, 0, blue);
        assert_eq!(fg(&session.app, rows.byte_x(i), 0), color, "{kind}, bytes");
        assert_eq!(
            fg(&session.app, rows.ascii_x() + i, 0),
            color,
            "{kind}, ASCII"
        );
    }
    // the prefix follows ui.text.inactive
    assert_eq!(fg(&session.app, 0, 0), Color::Rgb(0, 0, 9));
    session.close().await;
}

/// The last row, shorter than the others, has its ASCII column where the others have it, and an
/// empty file has one row.
#[tokio::test(flavor = "multi_thread")]
async fn short_last_rows_and_empty_files() {
    let (_dir, path) = file_with("short.bin", &png()[..41]);
    let session = Session::open(&path, 100).await;
    let rows = first_rows(&session.app, 4);
    let ascii_x = Rows::new(Layout::Hex, 41, 100).ascii_x();
    assert_eq!(ascii_x, 62);
    assert_eq!(
        rows[2],
        "0x00000020  66 00 00 00 09 70 48 59  73                       f....pHYs"
    );
    for (n, row) in rows[..3].iter().enumerate() {
        let ascii: String = row.chars().skip(ascii_x).collect();
        let expected = [".PNG........IHDR", ".............\\r.", "f....pHYs"][n];
        assert_eq!(ascii, expected, "row {n}");
    }
    assert_eq!(rows[3], "");
    assert!(statusline(&session.app).contains("of 41 B"));
    session.close().await;

    // an empty file is text: `:hex` shows it as one row
    let (_dir, path) = file_with("empty.bin", b"");
    let mut session = Session::open(&path, 100).await;
    assert!(!doc(&session.app).is_hex());
    session.keys(":hex<ret>").await;
    assert!(doc(&session.app).is_hex());
    assert_eq!(first_rows(&session.app, 2), ["0x00000000", ""]);
    assert!(statusline(&session.app).contains("of 0 B"));
    assert!(statusline(&session.app).contains("0x0 (0)"));
    // nothing to overwrite
    session.keys("i").await;
    assert!(!hex(&session.app).cursor(view_id(&session.app)).overwrite);
    session.close().await;
}

// Going to an offset

/// `:goto`, `:<offset>` and `G` go to an offset, in hex after `0x`, else in decimal.
#[tokio::test(flavor = "multi_thread")]
async fn goto_offsets() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;

    session.keys(":goto 0x1a<ret>").await;
    assert_eq!(offset(&session.app), 26);
    session.keys("gg:goto 26<ret>").await;
    assert_eq!(offset(&session.app), 26);
    helpers::assert_status_not_error(&session.app.editor);

    session.keys("gg:0x1a<ret>").await;
    assert_eq!(offset(&session.app), 26);
    session.keys("gg:40<ret>").await;
    assert_eq!(offset(&session.app), 40);
    helpers::assert_status_not_error(&session.app.editor);

    session.keys("gg").await;
    assert_eq!(offset(&session.app), 0);
    session.keys("G0x10<ret>").await;
    assert_eq!(offset(&session.app), 16);
    session.keys("G33<ret>").await;
    assert_eq!(offset(&session.app), 33);
    helpers::assert_status_not_error(&session.app.editor);
    assert!(statusline(&session.app).contains("0x21 (33)"));

    // past the end: the last byte, and it is said
    session.app.editor.clear_status();
    session.keys(":goto 0x270f<ret>").await;
    assert_eq!(offset(&session.app), 63);
    assert!(
        status(&session.app).contains("0x270F is past the end"),
        "{}",
        status(&session.app)
    );
    session.app.editor.clear_status();
    session.keys("gg:goto 64<ret>").await;
    assert_eq!(offset(&session.app), 63);
    assert!(status(&session.app).contains("past the end"));

    // not an offset
    session.keys(":goto 1a4<ret>").await;
    assert_error_containing(&session.app, "is not an offset");
    assert_eq!(offset(&session.app), 63);
    session.close().await;
}

// Searching

/// `/` finds hex bytes (spaces optional) and text in quotes; `n` and `N` go on, wrapping.
#[tokio::test(flavor = "multi_thread")]
async fn searching() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;

    session.keys("/de ad<ret>").await;
    assert_eq!(offset(&session.app), 50);
    assert_eq!(hex(&session.app).search.as_deref(), Some(&[0xde, 0xad][..]));
    session.keys("gg/dead<ret>").await;
    assert_eq!(offset(&session.app), 50);
    session.keys("gg/DE AD<ret>").await;
    assert_eq!(offset(&session.app), 50);
    session.keys("gg/\"IHDR\"<ret>").await;
    assert_eq!(offset(&session.app), 12);
    helpers::assert_status_not_error(&session.app.editor);

    // the next match, wrapping around the end, and back
    session.keys("gg/de ad<ret>").await;
    assert_eq!(offset(&session.app), 50);
    session.app.editor.clear_status();
    session.keys("n").await;
    assert_eq!(offset(&session.app), 62);
    assert_eq!(status(&session.app), "");
    session.keys("n").await;
    assert_eq!(offset(&session.app), 50);
    assert_eq!(status(&session.app), "Wrapped around document");
    session.app.editor.clear_status();
    session.keys("N").await;
    assert_eq!(offset(&session.app), 62);
    assert_eq!(status(&session.app), "Wrapped around document");
    session.app.editor.clear_status();
    session.keys("N").await;
    assert_eq!(offset(&session.app), 50);
    assert_eq!(status(&session.app), "");

    // `N` searches backwards: from the start, around to the last match
    session.keys("gg/de ad<ret>ggN").await;
    assert_eq!(offset(&session.app), 62);
    session.close().await;
}

/// Patterns that are not bytes are errors, and what is not there is "not found".
#[tokio::test(flavor = "multi_thread")]
async fn search_errors() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;

    session.keys("n").await;
    assert!(
        status(&session.app).contains("no search yet"),
        "{}",
        status(&session.app)
    );

    session.keys("l/xyz<ret>").await;
    assert_error_containing(&session.app, "is not a hex digit");
    session.keys("/abc<ret>").await;
    assert_error_containing(&session.app, "odd number of hex digits");
    session.keys("/\"open<ret>").await;
    assert_error_containing(&session.app, "ends with a quote");
    // none of them searched, or moved
    assert_eq!(hex(&session.app).search, None);
    assert_eq!(offset(&session.app), 1);

    session.keys("/ff ff ff<ret>").await;
    assert_error_containing(&session.app, "not found: ff ff ff");
    assert_eq!(offset(&session.app), 1);
    session.keys("/\"nope\"<ret>").await;
    assert_error_containing(&session.app, "not found: 6e 6f 70 65");
    session.close().await;
}

// Yanking

/// `v` selects bytes, `y` yanks them as hex text.
#[tokio::test(flavor = "multi_thread")]
async fn yanking() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    let register = |app: &Application, name: char| {
        app.editor
            .registers
            .read(name, &app.editor)
            .map(|values| values.map(|v| v.to_string()).collect::<Vec<_>>())
    };

    session.keys("vlll").await;
    assert_eq!(
        hex(&session.app).cursor(view_id(&session.app)).selection(),
        0..4
    );
    assert!(statusline(&session.app).contains("[4 B sel]"));
    session.keys("y").await;
    assert_eq!(
        register(&session.app, '"'),
        Some(vec!["89 50 4e 47".to_owned()])
    );
    assert_eq!(status(&session.app), "yanked 4 bytes as hex to register \"");
    // the selection is over
    assert_eq!(hex(&session.app).cursor(view_id(&session.app)).anchor, None);

    // a register, from the byte under the cursor (and a selection backwards)
    session.keys("\"ay").await;
    assert_eq!(register(&session.app, 'a'), Some(vec!["47".to_owned()]));
    assert_eq!(status(&session.app), "yanked 1 byte as hex to register a");
    session.keys("vhhy").await;
    assert_eq!(
        register(&session.app, '"'),
        Some(vec!["50 4e 47".to_owned()])
    );
    // the other register is as it was
    assert_eq!(register(&session.app, 'a'), Some(vec!["47".to_owned()]));

    // `;` ends a selection without yanking
    session.keys("v;").await;
    assert_eq!(hex(&session.app).cursor(view_id(&session.app)).anchor, None);
    session.close().await;
}

// Read-only

/// Typing without `i` does not change anything, and says so.
#[tokio::test(flavor = "multi_thread")]
async fn typing_is_refused_in_read_only_mode() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    for key in ["x", "d", "A", "5", "p", "c"] {
        session.app.editor.clear_status();
        session.keys(key).await;
        assert_eq!(status(&session.app), "read-only", "after {key:?}");
        assert_eq!(bytes(&session.app), png(), "after {key:?}");
        assert!(!doc(&session.app).is_modified());
    }
    assert_eq!(offset(&session.app), 0);
    assert_eq!(doc(&session.app).text().len_chars(), 0);

    // `u` has nothing to undo
    session.keys("u").await;
    assert_eq!(status(&session.app), "Already at oldest change");
    session.keys("U").await;
    assert_eq!(status(&session.app), "Already at newest change");
    session.close().await;
}

/// The keys of the movement.
#[tokio::test(flavor = "multi_thread")]
async fn moving() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    let at = |keys: &'static str| keys;
    let cases: [(&str, u64); 19] = [
        (at("l"), 1),
        (at("lj"), 17),
        (at("ljk"), 1),
        (at("lll"), 3),
        (at("llh"), 1),
        (at("w"), 4),
        (at("ww"), 8),
        (at("wwb"), 4),
        (at("wllb"), 4),
        (at("<right><down>"), 17),
        (at("jjj"), 48),
        (at("jjjj"), 48),
        (at("jjjl<end>"), 63),
        (at("jjl<home>"), 32),
        (at("ge"), 63),
        (at("gejgg"), 0),
        (at("jjgl"), 47),
        (at("jjlgh"), 32),
        (at("ge<C-d>"), 63),
    ];
    for (keys, expected) in cases {
        session.keys("gg").await;
        session.keys(keys).await;
        assert_eq!(offset(&session.app), expected, "after {keys}");
    }
    // not past the ends
    session.keys("gghhkk").await;
    assert_eq!(offset(&session.app), 0);
    session.keys("gellll").await;
    assert_eq!(offset(&session.app), 63);

    // Tab switches the column and marks the cursor in it
    session.keys("gg<tab>").await;
    assert_eq!(
        hex(&session.app).cursor(view_id(&session.app)).column,
        vortex_view::hex::Column::Ascii
    );
    session.keys("<tab>").await;
    assert_eq!(
        hex(&session.app).cursor(view_id(&session.app)).column,
        vortex_view::hex::Column::Bytes
    );
    session.close().await;
}

// Overwriting

/// In the hex column, digits overwrite nibble by nibble, and move on after the second one.
#[tokio::test(flavor = "multi_thread")]
async fn overwriting_nibbles_in_the_hex_column() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;

    session.keys("i").await;
    assert!(statusline(&session.app).contains("HEX OVR"));
    session.keys("d").await;
    // only the high nibble
    assert_eq!(bytes(&session.app)[0], 0xd9);
    assert_eq!(offset(&session.app), 0);
    assert!(doc(&session.app).is_modified());
    assert!(hex(&session.app).is_edited(0));
    session.keys("e").await;
    assert_eq!(bytes(&session.app)[0], 0xde);
    // on to the next byte
    assert_eq!(offset(&session.app), 1);
    session.keys("<esc>").await;
    assert!(statusline(&session.app).contains("HEX RO"));
    assert_eq!(bytes(&session.app)[1..], png()[1..]);
    // nothing is on disk yet
    assert_eq!(std::fs::read(&path).unwrap(), png());

    // Left and Right move by digit: overwrite the low nibble of byte 1 only
    session.keys("i<right>7<esc>").await;
    assert_eq!(bytes(&session.app)[1], 0x57);
    session.keys("i<right><left>3<esc>").await;
    assert_eq!(
        bytes(&session.app)[1],
        0x57,
        "byte 1 is where the cursor was left"
    );

    // not a hex digit
    session.keys("gg").await;
    session.keys("izz").await;
    assert!(
        status(&session.app).contains("type hex digits here"),
        "{}",
        status(&session.app)
    );
    assert_eq!(bytes(&session.app)[0], 0xde);
    session.close().await;
}

/// In the ASCII column, a character overwrites a whole byte.
#[tokio::test(flavor = "multi_thread")]
async fn overwriting_characters_in_the_ascii_column() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("<tab>iAB<esc>").await;
    let mut expected = png();
    expected[0] = b'A';
    expected[1] = b'B';
    assert_eq!(bytes(&session.app), expected);
    assert_eq!(offset(&session.app), 2);
    let rows = first_rows(&session.app, 1);
    assert!(
        rows[0].starts_with("0x00000000  41 42 4e 47"),
        "{}",
        rows[0]
    );
    assert!(rows[0].ends_with("ABNG........IHDR"), "{}", rows[0]);

    // a character past ASCII overwrites its bytes (UTF-8)
    session.keys("i\u{e9}<esc>").await;
    expected[2] = 0xc3;
    expected[3] = 0xa9;
    assert_eq!(bytes(&session.app), expected);
    session.close().await;
}

/// In the bin column, 0 and 1 overwrite the bit under the cursor.
#[tokio::test(flavor = "multi_thread")]
async fn overwriting_bits_in_the_bin_column() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("~i").await;
    assert!(statusline(&session.app).contains("BIN OVR"));

    // 0x89 is 10001001: the high 4 bits set
    session.keys("1111").await;
    assert_eq!(bytes(&session.app)[0], 0b1111_1001);
    assert_eq!(offset(&session.app), 0);
    // the 8 bits, then on to the next byte: 0x50 is 01010000
    session.keys("0000").await;
    assert_eq!(bytes(&session.app)[0], 0b1111_0000);
    assert_eq!(offset(&session.app), 1);
    session.keys("1").await;
    assert_eq!(bytes(&session.app)[1], 0b1101_0000);
    assert_eq!(offset(&session.app), 1);
    session.keys("2").await;
    assert!(
        status(&session.app).contains("type 0 or 1 here"),
        "{}",
        status(&session.app)
    );
    session.keys("<esc>").await;
    assert!(statusline(&session.app).contains("BIN RO"));
    assert_eq!(bytes(&session.app)[2..], png()[2..]);
    session.close().await;
}

/// `u` undoes a stay in overwrite mode, `U` redoes it; writing puts exactly the changed bytes
/// in the file.
#[tokio::test(flavor = "multi_thread")]
async fn undo_redo_and_write() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    let original = png();
    let mut first = png();
    first[0] = 0xab;
    let mut second = first.clone();
    second[1] = 0xcd;

    // one step for each stay in overwrite mode
    session.keys("iab<esc>").await;
    assert_eq!(bytes(&session.app), first);
    session.keys("icd<esc>").await;
    assert_eq!(bytes(&session.app), second);

    session.keys("u").await;
    assert_eq!(bytes(&session.app), first);
    assert_eq!(offset(&session.app), 1);
    session.keys("u").await;
    assert_eq!(bytes(&session.app), original);
    assert!(!doc(&session.app).is_modified(), "everything undone");
    session.keys("u").await;
    assert_eq!(status(&session.app), "Already at oldest change");

    session.keys("U").await;
    assert_eq!(bytes(&session.app), first);
    assert_eq!(offset(&session.app), 0);
    session.keys("U").await;
    assert_eq!(bytes(&session.app), second);
    assert!(doc(&session.app).is_modified());
    session.keys("U").await;
    assert_eq!(status(&session.app), "Already at newest change");

    // a new step drops what was undone
    session.keys("uu").await;
    session.keys("i11<esc>").await;
    session.keys("U").await;
    assert_eq!(status(&session.app), "Already at newest change");
    session.keys("uiab<esc>icd<esc>").await;
    assert_eq!(bytes(&session.app), second);

    // the file is as it was until `:w`
    assert_eq!(std::fs::read(&path).unwrap(), original);
    session.write(":w").await;
    assert_eq!(std::fs::read(&path).unwrap(), second);
    assert!(!doc(&session.app).is_modified());
    assert!(
        status(&session.app).contains("2 bytes changed in place"),
        "{}",
        status(&session.app)
    );
    helpers::assert_status_not_error(&session.app.editor);
    // the bytes shown are the ones of the file
    assert_eq!(bytes(&session.app), second);
    assert!(!hex(&session.app).is_edited(0));

    // writing can be undone like any change, and written again
    session.keys("u").await;
    assert!(doc(&session.app).is_modified());
    assert_eq!(bytes(&session.app)[1], 0x50);
    session.write(":w").await;
    let mut third = second.clone();
    third[1] = 0x50;
    assert_eq!(std::fs::read(&path).unwrap(), third);
    assert!(
        status(&session.app).contains("1 byte changed in place"),
        "{}",
        status(&session.app)
    );
    session.close().await;
}

/// Overwriting the last byte over and over never makes the file longer.
#[tokio::test(flavor = "multi_thread")]
async fn the_size_never_changes() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;

    session.keys("gei12345<esc>").await;
    assert_eq!(offset(&session.app), 63);
    assert_eq!(hex(&session.app).len(), 64);
    // the cursor stays on the last byte: the first digit typed there is its high nibble, the
    // others replace its low nibble
    assert_eq!(bytes(&session.app).len(), 64);
    assert_eq!(bytes(&session.app)[63], 0x15);
    session.keys("<tab>iXYZ<esc>").await;
    assert_eq!(offset(&session.app), 63);
    assert_eq!(
        bytes(&session.app)[62..],
        png()[62..63]
            .iter()
            .chain(b"Z")
            .copied()
            .collect::<Vec<_>>()
    );
    // a character of two bytes on the last byte doesn't fit: refused, nothing written
    session.keys("i\u{e9}").await;
    assert!(
        status(&session.app).contains("no room"),
        "{}",
        status(&session.app)
    );
    assert_eq!(bytes(&session.app)[62..], [0xde, b'Z']);
    // one byte before, it fits
    session.keys("<left>\u{e9}<esc>").await;
    assert_eq!(bytes(&session.app).len(), 64);
    assert_eq!(bytes(&session.app)[62..], [0xc3, 0xa9]);

    session.write(":w").await;
    let on_disk = std::fs::read(&path).unwrap();
    assert_eq!(on_disk.len(), 64);
    assert_eq!(on_disk[..62], png()[..62]);
    assert_eq!(on_disk[62..], [0xc3, 0xa9]);

    // and in bin
    session.keys("~iiiiiiiii<esc>").await;
    assert_eq!(bytes(&session.app).len(), 64);
    session.close().await;
}

// Files changed on disk

/// Writes the file and moves its modification time two seconds ahead of what it was.
fn change_on_disk(path: &Path, content: &[u8]) {
    let before = std::fs::metadata(path).unwrap().modified().unwrap();
    std::fs::write(path, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(before.max(SystemTime::now()) + Duration::from_secs(2))
        .unwrap();
}

/// A file changed from outside is not written over, unless forced.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_changed_on_disk_is_not_written_over() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("ide<esc>").await;
    assert!(doc(&session.app).is_modified());

    let mut outside = png();
    outside[10] = 0xee;
    outside[11] = 0xff;
    change_on_disk(&path, &outside);

    session.keys(":w<ret>").await;
    assert_error_containing(&session.app, "changed on disk");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        outside,
        "the disk is untouched"
    );
    assert!(doc(&session.app).is_modified());

    session.write(":w!").await;
    // only the overwritten byte is written, on top of what is there
    let mut expected = outside.clone();
    expected[0] = 0xde;
    assert_eq!(std::fs::read(&path).unwrap(), expected);
    assert!(!doc(&session.app).is_modified());

    // and the next write is fine again
    session.keys("gg").await;
    session.keys("ief<esc>").await;
    assert!(doc(&session.app).is_modified());
    session.write(":w").await;
    assert_eq!(std::fs::read(&path).unwrap()[0], 0xef);
    session.close().await;
}

// A big file

/// A file of 4 GiB opens at once: it is never read as a whole, only what is on screen.
#[tokio::test(flavor = "multi_thread")]
async fn a_sparse_file_of_4_gib() {
    const SIZE: u64 = 4 << 30;
    let dir = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(dir.path()).unwrap().join("huge.bin");
    {
        use std::os::unix::fs::FileExt;
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(SIZE).unwrap();
        file.write_all_at(b"\x89PNG", 0).unwrap();
        file.write_all_at(b"end", SIZE - 3).unwrap();
    }

    let started = Instant::now();
    let app = helpers::AppBuilder::new()
        .with_file(&path, None)
        .build()
        .unwrap();
    let mut session = Session::new(app);
    session.resize(100, 30).await;
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(2), "opened in {elapsed:?}");
    assert!(doc(&session.app).is_hex());
    assert_eq!(hex(&session.app).len(), SIZE);
    assert!(first_rows(&session.app, 1)[0].starts_with("0x00000000  89 50 4e 47"));
    assert!(statusline(&session.app).contains("of 4.0 GiB"));

    let started = Instant::now();
    session.keys("ge").await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(offset(&session.app), SIZE - 1);
    let rows = first_rows(&session.app, 30);
    let last_row = rows
        .iter()
        .find(|row| row.starts_with("0xfffffff0"))
        .unwrap_or_else(|| panic!("the last row is on screen: {rows:#?}"));
    // offsets of 8 digits, the last 3 bytes are "end"
    assert!(last_row.contains("65 6e 64"), "{last_row}");
    assert!(last_row.ends_with(".............end"), "{last_row}");
    let status_row = statusline(&session.app);
    assert!(
        status_row.contains("0xFFFFFFFF (4294967295)"),
        "{status_row}"
    );
    assert!(status_row.contains("of 4.0 GiB"), "{status_row}");

    // overwrite the last byte and write: only that byte goes to the file
    session.keys("iab<esc>").await;
    assert_eq!(hex(&session.app).byte(SIZE - 1), Some(0xab));
    session.write(":w").await;
    assert!(
        status(&session.app).contains("1 byte changed in place"),
        "{}",
        status(&session.app)
    );
    let mut file = std::fs::File::open(&path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), SIZE);
    file.seek(SeekFrom::Start(SIZE - 3)).unwrap();
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, [b'e', b'n', 0xab]);
    drop(file);

    session.close().await;
    // (the file goes away with the directory, it was never allocated)
    drop(dir);
    assert!(!path.exists());
}

// `:hex`

/// `:hex` shows a text file in the hex view and back, and refuses when text or bytes would be
/// lost.
#[tokio::test(flavor = "multi_thread")]
async fn the_hex_command_toggles_the_view() {
    let (_dir, path) = file_with("notes.txt", b"hello\nworld\n");
    let mut session = Session::open(&path, 100).await;
    assert!(!doc(&session.app).is_hex());

    session.keys(":hex<ret>").await;
    assert!(doc(&session.app).is_hex());
    helpers::assert_status_not_error(&session.app.editor);
    assert_eq!(
        first_rows(&session.app, 1)[0],
        "0x00000000  68 65 6c 6c 6f 0a 77 6f  72 6c 64 0a              hello.world."
    );

    session.keys(":hex<ret>").await;
    assert!(!doc(&session.app).is_hex());
    assert_eq!(doc(&session.app).text().to_string(), "hello\nworld\n");
    helpers::assert_status_not_error(&session.app.editor);
    assert!(first_rows(&session.app, 1)[0].contains("hello"));

    // a modified text buffer would lose its changes
    session.keys("ix<esc>:hex<ret>").await;
    assert_error_containing(&session.app, "write");
    assert!(!doc(&session.app).is_hex());
    assert_eq!(doc(&session.app).text().to_string(), "xhello\nworld\n");
    session.keys(":w<ret>").await;
    session
        .wait_for("the write", |app| !doc(app).is_modified())
        .await;
    session.keys(":hex<ret>").await;
    assert!(doc(&session.app).is_hex());
    assert_eq!(bytes(&session.app), b"xhello\nworld\n");

    // overwritten bytes not written would be lost too
    session.keys("i41<esc>").await;
    assert_eq!(bytes(&session.app)[0], 0x41);
    session.keys(":hex<ret>").await;
    assert_error_containing(&session.app, "overwritten bytes not written");
    assert!(doc(&session.app).is_hex());
    assert_eq!(bytes(&session.app)[0], 0x41);

    session.write(":w").await;
    session.keys(":hex<ret>").await;
    assert!(!doc(&session.app).is_hex());
    assert_eq!(doc(&session.app).text().to_string(), "Ahello\nworld\n");
    assert_eq!(std::fs::read(&path).unwrap(), b"Ahello\nworld\n");
    session.close().await;
}

/// A binary file toggled to text and back.
#[tokio::test(flavor = "multi_thread")]
async fn a_binary_file_can_be_shown_as_text() {
    let (_dir, path) = file_with("data.bin", b"ab\0cd\n");
    let mut session = Session::open(&path, 100).await;
    assert!(doc(&session.app).is_hex());
    session.keys(":hex<ret>").await;
    assert!(!doc(&session.app).is_hex());
    assert_eq!(doc(&session.app).text().to_string(), "ab\0cd\n");
    session.keys(":hex<ret>").await;
    assert!(doc(&session.app).is_hex());
    session.close().await;
}

// Views

/// Each view of a document in the hex view has its own cursor.
#[tokio::test(flavor = "multi_thread")]
async fn views_have_their_own_cursors() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("lll").await;
    let first = view_id(&session.app);
    assert_eq!(offset(&session.app), 3);

    session.keys("<C-w>v").await;
    let second = view_id(&session.app);
    assert_ne!(first, second);
    let cursors = |app: &Application| {
        (
            hex(app).cursor(first).offset,
            hex(app).cursor(second).offset,
        )
    };
    let (_, start) = cursors(&session.app);
    // a split starts with a copy of the cursor of the view it was split from
    assert_eq!(start, 3);
    assert_eq!(cursors(&session.app), (3, 3));

    session.keys("gg").await;
    assert_eq!(cursors(&session.app), (3, 0));
    // (a row of the views is 8 bytes)
    session.keys("jjl").await;
    assert_eq!(cursors(&session.app), (3, 17));

    session.keys("<C-w>h").await;
    assert_eq!(view_id(&session.app), first);
    session.keys("j").await;
    assert_eq!(cursors(&session.app), (11, 17));

    // the cursor of a view closed is dropped, the other one stays
    session.keys("<C-w>q").await;
    assert_eq!(session.app.editor.tree.views().count(), 1);
    assert_eq!(view_id(&session.app), second);
    assert_eq!(hex(&session.app).cursor(first).offset, 0);
    assert_eq!(offset(&session.app), 17);
    session.close().await;
}

// Quitting

/// `:q` does not drop overwritten bytes.
#[tokio::test(flavor = "multi_thread")]
async fn quitting_with_overwritten_bytes_is_refused() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    session.keys("iab<esc>").await;
    assert!(doc(&session.app).is_modified());

    session.keys(":q<ret>").await;
    assert_error_containing(&session.app, "unsaved");
    assert!(doc(&session.app).is_hex());
    assert!(doc(&session.app).is_modified());
    session.keys(":qa<ret>").await;
    assert_error_containing(&session.app, "unsaved");

    // written, it quits
    session.write(":w").await;
    for key in parse_macro(":q<ret>").unwrap() {
        session
            .tx
            .send(Ok(Event::Key(KeyEvent::from(key))))
            .unwrap();
    }
    // (the editor is gone, the loop ends)
    while session.app.event_loop_until_idle(&mut session.input).await {}
    let errors = session.app.close().await;
    assert!(errors.is_empty(), "{errors:?}");
}

/// `?` lists the keys of the view in an overlay, any key closes it, and it is not a search.
#[tokio::test(flavor = "multi_thread")]
async fn question_mark_opens_the_key_help() {
    let (_dir, path) = file_with("image.png", &png());
    let mut session = Session::open(&path, 100).await;
    assert!(!screen_rows(&session.app)
        .iter()
        .any(|row| row.contains("any key closes")));

    session.keys("?").await;
    let rows = screen_rows(&session.app);
    assert!(rows.iter().any(|row| row.contains("Hex view")), "{rows:#?}");
    assert!(
        rows.iter().any(|row| row.contains("any key closes")),
        "{rows:#?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("overwrite mode")),
        "{rows:#?}"
    );
    // not a prompt for a backward search
    assert!(!rows.iter().any(|row| row.contains("search:")), "{rows:#?}");
    assert_eq!(status(&session.app), "");

    // any key closes it, and is only that: the cursor did not move
    session.keys("l").await;
    let rows = screen_rows(&session.app);
    assert!(
        !rows.iter().any(|row| row.contains("any key closes")),
        "{rows:#?}"
    );
    assert_eq!(offset(&session.app), 0);
    session.close().await;
}
