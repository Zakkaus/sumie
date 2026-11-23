//! Fit pane-local viewers and retain hit targets only for images the renderer drew.

use std::{ops::Range, sync::Arc};

use sumie_core::{graphics::Placement, splits::Rect};
use sumie_render::{ImageViewer, PixelRect, Rgba};
use sumie_ui::IMAGE_VIEWER_MARGIN;

use super::View;

pub(super) struct ViewerPaint {
    placement: Placement,
    dest: PixelRect,
}

impl ViewerPaint {
    pub(super) fn new(placement: &Placement, pane: Rect, scale: u32) -> Option<Self> {
        let extent = [pane.width, pane.height];
        let [_, _, width, height] = placement.source;
        if extent.contains(&0) || width == 0 || height == 0 {
            return None;
        }
        let margin = (IMAGE_VIEWER_MARGIN * scale + 60) / 120;
        let available = extent.map(|pixels| pixels - 2 * margin.min((pixels - 1) / 2));
        let [w, h] = available.map(u64::from);
        let cropped_width = u64::from(width);
        let cropped_height = u64::from(height);
        let size = if w * cropped_height <= h * cropped_width {
            [w, (w * cropped_height / cropped_width).max(1)]
        } else {
            [(h * cropped_width / cropped_height).max(1), h]
        }
        .map(|n| u32::try_from(n).expect("fit inside the pane"));
        Some(Self {
            placement: placement.clone(),
            dest: PixelRect {
                origin: [
                    (pane.x + (pane.width - size[0]) / 2).cast_signed(),
                    (pane.y + (pane.height - size[1]) / 2).cast_signed(),
                ],
                size,
            },
        })
    }

    pub(super) const fn draw(&self, backdrop: Rgba) -> ImageViewer<'_> {
        ImageViewer {
            placement: &self.placement,
            dest: self.dest,
            backdrop,
        }
    }
}

#[derive(Default)]
pub(super) struct PresentedImages {
    panes: Vec<PresentedPane>,
    bounds: Vec<(usize, PixelRect)>,
}

struct PresentedPane {
    id: u64,
    images: Arc<Vec<Placement>>,
    exclusions: [Option<PixelRect>; 2],
    hits: Range<usize>,
}

impl PresentedImages {
    pub(super) fn clear(&mut self) {
        self.panes.clear();
        self.bounds.clear();
    }
}

impl View {
    /// Publish thumbnail hit layers in draw order after their surface presentation.
    pub(crate) fn images_presented(
        &mut self,
        admitted: impl Iterator<Item = (usize, usize, PixelRect)>,
    ) {
        let presented = &mut self.presented_images;
        presented.clear();
        let mut last_pane = None;
        for (pane_index, image_index, bounds) in admitted {
            let pane = &self.panes[pane_index];
            let next = presented.bounds.len();
            if last_pane != Some(pane_index) {
                presented.panes.push(PresentedPane {
                    id: pane.id.expect("drawn pane"),
                    images: Arc::clone(pane.images.as_ref().expect("drawn placements")),
                    exclusions: pane.image_exclusions,
                    hits: next..next,
                });
                last_pane = Some(pane_index);
            }
            presented.bounds.push((image_index, bounds));
            presented.panes.last_mut().unwrap().hits.end = next + 1;
        }
    }

    /// Find a thumbnail only if it is the foremost image at this logical window position.
    pub(crate) fn thumbnail_hit(&self, position: (f64, f64), pane_id: u64) -> Option<&Placement> {
        let presented = &self.presented_images;
        let pane = presented.panes.iter().find(|pane| pane.id == pane_id)?;
        let pixel = self.window_pixel(position);
        let contains = |rect: PixelRect| {
            (0..2).all(|i| {
                (0.0..f64::from(rect.size[i])).contains(&(pixel[i] - f64::from(rect.origin[i])))
            })
        };
        if pane.exclusions.into_iter().flatten().any(contains) {
            return None;
        }
        let (index, _) = presented.bounds[pane.hits.clone()]
            .iter()
            .rev()
            .find(|(_, rect)| contains(*rect))?;
        let placement = &pane.images[*index];
        placement.thumbnail.then_some(placement)
    }
}
