use std::ops::Range;

use super::{Batches, LayeredFrame, PaneBatch, Renderer, pixel_rect};
use crate::{Fill, PaneDraw, PixelRect, wgpu};

impl Renderer {
    pub(super) fn draw_layered(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        pane: &PaneBatch,
        layers: &LayeredFrame,
        vertex_offset: u64,
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
        let draw_grid = |pass: &mut wgpu::RenderPass<'_>, range: Range<u32>| {
            if !range.is_empty() {
                self.bind_grid(pass);
                pass.draw(0..4, range);
            }
        };
        let draw_images = |pass: &mut wgpu::RenderPass<'_>, range| {
            self.images.draw(pass, layers.exclusion, range, vertices);
        };
        draw_grid(pass, start..base);
        draw_images(pass, images.start..images.below_background);
        draw_grid(pass, base..backgrounds);
        draw_images(pass, images.below_background..images.below_text);
        draw_grid(pass, backgrounds..text);
        draw_images(pass, images.below_text..images.terminal_end);
        draw_grid(pass, text..end);
        self.images
            .draw(pass, 0, images.terminal_end..images.end, vertices);
    }

    pub(super) fn prepare_images<'a>(
        &mut self,
        panes: impl Iterator<Item = PaneDraw<'a>>,
        batches: &mut Batches,
        capacity: u32,
    ) {
        batches.grid = 0;
        for (pane, batch) in panes.zip(&mut self.panes) {
            if let Some(candidates) = batch.candidates.take()
                && let Some(images) = self.images.admit(
                    &self.device,
                    &self.queue,
                    &pane,
                    candidates,
                    capacity - batches.images,
                    batches.images,
                )
            {
                batches.images = images.end;
                let prefill = u32::from(batch.base.is_some());
                let grid = pane.draw.image_layers(images.base, batch.count - prefill);
                if images.terminal_end != images.end {
                    batch.backdrop = Some(Fill {
                        rect: pixel_rect(batch.rect),
                        color: pane.viewer.expect("admitted viewer").backdrop,
                    });
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
