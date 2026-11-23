use sumie_core::graphics::Placement;
use sumie_wayland::{Event, Modifiers, PointerButton as Button, PointerPhase as Phase};

use super::{Gpu, PaneState, Result, View, pane_input::PointerEvent};
use crate::input::Routing;

#[derive(Default)]
pub(super) struct Click {
    pressed: Option<((f64, f64), u64, u32)>,
}

impl Click {
    /// Track the click alongside selection so a drag can select text from a thumbnail.
    fn clicked(&mut self, phase: Phase, position: (f64, f64), hit: Option<&Placement>) -> bool {
        let target = hit.map(|p| (position, p.image.generation, p.placement_id));
        match phase {
            Phase::Press(Button::Left, _) => self.pressed = target,
            Phase::Motion if self.pressed == target => (),
            Phase::Release(Button::Left) => {
                return self.pressed.take().is_some_and(|p| Some(p) == target);
            }
            _ => self.pressed = None,
        }
        false
    }

    pub(super) fn handle(
        &mut self,
        pane: &mut PaneState,
        view: &mut View,
        routing: &mut Routing,
        gpu: &mut Gpu,
        event: PointerEvent,
    ) -> Result<bool> {
        let open = pane.viewer.is_some();
        let hit = if open {
            pane.viewer.as_ref()
        } else {
            pane.thumbnail_at(view, event.position, event.modifiers)
        };
        if self.clicked(event.phase, event.position, hit) {
            let next = if open { None } else { hit.cloned() };
            if !open {
                routing.release_pane_keys(
                    pane.id,
                    &mut pane.pane,
                    crate::input::key_modifiers(event.modifiers),
                )?;
            }
            pane.set_viewer(next, view, routing, gpu)?;
            return Ok(true);
        }
        Ok(open)
    }
}

impl PaneState {
    pub(super) fn thumbnail_at<'a>(
        &self,
        view: &'a View,
        position: (f64, f64),
        modifiers: Modifiers,
    ) -> Option<&'a Placement> {
        let hit = view.thumbnail_hit(position, self.id)?;
        if self.viewer.is_some()
            || modifiers.ctrl
            || modifiers.alt
            || modifiers.logo
            || modifiers.shift != self.pane.mouse_tracking()
        {
            return None;
        }
        Some(hit)
    }

    fn set_viewer(
        &mut self,
        placement: Option<Placement>,
        view: &mut View,
        routing: &mut Routing,
        gpu: &mut Gpu,
    ) -> Result<()> {
        self.pane.leave_tab()?;
        self.clear_selection();
        self.viewer = placement;
        self.paste_recipient = None;
        self.frame_dirty = true;
        view.preedit.clear();
        routing.reset_pointer();
        gpu.set_input_generation(routing.change_recipient());
        Ok(())
    }

    pub(super) fn viewer_input(
        &mut self,
        event: &Event,
        view: &mut View,
        routing: &mut Routing,
        gpu: &mut Gpu,
    ) -> Result<bool> {
        if self.viewer.is_none() {
            return Ok(false);
        }
        if matches!(
            event,
            Event::Key {
                keysym: xkeysym::key::Escape,
                pressed: true,
                repeat: false,
                ..
            }
        ) {
            self.set_viewer(None, view, routing, gpu)?;
        }
        Ok(true)
    }
}
