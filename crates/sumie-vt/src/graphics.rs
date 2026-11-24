//! Copies pixels and placement geometry from the live terminal into owned snapshots.

use crate::{Error, Terminal, check, ffi};
use std::collections::{HashMap, HashSet};
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
const PLACEMENT_SUMIE_STORAGE_KEY: c_int = 21;
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
    geometries: Vec<(usize, Geometry)>,
    covered: HashSet<Footprint>,
    generation: Option<u64>,
    max_dimension: u32,
    placements: Arc<Vec<Placement>>,
}

struct CachedImage {
    image: Arc<Image>,
    opaque: bool,
    visible: bool,
}

#[derive(Eq, Hash, PartialEq)]
struct Footprint {
    z: i32,
    cell: [i32; 2],
    offset: [u32; 2],
    size: [u32; 2],
}

#[derive(Default)]
struct Geometry {
    image_id: u32,
    generation: u64,
    placement_id: u32,
    storage_key: [u64; 3],
    z: i32,
    cell: [i32; 2],
    offset: [u32; 2],
    size: [u32; 2],
    source: [u32; 4],
    thumbnail: bool,
}

impl Geometry {
    const fn footprint(&self) -> Footprint {
        Footprint {
            z: self.z,
            cell: self.cell,
            offset: self.offset,
            size: self.size,
        }
    }

    fn can_cover(&self, image: &Image) -> bool {
        // Keep decorated thumbnails out of occlusion and match the renderer's crop validation.
        !self.thumbnail
            && (0..2).all(|i| {
                self.source[i]
                    .checked_add(self.source[2 + i])
                    .is_some_and(|end| end <= image.size[i])
            })
    }
}

impl Graphics {
    pub(super) fn new() -> Result<Self, Error> {
        let mut iter = null_mut();
        // SAFETY: The constructor accepts a null allocator and writes the owned handle.
        check(unsafe { ffi::ghostty_render_state_image_iterator_new(null(), &raw mut iter) })?;
        Ok(Self {
            iter,
            images: HashMap::new(),
            geometries: Vec::new(),
            covered: HashSet::new(),
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
        let fields: [(c_int, *mut c_void); 16] = [
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
            (
                PLACEMENT_SUMIE_STORAGE_KEY,
                from_mut(&mut geometry.storage_key).cast(),
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
    /// Advance Kitty animations by at most one frame each on the active screen.
    /// Use milliseconds from the same monotonic clock throughout this terminal's lifetime.
    /// Returns the next frame deadline on that clock, or `None` when no tick is needed.
    /// Call before taking the snapshot for a frame.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot access or advance its Kitty graphics storage.
    pub fn animation_tick(&mut self, now_ms: u64) -> Result<Option<u64>, Error> {
        let storage = self.kitty_graphics()?;
        let mut next_ms = 0;
        // SAFETY: Storage belongs to this exclusively borrowed terminal, and the output is a u64.
        let result = unsafe {
            ffi::ghostty_kitty_graphics_animation_tick(storage, now_ms, &raw mut next_ms)
        };
        if result == ffi::NO_VALUE {
            return Ok(None);
        }
        check(result)?;
        Ok(Some(next_ms))
    }

    /// Cap new natural-size Kitty placements that advance the primary-screen cursor.
    /// Zero disables the cap.
    pub fn set_thumbnail_rows(&mut self, rows: u16) {
        // SAFETY: The terminal is live and exclusively borrowed.
        unsafe { ffi::sumie_set_thumbnail_rows(self.raw, rows) };
    }

    /// Whether the exact placement and pixel revision remain in the active screen's
    /// image storage, even when outside the viewport. Does not update the render state.
    #[must_use]
    pub fn has_image_placement(&self, placement: &Placement) -> bool {
        self.placement_image_generation(placement) == Some(placement.image.generation)
    }

    /// Update a viewer's pixels from the same placement, even outside the viewport.
    /// Returns `false` if the placement or image was removed or replaced, or its pixels do not fit.
    ///
    /// # Errors
    /// Returns an error if Ghostty cannot read the current image.
    pub fn refresh_viewer(&mut self, placement: &mut Placement) -> Result<bool, Error> {
        let Some(generation) = self.placement_image_generation(placement) else {
            return Ok(false);
        };
        if generation != placement.image.generation {
            let Some((image, _)) =
                self.cache_image(placement.image_id, generation, &mut null_mut(), false)?
            else {
                return Ok(false);
            };
            placement.image = image;
        }
        Ok(true)
    }

    fn placement_image_generation(&self, placement: &Placement) -> Option<u64> {
        let [storage_id, revision, transmission] = placement.storage_key;
        // SAFETY: The terminal is live and Ghostty only reads its active storage.
        let generation = unsafe {
            ffi::sumie_placement_image_generation(
                self.raw,
                placement.image_id,
                storage_id,
                revision,
                transmission,
            )
        };
        (generation != 0).then_some(generation)
    }

    /// Changes on a full display erase or full terminal reset, including when
    /// that operation preserves placements in scrollback.
    #[must_use]
    pub fn screen_clear_epoch(&self) -> u64 {
        // SAFETY: The terminal is live and Ghostty only reads its clear epoch.
        unsafe { ffi::sumie_screen_clear_epoch(self.raw) }
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
        let mut geometries = std::mem::take(&mut self.graphics.geometries);
        let mut covered = std::mem::take(&mut self.graphics.covered);
        let result = self.copy_uncovered_images(&mut geometries, &mut covered);
        geometries.clear();
        covered.clear();
        self.graphics.geometries = geometries;
        self.graphics.covered = covered;
        result
    }

    fn copy_uncovered_images(
        &mut self,
        geometries: &mut Vec<(usize, Geometry)>,
        covered: &mut HashSet<Footprint>,
    ) -> Result<Vec<Placement>, Error> {
        // SAFETY: IMAGE_ITERATOR fills the existing iterator from the just-updated render state.
        check(unsafe {
            ffi::ghostty_render_state_get(
                self.render,
                RENDER_IMAGE_ITERATOR,
                from_mut(&mut self.graphics.iter).cast(),
            )
        })?;
        // SAFETY: The iterator was filled above and the render state is not updated in this loop.
        while unsafe { ffi::ghostty_render_state_image_iterator_next(self.graphics.iter) } {
            let geometry = self.graphics.geometry()?;
            // A crop outside the image, or a subpixel virtual crop, can have no source pixels.
            if geometry.source[2..].contains(&0) {
                continue;
            }
            geometries.push((geometries.len(), geometry));
        }
        // Use the renderer's painter order, including native iterator order for ties.
        geometries.sort_unstable_by_key(|(index, g)| (g.z, g.image_id, g.placement_id, *index));
        let mut placements = Vec::new();
        let mut storage = null_mut();
        for (_, geometry) in geometries.drain(..).rev() {
            let footprint = geometry.footprint();
            if covered.contains(&footprint) {
                continue;
            }
            let Some((image, opaque)) =
                self.cache_image(geometry.image_id, geometry.generation, &mut storage, true)?
            else {
                continue;
            };
            if opaque && geometry.can_cover(&image) {
                covered.insert(footprint);
            }
            placements.push(Placement {
                image,
                image_id: geometry.image_id,
                placement_id: geometry.placement_id,
                storage_key: geometry.storage_key,
                z: geometry.z,
                cell: geometry.cell,
                offset: geometry.offset,
                size: geometry.size,
                source: geometry.source,
                thumbnail: geometry.thumbnail,
            });
        }
        placements.reverse();
        Ok(placements)
    }

    fn cache_image(
        &mut self,
        id: u32,
        generation: u64,
        storage: &mut ffi::KittyGraphics,
        visible: bool,
    ) -> Result<Option<(Arc<Image>, bool)>, Error> {
        if let Some(cached) = self.graphics.images.get_mut(&id)
            && cached.image.generation == generation
        {
            cached.visible |= visible;
            return Ok(Some((Arc::clone(&cached.image), cached.opaque)));
        }
        if storage.is_null() {
            *storage = self.kitty_graphics()?;
        }
        // SAFETY: Storage belongs to this terminal, which is unchanged during image extraction.
        let Some((image, opaque)) =
            (unsafe { read_image(*storage, id, generation, self.graphics.max_dimension) })?
        else {
            return Ok(None);
        };
        let image = Arc::new(image);
        self.graphics.images.insert(
            id,
            CachedImage {
                image: Arc::clone(&image),
                opaque,
                visible,
            },
        );
        Ok(Some((image, opaque)))
    }

    fn kitty_graphics(&self) -> Result<ffi::KittyGraphics, Error> {
        let mut storage: ffi::KittyGraphics = null_mut();
        // SAFETY: The live terminal writes a borrowed KittyGraphics handle to the output slot.
        check(unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                TERMINAL_KITTY_GRAPHICS,
                from_mut(&mut storage).cast(),
            )
        })?;
        Ok(storage)
    }
}

// `storage` must remain valid until the copy finishes. No borrowed pixels escape this function.
unsafe fn read_image(
    storage: ffi::KittyGraphics,
    id: u32,
    generation: u64,
    max_dimension: u32,
) -> Result<Option<(Image, bool)>, Error> {
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
    Ok(Some((
        Image {
            generation,
            size,
            rgba: to_rgba(pixels, channels, rgba_len),
        },
        matches!(format, FORMAT_RGB | FORMAT_GRAY),
    )))
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
