use std::ops::ControlFlow;

use sumie_core::{KeyAction, KeyModifiers, splits::Layout};

use super::{PaneState, PaneTab, Result, Session, View, WindowOps, links, tabs};
use crate::input::{self, Effect, Routing, SearchAction};
use crate::platform::{
    Event, PointerButton as Button, PointerEvent, PointerPhase as Phase, Position,
};

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

type LayoutKey = (u64, u64);

impl PaneTab {
    fn layout_key(&self) -> LayoutKey {
        // Pane IDs are unique across tabs; divider IDs are only unique within a tab.
        (self.panes[0].id, self.layout_revision)
    }
}

#[derive(Default)]
pub(super) struct Pointer {
    target: Option<Target>,
    held: [bool; 3],
    presented: Option<LayoutKey>,
    pressed_layout: Option<LayoutKey>,
    recover_frame: bool,
    /// The held left press only moved pane focus; its release stays out of the pane too.
    focus_click: bool,
    pub(super) image_click: super::viewer::Click,
}

impl Pointer {
    pub(super) fn present_layout(&mut self, tab: &PaneTab) {
        self.presented = Some(tab.layout_key());
    }

    pub(super) fn layout_is_presented(&self, tab: &PaneTab) -> bool {
        self.presented == Some(tab.layout_key())
    }

    pub(super) fn take_frame_recovery(&mut self) -> bool {
        std::mem::take(&mut self.recover_frame)
    }

    pub(super) fn reset(&mut self) {
        // A focus change ends the drag but does not transfer held buttons to a new pane.
        self.target = None;
        self.focus_click = false;
        self.image_click = super::viewer::Click::default();
    }

    fn cancel_stale_pane(&mut self, layout: LayoutKey) -> Option<u64> {
        if let Some(Target::Pane(id)) = self.target
            && self.pressed_layout != Some(layout)
        {
            self.reset();
            return Some(id);
        }
        None
    }

    fn route(
        &mut self,
        phase: Phase,
        layout: LayoutKey,
        hit: impl FnOnce() -> Option<Target>,
    ) -> Option<Target> {
        // Captured dividers can outpace presentation; only new hits need the presented layout.
        let presented = self.presented;
        let hit = || (presented == Some(layout)).then(hit).flatten();
        let pane_hit = |target: &Target| matches!(target, Target::Pane(_));
        match phase {
            Phase::Press(button, _) => {
                self.recover_frame |= presented != Some(layout);
                self.motion(hit)
                    .filter(|target| pane_hit(target) || button == Button::Left)
            }
            Phase::Motion => self.motion(|| hit().filter(pane_hit)),
            Phase::Wheel(_) | Phase::Touch(_) | Phase::TouchEnd => hit().filter(pane_hit),
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

    /// Hold `button` over `target`. Returns whether the press reaches the pane: as in Ghostty,
    /// a left press that moved pane focus does not, even with mouse tracking on.
    fn press(&mut self, button: Button, target: Target, moved_focus: bool) -> bool {
        self.target = Some(target);
        self.pressed_layout = self.presented;
        self.held[button as usize] = true;
        if button == Button::Left {
            self.focus_click = moved_focus;
        }
        !(button == Button::Left && moved_focus)
    }

    fn release(&mut self, button: Button) -> Option<Target> {
        if !std::mem::take(&mut self.held[button as usize]) {
            return None;
        }
        let target = if self.held.contains(&true) {
            self.target
        } else {
            self.target.take()
        };
        if button == Button::Left && std::mem::take(&mut self.focus_click) {
            return None;
        }
        target
    }

    fn cancel(&mut self) -> Option<Target> {
        self.held.fill(false);
        self.focus_click = false;
        self.target.take()
    }
}

impl Session {
    pub(super) fn input(
        &mut self,
        event: Event,
        window: &mut impl WindowOps,
        banner: bool,
    ) -> Result<Option<tabs::Action>> {
        self.link_pointer.record(&event);
        if let ControlFlow::Break(action) = self.bar_input(&event) {
            return Ok(action);
        }
        if let Event::Pointer(pointer) = event {
            self.pointer_input(pointer, window, banner)?;
            return Ok(None);
        }

        let Some((index, id, recipient)) = self.input_target(&event) else {
            return Ok(None);
        };
        let tab = &mut self.tabs.as_mut_slice()[index];
        let has_splits = tab.panes.len() > 1;
        let slot = tab.index(id);
        let pane = &mut tab.panes[slot];
        if let Event::ModifiersChanged(modifiers) = event {
            self.pane_pointer.image_click = super::viewer::Click::default();
            pane.pane.pointer_modifiers_changed(modifiers);
            return Ok(None);
        }
        if pane.viewer_presented() {
            if super::viewer::escape_pressed(&event) {
                self.pane_pointer
                    .image_click
                    .dismiss(pane, Some((&mut self.view, &mut self.routing, window)));
            }
            return Ok(None);
        }
        if matches!(
            event,
            Event::Key {
                action: KeyAction::Press | KeyAction::Repeat,
                ..
            } | Event::ImeCommit { .. }
        ) {
            window.reset_blink(id);
        }
        let searching = pane.search.is_some();
        let scope = if pane.launch == super::Launch::Settings
            && matches!(event, Event::Key { .. })
            && pane.pane.terminal.reports_key_events()?
        {
            input::Scope::Recording
        } else if has_splits {
            input::Scope::Split
        } else {
            input::Scope::Single
        };
        match input::route(
            (id, recipient),
            &mut pane.pane,
            &mut self.routing,
            searching,
            event,
            scope,
        )? {
            Effect::Tab(action) => return Ok(Some(tabs::Action::Key(action))),
            Effect::Settings => return Ok(Some(tabs::Action::Settings)),
            Effect::Pane(action) => return Ok(Some(tabs::Action::Pane(action))),
            Effect::Zoom(action) => self.zoom(action)?,
            effect => apply_pane_effect(
                pane,
                &mut self.view,
                &mut self.routing,
                window,
                &mut self.link_pointer,
                searching,
                effect,
            )?,
        }
        Ok(None)
    }

    /// The window gained or lost the keyboard. A loss releases each reported key in the pane
    /// that received its press, wherever that pane is now, and drops every held key, so a
    /// repeat or release that arrives after refocus stays out of the PTY. Mode 1004 reports
    /// the change to the focused pane.
    pub(super) fn keyboard_focus(&mut self, focused: bool) -> Result<()> {
        if !focused {
            for key in self.routing.keyboard_left() {
                let owner = self
                    .tabs
                    .as_mut_slice()
                    .iter_mut()
                    .find_map(|tab| tab.panes.iter_mut().find(|pane| pane.id == key.pane));
                if let Some(owner) = owner {
                    input::release(&mut owner.pane, &key, KeyModifiers::default())?;
                }
            }
        }
        input::report_focus(&mut self.active_mut().pane, focused)
    }

    fn pointer_input(
        &mut self,
        event: PointerEvent,
        window: &mut impl WindowOps,
        banner: bool,
    ) -> Result<()> {
        let tab = self.tabs.active_mut().expect("live window");
        let key = tab.layout_key();
        if let Some(id) = self.pane_pointer.cancel_stale_pane(key) {
            self.routing.reset_pointer();
            if let Ok(index) = tab.panes.binary_search_by_key(&id, |pane| pane.id) {
                tab.panes[index].pane.leave_tab()?;
            }
        }
        let view = &self.view;
        let target = self.pane_pointer.route(event.phase, key, || {
            Target::at(&tab.layout, view.window_pixel(event.position), view.device)
        });
        if matches!(event.phase, Phase::Cancel) && !matches!(target, Some(Target::Pane(_))) {
            self.routing.reset_pointer();
        }
        let Some(target) = target else {
            return Ok(());
        };
        if let Phase::Press(button, _) = event.phase {
            let moved_focus = match target {
                Target::Pane(id) => self.focus_pane(id, window)?,
                Target::Divider(_) => false,
            };
            if !self.pane_pointer.press(button, target, moved_focus) {
                return Ok(());
            }
        }
        match target {
            Target::Pane(id) => {
                let tab = self.tabs.active_mut().expect("live window");
                let focused = tab.tree.focused() == id;
                let pane = tab.get_mut(id);
                let searching = focused && pane.search.is_some();
                if self.pane_pointer.image_click.handle(
                    pane,
                    &mut self.view,
                    &mut self.routing,
                    window,
                    event,
                )? {
                    if pane.viewer.is_some() {
                        self.link_pointer.clear_link();
                    }
                    return Ok(());
                }
                // The search bar and the config banner sit over the grid; they would slide with it.
                if self.smooth_scroll && !searching && !(banner && focused) {
                    let now = self.clock.elapsed();
                    if pane.smooth_wheel(event.phase, self.view.scale(), now) {
                        return Ok(());
                    }
                }
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
                    window,
                    &mut self.link_pointer,
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

    pub(super) fn pane_under_pointer(&self, position: Position) -> Option<u64> {
        let tab = self.tabs.active()?;
        if !self.pane_pointer.layout_is_presented(tab) || self.view.over_tab_bar(position) {
            return None;
        }
        let pixel = self.view.window_pixel(position);
        match Target::at(&tab.layout, pixel, self.view.device)? {
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
        view.pointer_position(position, rect, pane.scroll.shown()),
    );
    let scroll = pane.scroll.shown();
    let hit = || view.link_cell(position, rect, banner, scroll);
    let uri = pane.prepare_pointer_link(&mut mouse, hit, host)?;
    let hit = || view.search_hit(position, rect);
    match routing.pointer(&mut pane.pane, searching, mouse, hit)? {
        Effect::OpenLink => {
            links::Hover::open(uri.expect("validated link release"));
            Ok(Effect::None)
        }
        Effect::Select(step) if pane.select(step, mouse.cell)? => Ok(Effect::Copy),
        effect => Ok(effect),
    }
}

fn apply_pane_effect(
    pane: &mut PaneState,
    view: &mut View,
    routing: &mut Routing,
    window: &mut impl WindowOps,
    link_pointer: &mut links::Pointer,
    searching: bool,
    effect: Effect,
) -> Result<()> {
    match effect {
        Effect::Search(action) => {
            if matches!(action, SearchAction::Open) {
                link_pointer.clear_link();
            }
            if matches!(action, SearchAction::Open | SearchAction::Close) {
                pane.pane.reset_pointer_gestures();
                pane.paste_recipient = None;
                window.set_input_generation(routing.change_recipient());
            }
            pane.search_action(action, view)?;
        }
        Effect::Copy => pane.copy_selection(window)?,
        Effect::ViewportChanged => {
            pane.scroll.reset();
            link_pointer.clear_link();
            pane.frame_dirty |= searching || pane.selection.range().is_some();
            pane.clear_selection();
            pane.snapshot_dirty = true;
        }
        Effect::ReadClipboard => {
            window.read_clipboard(*pane.paste_recipient.get_or_insert(routing.generation));
        }
        Effect::Preedit(text, cursor) if text != view.preedit || cursor != view.preedit_cursor => {
            view.preedit = text;
            view.preedit_cursor = cursor;
            pane.frame_dirty = true;
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sumie_core::splits::{Axis, Rect, SplitTree};

    use super::{Button, Layout, LayoutKey, Phase, Pointer, Target};

    const SHOWN: LayoutKey = (1, 0);
    const PENDING: LayoutKey = (1, 1);

    fn pointer() -> Pointer {
        Pointer {
            presented: Some(SHOWN),
            ..Pointer::default()
        }
    }

    #[test]
    fn hits_are_absolute_and_wait_for_zoom_presentation() {
        let mut tree = SplitTree::new(1);
        tree.split(Axis::Horizontal, 2);
        let mut layout = Layout::default();
        let bounds = Rect {
            x: 0,
            y: 20,
            width: 81,
            height: 80,
        };
        tree.layout(bounds, [1; 2], [0; 2], &mut layout);
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

        let press = Phase::Press(Button::Left, 0);
        let mut pointer = pointer();
        tree.toggle_zoom();
        tree.layout(bounds, [1; 2], [0; 2], &mut layout);
        let hit = || Target::at(&layout, [39.9, 20.0], [81, 100]);
        assert_eq!(pointer.route(press, PENDING, hit), None);
        assert!(pointer.take_frame_recovery());
        pointer.presented = Some(PENDING);
        assert_eq!(pointer.route(press, PENDING, hit), Some(Target::Pane(2)));
    }

    #[test]
    fn drag_and_release_stay_with_the_pressed_pane_but_wheel_does_not() {
        let mut pointer = pointer();
        let left = Target::Pane(1);
        let right = Target::Pane(2);
        let over_right = || Some(right);
        assert!(pointer.press(Button::Left, left, false));
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_right), Some(left));
        assert_eq!(
            pointer.route(Phase::Wheel(120.0), SHOWN, over_right),
            Some(right)
        );
        assert_eq!(
            pointer.route(Phase::Release(Button::Left), SHOWN, over_right),
            Some(left)
        );
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_right), Some(right));
    }

    #[test]
    fn a_left_click_that_moves_focus_stays_out_of_the_pane() {
        let pane = Target::Pane(2);
        let over_pane = || Some(pane);
        let left = Phase::Release(Button::Left);
        let right = Phase::Release(Button::Right);
        let mut pointer = pointer();
        // Focusing left click: neither press nor release reaches the pane; the drag does.
        assert!(!pointer.press(Button::Left, pane, true));
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), Some(pane));
        assert!(pointer.press(Button::Right, pane, false));
        assert_eq!(pointer.route(right, SHOWN, over_pane), Some(pane));
        assert_eq!(pointer.route(left, SHOWN, over_pane), None);
        // The next click in the focused pane is delivered whole.
        assert!(pointer.press(Button::Left, pane, false));
        assert_eq!(pointer.route(left, SHOWN, over_pane), Some(pane));
        // Other buttons focus and still reach the pane.
        assert!(pointer.press(Button::Right, pane, true));
        assert_eq!(pointer.route(right, SHOWN, over_pane), Some(pane));
        // A cancelled focusing click leaves no swallowed release behind.
        assert!(!pointer.press(Button::Left, pane, true));
        pointer.route(Phase::Cancel, SHOWN, over_pane);
        assert!(pointer.press(Button::Left, pane, false));
        assert_eq!(pointer.route(left, SHOWN, over_pane), Some(pane));
    }

    #[test]
    fn divider_drag_requires_a_left_press_and_ends_on_focus_change() {
        let mut pointer = pointer();
        let divider = Target::Divider(0);
        let over_divider = || Some(divider);
        let over_pane = || Some(Target::Pane(1));
        let left_press = Phase::Press(Button::Left, 0);
        let right_press = Phase::Press(Button::Right, 0);
        let release = Phase::Release(Button::Left);
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_divider), None);
        assert_eq!(pointer.route(right_press, SHOWN, over_divider), None);
        assert_eq!(
            pointer.route(left_press, SHOWN, over_divider),
            Some(divider)
        );
        pointer.press(Button::Left, divider, false);
        assert_eq!(pointer.cancel_stale_pane(PENDING), None);
        assert_eq!(
            pointer.route(Phase::Motion, PENDING, over_pane),
            Some(divider)
        );
        assert_eq!(pointer.route(release, PENDING, over_pane), Some(divider));

        pointer.press(Button::Left, divider, false);
        pointer.reset();
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), None);
        assert_eq!(pointer.route(release, SHOWN, over_pane), None);
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), over_pane());

        pointer.press(Button::Left, divider, false);
        assert_eq!(
            pointer.route(Phase::Cancel, SHOWN, over_pane),
            Some(divider)
        );
        assert_eq!(pointer.route(release, SHOWN, over_pane), None);
        assert_eq!(pointer.route(Phase::Motion, SHOWN, over_pane), over_pane());
    }
}
