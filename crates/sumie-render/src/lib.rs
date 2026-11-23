//! Owns wgpu rendering from atlas data and draw lists. Does not change terminal
//! state, discover fonts, depend on `sumie-font`, allocate GPU objects per frame,
//! or submit GPU commands while holding a terminal lock.
#![forbid(unsafe_code)]

mod atlas;
mod grid;
mod images;
mod renderer;
mod surface;

pub use atlas::{AtlasRect, AtlasUpload};
pub use grid::{
    ChromeLayer, Cursor, CursorShape, DrawList, Fill, FrameDraw, GlyphQuad, PaneDraw, PixelRect,
    Rgba,
};
pub use renderer::{OffscreenTarget, Renderer};
pub use surface::{SurfaceError, SurfaceFrame, SurfaceRenderer};
pub use wgpu;

/// Select the instance's backends before creating a surface or requesting an adapter.
#[must_use]
pub fn backends() -> wgpu::Backends {
    if cfg!(target_os = "linux")
        && std::env::var_os("SUMIE_GPU_BACKEND").as_deref() == Some("gl".as_ref())
    {
        wgpu::Backends::GL
    } else {
        wgpu::Backends::PRIMARY
    }
}

/// Set the backends without discarding the caller's display handle or other instance options.
#[must_use]
pub fn instance_descriptor(
    mut descriptor: wgpu::InstanceDescriptor,
    backends: wgpu::Backends,
) -> wgpu::InstanceDescriptor {
    descriptor.backends = backends;
    if backends == wgpu::Backends::GL {
        // wgpu's indirect validation compute pipeline can prevent GL device creation.
        // This renderer only issues direct draws.
        descriptor
            .flags
            .remove(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL);
    }
    descriptor
}
