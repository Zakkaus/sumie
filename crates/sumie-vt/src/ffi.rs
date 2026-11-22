//! Subset of libghostty-vt-sys bindings at cd4d497e8c698b27054a677ce19d1bd1a1a4d8bf.
//! Source: <https://github.com/Uzaaft/libghostty-rs> (MIT, see `../LICENSE.bindings`).

use std::ffi::{c_int, c_void};

#[repr(C)]
pub struct TerminalImpl {
    _unused: [u8; 0],
}
pub type Terminal = *mut TerminalImpl;

#[repr(C)]
pub struct RenderStateImpl {
    _unused: [u8; 0],
}
pub type RenderState = *mut RenderStateImpl;

#[repr(C)]
pub struct RenderStateRowIteratorImpl {
    _unused: [u8; 0],
}
pub type RenderStateRowIterator = *mut RenderStateRowIteratorImpl;

#[repr(C)]
pub struct RenderStateRowCellsImpl {
    _unused: [u8; 0],
}
pub type RenderStateRowCells = *mut RenderStateRowCellsImpl;

// The wrapper passes NULL to select Ghostty's default allocator.
#[repr(C)]
pub struct Allocator {
    _unused: [u8; 0],
}

#[repr(C)]
pub struct Buffer {
    pub ptr: *mut u8,
    pub cap: usize,
    pub len: usize,
}

pub type Cell = u64;
pub const SUCCESS: c_int = 0;
pub const INVALID_VALUE: c_int = -2;
pub const OUT_OF_SPACE: c_int = -3;
pub const CELL_DATA_WIDE: c_int = 3;
pub const CELL_WIDE_NARROW: c_int = 0;
pub const CELL_WIDE_WIDE: c_int = 1;
pub const CELL_WIDE_SPACER_TAIL: c_int = 2;
pub const CELL_WIDE_SPACER_HEAD: c_int = 3;
pub const RENDER_STATE_DATA_COLS: c_int = 1;
pub const RENDER_STATE_DATA_ROWS: c_int = 2;
pub const RENDER_STATE_DATA_ROW_ITERATOR: c_int = 4;
pub const RENDER_STATE_ROW_DATA_CELLS: c_int = 3;
pub const RENDER_STATE_ROW_CELLS_DATA_RAW: c_int = 1;
pub const RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8: c_int = 9;

// SAFETY: ABI declarations and values come from the pinned vt-1.0.1 bindings.
unsafe extern "C" {
    pub fn ghostty_terminal_new(
        allocator: *const Allocator,
        terminal: *mut Terminal,
        cols: u16,
        rows: u16,
    ) -> c_int;
    pub fn ghostty_terminal_free(terminal: Terminal);
    pub fn ghostty_terminal_resize(
        terminal: Terminal,
        cols: u16,
        rows: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) -> c_int;
    pub fn ghostty_terminal_vt_write(terminal: Terminal, data: *const u8, len: usize);
    pub fn ghostty_render_state_new(allocator: *const Allocator, state: *mut RenderState) -> c_int;
    pub fn ghostty_render_state_free(state: RenderState);
    pub fn ghostty_render_state_update(state: RenderState, terminal: Terminal) -> c_int;
    pub fn ghostty_render_state_get(state: RenderState, data: c_int, out: *mut c_void) -> c_int;
    pub fn ghostty_render_state_row_iterator_new(
        allocator: *const Allocator,
        out_iterator: *mut RenderStateRowIterator,
    ) -> c_int;
    pub fn ghostty_render_state_row_iterator_free(iterator: RenderStateRowIterator);
    pub fn ghostty_render_state_row_iterator_next(iterator: RenderStateRowIterator) -> bool;
    pub fn ghostty_render_state_row_get(
        iterator: RenderStateRowIterator,
        data: c_int,
        out: *mut c_void,
    ) -> c_int;
    pub fn ghostty_render_state_row_cells_new(
        allocator: *const Allocator,
        out_cells: *mut RenderStateRowCells,
    ) -> c_int;
    pub fn ghostty_render_state_row_cells_free(cells: RenderStateRowCells);
    pub fn ghostty_render_state_row_cells_next(cells: RenderStateRowCells) -> bool;
    pub fn ghostty_render_state_row_cells_get(
        cells: RenderStateRowCells,
        data: c_int,
        out: *mut c_void,
    ) -> c_int;
    pub fn ghostty_cell_get(cell: Cell, data: c_int, out: *mut c_void) -> c_int;
}
