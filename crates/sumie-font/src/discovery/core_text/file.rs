use std::{collections::HashMap, path::Path};

use skrifa::{MetadataProvider as _, font::Blob, raw::FileRef, string::StringId};

use super::FontError;

pub(super) struct FontFile {
    pub bytes: Blob,
    // Core Text identifies a face by name, not its TTC directory index.
    pub collection: Option<HashMap<String, u32>>,
}

impl FontFile {
    pub fn read(path: &Path) -> Result<Self, FontError> {
        let bytes: Blob = std::fs::read(path)
            .map_err(|source| FontError::ReadFont {
                path: path.to_owned(),
                source,
            })?
            .into();
        let file = FileRef::new(&bytes).map_err(|_| FontError::InvalidFont(path.to_owned()))?;
        let collection = match file {
            FileRef::Font(_) => None,
            FileRef::Collection(ttc) => {
                let mut names = HashMap::new();
                for (index, face) in (0..ttc.len()).zip(ttc.iter()) {
                    if let Ok(face) = face
                        && let Some(name) = face.localized_strings(StringId::POSTSCRIPT_NAME).next()
                    {
                        names.insert(name.to_string(), index);
                    }
                }
                Some(names)
            }
        };
        Ok(Self { bytes, collection })
    }
}

impl super::Match {
    pub(crate) fn draw_color(
        &self,
        key: crate::GlyphKey,
        height: u32,
        atlas: &mut crate::Atlas,
    ) -> Result<crate::AtlasGlyph, FontError> {
        use objc2_core_foundation::{CFString, CGPoint};
        use objc2_core_graphics::{
            CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
            kCGColorSpaceSRGB,
        };
        use objc2_core_text::{CTFont, CTFontOrientation};
        use std::ptr::{self, NonNull};
        let width = key.color_width;
        let mut id = u16::try_from(key.glyph_id()).map_err(|_| FontError::ColorGlyph)?;
        let name = CFString::from_str(&self.postscript);
        // SAFETY: name identifies the matched face; both glyph calls receive one live glyph.
        let (font, bounds, space) = unsafe {
            let font = CTFont::with_name(&name, f64::from(height), ptr::null());
            let bounds = font.bounding_rects_for_glyphs(
                CTFontOrientation::Horizontal,
                NonNull::from(&mut id),
                ptr::null_mut(),
                1,
            );
            (
                font,
                bounds,
                CGColorSpace::with_name(Some(kCGColorSpaceSRGB)),
            )
        };
        let (w, h) = (f64::from(width), f64::from(height));
        let scale = (w / bounds.size.width).max(h / bounds.size.height);
        if !scale.is_finite() {
            return Err(FontError::ColorGlyph);
        }
        let space = space.ok_or(FontError::ColorGlyph)?;
        let pitch = width as usize * 4;
        let mut pixels = vec![0_u8; pitch * height as usize];
        // SAFETY: the context borrows exactly height initialized RGBA rows. It drops before
        // pixels is read or freed. The glyph and position pointers each address one element.
        unsafe {
            let ctx = CGBitmapContextCreate(
                pixels.as_mut_ptr().cast(),
                width as usize,
                height as usize,
                8,
                pitch,
                Some(&space),
                CGImageByteOrderInfo::Order32Big.0 | CGImageAlphaInfo::PremultipliedLast.0,
            )
            .ok_or(FontError::ColorGlyph)?;
            CGContext::translate_ctm(Some(&ctx), w * 0.5, h * 0.5);
            CGContext::scale_ctm(Some(&ctx), scale, scale);
            let mut origin = CGPoint::new(
                -bounds.origin.x - bounds.size.width * 0.5,
                -bounds.origin.y - bounds.size.height * 0.5,
            );
            font.draw_glyphs(NonNull::from(&mut id), NonNull::from(&mut origin), 1, &ctx);
        }
        atlas.insert(
            key,
            zeno::Placement {
                width,
                height,
                ..Default::default()
            },
            |dst, stride| {
                for (src, dst) in pixels.chunks_exact(pitch).zip(dst.chunks_mut(stride)) {
                    dst[..pitch].copy_from_slice(src);
                }
            },
        )
    }
}
