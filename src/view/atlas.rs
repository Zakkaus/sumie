use sumie_font::FontSystem;
use sumie_render::{AtlasRect, AtlasUpload};

pub(super) fn collect_dirty(font: &FontSystem, out: &mut [Vec<AtlasRect>; 2]) {
    for (atlas, out) in [font.atlas(), font.color_atlas()].into_iter().zip(out) {
        out.clear();
        out.extend(atlas.dirty_regions().iter().map(|rect| AtlasRect {
            origin: [rect.x, rect.y],
            size: [rect.width, rect.height],
        }));
    }
}

pub(super) fn upload<'a>(
    font: &'a FontSystem,
    dirty: &'a [Vec<AtlasRect>; 2],
) -> [AtlasUpload<'a>; 2] {
    let planes = [font.atlas(), font.color_atlas()];
    std::array::from_fn(|i| {
        let [width, height] = planes[i].dimensions();
        AtlasUpload {
            width,
            height,
            bytes: planes[i].pixels(),
            dirty: &dirty[i],
        }
    })
}
