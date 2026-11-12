//! The memory of a debugged program as the bytes of a hex view (this fork).
//!
//! The address space is huge and is read through the debug adapter (`readMemory`), which answers
//! later: a [`MemorySource`] is a cache of pages read at a stop. Drawing asks for bytes; the pages
//! it finds missing, or read at an earlier stop, are noted as wanted, and the debugger reads them
//! (`vortex-term/src/debug/memory.rs`) and puts them back with [`MemorySource::fill`]. Until then a
//! page read at an earlier stop shows its old bytes, so a running program shows its last stop.
//!
//! Offsets in the hex view are the addresses themselves. Reads go through the memory reference
//! the view was opened from, with an offset from the address it points to, as DAP has them.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
};

use vortex_dap::registry::DebugAdapterId;

/// The bytes of a page, the unit memory is read and cached in.
pub const PAGE: u64 = 1024;
/// Pages kept at most: the ones read at the oldest stops go first.
const MAX_PAGES: usize = 512;

/// What a byte of memory is, as drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Byte(u8),
    /// A byte the program changed since the stop before (or the refresh before, while playing).
    Changed(u8),
    /// Freed or unmapped memory, or a read that failed: `??`.
    Unreadable,
    /// Not read yet.
    Loading,
    /// In rewind, a byte outside the window recorded at that stop.
    NotRecorded,
}

impl Cell {
    pub fn byte(self) -> Option<u8> {
        match self {
            Cell::Byte(byte) | Cell::Changed(byte) => Some(byte),
            _ => None,
        }
    }
}

/// A page of memory as read at a stop.
#[derive(Debug, Clone)]
pub struct Page {
    /// [`PAGE`] bytes, `None` where unreadable.
    pub bytes: Vec<Option<u8>>,
    /// The bytes that differ from the read before.
    pub changed: Vec<bool>,
    /// The stop it was read at.
    pub stop: u64,
}

/// The bytes recorded at a stop for rewind: a window of memory around what was on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The stop's number in the history (to know when to show another one).
    pub step: usize,
    pub start: u64,
    pub bytes: Vec<Option<u8>>,
    /// The bytes that differ from the window recorded at the stop before.
    pub changed: Vec<bool>,
}

impl Recorded {
    fn cell(&self, address: u64) -> Cell {
        let Some(index) = address.checked_sub(self.start) else {
            return Cell::NotRecorded;
        };
        match self.bytes.get(index as usize) {
            None => Cell::NotRecorded,
            Some(None) => Cell::Unreadable,
            Some(Some(byte)) if self.changed[index as usize] => Cell::Changed(*byte),
            Some(Some(byte)) => Cell::Byte(*byte),
        }
    }
}

/// The memory of a debug session, seen from one memory reference.
#[derive(Debug)]
pub struct MemorySource {
    pub session: DebugAdapterId,
    /// The adapter's `memoryReference` the reads go through.
    pub reference: String,
    /// The address `reference` points to.
    pub base: u64,
    /// What the view was opened on: a variable's name, an expression, an address.
    pub label: String,
    /// The adapter writes memory (`supportsWriteMemoryRequest`).
    pub can_write: bool,
    pages: BTreeMap<u64, Page>,
    /// The pages whose read is under way, with the stop it was sent at.
    loading: BTreeMap<u64, u64>,
    /// The number of the stop the bytes are wanted for.
    stop: u64,
    /// Pages drawing found missing or stale since the debugger last took them.
    wanted: RefCell<BTreeSet<u64>>,
    /// In rewind: the bytes recorded at the stop shown, shown instead of memory.
    pub recorded: Option<Recorded>,
    /// The session ended: memory can't be read any more.
    pub ended: bool,
    /// When reads were last sent: while playing, memory is read at most at the screen's rate.
    pub refreshed: Option<std::time::Instant>,
}

impl MemorySource {
    pub fn new(
        session: DebugAdapterId,
        reference: String,
        base: u64,
        label: String,
        can_write: bool,
    ) -> Self {
        Self {
            session,
            reference,
            base,
            label,
            can_write,
            pages: BTreeMap::new(),
            loading: BTreeMap::new(),
            stop: 1,
            wanted: RefCell::default(),
            recorded: None,
            ended: false,
            refreshed: None,
        }
    }

    /// The offset of `address` from the reference, as reads take it.
    pub fn offset_of(&self, address: u64) -> i64 {
        address.wrapping_sub(self.base) as i64
    }

    /// The program stopped again: the pages read before are stale and are read again when they
    /// are drawn.
    pub fn next_stop(&mut self) {
        self.stop += 1;
    }

    pub fn stop(&self) -> u64 {
        self.stop
    }

    /// The byte at `address` as drawn.
    pub fn cell(&self, address: u64) -> Cell {
        if let Some(recorded) = &self.recorded {
            return recorded.cell(address);
        }
        let page = address / PAGE * PAGE;
        let index = (address - page) as usize;
        match self.pages.get(&page) {
            Some(read) => {
                if read.stop < self.stop && !self.ended {
                    self.want(page);
                }
                match read.bytes[index] {
                    None => Cell::Unreadable,
                    Some(byte) if read.changed[index] => Cell::Changed(byte),
                    Some(byte) => Cell::Byte(byte),
                }
            }
            None if self.ended => Cell::Unreadable,
            None => {
                self.want(page);
                Cell::Loading
            }
        }
    }

    /// The byte at `address` in memory as last read, without the overwritten bytes.
    pub fn byte(&self, address: u64) -> Option<u8> {
        self.cell(address).byte()
    }

    fn want(&self, page: u64) {
        if !self.loading.contains_key(&page) {
            self.wanted.borrow_mut().insert(page);
        }
    }

    /// The pages to read now, marked as loading for the current stop.
    pub fn take_wanted(&mut self) -> Vec<u64> {
        let wanted: Vec<u64> = std::mem::take(&mut *self.wanted.borrow_mut())
            .into_iter()
            .filter(|page| !self.loading.contains_key(page))
            .collect();
        for &page in &wanted {
            self.loading.insert(page, self.stop);
        }
        wanted
    }

    /// Whether drawing wants pages read.
    pub fn has_wanted(&self) -> bool {
        !self.wanted.borrow().is_empty()
    }

    /// Whether reads are under way.
    pub fn is_loading(&self) -> bool {
        !self.loading.is_empty()
    }

    /// Puts back a page read at `stop` (`None` bytes are unreadable), marking the bytes that
    /// differ from the read before.
    pub fn fill(&mut self, page: u64, bytes: Vec<Option<u8>>, stop: u64) {
        if self.loading.get(&page) == Some(&stop) {
            self.loading.remove(&page);
        }
        let changed = match self.pages.get(&page) {
            // a read for an older stop than the one shown is no news
            Some(old) if old.stop > stop => return,
            Some(old) if old.stop == stop => old.changed.clone(),
            Some(old) => old
                .bytes
                .iter()
                .zip(&bytes)
                .map(|(old, new)| old.is_some() && new.is_some() && old != new)
                .collect(),
            None => vec![false; bytes.len()],
        };
        self.pages.insert(
            page,
            Page {
                bytes,
                changed,
                stop,
            },
        );
        if self.pages.len() > MAX_PAGES {
            let oldest = self
                .pages
                .iter()
                .min_by_key(|(_, page)| page.stop)
                .map(|(&at, _)| at);
            if let Some(oldest) = oldest {
                self.pages.remove(&oldest);
            }
        }
    }

    /// A read that never came back (the request failed): drawing may ask again later.
    pub fn failed(&mut self, page: u64, stop: u64) {
        if self.loading.get(&page) == Some(&stop) {
            self.loading.remove(&page);
        }
    }

    /// Bytes written to memory: the cache has them now.
    pub fn written(&mut self, address: u64, value: u8) {
        let page = address / PAGE * PAGE;
        if let Some(read) = self.pages.get_mut(&page) {
            read.bytes[(address - page) as usize] = Some(value);
        }
    }

    /// The bytes read, `None` where unreadable or not read, from `start`: for recording.
    pub fn window(&self, start: u64, len: usize) -> Vec<Option<u8>> {
        (0..len as u64)
            .map(|i| match self.cell(start.saturating_add(i)) {
                Cell::Byte(byte) | Cell::Changed(byte) => Some(byte),
                _ => None,
            })
            .collect()
    }

    /// The runs of bytes read (from the pages in the cache, or the recorded window in rewind),
    /// in order of address: what a search can look through.
    pub fn runs(&self) -> Vec<(u64, Vec<u8>)> {
        let mut runs: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut push = |address: u64, byte: Option<u8>| {
            if let Some(byte) = byte {
                match runs.last_mut() {
                    Some((start, bytes)) if *start + bytes.len() as u64 == address => {
                        bytes.push(byte)
                    }
                    _ => runs.push((address, vec![byte])),
                }
            }
        };
        if let Some(recorded) = &self.recorded {
            for (i, byte) in recorded.bytes.iter().enumerate() {
                push(recorded.start + i as u64, *byte);
            }
        } else {
            for (&page, read) in &self.pages {
                for (i, byte) in read.bytes.iter().enumerate() {
                    push(page + i as u64, *byte);
                }
            }
        }
        runs
    }

    /// The number of bytes in the cache.
    pub fn cached_bytes(&self) -> u64 {
        self.pages.len() as u64 * PAGE
    }
}

/// The bytes of a `readMemory` answer for the page at `page`: `data` starts at `address`, and
/// everything outside it is unreadable.
pub fn page_bytes(page: u64, address: u64, data: &[u8]) -> Vec<Option<u8>> {
    span_bytes(page, PAGE as usize, address, data)
}

/// The `len` bytes from `start` of a `readMemory` answer whose `data` starts at `address`.
pub fn span_bytes(start: u64, len: usize, address: u64, data: &[u8]) -> Vec<Option<u8>> {
    (0..len as u64)
        .map(|i| {
            let at = start.saturating_add(i);
            at.checked_sub(address)
                .and_then(|index| data.get(index as usize))
                .copied()
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;

    fn session() -> DebugAdapterId {
        let mut registry: slotmap::SlotMap<DebugAdapterId, ()> = slotmap::SlotMap::with_key();
        registry.insert(())
    }

    fn source() -> MemorySource {
        MemorySource::new(session(), "0x1000".into(), 0x1010, "buf".into(), true)
    }

    #[test]
    fn missing_pages_are_wanted_once() {
        let mut memory = source();
        assert_eq!(memory.cell(0x1010), Cell::Loading);
        assert_eq!(memory.cell(0x1011), Cell::Loading);
        assert_eq!(memory.take_wanted(), vec![0x1000]);
        // under way: not wanted again
        assert_eq!(memory.cell(0x1010), Cell::Loading);
        assert!(memory.take_wanted().is_empty());
        memory.fill(0x1000, page_bytes(0x1000, 0x1000, &[7; PAGE as usize]), 1);
        assert_eq!(memory.cell(0x1010), Cell::Byte(7));
        assert!(memory.take_wanted().is_empty());
    }

    #[test]
    fn stale_pages_show_their_bytes_and_mark_changes() {
        let mut memory = source();
        memory.cell(0x1000);
        memory.take_wanted();
        memory.fill(0x1000, vec![Some(1); PAGE as usize], 1);
        memory.next_stop();
        // the old bytes, and a read again
        assert_eq!(memory.cell(0x1000), Cell::Byte(1));
        assert_eq!(memory.take_wanted(), vec![0x1000]);
        let mut bytes = vec![Some(1); PAGE as usize];
        bytes[3] = Some(9);
        bytes[4] = None;
        memory.fill(0x1000, bytes, 2);
        assert_eq!(memory.cell(0x1003), Cell::Changed(9));
        assert_eq!(memory.cell(0x1004), Cell::Unreadable);
        assert_eq!(memory.cell(0x1005), Cell::Byte(1));
        // a late answer for the stop before changes nothing
        memory.fill(0x1000, vec![Some(5); PAGE as usize], 1);
        assert_eq!(memory.cell(0x1003), Cell::Changed(9));
    }

    #[test]
    fn answers_cover_part_of_a_page() {
        let bytes = page_bytes(0x2000, 0x2010, &[1, 2, 3]);
        assert_eq!(bytes[0x0f], None);
        assert_eq!(bytes[0x10], Some(1));
        assert_eq!(bytes[0x12], Some(3));
        assert_eq!(bytes[0x13], None);
        assert_eq!(bytes.len(), PAGE as usize);
    }

    #[test]
    fn recorded_windows_replace_memory() {
        let mut memory = source();
        memory.recorded = Some(Recorded {
            step: 3,
            start: 0x1010,
            bytes: vec![Some(4), None],
            changed: vec![true, false],
        });
        assert_eq!(memory.cell(0x100f), Cell::NotRecorded);
        assert_eq!(memory.cell(0x1010), Cell::Changed(4));
        assert_eq!(memory.cell(0x1011), Cell::Unreadable);
        assert_eq!(memory.cell(0x1012), Cell::NotRecorded);
        assert!(memory.take_wanted().is_empty(), "rewind reads nothing");
        assert_eq!(memory.runs(), vec![(0x1010, vec![4])]);
    }

    #[test]
    fn offsets_from_the_reference() {
        let memory = source();
        assert_eq!(memory.offset_of(0x1010), 0);
        assert_eq!(memory.offset_of(0x1000), -16);
        assert_eq!(memory.offset_of(0x1410), 0x400);
    }
}
