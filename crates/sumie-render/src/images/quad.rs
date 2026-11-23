//! A clipped image instance. Thumbnail chrome uses the original edges, not the scissor.

use sumie_core::graphics::Placement;

use super::Clip;
use crate::{Rgba, ThumbnailChrome};

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub(super) struct Quad {
    dest: [f32; 4],
    source: [f32; 4],
    // A negative exclusive end marks an axis drawn at native size.
    crop: [i32; 4],
    edges: [f32; 4],
    border: Rgba,
    ink: Rgba,
    chip_size: u32,
}

impl Quad {
    pub(super) const ATTRS: [wgpu::VertexAttribute; 7] = wgpu::vertex_attr_array![
        0 => Float32x4, 1 => Float32x4, 2 => Sint32x4, 3 => Float32x4,
        4 => Unorm8x4, 5 => Unorm8x4, 6 => Uint32
    ];

    #[expect(
        clippy::cast_possible_truncation,
        reason = "GPU vertex coordinates are f32"
    )]
    pub(super) fn clipped(
        p: &Placement,
        origin: [i64; 2],
        size: [u32; 2],
        bounds: Clip,
        ndc: [f64; 2],
    ) -> Option<Self> {
        let axis = |i: usize| {
            let source_size = p.source[2 + i];
            let source_start = p.source[i];
            let texture_size = p.image.size[i];
            if source_size == 0 || source_start > texture_size.checked_sub(source_size)? {
                return None;
            }
            let [first, last] = bounds.0[i];
            let screen = |edge| {
                let pixel = f64::from(u32::try_from(edge).unwrap());
                (pixel * ndc[i] - ndc[i].signum()) as f32
            };
            let source = |edge| {
                let offset = f64::from(u32::try_from(edge - origin[i]).unwrap());
                (f64::from(source_start) + offset * f64::from(source_size) / f64::from(size[i]))
                    as f32
            };
            let end = i32::try_from(source_start + source_size).ok()?;
            Some((
                [screen(first), screen(last)],
                [source(first), source(last)],
                [
                    i32::try_from(source_start).ok()?,
                    if source_size == size[i] { -end } else { end },
                ],
            ))
        };
        let ([left, right], [x0, x1], [crop_left, crop_right]) = axis(0)?;
        let ([top, bottom], [y0, y1], [crop_top, crop_bottom]) = axis(1)?;
        Some(Self {
            dest: [left, top, right, bottom],
            source: [x0, y0, x1, y1],
            crop: [crop_left, crop_top, crop_right, crop_bottom],
            edges: [0.0; 4],
            border: [0; 4],
            ink: [0; 4],
            chip_size: 0,
        })
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "on-screen pixel edges fit in f32"
    )]
    pub(super) fn decorate(
        &mut self,
        [x, y]: [i64; 2],
        [w, h]: [u32; 2],
        style: ThumbnailChrome,
        hovered: bool,
    ) {
        self.edges = [x, y, x + i64::from(w), y + i64::from(h)].map(|edge| edge as f32);
        self.border = if hovered { style.accent } else { style.border };
        if hovered {
            self.ink = style.on_accent;
            self.chip_size = style.chip_size.min(w.min(h).saturating_sub(2));
        }
    }
}
