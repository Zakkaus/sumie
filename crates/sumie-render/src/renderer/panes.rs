use std::ops::Range;

use super::{AlphaPipelines, Batches, LayeredFrame, PaneBatch, Renderer, pixel_rect};
use crate::images::{Images, Upload};
use crate::{Fill, PaneDraw, PixelRect, Rgba, wgpu};

impl PaneBatch {
    pub(super) fn set_base(&mut self, pane: &PaneDraw<'_>, background: Rgba, translucent: bool) {
        let mut color = pane.draw.default_background;
        if !translucent {
            color[3] = 255;
        }
        // A viewer uses an opaque pane only if its texture is admitted. Reserve a base for it.
        if color != background || (translucent && pane.viewer.is_some()) {
            self.base = Some(Fill {
                rect: pixel_rect(self.rect),
                color,
            });
        }
    }

    fn set_viewer_backdrop(&mut self, color: Rgba, translucent: bool) {
        self.backdrop = Some(Fill {
            rect: pixel_rect(self.rect),
            color,
        });
        if translucent && let Some(base) = &mut self.base {
            base.color[3] = 255;
        }
    }
}

impl Renderer {
    pub(super) fn draw_grid_pane(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pane: &PaneBatch,
        alpha: Option<&AlphaPipelines>,
        mut bound: bool,
    ) -> bool {
        let mut start = pane.start;
        let end = start + pane.count;
        if let Some(alpha) = alpha
            && pane.backgrounds != 0
        {
            let background_end = start + pane.backgrounds;
            self.bind_grid(pass, &alpha.replace);
            pass.draw(0..4, start..background_end);
            start = background_end;
            if start == end {
                return false;
            }
            bound = false;
        }
        if !bound {
            self.bind_grid(pass, &self.pipeline);
        }
        pass.draw(0..4, start..end);
        true
    }

    pub(super) fn draw_layered(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pane: &PaneBatch,
        layers: &LayeredFrame,
        vertex_offset: u64,
        alpha: Option<&AlphaPipelines>,
    ) {
        let grid = &layers.grid;
        let images = &layers.images;
        let vertices = self.frame.slice(vertex_offset..);
        let start = pane.start;
        let prefill = u32::from(pane.base.is_some());
        let base = start + prefill + u32::from(grid.base.is_some());
        let backgrounds = start + prefill + grid.backgrounds;
        let text = start + prefill + grid.text;
        let end = start + pane.count;
        let base_pipeline = alpha.map_or(&self.pipeline, |alpha| &alpha.replace);
        let draw_grid = |pass: &mut wgpu::RenderPass<'_>, range: Range<u32>, pipeline| {
            if !range.is_empty() {
                self.bind_grid(pass, pipeline);
                pass.draw(0..4, range);
            }
        };
        let draw_images = |pass: &mut wgpu::RenderPass<'_>, range| {
            self.images.draw(pass, layers.exclusion, range, vertices);
        };
        draw_grid(pass, start..base, base_pipeline);
        draw_images(pass, images.start..images.below_background);
        draw_grid(pass, base..backgrounds, base_pipeline);
        draw_images(pass, images.below_background..images.below_text);
        draw_grid(pass, backgrounds..text, &self.pipeline);
        draw_images(pass, images.below_text..images.terminal_end);
        draw_grid(pass, text..end, &self.pipeline);
        self.images
            .draw(pass, 0, images.terminal_end..images.end, vertices);
    }

    fn reserve_viewers<'a>(
        images: &mut Images,
        batches: &mut [PaneBatch],
        upload: &mut Upload<'_>,
        panes: impl Iterator<Item = PaneDraw<'a>>,
        remaining: &mut u64,
    ) {
        for (pane, batch) in panes.zip(batches) {
            if let (Some(viewer), Some(candidates)) = (pane.viewer, &mut batch.candidates) {
                images.reserve_viewer(upload, viewer.placement, candidates, remaining);
            }
        }
    }

    pub(super) fn prepare_images<'a>(
        &mut self,
        panes: impl Clone + Iterator<Item = PaneDraw<'a>>,
        batches: &mut Batches,
        mut remaining: u64,
        has_viewers: bool,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let mut upload = Upload::new(&self.device, &self.queue, &mut self.staging, encoder);
        // Pin viewers and reserve their instance bytes before any terminal image can fill them.
        if has_viewers {
            Self::reserve_viewers(
                &mut self.images,
                &mut self.panes,
                &mut upload,
                panes.clone(),
                &mut remaining,
            );
        }
        batches.grid = 0;
        for (pane, batch) in panes.zip(&mut self.panes) {
            if let Some(candidates) = &batch.candidates
                && let Some(images) = self.images.admit(
                    &mut upload,
                    &pane.draw,
                    candidates,
                    &mut remaining,
                    batches.images,
                )
            {
                batches.images = images.end;
                let prefill = u32::from(batch.base.is_some());
                let grid = pane.draw.image_layers(images.base, batch.count - prefill);
                if images.terminal_end != images.end {
                    batch.set_viewer_backdrop(
                        pane.viewer.expect("admitted viewer").backdrop,
                        batches.translucent,
                    );
                }
                batch.count = grid.end + prefill + u32::from(batch.backdrop.is_some());
                let exclusion = if pane.draw.image_exclusions == [None; 2]
                    || images.start == images.terminal_end
                {
                    0
                } else {
                    let slot = batches.exclusions;
                    batches.exclusions += 1;
                    slot
                };
                batch.layered = Some(LayeredFrame {
                    grid,
                    images,
                    exclusion,
                });
            }
            batch.start = batches.grid;
            batches.grid += batch.count;
        }
        self.images.finish_frame(batches.images);
    }

    pub(crate) fn viewer_presented(&self, pane_index: usize) -> bool {
        self.panes
            .get(pane_index)
            .is_some_and(|pane| pane.backdrop.is_some())
    }

    pub(crate) fn thumbnail_layers(&self) -> impl Iterator<Item = (usize, usize, PixelRect)> + '_ {
        self.panes
            .iter()
            .enumerate()
            .filter_map(|(pane, batch)| {
                let images = &batch.layered.as_ref()?.images;
                images
                    .has_thumbnail
                    .then_some((pane, images.start..images.terminal_end))
            })
            .flat_map(|(pane, range)| {
                self.images
                    .bounds(range)
                    .map(move |(index, rect)| (pane, index, rect))
            })
    }
}
