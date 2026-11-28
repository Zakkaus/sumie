/// Pixel rectangle in the atlas, with its origin at the top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasRect {
    pub origin: [u32; 2],
    pub size: [u32; 2],
}

/// Tightly packed R8 coverage or premultiplied sRGB RGBA8. Dirty rectangles use pixels.
#[derive(Clone, Copy)]
pub struct AtlasUpload<'a> {
    pub width: u32,
    pub height: u32,
    pub bytes: &'a [u8],
    pub dirty: &'a [AtlasRect],
}

pub(crate) fn extent([width, height]: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

pub(crate) fn create(
    device: &wgpu::Device,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sumie glyph atlas"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

pub(crate) fn write(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    upload: &AtlasUpload<'_>,
    rect: AtlasRect,
) {
    if rect.size.contains(&0) {
        return;
    }
    let [x, y] = rect.origin;
    let [width, height] = rect.size;
    let channels = texture
        .format()
        .block_copy_size(None)
        .expect("uncompressed atlas");
    let stride = upload.width * channels;
    let start = y as usize * stride as usize + (x * channels) as usize;
    let end = start + (height as usize - 1) * stride as usize + (width * channels) as usize;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        &upload.bytes[start..end],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(stride),
            rows_per_image: None,
        },
        extent(rect.size),
    );
}
