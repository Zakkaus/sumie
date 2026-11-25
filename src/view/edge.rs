//! Progress along the top edge of the window while a single tab hides the strip.

use sumie_core::notify::Progress;
use sumie_render::{Fill, PixelRect};
use sumie_ui::{TabPalette, progress_bar};
use sumie_vt::Snapshot;

use super::{View, rgba};

impl View {
    /// Call after composing the terminal panes. The bar lies over the first device pixels without
    /// moving the grid, and under the active pane's banner and search bar.
    pub(crate) fn draw_edge_progress(&mut self, progress: Option<Progress>, snapshot: &Snapshot) {
        let Some(progress) = progress else {
            return;
        };
        let palette = TabPalette::new(
            rgba(snapshot.background),
            rgba(snapshot.foreground),
            rgba(snapshot.ansi_yellow),
            rgba(snapshot.ansi_red),
        );
        let mut bar = Vec::new();
        let [width, height] = self.device;
        progress_bar(
            palette,
            self.scale_units(),
            [0, width],
            height,
            progress,
            &mut bar,
        );
        let overlays = self
            .active_pane
            .map_or([None; 2], |pane| self.panes[pane].image_exclusions);
        for fill in bar {
            clear_of(fill, overlays.into_iter().flatten(), &mut self.fills);
        }
    }
}

/// Push `fill` minus the horizontal spans where an overlay reaches into its rows.
fn clear_of(fill: Fill, overlays: impl Iterator<Item = PixelRect>, out: &mut Vec<Fill>) {
    let [x, y] = fill.rect.origin;
    let [width, height] = fill.rect.size;
    let rows = y..y + height.cast_signed();
    let mut spans = vec![(x, x + width.cast_signed())];
    for overlay in overlays {
        let [left, top] = overlay.origin;
        if top >= rows.end || top + overlay.size[1].cast_signed() <= rows.start {
            continue;
        }
        let right = left + overlay.size[0].cast_signed();
        spans = spans
            .into_iter()
            .flat_map(|(start, end)| [(start, end.min(left)), (start.max(right), end)])
            .filter(|(start, end)| start < end)
            .collect();
    }
    out.extend(spans.into_iter().map(|(start, end)| Fill {
        rect: PixelRect {
            origin: [start, y],
            size: [(end - start).cast_unsigned(), height],
        },
        color: fill.color,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_cut_the_bar_only_where_they_reach_its_rows() {
        let at = |x, y, width, height| PixelRect {
            origin: [x, y],
            size: [width, height],
        };
        let bar = Fill {
            rect: at(0, 0, 100, 2),
            color: [1, 2, 3, 255],
        };
        // (overlays, spans left of the bar)
        let table = [
            (vec![], vec![(0, 100)]),
            (vec![at(10, 2, 20, 5)], vec![(0, 100)]),
            (vec![at(10, 1, 20, 5)], vec![(0, 10), (30, 100)]),
            (vec![at(0, 0, 100, 18)], vec![]),
            (
                vec![at(80, 0, 40, 9), at(10, 0, 10, 9)],
                vec![(0, 10), (20, 80)],
            ),
        ];
        for (overlays, expected) in table {
            let mut out = Vec::new();
            clear_of(bar, overlays.iter().copied(), &mut out);
            let spans: Vec<_> = out
                .iter()
                .map(|fill| {
                    (
                        fill.rect.origin[0],
                        fill.rect.origin[0] + fill.rect.size[0].cast_signed(),
                    )
                })
                .collect();
            assert_eq!(spans, expected, "{overlays:?}");
            assert!(
                out.iter()
                    .all(|fill| fill.rect.size[1] == 2 && fill.color == bar.color)
            );
        }
    }
}
