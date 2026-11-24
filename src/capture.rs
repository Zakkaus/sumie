use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use sumie_render::wgpu;

use crate::Result;

#[derive(Default)]
pub(crate) struct Options {
    pub(crate) frames: Option<u64>,
    pub(crate) screenshot: Option<PathBuf>,
}

impl Options {
    pub(crate) fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Self> {
        let mut options = Self::default();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--exit-after-frames") if options.frames.is_none() => {
                    let count = args
                        .next()
                        .and_then(|value| value.to_str()?.parse::<std::num::NonZeroU64>().ok());
                    options.frames = Some(
                        count
                            .ok_or("--exit-after-frames requires a positive integer")?
                            .get(),
                    );
                }
                Some("--screenshot") if options.screenshot.is_none() => {
                    options.screenshot =
                        Some(args.next().ok_or("--screenshot requires a path")?.into());
                }
                _ => return Err("usage: sumie [--exit-after-frames N [--screenshot PATH]] | sumie config {schema | show --defaults}".into()),
            }
        }
        if options.screenshot.is_some() && options.frames.is_none() {
            return Err("--screenshot requires --exit-after-frames".into());
        }
        Ok(options)
    }
}

pub(crate) struct Readback {
    buffer: wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    mapped: mpsc::Receiver<std::result::Result<(), wgpu::BufferAsyncError>>,
    extent: wgpu::Extent3d,
    stride: u32,
    bgra: bool,
    premultiplied: bool,
}

impl Readback {
    pub(crate) fn copy(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &wgpu::Texture,
        premultiplied: bool,
    ) -> Result<Self> {
        let format = frame.format();
        let bgra = match format {
            wgpu::TextureFormat::Rgba8UnormSrgb => false,
            wgpu::TextureFormat::Bgra8UnormSrgb => true,
            _ => return Err("cannot read this surface's color format as RGBA8".into()),
        };
        let extent = frame.size();
        let stride = (extent.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sumie screenshot readback"),
            size: u64::from(stride) * u64::from(extent.height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let offscreen = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("sumie screenshot copy"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_texture(frame.as_image_copy(), offscreen.as_image_copy(), extent);
        encoder.copy_texture_to_buffer(
            offscreen.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
            },
            extent,
        );
        let (sender, mapped) = mpsc::sync_channel(1);
        encoder.map_buffer_on_submit(&buffer, wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        let submission = queue.submit([encoder.finish()]);
        Ok(Self {
            buffer,
            submission,
            mapped,
            extent,
            stride,
            bgra,
            premultiplied,
        })
    }

    pub(crate) fn write(self, device: &wgpu::Device, path: &Path) -> Result<()> {
        let Self {
            buffer,
            submission,
            mapped,
            extent,
            stride,
            bgra,
            premultiplied,
        } = self;
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(Duration::from_secs(5)),
        })?;
        mapped.try_recv()??;
        let pixels = buffer.get_mapped_range(..)?;
        let row_bytes = (extent.width * 4) as usize;
        let mut encoder = png::Encoder::new(
            BufWriter::new(File::create(path)?),
            extent.width,
            extent.height,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        let mut writer = encoder.write_header()?;
        let mut stream = writer.stream_writer()?;
        let convert = bgra || premultiplied;
        let mut rgba = vec![0; if convert { row_bytes } else { 0 }];
        for row in pixels
            .chunks_exact(stride as usize)
            .map(|row| &row[..row_bytes])
        {
            if convert {
                for (dst, &src) in rgba
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(row.as_chunks::<4>().0)
                {
                    *dst = src;
                    if bgra {
                        dst.swap(0, 2);
                    }
                    let alpha = u16::from(dst[3]);
                    if premultiplied && (1..255).contains(&alpha) {
                        for channel in &mut dst[..3] {
                            *channel =
                                u8::try_from((u16::from(*channel) * 255 + alpha / 2) / alpha)
                                    .unwrap_or(255);
                        }
                    }
                }
                stream.write_all(&rgba)?;
            } else {
                stream.write_all(row)?;
            }
        }
        stream.finish()?;
        writer.finish()?;
        Ok(())
    }
}
