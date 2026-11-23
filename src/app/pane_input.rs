use std::ops::ControlFlow;

use sumie_core::splits::Layout;
use sumie_wayland::{Event, Modifiers, PointerButton as Button, PointerPhase as Phase};

use super::{Gpu, PaneState, Result, Session, View, links, tabs};
use crate::input::{self, Effect, Routing, SearchAction};

#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
enum Target {
    Pane(u64),
    Divider(u64),
}

impl Target {
    fn at(layout: &Layout, pixel: [f64; 2], device: [u32; 2]) -> Option<Self> {
        if !pixel
            .into_iter()
            .zip(device)
            .all(|(pos, size)| (0.0..f64::from(size)).contains(&pos))
        {
            return None;
        }
        let point = device_point(pixel);
        layout
            .panes
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|&(id, _)| Self::Pane(id))
            .or_else(|| {
                layout
                    .dividers
                    .iter()
                    .find(|divider| divider.rect.contains(point))
                    .map(|divider| Self::Divider(divider.id))
            })
    }
}

#[derive(Default)]
pub(super) struct Pointer {
    target: Option<Target>,
    held: [bool; 3],
}

impl Pointer {
    pub(super) fn reset(&mut self) {
        // A focus change ends the drag but does not transfer held buttons to a new pane.
        self.target = None;
    }

    fn route(&mut self, phase: Phase, hit: impl FnOnce() -> Option<Target>) -> Option<Target> {
        let pane_hit = |target: &Target| matches!(target, Target::Pane(_));
        match phase {
            Phase::Press(button, _) => self
                .motion(hit)
                .filter(|target| pane_hit(target) || button == Button::Left),
            Phase::Motion => self.motion(|| hit().filter(pane_hit)),
            Phase::Wheel(_) => hit().filter(pane_hit),
            Phase::Release(button) => self.release(button),
            Phase::Cancel => self.cancel().or_else(|| hit().filter(pane_hit)),
        }
    }

    fn motion(&self, hit: impl FnOnce() -> Option<Target>) -> Option<Target> {
        if self.held.contains(&true) {
            self.target
        } else {
            hit()
        }
    }

    fn press(&mut self, button: Button, target: Target) {
        self.target = Some(target);
        self.held[button as usize] = true;
    }

    fn release(&mut self, button: Button) -> Option<Target> {
        if !std::mem::take(&mut self.held[button as usize]) {
            return None;
        }
        if self.held.contains(&true) {
            self.target
        } else {
            self.target.take()
        }
    }

    fn cancel(&mut self) -> Option<Target> {
        self.held.fill(false);
        self.target.take()
    }
}

#[derive(Clone, Copy)]
struct PointerEvent {
    phase: Phase,
    position: (f64, f64),
    serial: u32,
    modifiers: Modifiers,
}

impl Session {
    pub(super) fn input(
        &mut self,
        event: Event,
        gpu: &mut Gpu,
        banner: bool,
    ) -> Result<Option<tabs::Action>> {
        self.link_pointer.record(&event);
        if let ControlFlow::Break(action) = self.bar_input(&event) {
            return Ok(action);
        }
        if let Event::Pointer {
            phase,
            position,
            serial,
            modifiers,
        } = event
        {
            self.pointer_input(
                PointerEvent {
                    phase,
                    position,
                    serial,
                    modifiers,
                },
                gpu,
                banner,
            )?;
            return Ok(None);
        }

        let Some((index, id, recipient)) = self.input_target(&event) else {
            return Ok(None);
        };
        let pane = self.tabs.as_mut_slice()[index].get_mut(id);
        if let Event::ModifiersChanged(modifiers) = event {
            pane.pane
                .pointer_modifiers_changed(input::key_modifiers(modifiers));
            return Ok(None);
        }
        if matches!(
            event,
            Event::Key { pressed: true, .. } | Event::ImeCommit { .. }
        ) {
            pane.sources.reset_blink();
        }
        let searching = pane.search.is_some();
        match input::route(
            recipient,
            &mut pane.pane,
            &mut self.routing,
            searching,
            event,
        )? {
            Effect::Tab(action) => return Ok(Some(tabs::Action::Key(action))),
            Effect::Pane(action) => return Ok(Some(tabs::Action::Pane(action))),
            Effect::Zoom(action) => self.zoom(action)?,
            effect => apply_pane_effect(
                pane,
                &mut self.view,
                &mut self.routing,
                gpu,
                searching,
                effect,
            )?,
        }
        Ok(None)
    }

    fn pointer_input(&mut self, event: PointerEvent, gpu: &mut Gpu, banner: bool) -> Result<()> {
        let view = &self.view;
        let layout = &self.tabs.active().expect("live window").layout;
        let target = self.pane_pointer.route(event.phase, || {
            Target::at(layout, view.window_pixel(event.position), view.device)
        });
        if matches!(event.phase, Phase::Cancel) && !matches!(target, Some(Target::Pane(_))) {
            self.routing.reset_pointer();
        }
        let Some(target) = target else {
            return Ok(());
        };
        if let Phase::Press(button, _) = event.phase {
            if let Target::Pane(id) = target {
                self.focus_pane(id, gpu)?;
            }
            self.pane_pointer.press(button, target);
        }
        match target {
            Target::Pane(id) => {
                let tab = self.tabs.active_mut().expect("live window");
                let focused = tab.tree.focused() == id;
                let pane = tab.get_mut(id);
                let searching = focused && pane.search.is_some();
                let effect = pointer_effect(
                    pane,
                    &self.view,
                    &mut self.routing,
                    &self.host,
                    event,
                    searching,
                    banner && focused,
                )?;
                apply_pane_effect(
                    pane,
                    &mut self.view,
                    &mut self.routing,
                    gpu,
                    searching,
                    effect,
                )
            }
            Target::Divider(id) if matches!(event.phase, Phase::Motion | Phase::Release(_)) => {
                let pixel = self.view.window_pixel(event.position);
                self.drag_divider(id, device_point(pixel))
            }
            Target::Divider(_) => Ok(()),
        }
    }

    pub(super) fn pane_under_pointer(&self, position: (f64, f64)) -> Option<u64> {
        if self.view.over_tab_bar(position) {
            return None;
        }
        let layout = &self.tabs.active()?.layout;
        let pixel = self.view.window_pixel(position);
        match Target::at(layout, pixel, self.view.device)? {
            Target::Pane(id) => Some(id),
            Target::Divider(_) => None,
        }
    }
}

fn device_point(pixel: [f64; 2]) -> [u32; 2] {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "use the containing device pixel, saturating drags outside the window"
    )]
    pixel.map(|pos| pos as u32)
}

fn pointer_effect(
    pane: &mut PaneState,
    view: &View,
    routing: &mut Routing,
    host: &str,
    event: PointerEvent,
    searching: bool,
    banner: bool,
) -> Result<Effect> {
    let rect = pane.rect;
    let position = event.position;
    let mut mouse = input::mouse_event(
        event.phase,
        event.modifiers,
        view.pointer_position(position, rect),
    );
    let hit = || view.link_cell(position, rect, banner);
    let uri = pane.prepare_pointer_link(&mut mouse, hit, host)?;
    let hit = || view.search_hit(position, rect);
    match routing.pointer(&mut pane.pane, searching, mouse, hit)? {
        Effect::OpenLink => {
            links::Hover::open(uri.expect("validated link release"));
            Ok(Effect::None)
        }
        Effect::Select(step) if pane.select(step, mouse.cell)? => Ok(Effect::Copy(event.serial)),
        effect => Ok(effect),
    }
}

fn apply_pane_effect(
    pane: &mut PaneState,
    view: &mut View,
    routing: &mut Routing,
    gpu: &mut Gpu,
    searching: bool,
    effect: Effect,
) -> Result<()> {
    match effect {
        Effect::Search(action) => {
            if matches!(action, SearchAction::Open | SearchAction::Close) {
                pane.pane.reset_pointer_gestures();
                pane.paste_recipient = None;
                gpu.set_input_generation(routing.change_recipient());
            }
            pane.search_action(action, view)?;
        }
        Effect::Copy(serial) => pane.copy_selection(gpu, serial)?,
        Effect::ViewportChanged => {
            pane.frame_dirty |= searching || pane.selection.range().is_some();
            pane.clear_selection();
            pane.snapshot_dirty = true;
        }
        Effect::ReadClipboard => {
            gpu.read_clipboard(*pane.paste_recipient.get_or_insert(routing.generation));
        }
        Effect::Preedit(text) if text != view.preedit => {
            view.preedit = text;
            pane.frame_dirty = true;
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sumie_core::splits::{Axis, Rect, SplitTree};

    use super::{Button, Layout, Phase, Pointer, Target};

    #[test]
    fn hits_are_absolute_at_the_bar_and_divider() {
        let mut tree = SplitTree::new(1);
        tree.split(Axis::Horizontal, 2);
        let mut layout = Layout::default();
        tree.layout(
            Rect {
                x: 0,
                y: 20,
                width: 81,
                height: 80,
            },
            &mut layout,
        );
        let divider = layout.dividers[0].id;
        for (pixel, target) in [
            ([-0.1, 60.0], None),
            ([0.0, 19.9], None),
            ([39.9, 20.0], Some(Target::Pane(1))),
            ([40.0, 20.0], Some(Target::Divider(divider))),
            ([40.9, 99.9], Some(Target::Divider(divider))),
            ([41.0, 20.0], Some(Target::Pane(2))),
            ([81.0, 99.9], None),
            ([40.0, 100.0], None),
        ] {
            assert_eq!(Target::at(&layout, pixel, [81, 100]), target);
        }
    }

    #[test]
    fn drag_and_release_stay_with_the_pressed_pane_but_wheel_does_not() {
        let mut pointer = Pointer::default();
        let left = Target::Pane(1);
        let right = Target::Pane(2);
        let over_right = || Some(right);
        pointer.press(Button::Left, left);
        assert_eq!(pointer.route(Phase::Motion, over_right), Some(left));
        assert_eq!(pointer.route(Phase::Wheel(120.0), over_right), Some(right));
        assert_eq!(
            pointer.route(Phase::Release(Button::Left), over_right),
            Some(left)
        );
        assert_eq!(pointer.route(Phase::Motion, over_right), Some(right));
    }

    #[test]
    fn divider_drag_requires_a_left_press_and_ends_on_focus_change() {
        let mut pointer = Pointer::default();
        let divider = Target::Divider(0);
        let over_divider = || Some(divider);
        let over_pane = || Some(Target::Pane(1));
        let left_press = Phase::Press(Button::Left, 0);
        let right_press = Phase::Press(Button::Right, 0);
        let release = Phase::Release(Button::Left);
        assert_eq!(pointer.route(Phase::Motion, over_divider), None);
        assert_eq!(pointer.route(right_press, over_divider), None);
        assert_eq!(pointer.route(left_press, over_divider), Some(divider));
        pointer.press(Button::Left, divider);
        assert_eq!(pointer.route(Phase::Motion, over_pane), Some(divider));
        assert_eq!(pointer.route(release, over_pane), Some(divider));

        pointer.press(Button::Left, divider);
        pointer.reset();
        assert_eq!(pointer.route(Phase::Motion, over_pane), None);
        assert_eq!(pointer.route(release, over_pane), None);
        assert_eq!(pointer.route(Phase::Motion, over_pane), over_pane());

        pointer.press(Button::Left, divider);
        assert_eq!(pointer.route(Phase::Cancel, over_pane), Some(divider));
        assert_eq!(pointer.route(release, over_pane), None);
        assert_eq!(pointer.route(Phase::Motion, over_pane), over_pane());
    }
}
