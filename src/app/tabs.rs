use std::ops::ControlFlow::{self, Break, Continue};
use std::path::Path;
use std::time::Instant;

use sumie_core::{Pane, PaneAction, PaneCommand, Selection, TabAction, splits::Rect};
use sumie_vt::Event as TerminalEvent;
use sumie_wayland::{Event, PointerButton as Button, PointerPhase as Phase, SourceHandle};

use super::{
    Gpu, LocalPane, PaneState, PaneTab, Result, Session, Sources, View, directory, sources,
};

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
        hit: Option<usize>,
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
                Break(hit.and_then(|i| match button {
                    Button::Left => Some(Action::Key(TabAction::Select(i))),
                    Button::Middle => Some(Action::Close(i)),
                    Button::Right => None,
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
    handle: SourceHandle,
    click_interval: Option<u32>,
    image_dimension_limit: u32,
}

impl PaneHost {
    pub(super) fn new(handle: SourceHandle, image_dimension_limit: u32) -> Self {
        Self {
            handle,
            click_interval: sumie_wayland::double_click_interval(),
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
    ) -> Result<PaneState> {
        let mut pane = LocalPane::login(view.pane_size(rect), view.cell, config, cwd)?;
        pane.terminal
            .set_image_dimension_limit(self.image_dimension_limit);
        if let Some(millis) = self.click_interval {
            pane.set_click_interval(millis);
        }
        let sources = Sources::new(self.handle.clone(), &pane.pty)?;
        Ok(PaneState {
            id,
            rect,
            cell: view.cell,
            exited: false,
            paste_recipient: None,
            search: None,
            selection: Selection::default(),
            selected_text: String::new(),
            pane,
            sources,
            directory: directory::Directory::at_spawn(cwd),
            title: String::new(),
            snapshot: None,
            snapshot_dirty: true,
            frame_dirty: true,
            hover: super::links::Hover::default(),
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
        if old.width == rect.width && old.height == rect.height && self.cell == view.cell {
            return Ok(());
        }
        self.cell = view.cell;
        self.clear_selection();
        let size = view.pane_size(rect);
        self.pane.command(PaneCommand::Resize {
            cols: size.cols,
            rows: size.rows,
            px: [size.width_px, size.height_px],
            cell_px: view.cell,
        })?;
        if let Some(bar) = &mut self.search {
            bar.resized();
        }
        self.snapshot_dirty = true;
        self.frame_dirty = true;
        Ok(())
    }

    pub(super) fn update_snapshot(&mut self) -> Result<()> {
        if self.snapshot_dirty {
            self.snapshot_dirty = false;
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
        gpu: &mut Gpu,
        active: bool,
        host: &[u8],
        labels_changed: &mut bool,
    ) -> Result<bool> {
        let sources = &mut self.sources;
        let ready = sources.take_ready();
        if ready.exit() {
            self.pane.pty.service_child()?;
            if self.pane.pty.exit_fd().is_none() {
                return Ok(true);
            }
        }
        if ready.output() {
            let (dirty, eof) = self.pane.drain_output()?;
            self.snapshot_dirty |= dirty;
            if dirty {
                sources.reset_blink();
            }
            if dirty && let Some(bar) = &mut self.search {
                bar.output(&self.pane.terminal)?;
                self.frame_dirty = true;
            }
            for event in self.pane.terminal.drain_events() {
                match event {
                    TerminalEvent::Clipboard(bytes) if active => {
                        gpu.write_terminal_clipboard(bytes);
                    }
                    TerminalEvent::Clipboard(_) => (),
                    TerminalEvent::Title(title) => {
                        *labels_changed |= self.title != title;
                        self.title = title;
                    }
                }
            }
            if dirty {
                let changed = self
                    .pane
                    .terminal
                    .with_pwd(|uri| self.directory.update(uri, host))?;
                *labels_changed |= changed && self.title.is_empty();
            }
            if eof {
                sources.remove_output();
            }
        }
        Ok(ready.input() && !self.pane.pty.flush_input()?)
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
        let Event::Pointer {
            position, phase, ..
        } = event
        else {
            return Continue(());
        };
        self.pointer.route(
            self.view.tab_hit(*position),
            self.view.over_tab_bar(*position),
            *phase,
        )
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

    fn open_tab(&mut self, gpu: &mut Gpu, config: &sumie_config::Config) -> Result<()> {
        let count = self.tabs.as_slice().len();
        let resized = self.view.set_tab_count(count + 1);
        let prepared = resized.and_then(|resized| {
            self.spawn_pane(self.view.content_rect(), config)
                .map(|pane| (PaneTab::new(pane), resized))
        });
        let (tab, resized) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.view.set_tab_count(count)?;
                self.active_mut().frame_dirty = true;
                tracing::warn!(%error, "could not open tab");
                return Ok(());
            }
        };
        if resized {
            self.resize_grid()?;
        }
        let old = self.tabs.active_index().map(|i| (i, self.active().id));
        self.tabs.open(tab);
        self.focus_changed(old, gpu, true)
    }

    pub(super) fn apply(
        &mut self,
        action: Action,
        gpu: &mut Gpu,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let index = self.tabs.active_index().unwrap();
        let old = self.active().id;
        match action {
            Action::Key(TabAction::Open) => self.open_tab(gpu, config)?,
            Action::Close(i) => self.close_tab(i, gpu)?,
            Action::Key(TabAction::Close) => self.close_pane(index, old, gpu)?,
            Action::Pane(command) => self.pane_action(command, old, gpu, config)?,
            Action::Key(nav) if self.tabs.navigate(nav) => {
                if matches!(nav, TabAction::MoveLeft | TabAction::MoveRight) {
                    self.view.reset_hits();
                    self.active_mut().frame_dirty = true;
                } else {
                    self.focus_changed(Some((index, old)), gpu, true)?;
                }
            }
            Action::Key(_) => (),
        }
        Ok(())
    }

    pub(super) fn focus_changed(
        &mut self,
        old: Option<(usize, u64)>,
        gpu: &mut Gpu,
        tab_changed: bool,
    ) -> Result<()> {
        if let Some((i, id)) = old {
            let tab = self.tabs.as_mut_slice()[i].get_mut(id);
            tab.frame_dirty = true;
            tab.sources.watch_blink(false)?;
            tab.pane.leave_tab()?;
        }
        if tab_changed {
            self.pointer.change_recipient();
            self.pane_pointer = super::pane_input::Pointer::default();
        } else {
            self.pane_pointer.reset();
        }
        self.routing.reset_pointer();
        gpu.set_input_generation(self.routing.change_recipient());
        self.view.reset_hits();
        self.view.preedit.clear();
        self.view.search_bar = self.active().search.as_ref().map(|_| String::new());
        self.view.search_highlights.clear();
        self.active_mut().frame_dirty = true;
        gpu.queue_title(self.active().label(&self.shell).to_owned());
        Ok(())
    }

    fn close_tab(&mut self, index: usize, gpu: &mut Gpu) -> Result<()> {
        let focused = self.tabs.active_index() == Some(index);
        self.retire_tab(index)?;
        let count = self.tabs.as_slice().len();
        if count != 0 {
            if self.view.set_tab_count(count)? {
                self.resize_grid()?;
            }
            if focused {
                self.focus_changed(None, gpu, true)?;
            }
            self.active_mut().frame_dirty = true;
        }
        Ok(())
    }

    fn retire_tab(&mut self, index: usize) -> Result<()> {
        let tab = self.tabs.close(index).expect("existing tab");
        for pane in tab.panes {
            self.retire(pane)?;
        }
        Ok(())
    }

    fn retire(&mut self, mut tab: PaneState) -> Result<()> {
        self.routing.forget_pane(tab.id);
        tab.pane.pty.close()?;
        if tab.pane.pty.exit_fd().is_some() {
            tab.sources.remove_output();
            tab.sources.watch_input(None)?;
            tab.sources.watch_blink(false)?;
            self.closing.push((tab.sources, tab.pane.pty));
        }
        Ok(())
    }

    pub(super) fn close_all(&mut self) -> Result<()> {
        while let Some(index) = self.tabs.active_index() {
            self.retire_tab(index)?;
        }
        Ok(())
    }

    pub(super) fn reap_closed(&mut self) -> Result<()> {
        for (sources, mut pty) in self.closing.drain(..) {
            drop(sources);
            sources::reap(&mut pty)?;
        }
        Ok(())
    }

    pub(super) fn service_pty(&mut self, gpu: &mut Gpu) -> Result<bool> {
        let now = Instant::now();
        for (sources, pty) in &mut self.closing {
            if sources.take_ready().exit() || pty.close_deadline().is_some_and(|at| at <= now) {
                pty.service_child()?;
            }
        }
        self.closing.retain(|(_, pty)| pty.exit_fd().is_some());
        let mut exited = false;
        let mut labels_changed = false;
        let active = self.active().id;
        for tab in self.tabs.as_mut_slice() {
            let focused = tab.tree.focused();
            for pane in &mut tab.panes {
                let mut changed = false;
                pane.exited |=
                    pane.service(gpu, pane.id == active, self.host.as_bytes(), &mut changed)?;
                exited |= pane.exited;
                labels_changed |= changed && pane.id == focused;
                if changed && pane.id == active {
                    gpu.queue_title(pane.label(&self.shell).to_owned());
                }
            }
        }
        self.active_mut().frame_dirty |= labels_changed;
        Ok(exited)
    }

    fn close_pane(&mut self, index: usize, id: u64, gpu: &mut Gpu) -> Result<()> {
        if self.tabs.as_slice()[index].panes.len() == 1 {
            return self.close_tab(index, gpu);
        }
        let current_tab = self.tabs.active_index() == Some(index);
        let tab = &mut self.tabs.as_mut_slice()[index];
        let changed = tab.tree.focused() == id;
        tab.tree.close(id);
        let pane = tab.remove(id);
        tab.resize_grid(&self.view)?;
        self.retire(pane)?;
        if current_tab && changed {
            self.focus_changed(None, gpu, false)?;
        } else if changed {
            self.active_mut().frame_dirty = true;
        }
        Ok(())
    }

    pub(super) fn close_exited(&mut self, gpu: &mut Gpu) -> Result<bool> {
        for index in (0..self.tabs.as_slice().len()).rev() {
            for leaf in (0..self.tabs.as_slice()[index].panes.len()).rev() {
                let pane = &self.tabs.as_slice()[index].panes[leaf];
                if pane.exited {
                    self.close_pane(index, pane.id, gpu)?;
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
