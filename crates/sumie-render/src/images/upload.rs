use std::num::NonZeroU64;

use sumie_core::graphics::{Image, PixelFormat};
use wgpu::util::StagingBelt;

use super::{BoundTexture, expand::Expander};

const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) struct Upload<'a> {
    pub(crate) device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    staging: &'a mut StagingBelt,
    encoder: &'a mut wgpu::CommandEncoder,
    chunk: Option<(wgpu::Buffer, u64)>,
    chunk_bytes: u64,
    used: u64,
    scratch_row: u32,
}

impl<'a> Upload<'a> {
    pub(crate) fn new(
        device: &'a wgpu::Device,
        queue: &'a wgpu::Queue,
        staging: &'a mut StagingBelt,
        encoder: &'a mut wgpu::CommandEncoder,
    ) -> Self {
        let chunk_bytes = CHUNK_BYTES.min(device.limits().max_buffer_size);
        Self {
            device,
            queue,
            staging,
            encoder,
            chunk: None,
            chunk_bytes: chunk_bytes / wgpu::MAP_ALIGNMENT * wgpu::MAP_ALIGNMENT,
            used: 0,
            scratch_row: 0,
        }
    }

    pub(super) fn write(
        &mut self,
        bound: &BoundTexture,
        image: &Image,
        pixels: &[u8],
        recycled: bool,
        expander: &mut Expander,
    ) {
        let texture = bound.view.texture();
        if image.format != PixelFormat::Rgba {
            let base = expander.stage(self.device, self.queue, &mut self.scratch_row, pixels);
            expander.draw(self.staging, self.encoder, &bound.view, image, base);
            return;
        }
        if recycled && self.stage(texture, image, pixels) {
            return;
        }
        self.queue.write_texture(
            texture.as_image_copy(),
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.size[0] * 4),
                rows_per_image: None,
            },
            texture.size(),
        );
    }

    fn stage(&mut self, texture: &wgpu::Texture, image: &Image, pixels: &[u8]) -> bool {
        let row = image.size[0] * 4;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let stride = row.next_multiple_of(alignment);
        let bytes = u64::from(stride) * u64::from(image.size[1] - 1) + u64::from(row);
        let end = self.used + bytes;
        // The belt retains its chunks. Reserve one fixed-size range, even for small updates.
        if end > self.chunk_bytes {
            return false;
        }
        let (buffer, base) = self.chunk.get_or_insert_with(|| {
            let slice = self.staging.allocate(
                NonZeroU64::new(self.chunk_bytes).unwrap(),
                const { NonZeroU64::new(wgpu::MAP_ALIGNMENT).unwrap() },
            );
            (slice.buffer().clone(), slice.offset())
        });
        let offset = *base + self.used;
        self.used = end.next_multiple_of(wgpu::MAP_ALIGNMENT);
        let mut dst = buffer
            .slice(offset..offset + bytes)
            .get_mapped_range_mut()
            .unwrap();
        if row == stride || image.size[1] == 1 {
            dst.slice(..).copy_from_slice(pixels);
        } else {
            for (i, src) in pixels.chunks_exact(row as usize).enumerate() {
                let start = i * stride as usize;
                dst.slice(start..start + row as usize).copy_from_slice(src);
            }
        }
        drop(dst);
        // This copy follows the previous submission and precedes this frame's render pass.
        self.encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            texture.as_image_copy(),
            texture.size(),
        );
        true
    }
}
