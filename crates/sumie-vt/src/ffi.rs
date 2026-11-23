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

#[repr(C)]
pub struct Str {
    pub ptr: *const u8,
    pub len: usize,
}

#[repr(C)]
pub struct ClipboardContent {
    pub mime: Str,
    pub data: Str,
}

#[repr(C)]
pub struct ClipboardWrite {
    pub location: c_int,
    pub contents: *const ClipboardContent,
    pub contents_len: usize,
    name: Str,
    granted: bool,
    can_remember: bool,
    ctx: *const c_void,
    pub reply: unsafe extern "C" fn(*const Self, *const ClipboardWriteReply),
}

#[repr(C)]
pub struct ClipboardWriteReply {
    pub size: usize,
    pub result: c_int,
    pub remember: bool,
}

#[repr(C)]
pub struct ClipboardRead {
    location: c_int,
    mimes: *const Str,
    mimes_len: usize,
    list: bool,
    name: Str,
    granted: bool,
    can_remember: bool,
    ctx: *const c_void,
    pub reply: unsafe extern "C" fn(*const Self, *const ClipboardReadReply),
}

#[repr(C)]
pub struct ClipboardReadReply {
    pub size: usize,
    pub result: c_int,
    pub contents: *const ClipboardContent,
    pub contents_len: usize,
    pub available: *const Str,
    pub available_len: usize,
    pub remember: bool,
}

#[repr(C)]
pub struct TerminalModeConfig {
    pub mode: u16,
    pub value: bool,
}

#[repr(C)]
#[derive(Default)]
pub struct Style {
    colors: [[u8; 4]; 3],
    pub flags: u16,
    reserved: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PointCoordinate {
    pub x: u16,
    pub y: u32,
}

#[repr(C)]
pub union PointValue {
    pub coordinate: PointCoordinate,
    _padding: [u64; 2],
}

#[repr(C)]
pub struct Point {
    pub tag: c_int,
    pub value: PointValue,
}

#[repr(C)]
#[derive(Default)]
pub struct GridRef {
    node: *mut c_void,
    x: u16,
    y: u16,
}

#[repr(C)]
pub struct Selection {
    pub start: GridRef,
    pub end: GridRef,
    pub rectangle: bool,
}

#[repr(C)]
pub struct Writer {
    pub write: unsafe extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    pub userdata: *mut c_void,
}

pub type Formatter = *mut c_void;
pub const POINT_TAG_VIEWPORT: c_int = 1;
pub const POINT_TAG_SCREEN: c_int = 2;
pub const FORMATTER_OPT_UNWRAP: c_int = 2;
pub const FORMATTER_OPT_SELECTION: c_int = 4;

pub type KeyEncoder = *mut c_void;
pub type KeyEvent = *mut c_void;

pub type Cell = u64;
pub const SUCCESS: c_int = 0;
pub const INVALID_VALUE: c_int = -2;
pub const OUT_OF_SPACE: c_int = -3;
pub const NO_VALUE: c_int = -4;
pub const TERMINAL_OPT_USERDATA: c_int = 1;
pub const TERMINAL_OPT_WRITE_PTY: c_int = 2;
pub const TERMINAL_OPT_XTVERSION: c_int = 5;
pub const TERMINAL_OPT_COLOR_FOREGROUND: c_int = 12;
pub const TERMINAL_OPT_COLOR_BACKGROUND: c_int = 13;
pub const TERMINAL_OPT_COLOR_CURSOR: c_int = 14;
pub const TERMINAL_OPT_COLOR_PALETTE: c_int = 15;
pub const TERMINAL_OPT_DEFAULT_CURSOR_STYLE: c_int = 23;
pub const TERMINAL_OPT_DEFAULT_CURSOR_BLINK: c_int = 24;
pub const TERMINAL_OPT_TERMINFO_NAME: c_int = 38;
pub const TERMINAL_OPT_TITLE_CHANGED: c_int = 6;
pub const TERMINAL_OPT_CLIPBOARD_WRITE: c_int = 27;
pub const TERMINAL_OPT_CLIPBOARD_READ: c_int = 39;
pub const TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES: c_int = 40;
pub const TERMINAL_DATA_COLS: c_int = 1;
pub const TERMINAL_DATA_TITLE: c_int = 12;
pub const TERMINAL_DATA_TOTAL_ROWS: c_int = 14;
pub const TERMINAL_DATA_COLOR_PALETTE: c_int = 21;
pub const TERMINAL_DATA_MODE: c_int = 37;
pub const CLIPBOARD_LOCATION_STANDARD: c_int = 0;
pub const CLIPBOARD_WRITE_SUCCESS: c_int = 0;
pub const CLIPBOARD_WRITE_DENIED: c_int = 1;
pub const CLIPBOARD_WRITE_UNSUPPORTED: c_int = 2;
pub const CLIPBOARD_READ_DENIED: c_int = 1;
pub const MODE_BRACKETED_PASTE: u16 = 2004;
pub const STYLE_INVERSE: u16 = 1 << 4;
pub const STYLE_INVISIBLE: u16 = 1 << 5;
pub const ROW_DATA_WRAP: c_int = 1;
pub const ROW_DATA_GRAPHEME: c_int = 3;
pub const CELL_DATA_CODEPOINT: c_int = 1;
pub const CELL_DATA_WIDE: c_int = 3;
pub const CELL_WIDE_NARROW: c_int = 0;
pub const CELL_WIDE_WIDE: c_int = 1;
pub const CELL_WIDE_SPACER_TAIL: c_int = 2;
pub const CELL_WIDE_SPACER_HEAD: c_int = 3;
pub const RENDER_STATE_DATA_COLS: c_int = 1;
pub const RENDER_STATE_DATA_ROWS: c_int = 2;
pub const RENDER_STATE_DATA_ROW_ITERATOR: c_int = 4;
pub const RENDER_STATE_DATA_COLOR_BACKGROUND: c_int = 5;
pub const RENDER_STATE_DATA_COLOR_FOREGROUND: c_int = 6;
pub const RENDER_STATE_DATA_COLOR_CURSOR: c_int = 7;
pub const RENDER_STATE_DATA_CURSOR_VISUAL_STYLE: c_int = 10;
pub const RENDER_STATE_DATA_CURSOR_VISIBLE: c_int = 11;
pub const RENDER_STATE_DATA_CURSOR_BLINKING: c_int = 12;
pub const RENDER_STATE_DATA_CURSOR_VIEWPORT_HAS_VALUE: c_int = 14;
pub const RENDER_STATE_DATA_CURSOR_VIEWPORT_X: c_int = 15;
pub const RENDER_STATE_DATA_CURSOR_VIEWPORT_Y: c_int = 16;
pub const RENDER_STATE_DATA_CURSOR_VIEWPORT_WIDE_TAIL: c_int = 17;
pub const CURSOR_STYLE_BAR: c_int = 0;
pub const CURSOR_STYLE_BLOCK: c_int = 1;
pub const CURSOR_STYLE_UNDERLINE: c_int = 2;
pub const CURSOR_STYLE_BLOCK_HOLLOW: c_int = 3;
pub const RENDER_STATE_ROW_DATA_CELLS: c_int = 3;
pub const RENDER_STATE_ROW_CELLS_DATA_RAW: c_int = 1;
pub const RENDER_STATE_ROW_CELLS_DATA_STYLE: c_int = 2;
pub const RENDER_STATE_ROW_CELLS_DATA_BG_COLOR: c_int = 5;
pub const RENDER_STATE_ROW_CELLS_DATA_FG_COLOR: c_int = 6;
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
    pub fn ghostty_terminal_set(terminal: Terminal, option: c_int, value: *const c_void) -> c_int;
    pub fn ghostty_terminal_get(terminal: Terminal, data: c_int, out: *mut c_void) -> c_int;
    pub fn ghostty_terminal_grid_ref(terminal: Terminal, point: Point, out: *mut GridRef) -> c_int;
    pub fn ghostty_grid_ref_cell(reference: *const GridRef, out: *mut Cell) -> c_int;
    pub fn ghostty_grid_ref_row(reference: *const GridRef, out: *mut u64) -> c_int;
    pub fn ghostty_grid_ref_graphemes(
        reference: *const GridRef,
        buffer: *mut u32,
        capacity: usize,
        written: *mut usize,
    ) -> c_int;
    pub fn ghostty_row_get(row: u64, data: c_int, out: *mut c_void) -> c_int;
    pub fn ghostty_formatter_terminal_new(
        allocator: *const Allocator,
        formatter: *mut Formatter,
        terminal: Terminal,
    ) -> c_int;
    pub fn ghostty_formatter_set(
        formatter: Formatter,
        option: c_int,
        value: *const c_void,
    ) -> c_int;
    pub fn ghostty_formatter_format(formatter: Formatter, writer: Writer) -> c_int;
    pub fn ghostty_formatter_free(formatter: Formatter);
    pub fn ghostty_paste_encode(
        data: *mut u8,
        data_len: usize,
        bracketed: bool,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> c_int;
    pub fn ghostty_unicode_codepoint_width(value: u32) -> u8;
    pub fn ghostty_key_encoder_new(allocator: *const Allocator, out: *mut KeyEncoder) -> c_int;
    pub fn ghostty_key_encoder_free(encoder: KeyEncoder);
    pub fn ghostty_key_encoder_set_from_terminal(encoder: KeyEncoder, terminal: Terminal) -> c_int;
    pub fn ghostty_key_encoder_encode(
        encoder: KeyEncoder,
        event: KeyEvent,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> c_int;
    pub fn ghostty_key_event_new(allocator: *const Allocator, out: *mut KeyEvent) -> c_int;
    pub fn ghostty_key_event_free(event: KeyEvent);
    pub fn ghostty_key_event_set_action(event: KeyEvent, action: c_int) -> c_int;
    pub fn ghostty_key_event_set_key(event: KeyEvent, key: c_int) -> c_int;
    pub fn ghostty_key_event_set_mods(event: KeyEvent, mods: u16) -> c_int;
    pub fn ghostty_key_event_set_consumed_mods(event: KeyEvent, mods: u16) -> c_int;
    pub fn ghostty_key_event_set_unshifted_codepoint(event: KeyEvent, codepoint: u32) -> c_int;
    pub fn ghostty_key_event_set_utf8(event: KeyEvent, text: Str) -> c_int;
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
