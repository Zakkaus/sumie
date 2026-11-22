use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, globals::GlobalList,
    protocol::wl_surface::WlSurface,
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

use crate::Platform;

const UNIT: u32 = 120;

pub(crate) struct Scale {
    viewport: Option<(WpFractionalScaleV1, WpViewport)>,
    units: u32,
}

impl Scale {
    pub(crate) fn bind(
        globals: &GlobalList,
        qh: &QueueHandle<Platform>,
        surface: &WlSurface,
    ) -> Self {
        Self {
            viewport: bind_viewport(globals, qh, surface),
            units: UNIT,
        }
    }

    pub(crate) fn units(&self) -> u32 {
        self.units
    }

    pub(crate) fn device_size(&self, (width, height): (u32, u32)) -> (u32, u32) {
        (
            device_pixels(width, self.units),
            device_pixels(height, self.units),
        )
    }

    pub(crate) fn set_fractional(&mut self, units: u32) -> bool {
        self.set_units(units)
    }

    pub(crate) fn set_integer(&mut self, surface: &WlSurface, factor: i32) -> bool {
        if self.viewport.is_some() || surface.version() < 3 {
            return false;
        }
        self.set_units(factor.unsigned_abs().saturating_mul(UNIT))
    }

    fn set_units(&mut self, units: u32) -> bool {
        if units == 0 || units == self.units {
            return false;
        }
        self.units = units;
        true
    }

    pub(crate) fn apply(&self, surface: &WlSurface, (width, height): (u32, u32)) {
        if let Some((_, viewport)) = &self.viewport {
            // buffer_scale stays at its default of 1; only the destination is in logical pixels.
            viewport.set_destination(
                i32::try_from(width).unwrap_or(i32::MAX),
                i32::try_from(height).unwrap_or(i32::MAX),
            );
        } else if surface.version() >= 3 {
            surface.set_buffer_scale(i32::try_from(self.units / UNIT).unwrap_or(i32::MAX));
        }
    }
}

impl Drop for Scale {
    fn drop(&mut self) {
        if let Some((fractional, viewport)) = &self.viewport {
            fractional.destroy();
            viewport.destroy();
        }
    }
}

fn bind_viewport(
    globals: &GlobalList,
    qh: &QueueHandle<Platform>,
    surface: &WlSurface,
) -> Option<(WpFractionalScaleV1, WpViewport)> {
    let fractional = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(qh, 1..=1, ())
        .ok()?;
    let viewport = globals.bind::<WpViewporter, _, _>(qh, 1..=1, ());
    let result = viewport.ok().map(|viewport| {
        let object = viewport.get_viewport(surface, qh, ());
        viewport.destroy();
        (fractional.get_fractional_scale(surface, qh, ()), object)
    });
    fractional.destroy();
    result
}

fn device_pixels(logical: u32, units: u32) -> u32 {
    let pixels = (u64::from(logical) * u64::from(units) + u64::from(UNIT / 2)) / u64::from(UNIT);
    u32::try_from(pixels).unwrap_or(u32::MAX)
}

impl Dispatch<WpFractionalScaleV1, ()> for Platform {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.fractional_scale_changed(scale);
        }
    }
}

delegate_noop!(Platform: ignore WpFractionalScaleManagerV1);
delegate_noop!(Platform: ignore WpViewporter);
delegate_noop!(Platform: ignore WpViewport);

#[cfg(test)]
mod tests {
    use super::device_pixels;

    #[test]
    fn device_pixels_round_half_up() {
        let sizes = [1, 2, 3, 101, 800];
        for (scale, expected) in [
            (120, [1, 2, 3, 101, 800]),
            (150, [1, 3, 4, 126, 1000]),
            (180, [2, 3, 5, 152, 1200]),
            (240, [2, 4, 6, 202, 1600]),
        ] {
            assert_eq!(
                sizes.map(|size| device_pixels(size, scale)),
                expected,
                "scale {scale}/120"
            );
        }
    }
}
