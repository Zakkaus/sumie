#![forbid(unsafe_code)]

use std::{error::Error, fs::File, io::BufWriter};

use sumie_font::{Atlas, AtlasGlyph, FontOptions, FontSystem, ShapedGlyph};

fn main() -> Result<(), Box<dyn Error>> {
    const TEXT: &str = "hello 中文 -> != ===";
    const PADDING: u32 = 12;
    let output = std::env::args_os()
        .nth(1)
        .ok_or("usage: sample OUTPUT.png")?;
    let mut fonts = FontSystem::new("JetBrains Mono", 20.0, 1.5, FontOptions::default())?;
    let widths: Vec<_> = TEXT
        .chars()
        .map(|ch| if matches!(ch, '中' | '文') { 2 } else { 1 })
        .collect();
    let run = fonts.shape(TEXT, &widths)?;
    let metrics = fonts.metrics();
    println!(
        "{}: {metrics:?}, {} cells",
        fonts.family(fonts.primary_face()).unwrap(),
        run.cells
    );
    let width = run.cells * metrics.width + 2 * PADDING;
    let height = metrics.height + 2 * PADDING;
    let mut image = vec![24; (width * height) as usize];
    for glyph in run.glyphs {
        let mask = fonts.rasterize(glyph.key)?;
        composite(fonts.atlas(), glyph, mask, PADDING, &mut image, width)?;
    }
    let mut encoder = png::Encoder::new(BufWriter::new(File::create(output)?), width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&image)?;
    writer.finish()?;
    Ok(())
}

fn composite(
    atlas: &Atlas,
    glyph: ShapedGlyph,
    mask: AtlasGlyph,
    padding: u32,
    image: &mut [u8],
    width: u32,
) -> Result<(), Box<dyn Error>> {
    let rect = mask.rect;
    let x = u32::try_from(glyph.x + mask.left + padding.cast_signed())?;
    let y = u32::try_from(glyph.y + mask.top + padding.cast_signed())?;
    let stride = atlas.dimensions()[0];
    for row in 0..rect.height {
        let src = ((rect.y + row) * stride + rect.x) as usize;
        let dst = ((y + row) * width + x) as usize;
        let columns = rect.width as usize;
        for (pixel, &coverage) in image[dst..dst + columns]
            .iter_mut()
            .zip(&atlas.pixels()[src..src + columns])
        {
            let a = u16::from(coverage);
            *pixel = u8::try_from((u16::from(*pixel) * (255 - a) + 238 * a + 127) / 255)?;
        }
    }
    Ok(())
}
