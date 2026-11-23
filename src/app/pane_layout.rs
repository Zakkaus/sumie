use sumie_core::{PaneAction, splits::Axis};

use super::{Gpu, Layout, PaneState, PaneTab, Rect, Result, Session, SplitTree, View};

impl PaneTab {
    pub(super) fn new(pane: PaneState) -> Self {
        let tree = SplitTree::new(pane.id);
        let mut layout = Layout::default();
        tree.layout(pane.rect, [1; 2], &mut layout);
        Self {
            tree,
            layout,
            layout_revision: 0,
            panes: vec![pane],
        }
    }

    // IDs increase at spawn; removal preserves their order.
    fn index(&self, id: u64) -> usize {
        self.panes
            .binary_search_by_key(&id, |pane| pane.id)
            .expect("live leaf")
    }

    pub(super) fn get(&self, id: u64) -> &PaneState {
        &self.panes[self.index(id)]
    }

    pub(super) fn get_mut(&mut self, id: u64) -> &mut PaneState {
        let index = self.index(id);
        &mut self.panes[index]
    }

    pub(super) fn focused(&self) -> &PaneState {
        self.get(self.tree.focused())
    }

    pub(super) fn focused_mut(&mut self) -> &mut PaneState {
        self.get_mut(self.tree.focused())
    }

    pub(super) fn remove(&mut self, id: u64) -> PaneState {
        self.panes.remove(self.index(id))
    }

    pub(super) fn resize_grid(&mut self, view: &View) -> Result<()> {
        self.tree
            .layout(view.content_rect(), view.cell, &mut self.layout);
        self.layout_revision = self.layout_revision.wrapping_add(1);
        for &(id, rect) in self.layout.panes.iter().chain(&self.layout.hidden) {
            let index = self.index(id);
            self.panes[index].resize_grid(rect, view)?;
        }
        Ok(())
    }

    pub(super) fn refresh_visible(&mut self) -> Result<()> {
        for i in 0..self.layout.panes.len() {
            self.get_mut(self.layout.panes[i].0).update_snapshot()?;
        }
        Ok(())
    }
}

impl Session {
    pub(super) fn all_panes_mut(&mut self) -> impl Iterator<Item = &mut PaneState> {
        self.tabs
            .as_mut_slice()
            .iter_mut()
            .flat_map(|tab| &mut tab.panes)
    }

    pub(super) fn spawn_pane(
        &mut self,
        rect: Rect,
        config: &sumie_config::Config,
    ) -> Result<PaneState> {
        let cwd = self
            .active()
            .directory
            .path
            .as_deref()
            .filter(|p| p.is_dir())
            .unwrap_or(&self.home);
        let pane = self
            .spawn
            .open(self.next_id, rect, &self.view, config, cwd)?;
        self.next_id += 1;
        Ok(pane)
    }

    fn split(&mut self, axis: Axis, gpu: &mut Gpu, config: &sumie_config::Config) -> Result<()> {
        let tab = self.tabs.active().unwrap();
        let old = tab.tree.focused();
        let rect = tab
            .tree
            .split_preview(axis, self.view.content_rect(), self.view.cell);
        let pane = match self.spawn_pane(rect, config) {
            Ok(pane) => pane,
            Err(error) => {
                tracing::warn!(%error, "could not split pane");
                return Ok(());
            }
        };
        let tab = self.tabs.active_mut().unwrap();
        tab.tree.split(axis, pane.id);
        tab.panes.push(pane);
        tab.resize_grid(&self.view)?;
        self.focus_changed(Some((self.tabs.active_index().unwrap(), old)), gpu, false)
    }

    pub(super) fn focus_pane(&mut self, id: u64, gpu: &mut Gpu) -> Result<()> {
        let tab = self.tabs.active_mut().unwrap();
        let old = tab.tree.focused();
        if tab.tree.focus(id) {
            self.focus_changed(Some((self.tabs.active_index().unwrap(), old)), gpu, false)?;
        }
        Ok(())
    }

    pub(super) fn drag_divider(&mut self, id: u64, pos: [u32; 2]) -> Result<()> {
        let tab = self.tabs.active_mut().unwrap();
        if tab.tree.drag(id, pos, &tab.layout) {
            tab.resize_grid(&self.view)?;
            self.view.reset_hits();
        }
        Ok(())
    }

    pub(super) fn pane_action(
        &mut self,
        command: PaneAction,
        gpu: &mut Gpu,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let tab = self.tabs.active_mut().unwrap();
        let old = tab.tree.focused();
        match command {
            PaneAction::Split(axis) => return self.split(axis, gpu, config),
            PaneAction::Focus(dir) => {
                if tab.tree.focus_neighbor(dir, &tab.layout) {
                    self.focus_changed(Some((self.tabs.active_index().unwrap(), old)), gpu, false)?;
                }
                return Ok(());
            }
            PaneAction::Resize(dir) if !tab.tree.resize(dir, &tab.layout) => return Ok(()),
            PaneAction::Resize(_) => (),
            PaneAction::Zoom => tab.tree.toggle_zoom(),
        }
        tab.resize_grid(&self.view)?;
        tab.focused_mut().frame_dirty = true;
        self.view.reset_hits();
        Ok(())
    }
}
