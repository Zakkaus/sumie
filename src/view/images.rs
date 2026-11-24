//! Fit pane-local viewers and retain hit targets only for images the renderer drew.

use std::{
    ops::Range,
    sync::{Arc, Weak},
};

use sumie_core::{graphics::Placement, splits::Rect};
use sumie_render::{ImageViewer, PixelRect, Rgba, ThumbnailChrome};
use sumie_ui::IMAGE_VIEWER_MARGIN;

use super::{PanePaint, View};

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
    hovered: Option<HoveredThumbnail>,
    hover_dirty: bool,
}

struct PresentedPane {
    id: u64,
    images: Arc<Vec<Placement>>,
    exclusions: [Option<PixelRect>; 2],
    hits: Range<usize>,
}

struct HoveredThumbnail {
    pane: u64,
    // Do not keep the old pane's image buffers alive after a replacement frame.
    images: Weak<Vec<Placement>>,
    index: usize,
}

impl HoveredThumbnail {
    fn index_in(&self, pane: u64, images: &Arc<Vec<Placement>>) -> Option<usize> {
        (self.pane == pane && self.images.as_ptr() == Arc::as_ptr(images)).then_some(self.index)
    }
}

impl PresentedImages {
    pub(super) fn clear(&mut self) {
        self.panes.clear();
        self.bounds.clear();
        self.hovered = None;
        self.hover_dirty = false;
    }

    fn hit(&self, pixel: [f64; 2], id: u64) -> Option<(&PresentedPane, usize)> {
        let pane = self.panes.iter().find(|pane| pane.id == id)?;
        let contains = |rect: PixelRect| {
            (0..2).all(|i| {
                (0.0..f64::from(rect.size[i])).contains(&(pixel[i] - f64::from(rect.origin[i])))
            })
        };
        if pane.exclusions.into_iter().flatten().any(contains) {
            return None;
        }
        let &(index, _) = self.bounds[pane.hits.clone()]
            .iter()
            .rev()
            .find(|(_, rect)| contains(*rect))?;
        pane.images[index].thumbnail.then_some((pane, index))
    }
}

impl View {
    /// Publish the placements eligible for hit testing and animation after presentation.
    pub(crate) fn images_presented(
        &mut self,
        admitted: impl Iterator<Item = (usize, usize, PixelRect)>,
    ) {
        let presented = &mut self.presented_images;
        presented.panes.clear();
        presented.bounds.clear();
        presented.hover_dirty = false;
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

    pub(crate) fn visible_placements(
        &self,
        id: u64,
        current: &Arc<Vec<Placement>>,
    ) -> impl Iterator<Item = &Placement> {
        let presented = &self.presented_images;
        presented
            .panes
            .iter()
            .find(|pane| pane.id == id && Arc::ptr_eq(&pane.images, current))
            .into_iter()
            .flat_map(|pane| {
                presented.bounds[pane.hits.clone()]
                    .iter()
                    .map(|&(i, _)| &pane.images[i])
            })
    }

    /// Find a thumbnail only if it is the foremost image at this logical window position.
    pub(crate) fn thumbnail_hit(&self, position: (f64, f64), pane_id: u64) -> Option<&Placement> {
        let (pane, index) = self
            .presented_images
            .hit(self.window_pixel(position), pane_id)?;
        Some(&pane.images[index])
    }

    /// A pointer move only changes the image pass, not the cached terminal paint.
    pub(crate) fn hover_thumbnail(
        &mut self,
        target: Option<(u64, (f64, f64))>,
        eligible: impl FnOnce(u64) -> bool,
    ) -> bool {
        let target = target.map(|(pane, position)| (pane, self.window_pixel(position)));
        let presented = &mut self.presented_images;
        let hit = target
            .and_then(|(pane, pixel)| presented.hit(pixel, pane))
            .filter(|(pane, _)| eligible(pane.id));
        let over = hit.is_some();
        match (&presented.hovered, hit) {
            (None, None) => return false,
            (Some(old), Some((pane, index)))
                if old.index_in(pane.id, &pane.images) == Some(index) =>
            {
                return true;
            }
            _ => (),
        }
        presented.hovered = hit.map(|(pane, index)| HoveredThumbnail {
            pane: pane.id,
            images: Arc::downgrade(&pane.images),
            index,
        });
        presented.hover_dirty = true;
        over
    }

    pub(crate) const fn thumbnail_hover_dirty(&self) -> bool {
        self.presented_images.hover_dirty
    }

    pub(super) fn thumbnail_chrome(&self, pane: &PanePaint) -> Option<ThumbnailChrome> {
        let images = pane.images.as_ref()?;
        let hovered = self.presented_images.hovered.as_ref().and_then(|hover| {
            // A pending snapshot can have a different placement at the same draw-list index.
            hover.index_in(pane.id?, images)
        });
        Some(self.palette.thumbnails(self.scale_units(), hovered))
    }
}
