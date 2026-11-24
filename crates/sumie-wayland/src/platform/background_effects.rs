use smithay_client_toolkit::{
    background_effect::{BackgroundEffectHandler, BackgroundEffectState},
    compositor::{CompositorState, Region},
    reexports::client::{QueueHandle, globals::GlobalList, protocol::wl_surface::WlSurface},
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::Capability,
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};

use super::{Platform, State};

pub(super) struct BackgroundEffects {
    transparent: bool,
    blur: bool,
    commit_needed: bool,
    opaque_region: Option<(u32, u32)>,
    ext: BackgroundEffectState,
    ext_surface: Option<(ExtBackgroundEffectSurfaceV1, (u32, u32))>,
}

impl Platform {
    /// Stage background effects for the next buffer commit without presenting a frame.
    /// Returns whether the settings change the surface hints; unsupported blur is ignored.
    pub fn set_background_effects(&mut self, transparent: bool, blur: bool) -> bool {
        let effects = &mut self.state.background_effects;
        let changed = transparent != effects.transparent
            || (transparent && blur != effects.blur && effects.supports_blur());
        effects.transparent = transparent;
        effects.blur = blur;
        changed
    }

    /// Whether a compositor capability change needs a commit of the current frame.
    #[must_use]
    pub fn background_commit_needed(&self) -> bool {
        self.state.background_effects.commit_needed
    }
}

impl State {
    pub(super) fn stage_frame_geometry(&mut self) {
        let surface = self.window.wl_surface();
        if self.geometry_dirty {
            self.scale.apply(surface, self.logical);
            self.geometry_dirty = false;
        }
        self.background_effects
            .apply(surface, self.logical, &self.compositor, &self.qh);
    }
}

impl BackgroundEffects {
    pub(super) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Self {
            transparent: false,
            blur: false,
            commit_needed: false,
            opaque_region: None,
            ext: BackgroundEffectState::new(globals, qh),
            ext_surface: None,
        }
    }

    fn supports_blur(&self) -> bool {
        self.ext
            .capabilities()
            .is_some_and(|flags| flags.contains(Capability::Blur))
    }

    fn blur_enabled(&self) -> bool {
        self.transparent && self.blur && self.supports_blur()
    }

    fn apply(
        &mut self,
        surface: &WlSurface,
        logical: (u32, u32),
        compositor: &CompositorState,
        qh: &QueueHandle<State>,
    ) {
        let opaque = (!self.transparent).then_some(logical);
        if self.opaque_region != opaque {
            let region = opaque.and_then(|size| surface_region(compositor, size));
            surface.set_opaque_region(region.as_ref().map(Region::wl_region));
            self.opaque_region = opaque;
        }

        if self.blur_enabled() {
            self.apply_blur(surface, logical, compositor, qh);
        } else if let Some((effect, _)) = self.ext_surface.take() {
            effect.destroy();
        }
        self.commit_needed = false;
    }

    fn apply_blur(
        &mut self,
        surface: &WlSurface,
        logical: (u32, u32),
        compositor: &CompositorState,
        qh: &QueueHandle<State>,
    ) {
        let effect = match &mut self.ext_surface {
            Some((_, size)) if *size == logical => return,
            Some((effect, size)) => {
                *size = logical;
                effect
            }
            None => {
                let Ok(effect) = self.ext.get_background_effect(surface, qh) else {
                    return;
                };
                &self.ext_surface.insert((effect, logical)).0
            }
        };
        let region = surface_region(compositor, logical);
        effect.set_blur_region(region.as_ref().map(Region::wl_region));
    }
}

impl Drop for BackgroundEffects {
    fn drop(&mut self) {
        if let Some((effect, _)) = &self.ext_surface {
            effect.destroy();
        }
        if let Ok(manager) = self.ext.ext_background_effect_manager_v1() {
            manager.destroy();
        }
    }
}

fn surface_region(compositor: &CompositorState, (width, height): (u32, u32)) -> Option<Region> {
    let region = Region::new(compositor).ok()?;
    region.add(
        0,
        0,
        i32::try_from(width).unwrap_or(i32::MAX),
        i32::try_from(height).unwrap_or(i32::MAX),
    );
    Some(region)
}

impl BackgroundEffectHandler for State {
    fn background_effect_state(&mut self) -> &mut BackgroundEffectState {
        &mut self.background_effects.ext
    }

    fn update_capabilities(&mut self) {
        let effects = &mut self.background_effects;
        effects.commit_needed = effects.blur_enabled() != effects.ext_surface.is_some();
    }
}
