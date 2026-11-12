//! The bytes of a document shown in the hex view (this fork).
//!
//! A [`HexBuffer`] never loads its file: it reads the bytes it is asked for, through a small
//! window cache, so a file of any size opens at once and only what is on screen is read. The
//! bytes overwritten in the view are kept apart, in a sparse map over the file, until they are
//! written back in place. The size never changes: the view overwrites bytes, it never inserts
//! or deletes them.
//!
//! The buffer also keeps each view's cursor ([`HexCursor`]) and the last search, the way a
//! document keeps its selections.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    fs::File,
    io,
    ops::Range,
    path::Path,
};

use crate::ViewId;

pub mod memory;
pub use memory::{Cell, MemorySource};

/// Whether the start of a file looks like the bytes of a binary file rather than text: a NUL in
/// its first KiB, or the signature of a PDF or a PNG. A byte order mark means text in a UTF
/// encoding, whatever follows.
pub fn is_binary(buffer: &[u8]) -> bool {
    // UTF-32 BOMs must be checked before UTF-16 (their BOMs overlap).
    const BYTE_ORDER_MARKS: &[&[u8]] = &[
        &[0xEF, 0xBB, 0xBF],       // UTF-8
        &[0x00, 0x00, 0xFE, 0xFF], // UTF-32BE
        &[0xFF, 0xFE, 0x00, 0x00], // UTF-32LE
        &[0xFE, 0xFF],             // UTF-16BE
        &[0xFF, 0xFE],             // UTF-16LE
    ];

    if BYTE_ORDER_MARKS.iter().any(|bom| buffer.starts_with(bom)) {
        return false;
    }

    let scan = &buffer[..buffer.len().min(1024)];
    scan.contains(&0) || buffer.starts_with(b"%PDF") || buffer.starts_with(b"\x89PNG")
}

/// Whether the file at `path` looks binary ([`is_binary`] on its first KiB). A file that can't be
/// read is not.
pub fn file_is_binary(path: &Path) -> bool {
    use io::Read;
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut start = Vec::with_capacity(1024);
    match file.take(1024).read_to_end(&mut start) {
        Ok(_) => is_binary(&start),
        Err(_) => false,
    }
}

/// The column of the view the cursor is in, where typing goes in overwrite mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Column {
    /// The bytes as hex digits or bits.
    #[default]
    Bytes,
    /// The bytes as ASCII characters.
    Ascii,
}

/// How the bytes column shows a byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Layout {
    /// Two hex digits.
    #[default]
    Hex,
    /// Eight bits.
    Bin,
}

impl Layout {
    /// The number of digits of a byte: a hex digit is a nibble, a bin digit a bit.
    pub fn digits(self) -> u8 {
        match self {
            Layout::Hex => 2,
            Layout::Bin => 8,
        }
    }
}

/// The cursor of one view of a [`HexBuffer`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HexCursor {
    /// The byte under the cursor.
    pub offset: u64,
    /// The other end of the selection, while selecting (`v`): the selection is every byte from
    /// it to `offset`, both included.
    pub anchor: Option<u64>,
    pub column: Column,
    pub layout: Layout,
    /// The digit of the byte under the cursor that the next typed digit overwrites, in overwrite
    /// mode: a nibble in hex (0 is the high one), a bit in bin (0 is the highest).
    pub digit: u8,
    /// Whether typing overwrites bytes (`OVR`) rather than being refused (`RO`).
    pub overwrite: bool,
    /// The offset of the first byte on screen (rounded down to a row when drawn, as the width
    /// of a row depends on the width of the view).
    pub top: u64,
}

impl HexCursor {
    /// The selected bytes: the selection while selecting, else the byte under the cursor.
    pub fn selection(&self) -> Range<u64> {
        let anchor = self.anchor.unwrap_or(self.offset);
        anchor.min(self.offset)..anchor.max(self.offset) + 1
    }
}

/// The layout of the rows of a view: how many bytes a row holds and where each part goes.
///
/// ```text
/// 0x00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  .PNG........IHDR
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rows {
    pub layout: Layout,
    /// The number of hex digits of the offsets (after `0x`): enough for the last offset of the
    /// file, at least [`Rows::MIN_OFFSET_DIGITS`].
    pub offset_digits: usize,
    pub bytes_per_row: usize,
}

impl Rows {
    pub const MIN_OFFSET_DIGITS: usize = 8;
    /// The space between the columns (offsets, bytes, ASCII).
    pub const GAP: usize = 2;

    /// The rows of a file of `len` bytes in a view `width` cells wide: 16 bytes per row in hex
    /// and 8 in bin, fewer when the view is too narrow for them (8 in hex; 4, 2 or 1 in bin).
    pub fn new(layout: Layout, len: u64, width: u16) -> Self {
        let last = len.saturating_sub(1);
        let offset_digits =
            ((64 - last.leading_zeros() as usize).div_ceil(4)).max(Self::MIN_OFFSET_DIGITS);
        let candidates: &[usize] = match layout {
            Layout::Hex => &[16, 8],
            Layout::Bin => &[8, 4, 2, 1],
        };
        let mut rows = Self {
            layout,
            offset_digits,
            bytes_per_row: *candidates.last().unwrap(),
        };
        for &bytes_per_row in candidates {
            let candidate = Self {
                bytes_per_row,
                ..rows
            };
            if candidate.width() <= width as usize {
                rows = candidate;
                break;
            }
        }
        rows
    }

    /// The bytes of a group: groups are separated by one more space.
    pub fn group(&self) -> usize {
        match self.layout {
            Layout::Hex => 8,
            Layout::Bin => 4,
        }
    }

    /// The width of the offsets, `0x` included.
    pub fn offset_width(&self) -> usize {
        2 + self.offset_digits
    }

    /// The width of a byte in the bytes column.
    pub fn byte_width(&self) -> usize {
        self.layout.digits() as usize
    }

    /// Where the `i`th byte of a row starts, from the left of the row.
    pub fn byte_x(&self, i: usize) -> usize {
        self.offset_width() + Self::GAP + i * (self.byte_width() + 1) + i / self.group()
    }

    /// Where the ASCII column starts.
    pub fn ascii_x(&self) -> usize {
        let n = self.bytes_per_row;
        self.byte_x(n - 1) + self.byte_width() + Self::GAP
    }

    /// The width of a full row.
    pub fn width(&self) -> usize {
        self.ascii_x() + self.bytes_per_row
    }

    /// The row of the byte at `offset`.
    pub fn row(&self, offset: u64) -> u64 {
        offset / self.bytes_per_row as u64
    }

    /// The number of rows of a file of `len` bytes (one for an empty file).
    pub fn count(&self, len: u64) -> u64 {
        len.div_ceil(self.bytes_per_row as u64).max(1)
    }
}

/// One byte overwritten: its offset, and its values before and after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edit {
    offset: u64,
    old: u8,
    new: u8,
}

/// The bytes overwritten in one stay in overwrite mode.
#[derive(Debug, Clone, Default)]
struct Step {
    edits: Vec<Edit>,
    /// The step was written to the debugged program's memory: undoing or redoing it writes
    /// memory again.
    written: bool,
}

/// What `u` or `U` changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undone {
    /// Where to put the cursor: the first byte of the step undone, the last of a step redone.
    pub offset: u64,
    /// The step had been written to memory.
    pub written: bool,
    /// The bytes it changed.
    pub offsets: Vec<u64>,
}

/// Where the bytes come from.
enum Source {
    File(File),
    /// Bytes held by the buffer, for previews and tests. They can't be written.
    Bytes(Vec<u8>),
    /// The memory of a debugged program, read through its debug adapter.
    Debuggee(Box<MemorySource>),
}

/// The last window of the file read, which drawing and moving mostly read again.
#[derive(Default)]
struct Window {
    start: u64,
    bytes: Vec<u8>,
}

/// The size of the window cache. A screen of rows is a few KiB.
const WINDOW: u64 = 64 * 1024;
/// The size of the chunks a search reads.
const SEARCH_CHUNK: usize = 1024 * 1024;

pub struct HexBuffer {
    source: Source,
    len: u64,
    window: RefCell<Window>,
    /// The bytes overwritten and not yet written, by offset: only those that differ from the
    /// file, so the buffer is modified exactly when this isn't empty.
    edits: BTreeMap<u64, u8>,
    /// The steps that `u` undoes and `U` redoes. A step is the bytes overwritten in one stay in
    /// overwrite mode, like an insert in a text buffer.
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The bytes overwritten in the stay in overwrite mode under way.
    pending: Vec<Edit>,
    cursors: HashMap<ViewId, HexCursor>,
    /// The bytes searched for last (`/`), which `n` and `N` look for and the view highlights.
    pub search: Option<Vec<u8>>,
}

impl std::fmt::Debug for HexBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HexBuffer")
            .field("len", &self.len)
            .field("edits", &self.edits.len())
            .finish_non_exhaustive()
    }
}

impl HexBuffer {
    /// Opens the file at `path` without reading it.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self::new(Source::File(file), len))
    }

    /// A buffer of bytes in memory, which can't be written.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let len = bytes.len() as u64;
        Self::new(Source::Bytes(bytes), len)
    }

    /// The memory of a debugged program: the whole address space, its offsets the addresses.
    pub fn debuggee(memory: MemorySource) -> Self {
        Self::new(Source::Debuggee(Box::new(memory)), u64::MAX)
    }

    /// The debugged program's memory, for a buffer that shows it.
    pub fn memory(&self) -> Option<&MemorySource> {
        match &self.source {
            Source::Debuggee(memory) => Some(memory),
            _ => None,
        }
    }

    pub fn memory_mut(&mut self) -> Option<&mut MemorySource> {
        match &mut self.source {
            Source::Debuggee(memory) => Some(memory),
            _ => None,
        }
    }

    pub fn is_memory(&self) -> bool {
        matches!(self.source, Source::Debuggee(_))
    }

    fn new(source: Source, len: u64) -> Self {
        Self {
            source,
            len,
            window: RefCell::default(),
            edits: BTreeMap::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            pending: Vec::new(),
            cursors: HashMap::new(),
            search: None,
        }
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether some bytes are overwritten and not written yet.
    pub fn is_modified(&self) -> bool {
        !self.edits.is_empty()
    }

    /// The number of bytes overwritten and not written yet.
    pub fn modified_bytes(&self) -> usize {
        self.edits.len()
    }

    /// Whether the byte at `offset` is overwritten and not written yet.
    pub fn is_edited(&self, offset: u64) -> bool {
        self.edits.contains_key(&offset)
    }

    // Cursors

    /// The cursor of `view` (at the start, for a view that never moved in this buffer).
    pub fn cursor(&self, view: ViewId) -> HexCursor {
        self.cursors.get(&view).cloned().unwrap_or_default()
    }

    pub fn cursor_mut(&mut self, view: ViewId) -> &mut HexCursor {
        self.cursors.entry(view).or_default()
    }

    pub fn remove_view(&mut self, view: ViewId) {
        self.cursors.remove(&view);
    }

    /// Keeps the cursors of `other` (the buffer this one replaces when the file is read again),
    /// moved back inside this buffer.
    pub fn keep_cursors(&mut self, other: &HexBuffer) {
        let last = self.len.saturating_sub(1);
        self.cursors = other
            .cursors
            .iter()
            .map(|(view, cursor)| {
                let mut cursor = cursor.clone();
                cursor.offset = cursor.offset.min(last);
                cursor.top = cursor.top.min(last);
                cursor.anchor = None;
                cursor.overwrite = false;
                (*view, cursor)
            })
            .collect();
        self.search = other.search.clone();
    }

    // Reading

    /// Reads the bytes from `offset` into `buf`, overwritten bytes included, and returns how
    /// many there were (fewer than asked at the end of the file).
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> usize {
        let n = self.read_file(offset, buf);
        let end = offset + n as u64;
        for (&at, &byte) in self.edits.range(offset..end) {
            buf[(at - offset) as usize] = byte;
        }
        n
    }

    /// The bytes in `range` (cut at the end of the file), overwritten bytes included.
    pub fn bytes(&self, range: Range<u64>) -> Vec<u8> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        let mut bytes = vec![0; (end - start) as usize];
        let n = self.read(start, &mut bytes);
        bytes.truncate(n);
        bytes
    }

    /// The byte at `offset`, overwritten or not.
    pub fn byte(&self, offset: u64) -> Option<u8> {
        let mut byte = [0];
        (self.read(offset, &mut byte) == 1).then_some(byte[0])
    }

    /// The byte at `offset` in the file, without the overwritten bytes.
    fn file_byte(&self, offset: u64) -> Option<u8> {
        let mut byte = [0];
        (self.read_file(offset, &mut byte) == 1).then_some(byte[0])
    }

    /// What the bytes in `range` are, as drawn: overwritten bytes included, and in memory the
    /// bytes not read yet, unreadable or not recorded.
    pub fn cells(&self, range: Range<u64>) -> Vec<Cell> {
        let end = range.end.min(self.len);
        let start = range.start.min(end);
        let edited = |offset: u64, cell: Cell| match (self.edits.get(&offset), cell) {
            (Some(&byte), Cell::Byte(_) | Cell::Changed(_)) => Cell::Byte(byte),
            _ => cell,
        };
        match &self.source {
            Source::Debuggee(memory) => (start..end)
                .map(|offset| edited(offset, memory.cell(offset)))
                .collect(),
            _ => self.bytes(start..end).into_iter().map(Cell::Byte).collect(),
        }
    }

    /// Reads from the file, through the window cache for small reads.
    fn read_file(&self, offset: u64, buf: &mut [u8]) -> usize {
        if offset >= self.len || buf.is_empty() {
            return 0;
        }
        let want = buf.len().min((self.len - offset) as usize);
        let buf = &mut buf[..want];
        match &self.source {
            Source::Bytes(bytes) => {
                let start = offset as usize;
                buf.copy_from_slice(&bytes[start..start + want]);
                want
            }
            // the bytes read, up to the first one that isn't
            Source::Debuggee(memory) => {
                for (i, slot) in buf.iter_mut().enumerate() {
                    match memory.byte(offset + i as u64) {
                        Some(byte) => *slot = byte,
                        None => return i,
                    }
                }
                want
            }
            Source::File(file) => {
                if want as u64 > WINDOW / 2 {
                    return read_at(file, offset, buf);
                }
                let mut window = self.window.borrow_mut();
                let cached = offset >= window.start
                    && offset + want as u64 <= window.start + window.bytes.len() as u64;
                if !cached {
                    // a window around the read, so that moving back a little is cached too
                    let start = offset.saturating_sub(WINDOW / 4);
                    let len = WINDOW.min(self.len - start) as usize;
                    window.bytes.resize(len, 0);
                    let n = read_at(file, start, &mut window.bytes);
                    window.bytes.truncate(n);
                    window.start = start;
                }
                let from = (offset - window.start) as usize;
                let n = want.min(window.bytes.len().saturating_sub(from));
                buf[..n].copy_from_slice(&window.bytes[from..from + n]);
                n
            }
        }
    }

    // Overwriting

    /// Overwrites the byte at `offset` with `value`, as part of the step under way (see
    /// [`Self::commit`]). Returns whether there is a byte there.
    pub fn overwrite(&mut self, offset: u64, value: u8) -> bool {
        let Some(old) = self.byte(offset) else {
            return false;
        };
        if old != value {
            self.pending.push(Edit {
                offset,
                old,
                new: value,
            });
            self.set(offset, value);
        }
        true
    }

    /// Ends the step under way: `u` undoes it at once.
    pub fn commit(&mut self) {
        if !self.pending.is_empty() {
            self.undo.push(Step {
                edits: std::mem::take(&mut self.pending),
                written: false,
            });
            self.redo.clear();
        }
    }

    /// Undoes the last step and returns the offset of its first byte.
    pub fn undo(&mut self) -> Option<u64> {
        self.undo_step().map(|undone| undone.offset)
    }

    /// Redoes the last step undone and returns the offset of its last byte.
    pub fn redo(&mut self) -> Option<u64> {
        self.redo_step().map(|redone| redone.offset)
    }

    /// Undoes the last step, saying what it changed.
    pub fn undo_step(&mut self) -> Option<Undone> {
        self.commit();
        let step = self.undo.pop()?;
        for edit in step.edits.iter().rev() {
            self.set(edit.offset, edit.old);
        }
        let undone = Undone {
            offset: step.edits.first()?.offset,
            written: step.written,
            offsets: step.edits.iter().map(|edit| edit.offset).collect(),
        };
        self.redo.push(step);
        Some(undone)
    }

    /// Redoes the last step undone, saying what it changed.
    pub fn redo_step(&mut self) -> Option<Undone> {
        self.commit();
        let step = self.redo.pop()?;
        for edit in &step.edits {
            self.set(edit.offset, edit.new);
        }
        let redone = Undone {
            offset: step.edits.last()?.offset,
            written: step.written,
            offsets: step.edits.iter().map(|edit| edit.offset).collect(),
        };
        self.undo.push(step);
        Some(redone)
    }

    /// Whether `u` (`undo`) or `U` would change a step written to memory, which writes memory
    /// again.
    pub fn next_step_written(&self, undo: bool) -> bool {
        if undo && !self.pending.is_empty() {
            return false;
        }
        let stack = if undo { &self.undo } else { &self.redo };
        stack.last().is_some_and(|step| step.written)
    }

    /// The overwritten bytes not written yet, in runs of consecutive offsets; only `offsets`
    /// when given.
    pub fn edit_runs(&self, offsets: Option<&[u64]>) -> Vec<(u64, Vec<u8>)> {
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        for (&offset, &byte) in &self.edits {
            if offsets.is_some_and(|offsets| !offsets.contains(&offset)) {
                continue;
            }
            match runs.last_mut() {
                Some((start, bytes)) if *start + bytes.len() as u64 == offset => bytes.push(byte),
                _ => runs.push((offset, vec![byte])),
            }
        }
        runs
    }

    /// The bytes at `offsets` (all the overwritten bytes when `None`) were written to the
    /// debugged program's memory: they are its bytes now, no longer edits, and the steps that
    /// made them are marked written.
    pub fn memory_written(&mut self, offsets: Option<&[u64]>) {
        self.commit();
        let written: Vec<(u64, u8)> = self
            .edits
            .iter()
            .filter(|(offset, _)| offsets.is_none_or(|offsets| offsets.contains(offset)))
            .map(|(&offset, &byte)| (offset, byte))
            .collect();
        for (offset, byte) in written {
            self.edits.remove(&offset);
            if let Source::Debuggee(memory) = &mut self.source {
                memory.written(offset, byte);
            }
        }
        if offsets.is_none() {
            for step in &mut self.undo {
                step.written = true;
            }
        }
    }

    /// Sets the byte at `offset`, keeping it among the edits only if it differs from the file.
    fn set(&mut self, offset: u64, value: u8) {
        if self.file_byte(offset) == Some(value) {
            self.edits.remove(&offset);
        } else {
            self.edits.insert(offset, value);
        }
    }

    /// Writes the overwritten bytes to the file at `path`, in place: only the runs of changed
    /// bytes are written, never the whole file. Returns the number of bytes written. The undo
    /// history stays, so writing can be undone like in a text buffer.
    pub fn write_in_place(&mut self, path: &Path) -> io::Result<usize> {
        match self.source {
            Source::File(_) => (),
            Source::Bytes(_) => {
                return Err(io::Error::other("these bytes have no file to write to"))
            }
            Source::Debuggee(_) => {
                return Err(io::Error::other(
                    "memory isn't a file: :w writes the changed bytes to the program's memory",
                ))
            }
        }
        self.commit();
        let file = std::fs::OpenOptions::new().write(true).open(path)?;
        let mut run: Vec<u8> = Vec::new();
        let mut run_start = 0;
        for (&offset, &byte) in &self.edits {
            if !run.is_empty() && run_start + run.len() as u64 != offset {
                write_at(&file, run_start, &run)?;
                run.clear();
            }
            if run.is_empty() {
                run_start = offset;
            }
            run.push(byte);
        }
        if !run.is_empty() {
            write_at(&file, run_start, &run)?;
        }
        match file.sync_data() {
            Ok(()) => (),
            Err(err) if err.kind() == io::ErrorKind::Unsupported => (),
            Err(err) => return Err(err),
        }
        let written = self.edits.len();
        self.edits.clear();
        // the file has the bytes now, and the cache may not
        *self.window.borrow_mut() = Window::default();
        Ok(written)
    }

    // Searching

    /// The first match of `pattern` from `from` on (forward) or before `from` (backward),
    /// wrapping around the end of the file. Returns its offset and whether the search wrapped.
    pub fn find(&self, pattern: &[u8], from: u64, forward: bool) -> Option<(u64, bool)> {
        if pattern.is_empty() || pattern.len() as u64 > self.len {
            return None;
        }
        if let Source::Debuggee(memory) = &self.source {
            return self.find_in_memory(memory, pattern, from, forward);
        }
        let from = from.min(self.len);
        if forward {
            self.find_forward(pattern, from, self.len)
                .map(|at| (at, false))
                .or_else(|| {
                    let end = (from + pattern.len() as u64 - 1).min(self.len);
                    self.find_forward(pattern, 0, end).map(|at| (at, true))
                })
        } else {
            self.find_backward(pattern, 0, from)
                .map(|at| (at, false))
                .or_else(|| {
                    self.find_backward(pattern, from, self.len)
                        .map(|at| (at, true))
                })
        }
    }

    /// [`Self::find`] in a debugged program's memory: the address space is too big to read
    /// through, so it looks through the bytes read so far (around where the view has been).
    fn find_in_memory(
        &self,
        memory: &MemorySource,
        pattern: &[u8],
        from: u64,
        forward: bool,
    ) -> Option<(u64, bool)> {
        let mut matches: Vec<u64> = Vec::new();
        for (start, mut bytes) in memory.runs() {
            for (i, byte) in bytes.iter_mut().enumerate() {
                if let Some(&edited) = self.edits.get(&(start + i as u64)) {
                    *byte = edited;
                }
            }
            matches.extend(memchr::memmem::find_iter(&bytes, pattern).map(|at| start + at as u64));
        }
        if forward {
            matches
                .iter()
                .find(|&&at| at >= from)
                .map(|&at| (at, false))
                .or_else(|| matches.first().map(|&at| (at, true)))
        } else {
            matches
                .iter()
                .rev()
                .find(|&&at| at < from)
                .map(|&at| (at, false))
                .or_else(|| matches.last().map(|&at| (at, true)))
        }
    }

    /// The first match that lies entirely in `start..end`.
    fn find_forward(&self, pattern: &[u8], start: u64, end: u64) -> Option<u64> {
        let finder = memchr::memmem::Finder::new(pattern);
        let overlap = pattern.len() - 1;
        let mut chunk = vec![0; SEARCH_CHUNK + overlap];
        let mut at = start;
        while at + pattern.len() as u64 <= end {
            let want = ((end - at) as usize).min(chunk.len());
            let n = self.read(at, &mut chunk[..want]);
            if let Some(found) = finder.find(&chunk[..n]) {
                return Some(at + found as u64);
            }
            if n < pattern.len() {
                return None;
            }
            at += (n - overlap) as u64;
        }
        None
    }

    /// The last match that starts in `start..end` (and may end after it).
    fn find_backward(&self, pattern: &[u8], start: u64, end: u64) -> Option<u64> {
        let finder = memchr::memmem::FinderRev::new(pattern);
        let overlap = pattern.len() as u64 - 1;
        let mut chunk = vec![0; SEARCH_CHUNK + overlap as usize];
        let mut chunk_end = end;
        while chunk_end > start {
            let chunk_start = chunk_end.saturating_sub(SEARCH_CHUNK as u64).max(start);
            let read_end = (chunk_end + overlap).min(self.len);
            let n = self.read(chunk_start, &mut chunk[..(read_end - chunk_start) as usize]);
            if let Some(found) = finder.rfind(&chunk[..n]) {
                return Some(chunk_start + found as u64);
            }
            chunk_end = chunk_start;
        }
        None
    }

    /// The matches of `pattern` that overlap `range`, for highlighting them.
    pub fn matches_in(&self, pattern: &[u8], range: Range<u64>) -> Vec<Range<u64>> {
        if pattern.is_empty() || range.is_empty() {
            return Vec::new();
        }
        let overlap = pattern.len() as u64 - 1;
        let start = range.start.saturating_sub(overlap);
        let end = range.end.saturating_add(overlap);
        let found: Vec<u64> = if self.is_memory() {
            // runs of bytes read: an unreadable byte matches nothing
            let cells = self.cells(start..end);
            let mut found = Vec::new();
            let mut run_start = 0;
            for i in 0..=cells.len() {
                if cells.get(i).and_then(|cell| cell.byte()).is_none() {
                    let run: Vec<u8> = cells[run_start..i]
                        .iter()
                        .filter_map(|c| c.byte())
                        .collect();
                    found.extend(
                        memchr::memmem::find_iter(&run, pattern)
                            .map(|at| start + (run_start + at) as u64),
                    );
                    run_start = i + 1;
                }
            }
            found
        } else {
            let bytes = self.bytes(start..end);
            memchr::memmem::find_iter(&bytes, pattern)
                .map(|at| start + at as u64)
                .collect()
        };
        found
            .into_iter()
            .map(|at| at..at + pattern.len() as u64)
            .filter(|found| found.end > range.start && found.start < range.end)
            .collect()
    }
}

/// Parses what `/` searches for: hex bytes, spaces optional (`de ad be ef`, `DEADBEEF`), or text
/// in double quotes (`"IHDR"`), searched for as its UTF-8 bytes.
pub fn parse_pattern(input: &str) -> Result<Vec<u8>, String> {
    let input = input.trim();
    if let Some(text) = input
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        if text.is_empty() {
            return Err("nothing to search for between the quotes".into());
        }
        return Ok(text.as_bytes().to_vec());
    }
    if input.starts_with('"') {
        return Err("a text search ends with a quote too: \"IHDR\"".into());
    }
    let digits: Vec<char> = input.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.is_empty() {
        return Err("nothing to search for".into());
    }
    if let Some(bad) = digits.iter().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!(
            "'{bad}' is not a hex digit: search hex bytes (de ad be ef) or \"text\" in quotes"
        ));
    }
    if digits.len() % 2 == 1 {
        return Err("an odd number of hex digits: a byte has two (0a, not a)".into());
    }
    Ok(digits
        .chunks(2)
        .map(|pair| {
            let text: String = pair.iter().collect();
            u8::from_str_radix(&text, 16).unwrap()
        })
        .collect())
}

/// Parses an offset: hex after `0x` (`0x1a4`), decimal otherwise (`420`).
pub fn parse_offset(input: &str) -> Result<u64, String> {
    let input = input.trim();
    let parsed = match input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16),
        None => input.replace('_', "").parse(),
    };
    parsed.map_err(|_| format!("'{input}' is not an offset: 0x1a4 in hex, 420 in decimal"))
}

/// The bytes as hex text, the way `y` yanks them: `de ad be ef`.
pub fn hex_text(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 3);
    for (i, byte) in bytes.iter().enumerate() {
        if i > 0 {
            text.push(' ');
        }
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// The kind of a byte, which gives its color (as in hexyl).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteKind {
    Null,
    /// Printable ASCII, space excluded.
    Printable,
    /// ASCII whitespace: tab, line feed, vertical tab, form feed, carriage return and space.
    Whitespace,
    /// The other ASCII control bytes.
    Control,
    /// The bytes above ASCII, 0x80 to 0xff.
    Other,
}

impl ByteKind {
    pub fn of(byte: u8) -> Self {
        match byte {
            0 => ByteKind::Null,
            b'\t' | b'\n' | 0x0b | 0x0c | b'\r' | b' ' => ByteKind::Whitespace,
            0x21..=0x7e => ByteKind::Printable,
            0x01..=0x1f | 0x7f => ByteKind::Control,
            0x80..=0xff => ByteKind::Other,
        }
    }

    /// The theme scope of the kind, then the scopes to fall back to in themes without it, in
    /// order.
    pub fn scopes(self) -> &'static [&'static str] {
        match self {
            ByteKind::Null => &["ui.hex.null", "ui.text.inactive", "comment"],
            ByteKind::Printable => &["ui.hex.printable", "function"],
            ByteKind::Whitespace => &["ui.hex.whitespace", "string"],
            ByteKind::Control => &["ui.hex.control", "constant"],
            ByteKind::Other => &["ui.hex.other", "ui.text"],
        }
    }
}

/// The character of a byte in the ASCII column: itself if printable (space included), else `.`.
pub fn ascii_char(byte: u8) -> char {
    if (0x20..=0x7e).contains(&byte) {
        byte as char
    } else {
        '.'
    }
}

fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> usize {
    use std::os::unix::fs::FileExt;
    let mut n = 0;
    while n < buf.len() {
        match file.read_at(&mut buf[n..], offset + n as u64) {
            Ok(0) => break,
            Ok(read) => n += read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => {
                log::warn!("hex view: reading at {offset}: {err}");
                break;
            }
        }
    }
    n
}

fn write_at(file: &File, offset: u64, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(bytes, offset)
}

#[cfg(test)]
mod test {
    use super::*;

    fn view() -> ViewId {
        ViewId::default()
    }

    #[test]
    fn binary_detection() {
        assert!(!is_binary(b""));
        assert!(!is_binary(b"plain text\nsecond line"));
        assert!(is_binary(b"text\0with nul"));
        assert!(is_binary(b"%PDF-1.7 ..."));
        assert!(is_binary(b"\x89PNG\r\n"));
        assert!(!is_binary(b"\xEF\xBB\xBFtext"));
        assert!(!is_binary(b"\xFF\xFEt\0e\0x\0t\0"));
    }

    #[test]
    fn rows_in_hex() {
        let rows = Rows::new(Layout::Hex, 100, 160);
        assert_eq!(rows.bytes_per_row, 16);
        assert_eq!(rows.offset_digits, 8);
        assert_eq!(rows.byte_x(0), 12);
        assert_eq!(rows.byte_x(7), 12 + 21);
        // the second group starts after one more space
        assert_eq!(rows.byte_x(8), 12 + 24 + 1);
        assert_eq!(rows.ascii_x(), 62);
        assert_eq!(rows.width(), 78);

        // too narrow for 16 bytes
        assert_eq!(Rows::new(Layout::Hex, 100, 77).bytes_per_row, 8);
        assert_eq!(Rows::new(Layout::Hex, 100, 78).bytes_per_row, 16);
        assert_eq!(Rows::new(Layout::Hex, 100, 10).bytes_per_row, 8);
        assert_eq!(Rows::new(Layout::Hex, 100, 77).width(), 45);
    }

    #[test]
    fn rows_in_bin() {
        let rows = Rows::new(Layout::Bin, 100, 160);
        assert_eq!(rows.bytes_per_row, 8);
        assert_eq!(rows.width(), 94);
        assert_eq!(Rows::new(Layout::Bin, 100, 93).bytes_per_row, 4);
        assert_eq!(Rows::new(Layout::Bin, 100, 53).bytes_per_row, 4);
        assert_eq!(Rows::new(Layout::Bin, 100, 52).bytes_per_row, 2);
        assert_eq!(Rows::new(Layout::Bin, 100, 1).bytes_per_row, 1);
    }

    #[test]
    fn offset_digits_grow_with_the_file() {
        assert_eq!(Rows::new(Layout::Hex, 0, 100).offset_digits, 8);
        assert_eq!(Rows::new(Layout::Hex, 1 << 32, 100).offset_digits, 8);
        assert_eq!(Rows::new(Layout::Hex, (1 << 32) + 1, 100).offset_digits, 9);
        assert_eq!(Rows::new(Layout::Hex, 1 << 40, 100).offset_digits, 10);
    }

    #[test]
    fn row_counts() {
        let rows = Rows::new(Layout::Hex, 0, 100);
        assert_eq!(rows.count(0), 1);
        assert_eq!(rows.count(16), 1);
        assert_eq!(rows.count(17), 2);
    }

    #[test]
    fn overwrite_undo_redo() {
        let mut buffer = HexBuffer::from_bytes(b"abcdef".to_vec());
        assert!(!buffer.is_modified());
        assert!(buffer.overwrite(1, b'X'));
        assert!(buffer.overwrite(2, b'Y'));
        assert!(!buffer.overwrite(6, b'Z'), "past the end");
        assert_eq!(buffer.bytes(0..6), b"aXYdef");
        assert!(buffer.is_modified());
        assert!(buffer.is_edited(1) && !buffer.is_edited(0));
        buffer.commit();

        buffer.overwrite(5, b'!');
        // undo commits the step under way and undoes it
        assert_eq!(buffer.undo(), Some(5));
        assert_eq!(buffer.bytes(0..6), b"aXYdef");
        assert_eq!(buffer.undo(), Some(1));
        assert_eq!(buffer.bytes(0..6), b"abcdef");
        assert!(!buffer.is_modified());
        assert_eq!(buffer.undo(), None);
        assert_eq!(buffer.redo(), Some(2));
        assert_eq!(buffer.bytes(0..6), b"aXYdef");

        // overwriting a byte back to its value in the file is no change
        buffer.overwrite(1, b'b');
        buffer.overwrite(2, b'c');
        assert!(!buffer.is_modified());
    }

    #[test]
    fn writes_only_the_changed_bytes_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, [0u8; 64]).unwrap();
        let mut buffer = HexBuffer::open(&path).unwrap();
        assert_eq!(buffer.len(), 64);
        buffer.overwrite(3, 0xde);
        buffer.overwrite(4, 0xad);
        buffer.overwrite(60, 0xff);
        assert_eq!(buffer.write_in_place(&path).unwrap(), 3);
        let mut expected = [0u8; 64];
        expected[3] = 0xde;
        expected[4] = 0xad;
        expected[60] = 0xff;
        assert_eq!(std::fs::read(&path).unwrap(), expected);
        assert!(!buffer.is_modified());
        assert_eq!(buffer.byte(3), Some(0xde));

        // undoing after writing changes the bytes back, which are then unwritten edits
        buffer.undo();
        assert!(buffer.is_modified());
        assert_eq!(buffer.byte(3), Some(0));
    }

    #[test]
    fn reads_across_the_window() {
        let bytes: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, &bytes).unwrap();
        let buffer = HexBuffer::open(&path).unwrap();
        for start in [0u64, 100, 65_000, 65_536, 200_000, 299_990] {
            let got = buffer.bytes(start..start + 100);
            let end = (start as usize + 100).min(bytes.len());
            assert_eq!(got, &bytes[start as usize..end], "at {start}");
        }
        assert_eq!(buffer.byte(300_000), None);
    }

    #[test]
    fn finds_forward_and_backward_with_wrapping() {
        let mut bytes = vec![0u8; 3 * SEARCH_CHUNK];
        // across a chunk boundary
        bytes[SEARCH_CHUNK - 2..SEARCH_CHUNK + 2].copy_from_slice(b"IHDR");
        bytes[10..14].copy_from_slice(b"IHDR");
        let buffer = HexBuffer::from_bytes(bytes);
        let at = (SEARCH_CHUNK - 2) as u64;
        assert_eq!(buffer.find(b"IHDR", 0, true), Some((10, false)));
        assert_eq!(buffer.find(b"IHDR", 11, true), Some((at, false)));
        assert_eq!(buffer.find(b"IHDR", at + 1, true), Some((10, true)));
        assert_eq!(buffer.find(b"IHDR", at, false), Some((10, false)));
        assert_eq!(buffer.find(b"IHDR", 10, false), Some((at, true)));
        assert_eq!(buffer.find(b"nope", 0, true), None);
    }

    #[test]
    fn finds_overwritten_bytes() {
        let mut buffer = HexBuffer::from_bytes(vec![0; 32]);
        buffer.overwrite(20, 0xbe);
        buffer.overwrite(21, 0xef);
        assert_eq!(buffer.find(&[0xbe, 0xef], 0, true), Some((20, false)));
        assert_eq!(
            buffer.matches_in(&[0xbe, 0xef], 21..30),
            vec![20..22],
            "a match that starts before the range"
        );
    }

    #[test]
    fn patterns() {
        assert_eq!(
            parse_pattern("de ad be ef"),
            Ok(vec![0xde, 0xad, 0xbe, 0xef])
        );
        assert_eq!(parse_pattern("DEADbeef"), Ok(vec![0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(parse_pattern("\"IHDR\""), Ok(b"IHDR".to_vec()));
        assert_eq!(parse_pattern("\" a\""), Ok(b" a".to_vec()));
        assert!(parse_pattern("abc").is_err());
        assert!(parse_pattern("xy").is_err());
        assert!(parse_pattern("\"open").is_err());
        assert!(parse_pattern("\"\"").is_err());
        assert!(parse_pattern("  ").is_err());
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("0x1a4"), Ok(420));
        assert_eq!(parse_offset("0X1A4"), Ok(420));
        assert_eq!(parse_offset("420"), Ok(420));
        assert_eq!(parse_offset("1_000"), Ok(1000));
        assert!(parse_offset("1a4").is_err());
        assert!(parse_offset("-3").is_err());
    }

    #[test]
    fn kinds_and_text() {
        assert_eq!(ByteKind::of(0), ByteKind::Null);
        assert_eq!(ByteKind::of(b'A'), ByteKind::Printable);
        assert_eq!(ByteKind::of(b' '), ByteKind::Whitespace);
        assert_eq!(ByteKind::of(b'\n'), ByteKind::Whitespace);
        assert_eq!(ByteKind::of(0x1b), ByteKind::Control);
        assert_eq!(ByteKind::of(0x7f), ByteKind::Control);
        assert_eq!(ByteKind::of(0x80), ByteKind::Other);
        assert_eq!(ascii_char(b'A'), 'A');
        assert_eq!(ascii_char(b' '), ' ');
        assert_eq!(ascii_char(0), '.');
        assert_eq!(ascii_char(0xff), '.');
        assert_eq!(hex_text(&[0xde, 0xad, 0x0b]), "de ad 0b");
        assert_eq!(hex_text(&[]), "");
    }

    #[test]
    fn cursors_per_view() {
        let mut buffer = HexBuffer::from_bytes(vec![0; 10]);
        buffer.cursor_mut(view()).offset = 4;
        assert_eq!(buffer.cursor(view()).offset, 4);
        let mut cursor = buffer.cursor(view());
        cursor.anchor = Some(7);
        assert_eq!(cursor.selection(), 4..8);
        cursor.anchor = Some(1);
        assert_eq!(cursor.selection(), 1..5);
        buffer.remove_view(view());
        assert_eq!(buffer.cursor(view()).offset, 0);
    }
}
