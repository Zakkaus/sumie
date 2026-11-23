//! Copy one row of searchable cell data at a time. Grid references never
//! outlive the borrow of the terminal used to read them.

use crate::{Error, Terminal, check, ffi};

const CODEPOINT: u32 = (1 << 21) - 1;
const WIDE: u32 = 1 << 21;
const SPACER: u32 = 1 << 22;
const GRAPHEME: u32 = 1 << 23;

// SAFETY: These declarations match the typed bridge in grid.zig.
unsafe extern "C" {
    fn sumie_search_read_row(
        reference: *const ffi::GridRef,
        next: *mut ffi::GridRef,
        cells: *mut Cell,
        cap: u16,
        row: *mut RowData,
    ) -> bool;
    fn sumie_search_check_rows(
        reference: *const ffi::GridRef,
        stamps: *mut Stamp,
        len: usize,
    ) -> bool;
    fn sumie_search_line_start(reference: *const ffi::GridRef, boundary: u32) -> u32;
    fn sumie_search_graphemes(
        reference: *const ffi::GridRef,
        col: u16,
        buf: *mut u32,
        len: usize,
        written: *mut usize,
    ) -> std::ffi::c_int;
    fn sumie_search_fold(cp: u32) -> u32;
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub(super) struct Cell(pub u32);

impl Cell {
    pub(super) fn codepoint(self) -> u32 {
        self.0 & CODEPOINT
    }

    pub(super) fn is_wide(self) -> bool {
        self.0 & WIDE != 0
    }

    pub(super) fn is_spacer(self) -> bool {
        self.0 & SPACER != 0
    }

    pub(super) fn has_grapheme(self) -> bool {
        self.0 & GRAPHEME != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Size {
    pub cols: u16,
    pub rows: u32,
    pub active: u32,
}

impl Size {
    pub(super) fn read(terminal: &Terminal) -> Result<Self, Error> {
        let mut cols = 0_u16;
        let mut rows = 0_usize;
        // SAFETY: The live terminal's data keys write a u16 and a size_t.
        unsafe {
            check(ffi::ghostty_terminal_get(
                terminal.raw,
                ffi::TERMINAL_DATA_COLS,
                (&raw mut cols).cast(),
            ))?;
            check(ffi::ghostty_terminal_get(
                terminal.raw,
                ffi::TERMINAL_DATA_TOTAL_ROWS,
                (&raw mut rows).cast(),
            ))?;
        }
        let rows = u32::try_from(rows).map_err(|_| Error(ffi::INVALID_VALUE))?;
        Ok(Self {
            cols,
            rows,
            active: terminal.history_rows()?,
        })
    }
}

pub(super) struct Rows<'a> {
    next: ffi::GridRef,
    terminal: &'a Terminal,
}

impl<'a> Rows<'a> {
    pub(super) fn at(terminal: &'a Terminal, row: u32) -> Result<Self, Error> {
        Ok(Self {
            next: grid_ref(terminal, row)?,
            terminal,
        })
    }

    pub(super) fn read(&mut self, cells: &mut [Cell], cols: u16) -> Result<Row<'a>, Error> {
        let reference = std::mem::take(&mut self.next);
        let mut data = RowData::default();
        assert!(cells.len() >= usize::from(cols));
        // SAFETY: The borrowed terminal keeps the reference live. The slice
        // contains at least cols writable entries.
        if unsafe {
            sumie_search_read_row(
                &raw const reference,
                &raw mut self.next,
                cells.as_mut_ptr(),
                cols,
                &raw mut data,
            )
        } {
            Ok(Row {
                reference,
                data,
                _terminal: self.terminal,
            })
        } else {
            Err(Error(ffi::INVALID_VALUE))
        }
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct RowData {
    pub len: u16,
    pub cols: u16,
    pub wrapped: bool,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct Stamp {
    hash: u64,
    pub dirty: bool,
    pub neutral: bool,
}

/// Writes during a sweep invalidate its visited prefix, not its cursor.
#[derive(Default)]
pub(super) struct Recheck {
    remaining: std::ops::Range<usize>,
    prefix: usize,
}

impl Recheck {
    pub(super) fn schedule(&mut self, rows: usize) {
        self.prefix = self.prefix.max(self.remaining.start);
        self.remaining.end = rows;
    }

    pub(super) fn pending(&self) -> bool {
        !self.remaining.is_empty()
    }

    pub(super) fn advance(
        &mut self,
        terminal: &Terminal,
        grid: Size,
        stamps: &mut [Stamp],
        deadline: std::time::Instant,
    ) -> Result<(), Error> {
        let batch = (16_384 / usize::from(grid.cols)).max(1);
        while self.pending() {
            let start = self.remaining.start;
            let end = (start + batch).min(self.remaining.end);
            let row = grid.active + u32::try_from(start).map_err(|_| Error(ffi::INVALID_VALUE))?;
            let reference = grid_ref(terminal, row)?;
            check_stamps(&reference, &mut stamps[start..end])?;
            self.remaining.start = end;
            if self.remaining.is_empty() {
                self.remaining = 0..std::mem::take(&mut self.prefix);
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
        Ok(())
    }
}

pub(super) fn check_row(terminal: &Terminal, row: u32, stamp: &mut Stamp) -> Result<(), Error> {
    check_stamps(&grid_ref(terminal, row)?, std::slice::from_mut(stamp))
}

fn check_stamps(reference: &ffi::GridRef, stamps: &mut [Stamp]) -> Result<(), Error> {
    // SAFETY: The caller holds the terminal borrow. Zig writes at most stamps.len() entries.
    if unsafe { sumie_search_check_rows(reference, stamps.as_mut_ptr(), stamps.len()) } {
        Ok(())
    } else {
        Err(Error(ffi::INVALID_VALUE))
    }
}

pub(super) struct Row<'a> {
    reference: ffi::GridRef,
    pub data: RowData,
    _terminal: &'a Terminal,
}

impl Row<'_> {
    pub(super) fn stamp(&self, neutral: bool) -> Result<Stamp, Error> {
        let mut stamp = Stamp::default();
        check_stamps(&self.reference, std::slice::from_mut(&mut stamp))?;
        stamp.dirty = false;
        stamp.neutral = neutral;
        Ok(stamp)
    }

    pub(super) fn graphemes<'a>(
        &self,
        col: u16,
        buf: &'a mut Vec<u32>,
    ) -> Result<&'a [u32], Error> {
        if buf.is_empty() {
            buf.resize(8, 0);
        }
        let mut written = 0;
        let read = |buf: &mut [u32], written: &mut usize| {
            // SAFETY: The grid reference is live, col is from the projected
            // row and the slice describes initialized writable u32 storage.
            unsafe {
                sumie_search_graphemes(
                    &raw const self.reference,
                    col,
                    buf.as_mut_ptr(),
                    buf.len(),
                    written,
                )
            }
        };
        let result = read(buf, &mut written);
        if result == ffi::OUT_OF_SPACE {
            buf.resize(written, 0);
            check(read(buf, &mut written))?;
        } else {
            check(result)?;
        }
        Ok(&buf[..written])
    }
}

pub(super) fn line_start(terminal: &Terminal, boundary: u32) -> Result<u32, Error> {
    if boundary == 0 {
        return Ok(0);
    }
    let prior = grid_ref(terminal, boundary - 1)?;
    // SAFETY: Prior names the live row preceding boundary on this terminal.
    Ok(unsafe { sumie_search_line_start(&raw const prior, boundary) })
}

pub(super) fn fold(cp: u32) -> u32 {
    if (u32::from(b'A')..=u32::from(b'Z')).contains(&cp) {
        cp + u32::from(b'a' - b'A')
    } else if cp < 128 {
        cp
    } else {
        // SAFETY: Callers pass Unicode scalar values from the query or terminal grid.
        unsafe { sumie_search_fold(cp) }
    }
}

fn grid_ref(terminal: &Terminal, row: u32) -> Result<ffi::GridRef, Error> {
    let mut reference = ffi::GridRef::default();
    let point = ffi::Point {
        tag: ffi::POINT_TAG_SCREEN,
        value: ffi::PointValue {
            coordinate: ffi::PointCoordinate { x: 0, y: row },
        },
    };
    // SAFETY: The screen tag uses the coordinate field and the terminal and output are live.
    check(unsafe { ffi::ghostty_terminal_grid_ref(terminal.raw, point, &raw mut reference) })?;
    Ok(reference)
}
