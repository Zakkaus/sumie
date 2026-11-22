/// Pixel rectangle in the atlas, with its origin at the top left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasRect {
    pub origin: [u32; 2],
    pub size: [u32; 2],
}

/// A tightly packed, row-major R8 coverage atlas. Dirty rectangles lie within its bounds.
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

pub(crate) fn create(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sumie glyph coverage"),
        size: extent(size),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
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
    let stride = upload.width as usize;
    let start = y as usize * stride + x as usize;
    let end = start + (height as usize - 1) * stride + width as usize;
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
            bytes_per_row: Some(upload.width),
            rows_per_image: None,
        },
        extent(rect.size),
    );
}
