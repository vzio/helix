//! A debugged program's memory in the hex view, against the scripted fake adapter: what the
//! adapter's capabilities and the values' memory references offer, opening, windowed reads,
//! unreadable bytes, refresh and change highlighting at each stop, overwriting and `:w`,
//! the rules that refuse writes, and the windows recorded for rewind.
//!
//! The simulated memory is the fake's (`Script::memory`, `Stop::poke`): readable regions, all
//! the rest unreadable. Bytes are read when the views are drawn (`memory::load_wanted`), so the
//! tests pump the editor until the cells they look at have their bytes.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tempfile::TempDir;
use vortex_core::syntax::config::DebugAdapterConfig;
use vortex_term::{application::Application, debug, debug::panel};
use vortex_view::{
    current_ref,
    debug::{history::Setting, spectator::Mode, Section, Status},
    hex::{Cell, MemorySource, Rows},
    Document,
};

use super::{
    fake_adapter::{FakeAdapter, Scope, Script, Stop, Var},
    harness::Harness,
};
use crate::test::helpers::AppBuilder;

/// The terminal of the tests: 200 x 40 cells, so a hex view of the split is wide enough for 16
/// bytes a row.
const COLS: u16 = 200;
const ROWS: u16 = 40;

struct Env {
    h: Harness,
    fake: FakeAdapter,
    dir: PathBuf,
    _tmp: TempDir,
}

async fn env(script: impl FnOnce(&str) -> Script) -> anyhow::Result<Env> {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().canonicalize()?;
    let path = dir.join("main.rs");
    let source: String = (1..=30).map(|i| format!("    step({i});\n")).collect();
    std::fs::write(&path, source)?;
    let mut h = Harness::new(AppBuilder::new().with_file(&path, None))?;
    let path = {
        let (_, doc) = current_ref!(h.app.editor);
        doc.path().expect("the file has a path").to_owned()
    };
    let fake = FakeAdapter::start(script(&path.to_string_lossy())).await?;
    h.app.resize_screen(COLS, ROWS);
    h.keys("<esc>").await?;
    Ok(Env {
        h,
        fake,
        dir,
        _tmp: tmp,
    })
}

fn adapter_config(dir: &Path) -> DebugAdapterConfig {
    DebugAdapterConfig {
        name: "fake".into(),
        transport: "tcp".into(),
        command: String::new(),
        args: Vec::new(),
        port_arg: None,
        templates: Vec::new(),
        quirks: Default::default(),
        own_paths: Some(vec![dir.to_owned()]),
        std_paths: None,
        lib_paths: None,
        capabilities: None,
    }
}

impl Env {
    fn launch(&mut self) -> anyhow::Result<()> {
        // these tests are about the memory, not playback: the engine has its own tests
        self.h.app.editor.debug.engine.kind = vortex_view::debug::engine::EngineKind::Stepping;
        debug::start(
            &mut self.h.app.editor,
            Some(self.fake.addr),
            &adapter_config(&self.dir),
            "fake",
            "launch",
            json!({"program": self.dir.join("prog"), "cwd": self.dir}),
        )
    }

    /// Waits until `stops` stops are recorded, the program is stopped and no step runs.
    async fn settled(&mut self, stops: usize) {
        self.h
            .until(
                &format!("{stops} recorded stops and no step running"),
                |app| {
                    let spectator = &app.editor.debug.spectator;
                    spectator.history.stops.len() >= stops
                        && spectator.step.is_none()
                        && matches!(app.editor.debug.status, Status::Stopped { .. })
                },
            )
            .await;
    }

    /// Launches and waits for the first stop and its variables.
    async fn start(&mut self) -> anyhow::Result<()> {
        self.launch()?;
        self.settled(1).await;
        self.h
            .until("the variables", |app| {
                texts(app, Section::Variables)
                    .iter()
                    .any(|text| text.starts_with("x = "))
            })
            .await;
        Ok(())
    }

    /// Keys for the debug panel: focuses it first, and leaves it focused.
    async fn panel(&mut self, keys: &str) -> anyhow::Result<()> {
        if !self.h.app.editor.debug.is_focused() {
            self.h.keys("<C-g>").await?;
        }
        assert!(self.h.app.editor.debug.is_focused());
        self.h.keys(keys).await
    }

    /// Keys of debug mode (playback, rewind, capture settings) typed in the editor: leaves the
    /// panel, enters debug mode when it is off and leaves it again after the keys, so the editing
    /// that follows is outside it. `c` opens the capture settings and stays in debug mode.
    async fn editor(&mut self, keys: &str) -> anyhow::Result<()> {
        let entered = !self.h.app.editor.debug.mode_on();
        if self.h.app.editor.debug.is_focused() {
            self.h.keys(if entered { "<C-g>" } else { "<tab>" }).await?;
        }
        if entered {
            // one batch, so the program has no time to answer between the keys
            let leave = if keys.starts_with('c') { "" } else { "<esc>" };
            self.h.keys(&format!("<space>G{keys}{leave}")).await
        } else {
            self.h.keys(keys).await
        }
    }

    /// Debug mode on in view mode, for the rest of the test: the bar's rows are there when the
    /// memory view's size is measured, and the stops don't move the view away from it.
    async fn stay_in_debug_mode(&mut self) -> anyhow::Result<()> {
        self.h.keys("<space>Gv").await?;
        assert!(self.h.app.editor.debug.mode.as_ref().unwrap().viewing);
        Ok(())
    }

    /// `l` (one playback step) `steps` times, waiting for each stop to be recorded.
    async fn step(&mut self, steps: usize) -> anyhow::Result<()> {
        for _ in 0..steps {
            let next = self.h.app.editor.debug.spectator.history.stops.len() + 1;
            self.editor("l").await?;
            self.settled(next).await;
        }
        Ok(())
    }

    /// Opens the memory at `address` with `:debug-memory` and waits for the view.
    async fn open(&mut self, address: u64) -> anyhow::Result<()> {
        self.h
            .keys(&format!(":debug-memory<space>0x{address:x}<ret>"))
            .await?;
        self.h
            .until("the memory view", |app| memory_doc(app).is_some())
            .await;
        Ok(())
    }

    /// Waits until `address` shows `byte` (read).
    async fn has_byte(&mut self, address: u64, cell: Cell) {
        self.h
            .until(&format!("{cell:?} at 0x{address:x}"), |app| {
                cells(app, address, 1) == [cell]
            })
            .await;
    }

    /// Waits until the bytes from `address` are drawn as `expected`.
    async fn cells_are(&mut self, address: u64, expected: &[Cell]) {
        self.h
            .until(&format!("{expected:?} at 0x{address:x}"), |app| {
                cells(app, address, expected.len() as u64) == expected
            })
            .await;
    }

    /// Waits until the cursor of the memory view is on `address`.
    async fn cursor_at(&mut self, address: u64) {
        self.h
            .until(&format!("the cursor at 0x{address:x}"), |app| {
                cursor_is(app, address)
            })
            .await;
    }

    /// Waits until the status line says `text`.
    async fn says(&mut self, text: &str) {
        self.h
            .until(&format!("the status {text:?}"), |app| status(app) == text)
            .await;
    }
}

fn texts(app: &Application, section: Section) -> Vec<String> {
    panel::rows(&app.editor, section)
        .iter()
        .map(|row| row.text())
        .collect()
}

fn status(app: &Application) -> String {
    app.editor
        .get_status()
        .map(|(message, _)| message.to_string())
        .unwrap_or_default()
}

/// The document showing memory, if one is open.
/// Whether the cursor of the current view (the memory view) is on `address`.
fn cursor_is(app: &Application, address: u64) -> bool {
    let (view, doc) = current_ref!(app.editor);
    doc.hex()
        .is_some_and(|hex| hex.cursor(view.id).offset == address)
}

fn memory_doc(app: &Application) -> Option<&Document> {
    app.editor.documents().find(|doc| doc.memory().is_some())
}

fn memory(app: &Application) -> &MemorySource {
    memory_doc(app)
        .and_then(Document::memory)
        .expect("a memory view is open")
}

/// How the bytes `address..address + len` are drawn.
fn cells(app: &Application, address: u64, len: u64) -> Vec<Cell> {
    memory_doc(app)
        .and_then(Document::hex)
        .expect("a memory view is open")
        .cells(address..address + len)
}

fn bytes(values: &[u8]) -> Vec<Cell> {
    values.iter().map(|byte| Cell::Byte(*byte)).collect()
}

/// The text of the screen, one string per row.
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

fn screen_has(app: &Application, text: &str) -> bool {
    screen_rows(app).iter().any(|row| row.contains(text))
}

fn memory_views(app: &Application) -> usize {
    app.editor
        .tree
        .views()
        .filter(|(view, _)| app.editor.documents[&view.doc].memory().is_some())
        .count()
}

/// The program's memory: `0x1000..0x2000` holds `i % 251`, plus a pointer variable `p` to
/// `0x1010` and a plain `x`.
fn locals() -> Scope {
    Scope::new("Locals")
        .var(Var::new("x", "1").ty("int"))
        .var(Var::new("p", "0x1010").ty("char *").memory("0x1010"))
}

fn program(p: &str) -> Script {
    Script::new(p)
        .read_memory()
        .write_memory()
        .memory_with(0x1000, 0x1000, |i| (i % 251) as u8)
        .stop(Stop::new(2, "entry").scope(locals()))
        .stop(Stop::new(3, "entry").scope(locals()))
        .stop(Stop::new(4, "entry").scope(locals()))
}

/// The byte the program has at `address` at the start.
fn initial(address: u64) -> u8 {
    ((address - 0x1000) % 251) as u8
}

// ---------------------------------------------------------------------------------------------
// What is offered

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_adapter_that_cannot_read_memory_offers_none() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .memory_with(0x1000, 64, |i| i as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
    })
    .await?;
    e.start().await?;
    // the value has a memoryReference, but the adapter doesn't read memory: no badge
    for section in [Section::Variables, Section::Watch] {
        assert!(
            !texts(&e.h.app, section).iter().any(|t| t.contains('▦')),
            "{:?}",
            texts(&e.h.app, section)
        );
    }
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1  int", "p = 0x1010  char *"]
    );

    let message = "this debugger (fake) doesn't support viewing memory";
    e.h.keys(":debug-memory<space>x<ret>").await?;
    assert_eq!(status(&e.h.app), message);
    e.h.keys(":debug-memory<space>0x1000<ret>").await?;
    assert_eq!(status(&e.h.app), message);
    e.h.keys(":debug-memory<ret>").await?;
    assert_eq!(status(&e.h.app), message);

    // `M` on the value that has a reference says it too
    e.panel("jjj").await?;
    e.h.keys("M").await?;
    assert_eq!(status(&e.h.app), message);
    assert!(memory_doc(&e.h.app).is_none());
    assert_eq!(memory_views(&e.h.app), 0);
    e.h.pump_for(std::time::Duration::from_millis(200)).await;
    assert!(e.fake.requests_named("readMemory").is_empty());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn values_with_a_memory_reference_get_a_badge_and_m_opens_their_memory() -> anyhow::Result<()>
{
    let mut e = env(program).await?;
    e.start().await?;
    // the badge is on the value with a reference only
    assert_eq!(
        texts(&e.h.app, Section::Variables),
        ["Locals", "x = 1  int", "p = 0x1010  char *  ▦"]
    );
    // and on watches that evaluate to one
    e.h.keys("<space>Gw").await?;
    e.h.keys("p<ret>").await?;
    e.h.keys("<esc>").await?;
    e.h.until("the watch", |app| {
        texts(app, Section::Watch)
            .iter()
            .any(|t| t.starts_with("p = 0x1010") && t.contains('▦'))
    })
    .await;

    // a value without: no badge, and M says so
    e.panel("j").await?;
    assert_eq!(
        debug::panel::current_target(&e.h.app.editor).map(|t| format!("{t:?}")),
        Some("Var(VarPath { tree: Scopes, names: [\"Locals\", \"x\"] })".into())
    );
    e.h.keys("M").await?;
    e.says("x has no memory to show").await;
    assert!(memory_doc(&e.h.app).is_none());

    // p: opens a vertical split, the cursor on 0x1010 in the middle of the view
    e.h.keys("jM").await?;
    e.h.until("the memory view", |app| memory_doc(app).is_some())
        .await;
    assert!(cursor_is(&e.h.app, 0x1010));
    assert_eq!(memory_views(&e.h.app), 1);
    assert_eq!(e.h.app.editor.tree.views().count(), 2);
    let areas: Vec<_> = e.h.app.editor.tree.views().map(|(v, _)| v.area).collect();
    assert_eq!(areas[0].y, areas[1].y, "side by side: {areas:?}");
    assert_ne!(areas[0].x, areas[1].x);
    assert!(!e.h.app.editor.debug.is_focused(), "the view has the focus");
    assert_eq!(memory(&e.h.app).base, 0x1010);
    assert_eq!(memory(&e.h.app).label, "p");
    let (view, doc) = current_ref!(e.h.app.editor);
    assert!(doc.memory().is_some(), "the memory view is the focused one");
    let cursor = doc.hex().unwrap().cursor(view.id);
    assert_eq!(cursor.offset, 0x1010);
    let per_row =
        Rows::new(cursor.layout, doc.hex().unwrap().len(), view.area.width).bytes_per_row as u64;
    let rows_shown = view.area.height as u64 - 1;
    let from_top = (cursor.offset - cursor.top) / per_row;
    assert!(
        from_top.abs_diff(rows_shown / 2) <= 1,
        "the cursor is {from_top} rows from the top of {rows_shown}"
    );
    e.has_byte(0x1010, Cell::Byte(initial(0x1010))).await;

    // the same from the watch: the view of that memory is the one shown again
    e.panel("w").await?;
    assert_eq!(e.h.app.editor.debug.section, Section::Watch);
    e.h.keys("M").await?;
    e.h.until("the view", |app| !app.editor.debug.is_focused())
        .await;
    assert!(cursor_is(&e.h.app, 0x1010));
    assert_eq!(memory_views(&e.h.app), 1);
    assert_eq!(e.h.app.editor.tree.views().count(), 2);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debug_memory_at_an_address_opens_there_and_reuses_the_view() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    e.start().await?;
    e.open(0x1234).await?;
    assert!(cursor_is(&e.h.app, 0x1234));
    assert_eq!(memory(&e.h.app).base, 0x1234);
    let (view, doc) = current_ref!(e.h.app.editor);
    assert_eq!(doc.hex().unwrap().cursor(view.id).offset, 0x1234);
    e.has_byte(0x1234, Cell::Byte(initial(0x1234))).await;
    // the first request finds the address the reference means, then the pages come
    let reads = e.fake.memory_reads();
    assert_eq!(reads[0], (0x1234, 1), "{reads:?}");
    assert!(reads[1..].iter().all(|(a, n)| a % 1024 == 0 && *n == 1024));

    // another address of the session goes to the same view
    e.h.keys(":debug-memory<space>0x1800<ret>").await?;
    e.cursor_at(0x1800).await;
    assert_eq!(
        e.h.app
            .editor
            .documents()
            .filter(|d| d.memory().is_some())
            .count(),
        1
    );
    assert_eq!(memory_views(&e.h.app), 1);
    let (view, doc) = current_ref!(e.h.app.editor);
    assert_eq!(doc.hex().unwrap().cursor(view.id).offset, 0x1800);
    e.has_byte(0x1800, Cell::Byte(initial(0x1800))).await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debug_memory_of_an_expression_evaluates_it_in_the_current_frame() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    e.start().await?;
    // a value that doesn't point to memory
    e.h.keys(":debug-memory<space>x<ret>").await?;
    e.says("x doesn't point to memory").await;
    assert!(memory_doc(&e.h.app).is_none());
    // one that does
    e.h.keys(":debug-memory<space>p<ret>").await?;
    e.cursor_at(0x1010).await;
    assert_eq!(memory(&e.h.app).base, 0x1010);
    let evaluations: Vec<Value> = e
        .fake
        .requests_named("evaluate")
        .into_iter()
        .map(|(_, args)| args)
        .collect();
    assert_eq!(evaluations.len(), 2);
    assert_eq!(evaluations[1]["expression"], "p");
    assert_eq!(evaluations[1]["context"], "watch");
    assert!(evaluations[1]["frameId"].is_i64(), "the current frame");
    e.has_byte(0x1010, Cell::Byte(initial(0x1010))).await;
    // one that fails is an error
    e.h.errors.clear();
    e.h.keys(":debug-memory<space>nope<ret>").await?;
    e.h.until("an error", |app| {
        app.editor
            .get_status()
            .is_some_and(|(m, _)| m.starts_with("nope:"))
    })
    .await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offsets_are_absolute_addresses_of_16_hex_digits() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    e.start().await?;
    e.open(0x1010).await?;
    e.has_byte(0x1010, Cell::Byte(initial(0x1010))).await;
    e.h.until("the addresses on screen", |app| {
        screen_has(app, "0x0000000000001010")
    })
    .await;
    let (view, doc) = current_ref!(e.h.app.editor);
    let hex = doc.hex().unwrap();
    assert_eq!(hex.len(), u64::MAX);
    let rows = Rows::new(hex.cursor(view.id).layout, hex.len(), view.area.width);
    assert_eq!(rows.offset_digits, 16);
    assert_eq!(rows.bytes_per_row, 16);
    // 16 bytes a row, from the row of the cursor
    let row = screen_rows(&e.h.app)
        .into_iter()
        .find(|row| row.contains("0x0000000000001010"))
        .unwrap();
    let hexes: Vec<String> = (0x10..0x20)
        .map(|i| format!("{:02x}", initial(0x1000 + i)))
        .collect();
    assert!(row.contains(&hexes.join(" ")[..23]), "{row:?}");
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gg_and_ge_are_refused_in_memory() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    e.start().await?;
    e.open(0x1234).await?;
    for keys in ["gg", "ge"] {
        e.h.keys(keys).await?;
        assert_eq!(
            status(&e.h.app),
            "memory has no first or last byte",
            "{keys}"
        );
        let (view, doc) = current_ref!(e.h.app.editor);
        assert_eq!(doc.hex().unwrap().cursor(view.id).offset, 0x1234, "{keys}");
    }
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Reading

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_pages_drawn_are_read_a_page_at_a_time() -> anyhow::Result<()> {
    // 64 KiB are readable, a screenful is not much of it
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .memory_with(0x10000, 0x10000, |i| i as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
    })
    .await?;
    e.start().await?;
    e.open(0x12000).await?;
    e.has_byte(0x12000, Cell::Byte(0)).await;
    e.h.pump_for(std::time::Duration::from_millis(300)).await;
    let pages = |e: &Env| -> Vec<u64> {
        e.fake
            .memory_reads()
            .into_iter()
            .filter(|(_, count)| *count == 1024)
            .map(|(address, _)| address)
            .collect()
    };
    let first = pages(&e);
    assert!(!first.is_empty() && first.len() <= 3, "{first:?}");
    assert!(first
        .iter()
        .all(|a| a % 1024 == 0 && (0x11000..0x13000).contains(a)));
    assert!(first.contains(&0x12000));
    // everything else was a request for one byte, to find where the reference points
    assert!(e
        .fake
        .memory_reads()
        .iter()
        .all(|(_, count)| *count == 1 || *count == 1024));
    let (view, _) = current_ref!(e.h.app.editor);
    let shown = (view.area.height as u64 - 1) * 16;
    assert!(first.len() as u64 * 1024 >= shown, "the screen is covered");

    // the cache has them: more drawing reads nothing, scrolling reads the new pages
    e.h.keys("2j").await?;
    e.h.pump_for(std::time::Duration::from_millis(200)).await;
    assert_eq!(pages(&e), first, "nothing new was drawn");
    e.h.keys("60j").await?;
    e.has_byte(
        0x12000 + 62 * 16,
        Cell::Byte(((0x2000 + 62 * 16) & 0xff) as u8),
    )
    .await;
    let all = pages(&e);
    assert!(all.len() <= 6, "{all:?}");
    let mut unique = all.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), all.len(), "no page twice: {all:?}");
    assert!(
        all.iter().all(|a| (0x11000..0x14000).contains(a)),
        "{all:?}"
    );
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreadable_bytes_are_cells_and_question_marks_never_errors() -> anyhow::Result<()> {
    // 256 readable bytes: the rest of the page, and the pages around, are not
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .write_memory()
            .memory_with(0x1000, 0x100, |i| i as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
    })
    .await?;
    e.start().await?;
    e.open(0x10f0).await?;
    e.has_byte(0x10ff, Cell::Byte(0xff)).await;
    e.has_byte(0x1100, Cell::Unreadable).await;
    assert_eq!(
        cells(&e.h.app, 0x10fe, 4),
        [
            Cell::Byte(0xfe),
            Cell::Byte(0xff),
            Cell::Unreadable,
            Cell::Unreadable
        ]
    );
    e.h.until("?? on screen", |app| screen_has(app, "??")).await;
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    // a whole page of nothing, in the same view
    e.h.keys(":debug-memory<space>0x9000<ret>").await?;
    e.cursor_at(0x9000).await;
    e.cells_are(0x9000, &[Cell::Unreadable; 16]).await;
    assert_eq!(memory_views(&e.h.app), 1);
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    e.h.keys(":debug-memory<space>0x10f0<ret>").await?;
    e.cursor_at(0x10f0).await;
    e.has_byte(0x10ff, Cell::Byte(0xff)).await;

    // there is nothing to overwrite there
    e.h.keys("j").await?; // 0x1100
    e.h.keys("i").await?;
    e.h.keys("a").await?;
    e.says("this memory is unreadable (??): nothing to overwrite there")
        .await;
    assert!(e.fake.requests_named("writeMemory").is_empty());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_that_fails_shows_question_marks_not_an_error() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .read_errors()
            .memory_with(0x1000, 0x400, |i| i as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
    })
    .await?;
    e.start().await?;
    // an address nothing is at: the adapter fails the read
    e.open(0x5000).await?;
    e.has_byte(0x5000, Cell::Unreadable).await;
    assert_eq!(memory(&e.h.app).base, 0x5000);
    e.h.until("?? on screen", |app| screen_has(app, "??")).await;
    e.h.pump_for(std::time::Duration::from_millis(200)).await;
    assert!(e.h.errors.is_empty(), "{:?}", e.h.errors);
    assert!(!e.fake.requests_named("readMemory").is_empty());
    // a readable one still reads
    e.h.keys(":debug-memory<space>0x1000<ret>").await?;
    e.has_byte(0x1001, Cell::Byte(1)).await;
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Refresh at each stop

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn memory_is_read_again_at_each_stop_and_what_changed_is_marked_until_the_next(
) -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .memory_with(0x1000, 0x1000, |i| (i % 251) as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
            .stop(Stop::new(3, "entry").scope(locals()).poke(0x1011, [0xee]))
            .stop(Stop::new(4, "entry").scope(locals()))
    })
    .await?;
    e.start().await?;
    e.open(0x1010).await?;
    e.has_byte(0x1011, Cell::Byte(17)).await;
    let page_reads = |e: &Env| {
        e.fake
            .memory_reads()
            .iter()
            .filter(|(address, count)| *address == 0x1000 && *count == 1024)
            .count()
    };
    assert_eq!(page_reads(&e), 1);

    e.step(1).await?;
    e.has_byte(0x1011, Cell::Changed(0xee)).await;
    assert_eq!(
        cells(&e.h.app, 0x1010, 3),
        [Cell::Byte(16), Cell::Changed(0xee), Cell::Byte(18)]
    );
    assert_eq!(page_reads(&e), 2, "read again at the stop");

    // the next stop reads again, and nothing changed since
    e.step(1).await?;
    e.h.until("the refresh", |app| {
        cells(app, 0x1010, 3) == [Cell::Byte(16), Cell::Byte(0xee), Cell::Byte(18)]
    })
    .await;
    assert_eq!(page_reads(&e), 3);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn while_running_the_view_keeps_the_last_stops_bytes_and_says_so() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .memory_with(0x1000, 0x1000, |i| (i % 251) as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
            .stop(Stop::new(3, "entry").scope(locals()).delay(1500))
    })
    .await?;
    e.start().await?;
    e.open(0x1010).await?;
    e.has_byte(0x1010, Cell::Byte(16)).await;
    {
        let (_, doc) = current_ref!(e.h.app.editor);
        assert_eq!(debug::memory::note(&e.h.app.editor, doc), None);
    }

    e.editor("l").await?;
    e.h.until("running", |app| {
        matches!(app.editor.debug.status, Status::Running)
    })
    .await;
    e.fake.set_memory(0x1010, &[0x55]);
    let reads = e.fake.memory_reads().len();
    e.h.pump_for(std::time::Duration::from_millis(300)).await;
    {
        let doc = memory_doc(&e.h.app).unwrap();
        assert_eq!(
            debug::memory::note(&e.h.app.editor, doc).as_deref(),
            Some("running · the last stop's bytes")
        );
    }
    e.h.until("the note on screen", |app| {
        screen_has(app, "running · the last stop's bytes")
    })
    .await;
    assert_eq!(
        cells(&e.h.app, 0x1010, 1),
        [Cell::Byte(16)],
        "the last stop's"
    );
    assert_eq!(
        e.fake.memory_reads().len(),
        reads,
        "nothing is read while it runs"
    );

    // the stop reads what is there now
    e.settled(2).await;
    e.has_byte(0x1010, Cell::Changed(0x55)).await;
    let doc = memory_doc(&e.h.app).unwrap();
    assert_eq!(debug::memory::note(&e.h.app.editor, doc), None);
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Overwriting

/// Opens memory at `address` and waits for its byte.
async fn open_read(e: &mut Env, address: u64) -> anyhow::Result<()> {
    e.start().await?;
    e.open(address).await?;
    e.has_byte(address, Cell::Byte(initial(address))).await;
    Ok(())
}

/// Waits for the error `:w` (or what it ran) says: errors of commands are `'name': message`.
async fn command_error(e: &mut Env, command: &str, message: &str) {
    let expected = format!("'{command}': {message}");
    let start = std::time::Instant::now();
    while e.h.errors.last() != Some(&expected) {
        assert!(
            start.elapsed() < super::harness::DEADLINE,
            "wanted the error {expected:?}, got {:?}",
            e.h.errors
        );
        e.h.pump_for(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overwriting_then_w_sends_the_changed_bytes_and_u_writes_the_old_ones_back(
) -> anyhow::Result<()> {
    let mut e = env(program).await?;
    open_read(&mut e, 0x1010).await?;

    // nothing overwritten: nothing to write
    e.h.keys(":w<ret>").await?;
    e.says("no overwritten bytes to write").await;
    assert!(e.fake.requests_named("writeMemory").is_empty());

    // two bytes, not written yet
    e.h.keys("iabcd<esc>").await?;
    e.cells_are(0x1010, &bytes(&[0xab, 0xcd])).await;
    assert_eq!(e.fake.memory_at(0x1010, 2), [Some(16), Some(17)]);
    assert!(e.fake.requests_named("writeMemory").is_empty());

    e.h.keys(":w<ret>").await?;
    e.says("wrote 2 bytes to memory: the debugger wrote 2")
        .await;
    assert_eq!(e.fake.memory_writes(), [(0x1010, vec![0xab, 0xcd])]);
    assert_eq!(e.fake.memory_at(0x1010, 2), [Some(0xab), Some(0xcd)]);
    assert_eq!(cells(&e.h.app, 0x1010, 2), bytes(&[0xab, 0xcd]));
    // written: nothing left to write
    e.h.keys(":w<ret>").await?;
    e.says("no overwritten bytes to write").await;
    assert_eq!(e.fake.memory_writes().len(), 1);

    // u writes the old bytes back at once
    e.h.keys("u").await?;
    e.says("wrote back 2 bytes to memory: the debugger wrote 2")
        .await;
    assert_eq!(e.fake.memory_writes()[1], (0x1010, vec![16, 17]));
    assert_eq!(e.fake.memory_at(0x1010, 2), [Some(16), Some(17)]);
    e.cells_are(0x1010, &bytes(&[16, 17])).await;
    // and U the new ones
    e.h.keys("U").await?;
    e.h.until("the redo write", |_| e.fake.memory_writes().len() == 3)
        .await;
    assert_eq!(e.fake.memory_writes()[2], (0x1010, vec![0xab, 0xcd]));
    assert_eq!(e.fake.memory_at(0x1010, 2), [Some(0xab), Some(0xcd)]);
    e.cells_are(0x1010, &bytes(&[0xab, 0xcd])).await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_overwrite_not_written_yet_is_undone_without_a_request() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    open_read(&mut e, 0x1010).await?;
    e.h.keys("iab<esc>").await?;
    e.cells_are(0x1010, &bytes(&[0xab])).await;
    e.h.keys("u").await?;
    e.cells_are(0x1010, &bytes(&[16])).await;
    e.h.pump_for(std::time::Duration::from_millis(200)).await;
    assert!(e.fake.requests_named("writeMemory").is_empty());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w_says_how_many_bytes_the_debugger_wrote_and_a_path_is_refused() -> anyhow::Result<()> {
    let mut e = env(|p| program(p).bytes_written(7)).await?;
    open_read(&mut e, 0x1010).await?;
    e.h.keys("iab<esc>").await?;
    e.cells_are(0x1010, &bytes(&[0xab])).await;
    e.h.keys(":w<ret>").await?;
    e.says("wrote 1 byte to memory: the debugger wrote 7").await;

    e.h.keys(":w<space>some/path<ret>").await?;
    command_error(
        &mut e,
        "write",
        "memory isn't a file: it can't be written to one (:w writes it to the program)",
    )
    .await;
    assert!(!e.dir.join("some").exists());
    assert_eq!(e.fake.memory_writes().len(), 1);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_write_is_an_error_and_keeps_the_overwrite() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    open_read(&mut e, 0x1010).await?;
    e.h.keys("iabcd<esc>").await?;
    e.cells_are(0x1010, &bytes(&[0xab, 0xcd])).await;
    // the memory went away meanwhile (the view still has what it read)
    e.fake.unmap_memory(0x1010, 2);
    e.h.keys(":w<ret>").await?;
    e.h.until("the error", |app| {
        app.editor
            .get_status()
            .is_some_and(|(message, _)| message.starts_with("writing memory failed at 0x1010: "))
    })
    .await;
    assert_eq!(e.fake.memory_writes().len(), 1);
    assert_eq!(e.fake.memory_at(0x1010, 2), [None, None]);
    // still overwritten, not written
    let doc = memory_doc(&e.h.app).unwrap();
    assert_eq!(
        doc.hex().unwrap().edit_runs(None),
        [(0x1010, vec![0xab, 0xcd])]
    );
    assert_eq!(cells(&e.h.app, 0x1010, 2), bytes(&[0xab, 0xcd]));
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_adapter_that_cannot_write_memory_refuses_overwriting() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .memory_with(0x1000, 0x1000, |i| (i % 251) as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
    })
    .await?;
    open_read(&mut e, 0x1010).await?;
    assert!(!memory(&e.h.app).can_write);
    e.h.keys("i").await?;
    e.says("this debugger (fake) can't write memory").await;
    let (view, doc) = current_ref!(e.h.app.editor);
    assert!(!doc.hex().unwrap().cursor(view.id).overwrite);
    // the typed keys are the view's, which reads
    e.h.keys("ab").await?;
    e.h.pump_for(std::time::Duration::from_millis(100)).await;
    assert_eq!(cells(&e.h.app, 0x1010, 1), bytes(&[16]));
    e.h.keys(":w<ret>").await?;
    command_error(&mut e, "write", "this debugger (fake) can't write memory").await;
    assert!(e.fake.requests_named("writeMemory").is_empty());
    e.h.finish().await
}

/// `i` in the memory view, and the status it leaves: the cursor must not be overwriting.
async fn refused(e: &mut Env, why: &str) -> anyhow::Result<()> {
    e.h.keys("<esc>").await?;
    e.h.errors.clear();
    e.h.keys("i").await?;
    e.says(why).await;
    let (view, doc) = current_ref!(e.h.app.editor);
    assert!(!doc.hex().unwrap().cursor(view.id).overwrite, "{why}");
    e.h.keys(":w<ret>").await?;
    command_error(e, "write", why).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_are_refused_while_running_and_in_rewind() -> anyhow::Result<()> {
    let mut e = env(|p| {
        Script::new(p)
            .read_memory()
            .write_memory()
            .memory_with(0x1000, 0x1000, |i| (i % 251) as u8)
            .stop(Stop::new(2, "entry").scope(locals()))
            .stop(Stop::new(3, "entry").scope(locals()))
            .stop(Stop::new(4, "entry").scope(locals()).delay(60_000))
    })
    .await?;
    open_read(&mut e, 0x1010).await?;
    e.step(1).await?;
    // rewind: nothing executes
    e.editor("h").await?;
    assert!(e.h.app.editor.debug.is_rewinding());
    refused(
        &mut e,
        "REWIND: nothing is executing, so memory can't be changed",
    )
    .await?;
    // back live, it is allowed again
    e.editor("p").await?;
    e.h.keys("i").await?;
    e.h.until("overwriting", |app| {
        let (view, doc) = current_ref!(app.editor);
        doc.hex().unwrap().cursor(view.id).overwrite
    })
    .await;
    e.h.keys("<esc>").await?;

    // running
    e.editor("l").await?;
    e.h.until("running", |app| {
        matches!(app.editor.debug.status, Status::Running)
            && memory_doc(app)
                .and_then(|doc| debug::memory::note(&app.editor, doc))
                .is_some_and(|note| note.starts_with("running"))
    })
    .await;
    refused(
        &mut e,
        "the program is running: pause it to overwrite memory",
    )
    .await?;
    assert!(e.fake.requests_named("writeMemory").is_empty());
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_are_refused_while_playing_and_fast_forwarding() -> anyhow::Result<()> {
    // a program with many slow steps, so playback is under way while the keys are typed
    let mut e = env(|p| {
        (0..40)
            .fold(
                Script::new(p)
                    .read_memory()
                    .write_memory()
                    .memory_with(0x1000, 0x1000, |i| (i % 251) as u8),
                |script, i| script.stop(Stop::new(2 + i % 20, "entry").scope(locals()).delay(300)),
            )
            .run_forever()
    })
    .await?;
    e.launch()?;
    e.h.until("the first stop", |app| {
        matches!(app.editor.debug.status, Status::Stopped { .. })
    })
    .await;
    e.settled(1).await;
    e.open(0x1010).await?;
    e.has_byte(0x1010, Cell::Byte(16)).await;

    // fast forward needs something to wait for: a watch
    e.panel("2ax<ret>").await?;
    e.h.until("the watch", |app| {
        texts(app, Section::Watch)
            .iter()
            .any(|t| t.starts_with("x = "))
    })
    .await;
    e.editor("<space>").await?;
    e.h.until("playing", |app| {
        app.editor.debug.spectator.mode == Mode::Playing
    })
    .await;
    refused(&mut e, "playing: pause to overwrite memory").await?;
    // pause
    e.editor("<space>").await?;
    e.h.until("paused", |app| {
        app.editor.debug.spectator.mode == Mode::Paused
    })
    .await;

    e.editor("f").await?;
    e.h.until("fast forwarding", |app| {
        app.editor.debug.spectator.mode == Mode::FastForward
    })
    .await;
    refused(&mut e, "fast forwarding: stop it to overwrite memory").await?;
    assert!(e.fake.requests_named("writeMemory").is_empty());
    e.editor("f").await?;
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// Rewind

/// The rows the focused memory view shows: (first byte shown, its height in bytes).
fn shown(app: &Application) -> (u64, u64) {
    let (view, doc) = current_ref!(app.editor);
    let hex = doc.hex().unwrap();
    let cursor = hex.cursor(view.id);
    let rows = Rows::new(cursor.layout, hex.len(), view.area.width);
    let per_row = rows.bytes_per_row as u64;
    (
        rows.row(cursor.top) * per_row,
        (view.area.height as u64 - 1) * per_row,
    )
}

/// The windows recorded at the last stop.
fn last_windows(app: &Application) -> &[vortex_view::debug::history::MemoryWindow] {
    &app.editor
        .debug
        .spectator
        .history
        .stops
        .back()
        .expect("a recorded stop")
        .memory
}

/// A 16 KiB memory from 0x10000, with a change at the 3rd and the 4th stops.
fn recorded_program(p: &str) -> Script {
    Script::new(p)
        .read_memory()
        .memory_with(0x10000, 0x4000, |i| (i % 241) as u8)
        .stop(Stop::new(2, "entry").scope(locals()))
        .stop(Stop::new(3, "entry").scope(locals()))
        .stop(Stop::new(4, "entry").scope(locals()).poke(0x12003, [0xee]))
        .stop(Stop::new(5, "entry").scope(locals()))
}

fn original(address: u64) -> u8 {
    ((address - 0x10000) % 241) as u8
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_stop_records_the_window_of_each_memory_view_with_a_margin() -> anyhow::Result<()> {
    let mut e = env(recorded_program).await?;
    e.start().await?;
    e.stay_in_debug_mode().await?;
    assert!(last_windows(&e.h.app).is_empty(), "no memory view yet");
    e.open(0x12000).await?;
    e.has_byte(0x12000, Cell::Byte(original(0x12000))).await;
    let doc = memory_doc(&e.h.app).unwrap().id();
    let (top, height) = shown(&e.h.app);
    assert!(height + 1024 <= 4096);

    e.step(1).await?;
    let windows = last_windows(&e.h.app);
    assert_eq!(windows.len(), 1);
    let window = &windows[0];
    assert_eq!(window.doc, doc);
    assert_eq!(
        window.start,
        top - 512,
        "what is shown, and 512 bytes before"
    );
    assert_eq!(window.bytes.len() as u64, height + 1024, "and after");
    assert_eq!(
        window.bytes,
        e.fake.memory_at(window.start, window.bytes.len()),
        "the bytes of the program"
    );
    // it was read a page at a time
    let reads: Vec<_> = e
        .fake
        .memory_reads()
        .into_iter()
        .filter(|(a, _)| *a >= window.start && *a < window.start + window.bytes.len() as u64)
        .collect();
    assert!(reads.iter().all(|(_, count)| *count <= 1024), "{reads:?}");
    // a window that doesn't fit its pages is cut at the page borders: more than one request
    assert!(reads.len() >= 2);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewind_shows_the_recorded_bytes_and_never_live_memory() -> anyhow::Result<()> {
    let mut e = env(recorded_program).await?;
    e.start().await?;
    e.stay_in_debug_mode().await?;
    e.open(0x12000).await?;
    e.has_byte(0x12000, Cell::Byte(original(0x12000))).await;
    let (top, height) = shown(&e.h.app);
    let (start, end) = (top - 512, top + height + 512);
    // stops 2, 3 (it pokes) and 4
    e.step(3).await?;
    e.cells_are(0x12003, &[Cell::Byte(0xee)]).await;
    // the program's memory moves on, the history doesn't
    e.fake.set_memory(0x12005, &[0x77]);
    e.fake.set_memory(start, &[0x66]);
    let reads = e.fake.memory_reads().len();

    // stop 3: the window is there, the byte that changed is marked
    e.editor("h").await?;
    assert!(e.h.app.editor.debug.is_rewinding());
    e.h.until("the recorded stop 3", |app| {
        memory(app).recorded.as_ref().is_some_and(|r| r.step == 3)
    })
    .await;
    assert_eq!(
        cells(&e.h.app, 0x12002, 4),
        [
            Cell::Byte(original(0x12002)),
            Cell::Changed(0xee),
            Cell::Byte(original(0x12004)),
            Cell::Byte(original(0x12005)),
        ],
        "not the live 0x77"
    );
    // outside the window: not recorded, never live memory
    assert_eq!(cells(&e.h.app, start - 1, 1), [Cell::NotRecorded]);
    assert_eq!(cells(&e.h.app, end, 1), [Cell::NotRecorded]);
    assert_eq!(cells(&e.h.app, 0x10000, 1), [Cell::NotRecorded]);
    assert_eq!(cells(&e.h.app, start, 1), bytes(&[original(start)]));
    assert_eq!(cells(&e.h.app, end - 1, 1), bytes(&[original(end - 1)]));
    // what is shown is all in the window
    assert!(!screen_has(&e.h.app, "··"));
    {
        let doc = memory_doc(&e.h.app).unwrap();
        let note = debug::memory::note(&e.h.app.editor, doc).unwrap();
        assert!(note.ends_with(" recorded at this stop"), "{note}");
        assert!(!note.starts_with("not recorded"), "{note}");
    }
    // the panel says so too
    e.h.until("the marker on screen", |app| {
        screen_has(app, "REWIND · step 3/4")
    })
    .await;

    // stop 2: the same bytes, nothing marked
    e.editor("h").await?;
    e.h.until("the recorded stop 2", |app| {
        memory(app).recorded.as_ref().is_some_and(|r| r.step == 2)
    })
    .await;
    assert_eq!(
        cells(&e.h.app, 0x12002, 3),
        bytes(&[original(0x12002), original(0x12003), original(0x12004)])
    );

    // stop 1 was before the view: nothing recorded for it
    e.editor("h").await?;
    e.h.until("the recorded stop 1", |app| {
        memory(app).recorded.as_ref().is_some_and(|r| r.step == 1)
    })
    .await;
    assert_eq!(
        cells(&e.h.app, 0x12000, 4),
        [Cell::NotRecorded; 4],
        "not live memory"
    );
    {
        let doc = memory_doc(&e.h.app).unwrap();
        let note = debug::memory::note(&e.h.app.editor, doc).unwrap();
        assert_eq!(note, "not recorded at this stop");
    }
    // drawn as dots
    e.h.until("·· on screen", |app| screen_has(app, "··")).await;

    // forward again, and live: the last stop is the live one
    e.editor("ll").await?;
    e.h.until("the recorded stop 3", |app| {
        memory(app).recorded.as_ref().is_some_and(|r| r.step == 3)
    })
    .await;
    e.editor("l").await?;
    e.h.until("live", |app| memory(app).recorded.is_none())
        .await;
    e.cells_are(0x12003, &[Cell::Byte(0xee)]).await;
    // all that without reading memory
    assert_eq!(e.fake.memory_reads().len(), reads);

    // L from anywhere
    e.editor("hh").await?;
    e.h.until("rewinding", |app| memory(app).recorded.is_some())
        .await;
    e.editor("p").await?;
    e.h.until("live", |app| memory(app).recorded.is_none())
        .await;
    e.h.until("no marker", |app| !screen_has(app, "REWIND"))
        .await;
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_memory_in_rewind_is_refused() -> anyhow::Result<()> {
    let mut e = env(program).await?;
    e.start().await?;
    e.step(1).await?;
    e.editor("h").await?;
    assert!(e.h.app.editor.debug.is_rewinding());
    let why = "REWIND: a recorded stop's memory can't be read any more";
    e.h.keys(":debug-memory<space>0x1000<ret>").await?;
    assert_eq!(status(&e.h.app), why);
    e.panel("jjM").await?;
    assert_eq!(status(&e.h.app), why);
    assert!(memory_doc(&e.h.app).is_none());
    e.h.finish().await
}

// ---------------------------------------------------------------------------------------------
// The capture settings

fn settings_texts(app: &Application) -> Vec<String> {
    panel::settings_rows(&app.editor)
        .iter()
        .map(|row| row.text())
        .collect()
}

fn memory_cap(app: &Application) -> Option<usize> {
    app.editor.debug.spectator.caps.memory
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_memory_window_setting_changes_the_cap_and_the_recorded_windows() -> anyhow::Result<()>
{
    let mut e = env(|p| {
        let script = recorded_program(p);
        (0..6).fold(script, |s, i| {
            s.stop(Stop::new(6 + i, "entry").scope(locals()))
        })
    })
    .await?;
    assert_eq!(
        Setting::ALL.map(|s| s.title()),
        ["depth", "collection size", "history", "memory window"]
    );
    e.start().await?;
    e.stay_in_debug_mode().await?;
    e.open(0x12000).await?;
    e.has_byte(0x12000, Cell::Byte(original(0x12000))).await;
    let (top, height) = shown(&e.h.app);
    assert_eq!(memory_cap(&e.h.app), Some(4096), "the default");

    // the settings have the row, on its default
    e.editor("c").await?;
    let rows = settings_texts(&e.h.app);
    assert!(
        rows.iter()
            .any(|r| r.starts_with("memory window") && r.contains("4 KB per view")),
        "{rows:?}"
    );
    // to the row, a step down: 4096 -> 1024
    e.h.keys("jjjh").await?;
    assert_eq!(e.h.app.editor.debug.spectator.settings, Some(3));
    assert_eq!(memory_cap(&e.h.app), Some(1024));
    assert!(settings_texts(&e.h.app)
        .iter()
        .any(|r| r.starts_with("memory window") && r.contains("1 KB per view")));
    e.h.keys("<esc>").await?;
    e.step(1).await?;
    // the middle of what is shown, 1 KB of it
    let window = &last_windows(&e.h.app)[0];
    let middle = top + height / 2;
    assert_eq!(window.bytes.len(), 1024);
    assert_eq!(window.start, middle - 512);
    assert_eq!(window.bytes, e.fake.memory_at(window.start, 1024));

    // 256
    e.editor("c").await?;
    e.h.keys("jjjh").await?;
    assert_eq!(memory_cap(&e.h.app), Some(256));
    e.h.keys("<esc>").await?;
    e.step(1).await?;
    assert_eq!(last_windows(&e.h.app)[0].bytes.len(), 256);

    // off: no windows
    e.editor("c").await?;
    e.h.keys("jjjh").await?;
    assert_eq!(memory_cap(&e.h.app), Some(0));
    assert!(settings_texts(&e.h.app)
        .iter()
        .any(|r| r.starts_with("memory window") && r.contains("off")));
    e.h.keys("<esc>").await?;
    e.step(1).await?;
    assert!(last_windows(&e.h.app).is_empty());

    // unlimited: all that is shown and the margins
    e.editor("c").await?;
    e.h.keys("jjjllllll").await?;
    assert_eq!(memory_cap(&e.h.app), None);
    e.h.keys("<esc>").await?;
    e.step(1).await?;
    let window = &last_windows(&e.h.app)[0];
    assert_eq!(window.start, top - 512);
    assert_eq!(window.bytes.len() as u64, height + 1024);
    e.h.finish().await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cost_estimate_counts_memory_views() -> anyhow::Result<()> {
    let mut e = env(recorded_program).await?;
    e.start().await?;
    // an estimate is for a history without stops: turn it off (history row, down to off)
    e.editor("c").await?;
    e.h.keys("jjhhhhh").await?;
    assert_eq!(e.h.app.editor.debug.spectator.caps.history, Some(0));
    assert!(e.h.app.editor.debug.spectator.history.stops.is_empty());
    let cost = |app: &Application| {
        settings_texts(app)
            .into_iter()
            .find(|row| row.starts_with("cost"))
            .expect("a cost row")
    };
    let requests = |row: &str| -> usize {
        row.split("about ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("no estimate in {row:?}"))
    };
    let without = cost(&e.h.app);
    assert!(
        without.contains("estimated") && !without.contains("memory view"),
        "{without}"
    );
    e.h.keys("<esc>").await?;

    e.open(0x12000).await?;
    e.has_byte(0x12000, Cell::Byte(original(0x12000))).await;
    let one = cost(&e.h.app);
    assert!(
        one.contains(", 1 memory view,") || one.contains(", 1 memory view "),
        "{one}"
    );
    // the default window of 4 KB, read a page at a time: 4 requests
    assert_eq!(requests(&one), requests(&without) + 4, "{one} / {without}");

    // a bigger window costs more requests; none, none
    e.editor("c").await?;
    e.h.keys("jjjl").await?;
    assert_eq!(memory_cap(&e.h.app), Some(16384));
    assert_eq!(requests(&cost(&e.h.app)), requests(&without) + 16);
    e.h.keys("hhhh").await?;
    assert_eq!(memory_cap(&e.h.app), Some(0));
    assert_eq!(requests(&cost(&e.h.app)), requests(&without));
    e.h.finish().await
}
