use std::num::NonZeroU64;

use sumie_core::graphics::{Image, PixelFormat};
use wgpu::{WriteOnly, util::StagingBelt};

const CHUNK_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) struct Upload<'a> {
    pub(crate) device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    staging: &'a mut StagingBelt,
    encoder: &'a mut wgpu::CommandEncoder,
    chunk: Option<(wgpu::Buffer, u64)>,
    chunk_bytes: u64,
    used: u64,
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
        }
    }

    pub(super) fn write(
        &mut self,
        texture: &wgpu::Texture,
        image: &Image,
        pixels: &[u8],
        recycled: bool,
        rgba: &mut Vec<u8>,
    ) {
        if (recycled || image.format != PixelFormat::Rgba) && self.stage(texture, image, pixels) {
            return;
        }
        let pixels = if image.format == PixelFormat::Rgba {
            pixels
        } else {
            let len = pixels.len() / image.format.channels() * 4;
            if len > rgba.len() {
                rgba.reserve_exact(len - rgba.len());
                rgba.resize(len, 0);
            }
            let out = &mut rgba[..len];
            write_rgba(WriteOnly::from_mut(out), pixels, image.format);
            out
        };
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
            write_rgba(dst.slice(..), pixels, image.format);
        } else {
            let src_row = image.size[0] as usize * image.format.channels();
            for (i, src) in pixels.chunks_exact(src_row).enumerate() {
                let start = i * stride as usize;
                write_rgba(dst.slice(start..start + row as usize), src, image.format);
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

fn write_rgba(mut dst: WriteOnly<'_, [u8]>, pixels: &[u8], format: PixelFormat) {
    match format {
        PixelFormat::Rgba => dst.copy_from_slice(pixels),
        PixelFormat::Rgb => dst.into_chunks::<4>().0.write_iter(
            pixels
                .as_chunks::<3>()
                .0
                .iter()
                .map(|&[r, g, b]| [r, g, b, 255]),
        ),
        PixelFormat::Gray => dst
            .into_chunks::<4>()
            .0
            .write_iter(pixels.iter().map(|&gray| [gray, gray, gray, 255])),
        PixelFormat::GrayAlpha => dst.into_chunks::<4>().0.write_iter(
            pixels
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[gray, alpha]| [gray, gray, gray, alpha]),
        ),
    }
}
