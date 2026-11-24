use sumie_core::graphics::Placement;
use sumie_wayland::{Event, Modifiers, PointerButton as Button, PointerPhase as Phase};

use super::{Gpu, PaneState, Result, Session, View, pane_input::PointerEvent};
use crate::input::Routing;

pub(super) struct Viewer {
    pub(super) placement: Placement,
    clear_epoch: u64,
    presented: bool,
}

#[derive(Default)]
pub(super) struct Click {
    pressed: Option<((f64, f64), [u64; 3])>,
}

impl Click {
    /// Track the click alongside selection so a drag can select text from a thumbnail.
    fn clicked(&mut self, phase: Phase, position: (f64, f64), hit: Option<&Placement>) -> bool {
        let target = hit.map(|p| (position, p.storage_key));
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
        let open = pane.viewer_presented();
        let hit = if open {
            pane.viewer.as_ref().map(|viewer| &viewer.placement)
        } else {
            pane.thumbnail_at(view, event.position, event.modifiers)
        };
        if self.clicked(event.phase, event.position, hit) {
            if open {
                self.dismiss(pane, Some((view, routing, gpu)));
            } else {
                let Some(mut placement) = hit.cloned() else {
                    return Ok(false);
                };
                if !pane.pane.terminal.refresh_viewer(&mut placement)? {
                    return Ok(false);
                }
                pane.viewer = Some(Viewer {
                    placement,
                    clear_epoch: pane.pane.terminal.screen_clear_epoch(),
                    presented: false,
                });
                pane.pane.leave_tab()?;
                pane.clear_selection();
                pane.frame_dirty = true;
            }
            return Ok(true);
        }
        Ok(open)
    }

    pub(super) fn refresh_viewer(
        &mut self,
        pane: &mut PaneState,
        active: Option<(&mut View, &mut Routing, &mut Gpu)>,
    ) -> Result<()> {
        if pane.viewer_removed()? {
            self.dismiss(pane, active);
        }
        Ok(())
    }

    pub(super) fn dismiss(
        &mut self,
        pane: &mut PaneState,
        active: Option<(&mut View, &mut Routing, &mut Gpu)>,
    ) {
        pane.viewer = None;
        pane.frame_dirty = true;
        if let Some((view, routing, gpu)) = active {
            self.pressed = None;
            change_recipient(pane, view, routing, gpu);
        }
    }
}

impl PaneState {
    pub(super) fn thumbnail_at<'a>(
        &self,
        view: &'a View,
        position: (f64, f64),
        modifiers: Modifiers,
    ) -> Option<&'a Placement> {
        self.can_hover_thumbnail(modifiers)
            .then(|| view.thumbnail_hit(position, self.id))?
    }

    pub(super) fn can_hover_thumbnail(&self, modifiers: Modifiers) -> bool {
        self.viewer.is_none()
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.logo
            && modifiers.shift == self.pane.mouse_tracking()
    }

    fn viewer_removed(&mut self) -> Result<bool> {
        let Some(viewer) = &mut self.viewer else {
            return Ok(false);
        };
        let terminal = &mut self.pane.terminal;
        let generation = viewer.placement.image.generation;
        let removed = terminal.screen_clear_epoch() != viewer.clear_epoch
            || !terminal.refresh_viewer(&mut viewer.placement)?;
        self.frame_dirty |= generation != viewer.placement.image.generation;
        Ok(removed)
    }

    pub(super) fn viewer_presented(&self) -> bool {
        self.viewer.as_ref().is_some_and(|viewer| viewer.presented)
    }

    pub(super) fn hide_viewer(&mut self) {
        if let Some(viewer) = &mut self.viewer {
            viewer.presented = false;
        }
    }
}

fn change_recipient(pane: &mut PaneState, view: &mut View, routing: &mut Routing, gpu: &mut Gpu) {
    pane.paste_recipient = None;
    view.preedit.clear();
    routing.reset_pointer();
    gpu.set_input_generation(routing.change_recipient());
}

pub(super) fn escape_pressed(event: &Event) -> bool {
    matches!(
        event,
        Event::Key {
            keysym: xkeysym::key::Escape,
            pressed: true,
            repeat: false,
            ..
        }
    )
}

impl Session {
    pub(super) fn panes_presented(&mut self, gpu: &mut Gpu) -> Result<()> {
        let tab = self.tabs.active_mut().expect("live window");
        let focused = tab.tree.focused();
        for pane in &mut tab.panes {
            pane.frame_dirty = false;
            let Some(viewer) = &mut pane.viewer else {
                continue;
            };
            let id = pane.id;
            let Some(index) = tab
                .layout
                .panes
                .iter()
                .position(|&(visible, _)| visible == id)
            else {
                viewer.presented = false;
                continue;
            };
            if !gpu.viewer_presented(index) {
                self.pane_pointer.image_click.dismiss(
                    pane,
                    (id == focused).then_some((&mut self.view, &mut self.routing, &mut *gpu)),
                );
            } else if !viewer.presented {
                viewer.presented = true;
                self.routing.release_pane_keys(
                    id,
                    &mut pane.pane,
                    crate::input::key_modifiers(self.link_pointer.modifiers),
                )?;
                if id == focused {
                    self.pane_pointer.reset();
                    pane.pane.leave_tab()?;
                    change_recipient(pane, &mut self.view, &mut self.routing, gpu);
                }
            }
        }
        Ok(())
    }
}
