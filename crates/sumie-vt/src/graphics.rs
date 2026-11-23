//! Copies pixels and placement geometry from the live terminal into owned snapshots.

use crate::{Error, Terminal, check, ffi};
use std::collections::HashMap;
use std::ffi::{c_int, c_void};
use std::ptr::{from_mut, from_ref, null, null_mut};
use std::sync::Arc;
use sumie_core::graphics::{IMAGE_BUDGET, Image, Placement, texture_bytes};

pub(super) const PLACEHOLDER: char = '\u{10eeee}';

// Data keys and pixel formats from terminal.h, render.h, and kitty_graphics.h.
const STORAGE_LIMIT: c_int = 16;
#[cfg(target_os = "linux")]
const MEDIUM_FILE: c_int = 17;
#[cfg(target_os = "linux")]
const MEDIUM_TEMP_FILE: c_int = 18;
const TERMINAL_KITTY_GRAPHICS: c_int = 30;
const RENDER_IMAGE_ITERATOR: c_int = 22;
const PLACEMENTS_GENERATION: c_int = 23;
const PLACEMENT_IMAGE_ID: c_int = 2;
const PLACEMENT_IMAGE_GENERATION: c_int = 3;
const PLACEMENT_Z: c_int = 5;
const PLACEMENT_COL: c_int = 6;
const PLACEMENT_ROW: c_int = 7;
const PLACEMENT_OFFSET_X: c_int = 10;
const PLACEMENT_OFFSET_Y: c_int = 11;
const PLACEMENT_WIDTH: c_int = 12;
const PLACEMENT_HEIGHT: c_int = 13;
const PLACEMENT_SOURCE_X: c_int = 14;
const PLACEMENT_SOURCE_Y: c_int = 15;
const PLACEMENT_SOURCE_WIDTH: c_int = 16;
const PLACEMENT_SOURCE_HEIGHT: c_int = 17;
const PLACEMENT_KITTY_ID: c_int = 18;
const PLACEMENT_SUMIE_THUMBNAIL: c_int = 20;
const IMAGE_WIDTH: c_int = 3;
const IMAGE_HEIGHT: c_int = 4;
const IMAGE_FORMAT: c_int = 5;
const IMAGE_PIXELS: c_int = 7;
const FORMAT_RGB: c_int = 0;
const FORMAT_RGBA: c_int = 1;
const FORMAT_GRAY_ALPHA: c_int = 3;
const FORMAT_GRAY: c_int = 4;

pub(super) struct Graphics {
    iter: ffi::RenderStateImageIterator,
    images: HashMap<u32, CachedImage>,
    generation: Option<u64>,
    max_dimension: u32,
    placements: Arc<Vec<Placement>>,
}

struct CachedImage {
    image: Arc<Image>,
    visible: bool,
}

#[derive(Default)]
struct Geometry {
    image_id: u32,
    generation: u64,
    placement_id: u32,
    z: i32,
    cell: [i32; 2],
    offset: [u32; 2],
    size: [u32; 2],
    source: [u32; 4],
    thumbnail: bool,
}

impl Graphics {
    pub(super) fn new() -> Result<Self, Error> {
        let mut iter = null_mut();
        // SAFETY: The constructor accepts a null allocator and writes the owned handle.
        check(unsafe { ffi::ghostty_render_state_image_iterator_new(null(), &raw mut iter) })?;
        Ok(Self {
            iter,
            images: HashMap::new(),
            generation: Some(0),
            max_dimension: u32::MAX,
            placements: Arc::default(),
        })
    }

    fn geometry(&self) -> Result<Geometry, Error> {
        let mut geometry = Geometry::default();
        let [col, row] = &mut geometry.cell;
        let [offset_x, offset_y] = &mut geometry.offset;
        let [width, height] = &mut geometry.size;
        let [src_x, src_y, src_w, src_h] = &mut geometry.source;
        let fields: [(c_int, *mut c_void); 15] = [
            (PLACEMENT_IMAGE_ID, from_mut(&mut geometry.image_id).cast()),
            (
                PLACEMENT_IMAGE_GENERATION,
                from_mut(&mut geometry.generation).cast(),
            ),
            (
                PLACEMENT_KITTY_ID,
                from_mut(&mut geometry.placement_id).cast(),
            ),
            (PLACEMENT_Z, from_mut(&mut geometry.z).cast()),
            (PLACEMENT_COL, from_mut(col).cast()),
            (PLACEMENT_ROW, from_mut(row).cast()),
            (PLACEMENT_OFFSET_X, from_mut(offset_x).cast()),
            (PLACEMENT_OFFSET_Y, from_mut(offset_y).cast()),
            (PLACEMENT_WIDTH, from_mut(width).cast()),
            (PLACEMENT_HEIGHT, from_mut(height).cast()),
            (PLACEMENT_SOURCE_X, from_mut(src_x).cast()),
            (PLACEMENT_SOURCE_Y, from_mut(src_y).cast()),
            (PLACEMENT_SOURCE_WIDTH, from_mut(src_w).cast()),
            (PLACEMENT_SOURCE_HEIGHT, from_mut(src_h).cast()),
            (
                PLACEMENT_SUMIE_THUMBNAIL,
                from_mut(&mut geometry.thumbnail).cast(),
            ),
        ];
        let keys = fields.map(|(key, _)| key);
        let mut outputs = fields.map(|(_, out)| out);
        // SAFETY: The iterator is on a placement from this update. Each key is paired with
        // its documented output type. The pinned API only reports Kitty placements.
        check(unsafe {
            ffi::ghostty_render_state_image_get_multi(
                self.iter,
                fields.len(),
                keys.as_ptr(),
                outputs.as_mut_ptr(),
                null_mut(),
            )
        })?;
        Ok(geometry)
    }
}

impl Drop for Graphics {
    fn drop(&mut self) {
        // SAFETY: The iterator owns only its own allocation, independent of the render state.
        unsafe { ffi::ghostty_render_state_image_iterator_free(self.iter) };
    }
}

impl Terminal {
    /// Cap new natural Kitty placements at this many rows; zero disables the cap.
    pub fn set_thumbnail_rows(&mut self, rows: u16) {
        // SAFETY: The terminal is live and exclusively borrowed.
        unsafe { ffi::sumie_set_thumbnail_rows(self.raw, rows) };
    }

    /// Omit images too large for the target device before allocating snapshot pixels.
    pub fn set_image_dimension_limit(&mut self, limit: u32) {
        if self.graphics.max_dimension != limit {
            self.graphics.max_dimension = limit;
            self.graphics.generation = None;
            self.graphics.images.clear();
        }
    }

    pub(super) fn configure_graphics(&self) -> Result<(), Error> {
        const LIMIT: u64 = IMAGE_BUDGET as u64;
        // SAFETY: The terminal is live. The option reads a u64 during this call.
        check(unsafe {
            ffi::ghostty_terminal_set(self.raw, STORAGE_LIMIT, from_ref(&LIMIT).cast())
        })?;
        #[cfg(target_os = "linux")]
        {
            let dir = ffi::Str {
                ptr: b"/tmp".as_ptr(),
                len: 4,
            };
            // SAFETY: FILE reads a bool; TEMP_FILE copies the string data during the call.
            unsafe {
                check(ffi::ghostty_terminal_set(
                    self.raw,
                    MEDIUM_FILE,
                    from_ref(&true).cast(),
                ))?;
                check(ffi::ghostty_terminal_set(
                    self.raw,
                    MEDIUM_TEMP_FILE,
                    from_ref(&dir).cast(),
                ))?;
            }
        }
        Ok(())
    }

    pub(super) fn snapshot_images(&mut self) -> Result<Arc<Vec<Placement>>, Error> {
        // SAFETY: This render-state key writes the u64 revision of the entire placement list.
        let generation = unsafe { self.render_value::<u64>(PLACEMENTS_GENERATION)? };
        if Some(generation) == self.graphics.generation {
            return Ok(Arc::clone(&self.graphics.placements));
        }
        let placements = self.collect_images()?;
        self.graphics
            .images
            .retain(|_, cached| std::mem::take(&mut cached.visible));
        self.graphics.generation = Some(generation);
        self.graphics.placements = Arc::new(placements);
        Ok(Arc::clone(&self.graphics.placements))
    }

    fn collect_images(&mut self) -> Result<Vec<Placement>, Error> {
        // SAFETY: IMAGE_ITERATOR fills the existing iterator from the just-updated render state.
        check(unsafe {
            ffi::ghostty_render_state_get(
                self.render,
                RENDER_IMAGE_ITERATOR,
                from_mut(&mut self.graphics.iter).cast(),
            )
        })?;
        let mut placements = Vec::new();
        let mut storage = null_mut();
        // SAFETY: The iterator was filled above and the render state is not updated in this loop.
        while unsafe { ffi::ghostty_render_state_image_iterator_next(self.graphics.iter) } {
            let geometry = self.graphics.geometry()?;
            // A crop outside the image, or a subpixel virtual crop, can have no source pixels.
            if geometry.source[2..].contains(&0) {
                continue;
            }
            let Some(image) = self.cache_image(&geometry, &mut storage)? else {
                continue;
            };
            placements.push(Placement {
                image,
                image_id: geometry.image_id,
                placement_id: geometry.placement_id,
                z: geometry.z,
                cell: geometry.cell,
                offset: geometry.offset,
                size: geometry.size,
                source: geometry.source,
                thumbnail: geometry.thumbnail,
            });
        }
        Ok(placements)
    }

    fn cache_image(
        &mut self,
        geometry: &Geometry,
        storage: &mut ffi::KittyGraphics,
    ) -> Result<Option<Arc<Image>>, Error> {
        if let Some(cached) = self.graphics.images.get_mut(&geometry.image_id)
            && cached.image.generation == geometry.generation
        {
            cached.visible = true;
            return Ok(Some(Arc::clone(&cached.image)));
        }
        if storage.is_null() {
            // SAFETY: The live terminal writes a borrowed KittyGraphics handle to `storage`.
            check(unsafe {
                ffi::ghostty_terminal_get(
                    self.raw,
                    TERMINAL_KITTY_GRAPHICS,
                    from_mut(storage).cast(),
                )
            })?;
        }
        // SAFETY: Storage belongs to this terminal, which is unchanged during image extraction.
        let image = unsafe {
            read_image(
                *storage,
                geometry.image_id,
                geometry.generation,
                self.graphics.max_dimension,
            )
        }?;
        let Some(image) = image.map(Arc::new) else {
            return Ok(None);
        };
        self.graphics.images.insert(
            geometry.image_id,
            CachedImage {
                image: Arc::clone(&image),
                visible: true,
            },
        );
        Ok(Some(image))
    }
}

// `storage` must remain valid until the copy finishes. No borrowed pixels escape this function.
unsafe fn read_image(
    storage: ffi::KittyGraphics,
    id: u32,
    generation: u64,
    max_dimension: u32,
) -> Result<Option<Image>, Error> {
    let mut size = [0_u32; 2];
    let [width, height] = &mut size;
    let mut format: c_int = 0;
    let mut pixels = ffi::Str {
        ptr: null(),
        len: 0,
    };
    let keys = [IMAGE_WIDTH, IMAGE_HEIGHT, IMAGE_FORMAT, IMAGE_PIXELS];
    let mut outputs = [
        from_mut(width).cast(),
        from_mut(height).cast(),
        from_mut(&mut format).cast(),
        from_mut(&mut pixels).cast(),
    ];
    // SAFETY: Storage is live. The keys match the initialized output slots, and a missing
    // image is passed as null, which the getter rejects.
    unsafe {
        let image = ffi::ghostty_kitty_graphics_image(storage, id);
        check(ffi::ghostty_kitty_graphics_image_get_multi(
            image,
            keys.len(),
            keys.as_ptr(),
            outputs.as_mut_ptr(),
            null_mut(),
        ))?;
    }
    let channels = match format {
        FORMAT_RGBA => 4,
        FORMAT_RGB => 3,
        FORMAT_GRAY_ALPHA => 2,
        FORMAT_GRAY => 1,
        _ => return Err(Error(ffi::INVALID_VALUE)),
    };
    let Some(rgba_len) = texture_bytes(size, max_dimension) else {
        return Ok(None);
    };
    if pixels.ptr.is_null() || pixels.len != rgba_len / 4 * channels {
        return Err(Error(ffi::INVALID_VALUE));
    }
    // SAFETY: The getter lends initialized terminal memory until the next mutation. The
    // non-null buffer has exactly the expected byte length, bounded by IMAGE_BUDGET.
    let pixels = unsafe { std::slice::from_raw_parts(pixels.ptr, pixels.len) };
    Ok(Some(Image {
        generation,
        size,
        rgba: to_rgba(pixels, channels, rgba_len),
    }))
}

fn to_rgba(pixels: &[u8], channels: usize, rgba_len: usize) -> Vec<u8> {
    if channels == 4 {
        return pixels.to_vec();
    }
    let mut rgba = Vec::with_capacity(rgba_len);
    match channels {
        3 => {
            for &[r, g, b] in pixels.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        2 => {
            for &[gray, alpha] in pixels.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[gray, gray, gray, alpha]);
            }
        }
        1 => {
            for &gray in pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        _ => unreachable!(),
    }
    rgba
}
