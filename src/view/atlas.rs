use sumie_font::FontSystem;
use sumie_render::{AtlasRect, AtlasUpload};

pub(super) fn collect_dirty(font: &FontSystem, out: &mut Vec<AtlasRect>) {
    out.clear();
    out.extend(font.atlas().dirty_regions().iter().map(|rect| AtlasRect {
        origin: [rect.x, rect.y],
        size: [rect.width, rect.height],
    }));
}

pub(super) fn upload<'a>(font: &'a FontSystem, dirty: &'a [AtlasRect]) -> AtlasUpload<'a> {
    let atlas = font.atlas();
    let [width, height] = atlas.dimensions();
    AtlasUpload {
        width,
        height,
        bytes: atlas.pixels(),
        dirty,
    }
}
