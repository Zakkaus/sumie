mod activity;
mod pace;
mod strip;

pub(super) use pace::SnapshotPace;

use std::ops::ControlFlow::{self, Break, Continue};
use std::path::Path;
use std::time::Instant;

use crate::platform::{
    Event, PointerButton as Button, PointerEvent, PointerPhase as Phase, Position,
};
use sumie_core::{Pane, PaneAction, PaneCommand, Selection, TabAction, splits::Rect};
use sumie_ui::TabHit;

use super::notices::{Alerts, Serviced};
use super::{LocalPane, PaneState, PaneTab, Result, Session, View, directory};
use crate::login::Environment;
use crate::platform::{PaneWatches, WindowOps};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Key(TabAction),
    Pane(PaneAction),
    Close(usize),
}

#[derive(Default)]
pub(super) struct BarPointer {
    grid: [bool; 3],
    captured: [bool; 3],
    wheel: f64,
}

impl BarPointer {
    pub(super) fn route(
        &mut self,
        hit: Option<TabHit>,
        over: bool,
        phase: Phase,
    ) -> ControlFlow<Option<Action>> {
        if !over {
            self.wheel = 0.0;
        }
        match phase {
            Phase::Cancel => {
                *self = Self::default();
                Continue(())
            }
            Phase::Release(button) => {
                if std::mem::take(&mut self.grid[button as usize]) {
                    return Continue(());
                }
                if std::mem::take(&mut self.captured[button as usize]) || over {
                    Break(None)
                } else {
                    Continue(())
                }
            }
            Phase::Press(button, _) if over => {
                self.captured[button as usize] = true;
                Break(hit.and_then(|hit| match (button, hit) {
                    (Button::Left, TabHit::Tab(i)) => Some(Action::Key(TabAction::Select(i))),
                    (Button::Left, TabHit::New) => Some(Action::Key(TabAction::Open)),
                    (Button::Left, TabHit::Close(i))
                    | (Button::Middle, TabHit::Tab(i) | TabHit::Close(i)) => Some(Action::Close(i)),
                    _ => None,
                }))
            }
            Phase::Press(button, _) => {
                self.grid[button as usize] = true;
                Continue(())
            }
            Phase::Wheel(delta) if over => {
                self.wheel += delta;
                if self.wheel.abs() < 120.0 {
                    return Break(None);
                }
                #[expect(clippy::cast_possible_truncation, reason = "emit whole notches")]
                let steps = (self.wheel / 120.0) as isize;
                self.wheel %= 120.0;
                Break(Some(Action::Key(TabAction::Cycle(steps))))
            }
            Phase::Motion
                if self.captured.contains(&true) || (over && !self.grid.contains(&true)) =>
            {
                Break(None)
            }
            _ => Continue(()),
        }
    }

    fn change_recipient(&mut self) {
        for (captured, pressed) in self.captured.iter_mut().zip(&mut self.grid) {
            *captured |= std::mem::take(pressed);
        }
    }
}

pub(super) struct PaneHost {
    environment: Environment,
    click_interval: Option<u32>,
    image_dimension_limit: u32,
}

impl PaneHost {
    pub(super) const fn new(
        environment: Environment,
        image_dimension_limit: u32,
        click_interval: Option<u32>,
    ) -> Self {
        Self {
            environment,
            click_interval,
            image_dimension_limit,
        }
    }

    pub(super) fn open(
        &self,
        id: u64,
        rect: Rect,
        view: &View,
        config: &sumie_config::Config,
        cwd: &Path,
        watches: &mut impl PaneWatches,
    ) -> Result<PaneState> {
        let size = view.pane_size(rect);
        let mut pane = LocalPane::login(&self.environment, size, view.cell, config, cwd)?;
        pane.terminal
            .set_image_dimension_limit(self.image_dimension_limit);
        if let Some(millis) = self.click_interval {
            pane.set_click_interval(millis);
        }
        watches.register_pane(
            id,
            pane.pty.output_fd(),
            pane.pty.exit_fd().expect("new child watcher"),
        )?;
        Ok(PaneState {
            id,
            rect,
            cell: view.cell,
            viewport_px: [size.width_px, size.height_px],
            exited: false,
            paste_recipient: None,
            search: None,
            viewer: None,
            selection: Selection::default(),
            selected_text: String::new(),
            pane,
            directory: directory::Directory::at_spawn(cwd),
            title: String::new(),
            snapshot: None,
            snapshot_dirty: true,
            snapshot_pace: SnapshotPace::default(),
            frame_dirty: true,
            activity_after: None,
            hover: super::links::Hover::default(),
            scroll: super::scroll::PaneScroll::default(),
            progress: None,
        })
    }
}

impl PaneState {
    pub(super) fn label<'a>(&'a self, shell: &'a str) -> &'a str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.directory.label.is_empty() {
            &self.directory.label
        } else {
            shell
        }
    }

    pub(super) fn resize_grid(&mut self, rect: Rect, view: &View) -> Result<()> {
        let old = std::mem::replace(&mut self.rect, rect);
        self.frame_dirty |= old != rect;
        let size = view.pane_size(rect);
        let px = [size.width_px, size.height_px];
        if self.viewport_px == px && self.cell == view.cell {
            return Ok(());
        }
        self.cell = view.cell;
        self.viewport_px = px;
        self.scroll.reset();
        self.clear_selection();
        self.pane.command(PaneCommand::Resize {
            cols: size.cols,
            rows: size.rows,
            px,
            cell_px: view.cell,
        })?;
        self.activity_after = Some(Instant::now() + activity::RESIZE_QUIET);
        if let Some(bar) = &mut self.search {
            bar.resized();
        }
        self.snapshot_dirty = true;
        self.frame_dirty = true;
        Ok(())
    }

    /// When held output may be shown.
    pub(super) fn snapshot_due(&self) -> Option<Instant> {
        self.snapshot_pace.due()
    }

    /// Rebuild now, including output that is still waiting on the pace.
    pub(super) fn update_snapshot(&mut self) -> Result<()> {
        self.snapshot_dirty |= self.snapshot_pace.held();
        self.rebuild_snapshot()
    }

    /// Rebuild unless only paced output is pending.
    pub(super) fn rebuild_snapshot(&mut self) -> Result<()> {
        if self.snapshot_dirty {
            self.snapshot_dirty = false;
            self.snapshot_pace.rebuilt(Instant::now());
            self.hover.before_snapshot(&self.pane.terminal);
            let next = self.pane.snapshot()?;
            if let Some(range) = self.selection.range()
                && !self
                    .pane
                    .terminal
                    .selection_text_matches(range, &self.selected_text)?
            {
                self.clear_selection();
                self.frame_dirty = true;
            }
            if self.snapshot.as_ref() != Some(&next) {
                self.snapshot = Some(next);
                self.frame_dirty = true;
            }
        }
        Ok(())
    }

    fn service(
        &mut self,
        window: &mut impl WindowOps,
        active: bool,
        host: &[u8],
        watch_at: Option<Instant>,
        labels_changed: &mut bool,
        alerts: &mut Alerts,
    ) -> Result<Serviced> {
        let now = alerts.now;
        self.snapshot_dirty |= self.snapshot_pace.released(now);
        let ready = window.take_ready(self.id);
        let mut serviced = Serviced::default();
        if ready.exit {
            self.pane.pty.service_child()?;
            if self.pane.pty.exit_fd().is_none() {
                serviced.exited = true;
                return Ok(serviced);
            }
        }
        if ready.output {
            let batch =
                activity::Batch::begin(&mut self.pane.terminal, watch_at, self.activity_after)?;
            let (drained, eof) = self.pane.drain_output()?;
            let dirty = drained > 0;
            if dirty {
                self.snapshot_dirty |= self.snapshot_pace.output(now, self.snapshot_dirty, drained);
                self.follow_output()?;
                serviced.activity |= batch.changed(&mut self.pane.terminal)?;
                window.reset_blink(self.id);
            }
            if dirty && let Some(bar) = &mut self.search {
                bar.output(&self.pane.terminal)?;
                self.frame_dirty = true;
            }
            self.handle_events(window, active, labels_changed, alerts, &mut serviced);
            if dirty {
                let changed = self
                    .pane
                    .terminal
                    .with_pwd(|uri| self.directory.update(uri, host))?;
                *labels_changed |= changed && self.title.is_empty();
            }
            if eof {
                window.stop_output(self.id);
            }
        }
        serviced.exited = ready.input && !self.pane.pty.flush_input()?;
        Ok(serviced)
    }
}

impl Session {
    pub(super) fn active(&self) -> &PaneState {
        self.tabs.active().expect("live window").focused()
    }
    pub(super) fn active_mut(&mut self) -> &mut PaneState {
        self.tabs.active_mut().expect("live window").focused_mut()
    }

    pub(super) fn bar_input(&mut self, event: &Event) -> ControlFlow<Option<Action>> {
        match *event {
            Event::Pointer(PointerEvent {
                position, phase, ..
            }) => {
                let hit = if matches!(phase, Phase::Press(..)) {
                    self.view.tab_hit(position)
                } else {
                    None
                };
                let routed = self
                    .pointer
                    .route(hit, self.view.over_tab_bar(position), phase);
                self.update_tab_hover((!matches!(phase, Phase::Cancel)).then_some(position));
                routed
            }
            Event::PointerEntered { position, .. } => {
                self.update_tab_hover(Some(position));
                Continue(())
            }
            _ => Continue(()),
        }
    }

    fn update_tab_hover(&mut self, position: Option<Position>) {
        let pressed = self.pointer.grid.contains(&true) || self.pointer.captured.contains(&true);
        if self.view.set_tab_pointer(position.filter(|_| !pressed)) {
            self.active_mut().frame_dirty = true;
        }
    }

    pub(super) fn input_target(&self, event: &Event) -> Option<(usize, u64, u64)> {
        if let Some(id) = self.routing.release_owner(event)
            && id != self.active().id
        {
            let index = self
                .tabs
                .as_slice()
                .iter()
                .position(|tab| tab.panes.binary_search_by_key(&id, |p| p.id).is_ok())?;
            return Some((index, id, self.routing.generation));
        }
        if let Event::ClipboardText { generation, .. } = event {
            return self
                .tabs
                .as_slice()
                .iter()
                .enumerate()
                .find_map(|(i, tab)| {
                    let pane = tab
                        .panes
                        .iter()
                        .find(|p| p.paste_recipient == Some(*generation))?;
                    Some((i, pane.id, *generation))
                });
        }
        Some((
            self.tabs.active_index()?,
            self.active().id,
            self.routing.generation,
        ))
    }

    fn open_tab(
        &mut self,
        window: &mut impl WindowOps,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let count = self.tabs.as_slice().len();
        let resized = self.view.set_tab_count(count + 1);
        let prepared = resized.and_then(|resized| {
            self.spawn_pane(self.view.content_rect(), config, window)
                .map(|pane| (pane, resized))
        });
        let (pane, resized) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.view.set_tab_count(count)?;
                self.active_mut().frame_dirty = true;
                tracing::warn!(%error, "could not open tab");
                return Ok(());
            }
        };
        let resize = if resized { self.resize_grid() } else { Ok(()) };
        if resize.is_err() {
            return resize.and(self.retire(pane, window));
        }
        let old = self.tabs.active_index().map(|i| (i, self.active().id));
        self.tabs.open(PaneTab::new(pane));
        self.focus_changed(old, window, true)
    }

    pub(super) fn apply(
        &mut self,
        action: Action,
        window: &mut impl WindowOps,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let index = self.tabs.active_index().unwrap();
        let old = self.active().id;
        match action {
            Action::Key(TabAction::Open) => self.open_tab(window, config)?,
            Action::Close(i) => self.close_tab(i, window)?,
            Action::Key(TabAction::Close) => self.close_pane(index, old, window)?,
            Action::Pane(command) => self.pane_action(command, old, window, config)?,
            Action::Key(nav) if self.tabs.navigate(nav) => {
                if matches!(nav, TabAction::MoveLeft | TabAction::MoveRight) {
                    self.view.reset_hits();
                    self.active_mut().frame_dirty = true;
                } else {
                    self.focus_changed(Some((index, old)), window, true)?;
                }
            }
            Action::Key(_) => (),
        }
        Ok(())
    }

    pub(super) fn focus_changed(
        &mut self,
        old: Option<(usize, u64)>,
        window: &mut impl WindowOps,
        tab_changed: bool,
    ) -> Result<()> {
        self.link_pointer.clear_link();
        if let Some((i, id)) = old {
            self.link_pointer.leave_thumbnail(&self.view, id);
            let old_tab = &mut self.tabs.as_mut_slice()[i];
            for pane in &mut old_tab.panes {
                pane.clear_link_hover();
            }
            let tab = old_tab.get_mut(id);
            tab.frame_dirty = true;
            window.watch_blink(id, false)?;
            tab.pane.leave_tab()?;
        }
        if tab_changed {
            let tab = self.tabs.active_mut().expect("live window");
            tab.activity = false;
            for pane in &mut tab.panes {
                pane.hide_viewer();
            }
            self.pointer.change_recipient();
            self.pane_pointer = super::pane_input::Pointer::default();
        } else {
            self.pane_pointer.reset();
        }
        self.routing.reset_pointer();
        window.set_input_generation(self.routing.change_recipient());
        self.view.reset_hits();
        self.view.preedit.clear();
        self.view.search_bar = self.active().search.as_ref().map(|_| String::new());
        self.view.search_highlights.clear();
        self.active_mut().frame_dirty = true;
        window.queue_title(self.active().label(&self.shell).to_owned());
        Ok(())
    }

    fn close_tab(&mut self, index: usize, window: &mut impl WindowOps) -> Result<()> {
        let focused = self.tabs.active_index() == Some(index);
        self.retire_tab(index, window)?;
        let count = self.tabs.as_slice().len();
        if count != 0 {
            if self.view.set_tab_count(count)? {
                self.resize_grid()?;
            }
            if focused {
                self.focus_changed(None, window, true)?;
            }
            self.active_mut().frame_dirty = true;
        }
        Ok(())
    }

    pub(super) fn service_pty(
        &mut self,
        now: Instant,
        window_visible: bool,
        window: &mut impl WindowOps,
    ) -> Result<bool> {
        let Some(active_index) = self.tabs.active_index() else {
            return Ok(false);
        };
        let mut exited = false;
        let mut labels_changed = false;
        let active = self.active().id;
        let mut activity_changed = false;
        let mut alerts = Alerts {
            gate: &mut self.notices,
            now,
            visible: false,
            background: false,
        };
        for (index, tab) in self.tabs.as_mut_slice().iter_mut().enumerate() {
            let focused = tab.tree.focused();
            alerts.visible = window_visible && index == active_index;
            alerts.background = index != active_index;
            for pane in &mut tab.panes {
                let mut changed = false;
                let watch_at = (index != active_index && !tab.activity).then_some(now);
                let mut serviced = pane.service(
                    window,
                    pane.id == active,
                    self.host.as_bytes(),
                    watch_at,
                    &mut changed,
                    &mut alerts,
                )?;
                pane.settle_progress(&mut serviced, now);
                pane.exited |= serviced.exited;
                activity_changed |= serviced.activity || serviced.progress;
                tab.activity |= serviced.activity;
                if pane.snapshot_dirty {
                    self.pane_pointer.image_click.refresh_viewer(
                        pane,
                        (pane.id == active).then_some((
                            &mut self.view,
                            &mut self.routing,
                            &mut *window,
                        )),
                    )?;
                }
                exited |= pane.exited;
                labels_changed |= changed && pane.id == focused;
                if changed && pane.id == active {
                    window.queue_title(pane.label(&self.shell).to_owned());
                }
            }
        }
        self.active_mut().frame_dirty |= labels_changed || activity_changed;
        Ok(exited)
    }

    fn close_pane(&mut self, index: usize, id: u64, window: &mut impl WindowOps) -> Result<()> {
        if self.tabs.as_slice()[index].panes.len() == 1 {
            return self.close_tab(index, window);
        }
        let current_tab = self.tabs.active_index() == Some(index);
        let tab = &mut self.tabs.as_mut_slice()[index];
        let changed = tab.tree.focused() == id;
        tab.tree.close(id);
        let pane = tab.remove(id);
        let resize = tab.resize_grid(&self.view);
        resize.and(self.retire(pane, window))?;
        if current_tab && changed {
            self.focus_changed(None, window, false)?;
        } else if changed {
            self.active_mut().frame_dirty = true;
        }
        Ok(())
    }

    pub(super) fn close_exited(&mut self, window: &mut impl WindowOps) -> Result<bool> {
        for index in (0..self.tabs.as_slice().len()).rev() {
            for leaf in (0..self.tabs.as_slice()[index].panes.len()).rev() {
                let pane = &self.tabs.as_slice()[index].panes[leaf];
                if pane.exited {
                    self.close_pane(index, pane.id, window)?;
                }
            }
        }
        Ok(self.tabs.active().is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_counts_whole_notches_and_keeps_the_remainder() {
        let mut pointer = BarPointer::default();
        for (delta, expected) in [(375., 3), (105., 1), (-90., 0), (-150., -2)] {
            let steps = match pointer.route(None, true, Phase::Wheel(delta)) {
                Break(Some(Action::Key(TabAction::Cycle(steps)))) => steps,
                Break(None) => 0,
                _ => panic!("wheel did not stay on the tab bar"),
            };
            assert_eq!(steps, expected);
        }
    }
}
