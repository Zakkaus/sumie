use std::ops::ControlFlow::{self, Break, Continue};
use std::path::Path;
use std::time::Instant;

use sumie_core::{Pane, PaneCommand, Selection, TabAction};
use sumie_vt::Event as TerminalEvent;
use sumie_wayland::{Event, PointerButton as Button, PointerPhase as Phase, SourceHandle};

use super::{Gpu, LocalPane, Result, Session, Sources, Tab, View, directory, sources};

#[derive(Clone, Copy)]
pub(super) enum Action {
    Key(TabAction),
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

impl Tab {
    pub(super) fn open(
        view: &View,
        handle: &SourceHandle,
        config: &sumie_config::Config,
        cwd: &Path,
        click_interval: Option<u32>,
        image_dimension_limit: u32,
    ) -> Result<Self> {
        let mut pane = LocalPane::login(view.size(), view.cell, config, cwd)?;
        pane.terminal
            .set_image_dimension_limit(image_dimension_limit);
        if let Some(millis) = click_interval {
            pane.set_click_interval(millis);
        }
        let sources = Sources::new(handle.clone(), &pane.pty)?;
        Ok(Self {
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

    pub(super) fn label<'a>(&'a self, shell: &'a str) -> &'a str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.directory.label.is_empty() {
            &self.directory.label
        } else {
            shell
        }
    }

    pub(super) fn resize_grid(&mut self, view: &View) -> Result<()> {
        self.clear_selection();
        let size = view.size();
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
    pub(super) fn active(&self) -> &Tab {
        self.tabs.active().expect("live window")
    }
    pub(super) fn active_mut(&mut self) -> &mut Tab {
        self.tabs.active_mut().expect("live window")
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

    pub(super) fn input_target(&self, event: &Event) -> Option<(usize, u64)> {
        if let Event::ClipboardText { generation, .. } = event {
            let index = self
                .tabs
                .as_slice()
                .iter()
                .position(|tab| tab.paste_recipient == Some(*generation))?;
            Some((index, *generation))
        } else {
            Some((self.tabs.active_index()?, self.routing.generation))
        }
    }

    fn open_tab(&mut self, gpu: &mut Gpu, config: &sumie_config::Config) -> Result<()> {
        let count = self.tabs.as_slice().len();
        let resized = self.view.set_tab_count(count + 1);
        let cwd = self
            .active()
            .directory
            .path
            .as_deref()
            .filter(|p| p.is_dir())
            .unwrap_or(&self.home);
        let prepared = resized.and_then(|resized| {
            Tab::open(
                &self.view,
                &self.handle,
                config,
                cwd,
                self.click_interval,
                gpu.image_dimension_limit(),
            )
            .map(|tab| (tab, resized))
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
        let old = self.tabs.active_index();
        self.tabs.open(tab);
        self.focus_changed(old, gpu)
    }

    pub(super) fn apply(
        &mut self,
        action: Action,
        gpu: &mut Gpu,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let index = self.tabs.active_index().unwrap();
        match action {
            Action::Key(TabAction::Open) => self.open_tab(gpu, config)?,
            Action::Close(i) => self.close_tab(i, gpu)?,
            Action::Key(TabAction::Close) => self.close_tab(index, gpu)?,
            Action::Key(nav) if self.tabs.navigate(nav) => {
                if matches!(nav, TabAction::MoveLeft | TabAction::MoveRight) {
                    self.view.reset_hits();
                    self.active_mut().frame_dirty = true;
                } else {
                    self.focus_changed(Some(index), gpu)?;
                }
            }
            Action::Key(_) => (),
        }
        Ok(())
    }

    fn focus_changed(&mut self, old: Option<usize>, gpu: &mut Gpu) -> Result<()> {
        if let Some(i) = old {
            let tab = &mut self.tabs.as_mut_slice()[i];
            tab.sources.watch_blink(false)?;
            tab.pane.leave_tab()?;
        }
        self.pointer.change_recipient();
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
        self.retire(index)?;
        let count = self.tabs.as_slice().len();
        if count != 0 {
            if self.view.set_tab_count(count)? {
                self.resize_grid()?;
            }
            if focused {
                self.focus_changed(None, gpu)?;
            }
            self.active_mut().frame_dirty = true;
        }
        Ok(())
    }

    fn retire(&mut self, index: usize) -> Result<()> {
        let mut tab = self.tabs.close(index).expect("existing tab");
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
            self.retire(index)?;
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
        for index in 0..self.tabs.as_slice().len() {
            let active = self.tabs.active_index() == Some(index);
            let tab = &mut self.tabs.as_mut_slice()[index];
            let mut changed = false;
            if tab.service(gpu, active, self.host.as_bytes(), &mut changed)? {
                tab.exited = true;
                exited = true;
            } else if changed {
                if active {
                    gpu.queue_title(tab.label(&self.shell).to_owned());
                }
                self.active_mut().frame_dirty = true;
            }
        }
        Ok(exited)
    }

    pub(super) fn close_exited(&mut self, gpu: &mut Gpu) -> Result<bool> {
        for index in (0..self.tabs.as_slice().len()).rev() {
            if self.tabs.as_slice()[index].exited {
                self.close_tab(index, gpu)?;
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
