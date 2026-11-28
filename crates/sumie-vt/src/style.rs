use std::ptr::from_mut;

use crate::{Error, Rgb, Terminal, check, ffi};

/// Underline shape for a visible cell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

/// Font attributes and visible decorations for a cell.
#[expect(
    clippy::struct_excessive_bools,
    reason = "terminal style flags are independent"
)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub strikethrough: bool,
    pub overline: bool,
    /// This visible cell belongs to an OSC 8 hyperlink.
    pub hyperlink: bool,
    pub underline: Underline,
    /// Explicit underline color resolved against the active palette, or `None` to use the foreground.
    pub underline_color: Option<Rgb>,
}

impl Terminal {
    pub(super) fn row_may_have_hyperlinks(&self) -> Result<bool, Error> {
        let mut raw: ffi::Row = 0;
        let mut linked = false;
        // SAFETY: RAW writes the live iterator's GhosttyRow; HYPERLINK reads it and writes a bool.
        unsafe {
            check(ffi::ghostty_render_state_row_get(
                self.row_iter,
                ffi::RENDER_STATE_ROW_DATA_RAW,
                from_mut(&mut raw).cast(),
            ))?;
            check(ffi::ghostty_row_get(
                raw,
                ffi::ROW_DATA_HYPERLINK,
                from_mut(&mut linked).cast(),
            ))?;
        }
        Ok(linked)
    }

    pub(super) fn cell_style(
        &self,
        cell: ffi::Cell,
        row_may_have_hyperlinks: bool,
        native: &ffi::Style,
        palette: &mut Option<[Rgb; 256]>,
    ) -> Result<CellStyle, Error> {
        let flags = native.flags;
        let mut style = CellStyle {
            bold: flags & ffi::STYLE_BOLD != 0,
            italic: flags & ffi::STYLE_ITALIC != 0,
            faint: flags & ffi::STYLE_FAINT != 0,
            ..CellStyle::default()
        };
        if flags & ffi::STYLE_INVISIBLE != 0 {
            return Ok(style);
        }
        style.strikethrough = flags & ffi::STYLE_STRIKETHROUGH != 0;
        style.overline = flags & ffi::STYLE_OVERLINE != 0;
        style.underline = match (flags >> ffi::STYLE_UNDERLINE_SHIFT) & 0x7 {
            ffi::SGR_UNDERLINE_NONE => Underline::None,
            ffi::SGR_UNDERLINE_SINGLE => Underline::Single,
            ffi::SGR_UNDERLINE_DOUBLE => Underline::Double,
            ffi::SGR_UNDERLINE_CURLY => Underline::Curly,
            ffi::SGR_UNDERLINE_DOTTED => Underline::Dotted,
            ffi::SGR_UNDERLINE_DASHED => Underline::Dashed,
            _ => return Err(Error(ffi::INVALID_VALUE)),
        };
        style.hyperlink = row_may_have_hyperlinks && has_hyperlink(cell)?;
        style.underline_color = self.underline_color(native.underline_color, palette)?;
        Ok(style)
    }

    fn underline_color(
        &self,
        color: [u8; 4],
        palette: &mut Option<[Rgb; 256]>,
    ) -> Result<Option<Rgb>, Error> {
        match color {
            [ffi::STYLE_COLOR_NONE, ..] => Ok(None),
            [ffi::STYLE_COLOR_RGB, r, g, b] => Ok(Some([r, g, b])),
            [ffi::STYLE_COLOR_PALETTE, index, ..] => {
                let colors = if let Some(colors) = palette {
                    colors
                } else {
                    palette.insert(self.render_palette()?)
                };
                Ok(Some(colors[usize::from(index)]))
            }
            _ => Err(Error(ffi::INVALID_VALUE)),
        }
    }

    pub(super) fn palette_entry(
        &self,
        index: u8,
        palette: Option<&[Rgb; 256]>,
    ) -> Result<Rgb, Error> {
        if let Some(colors) = palette {
            return Ok(colors[usize::from(index)]);
        }
        let mut rgb = [0; 3];
        // SAFETY: The render state is live and the bridge writes exactly one captured RGB value.
        check(unsafe { ffi::sumie_palette_color(self.render, index, &raw mut rgb) })?;
        Ok(rgb)
    }

    fn render_palette(&self) -> Result<[Rgb; 256], Error> {
        let mut palette = [[0; 3]; 256];
        // SAFETY: The render state is live and COLOR_PALETTE writes 256 RGB values.
        unsafe {
            check(ffi::ghostty_render_state_get(
                self.render,
                ffi::RENDER_STATE_DATA_COLOR_PALETTE,
                from_mut(&mut palette).cast(),
            ))?;
        }
        Ok(palette)
    }
}

pub(super) fn has_hyperlink(cell: ffi::Cell) -> Result<bool, Error> {
    let mut linked = false;
    // SAFETY: Ghostty supplied the cell and HAS_HYPERLINK writes a C bool.
    check(unsafe {
        ffi::ghostty_cell_get(
            cell,
            ffi::CELL_DATA_HAS_HYPERLINK,
            from_mut(&mut linked).cast(),
        )
    })?;
    Ok(linked)
}

#[cfg(test)]
mod tests {
    use super::{CellStyle, Terminal, Underline};

    #[test]
    fn osc104_restores_extended_palette_default_after_reload() {
        let mut terminal = Terminal::new(5, 2).unwrap();
        terminal.write(b"\x1b[38;5;196mX");
        let original = terminal.snapshot().unwrap()[0][0].foreground;
        terminal.write(b"\x1b]4;196;rgb:12/34/56\x07");
        terminal
            .set_colors([255; 3], [0; 3], None, &[[0; 3]; 16])
            .unwrap();
        assert_eq!(
            terminal.snapshot().unwrap()[0][0].foreground,
            [0x12, 0x34, 0x56]
        );
        terminal.write(b"\x1b]104;196\x07");
        assert_eq!(terminal.snapshot().unwrap()[0][0].foreground, original);
    }

    #[test]
    fn snapshot_keeps_cell_style() {
        let mut terminal = Terminal::new(8, 1).unwrap();
        terminal.write(b"\x1b[1;2;3;9;53m\x1b[4:3m\x1b[58:2::1:2:3mA\x1b[8mB\x1b[0m");
        terminal.write(b"\x1b]4;9;rgb:12/34/56\x1b\\\x1b[4:2m\x1b[58:5:9mC\x1b[0m");
        terminal.write(b"\x1b]8;;https://example.test\x1b\\D\x1b[4:5mE\x1b]8;;\x1b\\\x1b[0mF");
        let screen = terminal.snapshot().unwrap();
        let styled = CellStyle {
            bold: true,
            italic: true,
            faint: true,
            strikethrough: true,
            overline: true,
            hyperlink: false,
            underline: Underline::Curly,
            underline_color: Some([1, 2, 3]),
        };
        let decorated = |underline, underline_color| CellStyle {
            underline,
            underline_color,
            ..CellStyle::default()
        };
        let expected = [
            ("A", styled),
            (
                "",
                CellStyle {
                    bold: true,
                    italic: true,
                    faint: true,
                    ..CellStyle::default()
                },
            ),
            ("C", decorated(Underline::Double, Some([0x12, 0x34, 0x56]))),
            (
                "D",
                CellStyle {
                    hyperlink: true,
                    ..CellStyle::default()
                },
            ),
            (
                "E",
                CellStyle {
                    hyperlink: true,
                    ..decorated(Underline::Dashed, None)
                },
            ),
            ("F", CellStyle::default()),
        ];
        for (cell, expected) in screen[0][..expected.len()].iter().zip(expected) {
            assert_eq!((cell.text.as_str(), cell.style), expected);
        }
    }
}
