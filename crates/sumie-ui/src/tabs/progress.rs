//! OSC 9;4 progress along the top edge of a tab, drawn from the strip's own colors.

use sumie_core::notify::{Progress, ProgressState};
use sumie_render::Fill;

use super::{TabLayout, fill, rect};
use crate::palette::{TabPalette, tint_over};
use crate::scaled;
use crate::tokens;

impl TabLayout {
    pub(super) fn progress_bars(&self, palette: TabPalette, out: &mut Vec<Fill>) {
        for index in self.visible() {
            if let Some(progress) = self.slot(index).progress {
                let content = self.content(index);
                let x = content.origin[0].cast_unsigned();
                progress_bar(
                    palette,
                    self.scale,
                    [x, content.size[0]],
                    self.height,
                    progress,
                    out,
                );
            }
        }
    }
}

/// A 2 px bar along the top edge at `[x, width]` device pixels, at most `limit` high: the track,
/// then the report's fill. Also the bar of a window whose strip is hidden.
///
/// # Panics
/// Never in practice: a share of a `u32` width fits a `u32`.
pub fn progress_bar(
    palette: TabPalette,
    scale: u16,
    [x, width]: [u32; 2],
    limit: u32,
    progress: Progress,
    out: &mut Vec<Fill>,
) {
    let height = scaled(tokens::INDICATOR, scale).min(limit).max(1);
    fill(
        out,
        rect(x, 0, width, height),
        tint_over(palette.surface, palette.selected, 23),
    );
    let color = match progress.state {
        ProgressState::Set | ProgressState::Indeterminate => palette.active_number,
        ProgressState::Paused => palette.label,
        ProgressState::Error => palette.error,
    };
    if progress.state == ProgressState::Indeterminate {
        let dash = scaled(tokens::ITEM_GAP, scale).max(1);
        let mut at = 0;
        while at < width {
            fill(out, rect(x + at, 0, dash.min(width - at), height), color);
            at += 2 * dash;
        }
        return;
    }
    let default = u8::from(progress.state != ProgressState::Set) * 100;
    let percent = u64::from(progress.percent.unwrap_or(default));
    let filled = u32::try_from(u64::from(width) * percent / 100).expect("a share of the width");
    fill(out, rect(x, 0, filled, height), color);
}
