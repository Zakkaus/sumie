use std::time::Instant;

use sumie_core::{PaneAction, splits::Axis};

use super::{Layout, PaneState, PaneTab, Rect, Result, Session, SplitTree, View, frame_log::Stage};
use crate::platform::{PaneWatches, WindowOps};

impl PaneTab {
    pub(super) fn new(pane: PaneState) -> Self {
        let tree = SplitTree::new(pane.id);
        let mut layout = Layout::default();
        tree.layout(pane.rect, [1; 2], [0; 2], &mut layout);
        Self {
            tree,
            layout,
            layout_revision: 0,
            panes: vec![pane],
            activity: false,
        }
    }

    // IDs increase at spawn; removal preserves their order.
    pub(super) fn index(&self, id: u64) -> usize {
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
        self.tree.layout(
            view.content_rect(),
            view.cell,
            view.pane_padding(),
            &mut self.layout,
        );
        self.layout_revision = self.layout_revision.wrapping_add(1);
        for &(id, rect) in self.layout.panes.iter().chain(&self.layout.hidden) {
            let index = self.index(id);
            self.panes[index].resize_grid(rect, view)?;
        }
        Ok(())
    }
}

impl Session {
    pub(super) fn refresh_visible(
        &mut self,
        now: Option<u64>,
        window: &mut impl WindowOps,
    ) -> Result<Option<u64>> {
        self.frame_log.mark(Stage::Start);
        let tab = self.tabs.active_mut().expect("live window");
        let focused = tab.tree.focused();
        let mut next = None;
        for i in 0..tab.layout.panes.len() {
            let pane = tab.get_mut(tab.layout.panes[i].0);
            pane.rebuild_snapshot()?;
            let Some(now) = now else { continue };
            let images = &pane
                .snapshot
                .as_ref()
                .expect("visible pane snapshot")
                .images;
            let visible = self.view.visible_placements(pane.id, images);
            let viewer = pane
                .viewer
                .as_ref()
                .filter(|_| pane.viewer_presented())
                .map(|v| &v.placement);
            let (changed, deadline) = pane
                .pane
                .terminal
                .animation_tick_visible(now, visible, viewer)?;
            if changed {
                pane.snapshot_dirty = true;
                self.pane_pointer.image_click.refresh_viewer(
                    pane,
                    (pane.id == focused).then_some((
                        &mut self.view,
                        &mut self.routing,
                        &mut *window,
                    )),
                )?;
                pane.update_snapshot()?;
            }
            if let Some(at) = deadline {
                next = Some(next.map_or(at, |earliest: u64| earliest.min(at)));
            }
        }
        self.frame_log.mark(Stage::Snapshots);
        Ok(next)
    }

    /// When held output in a visible pane may be shown.
    pub(super) fn snapshot_deadline(&self) -> Option<Instant> {
        let tab = self.tabs.active()?;
        tab.layout
            .panes
            .iter()
            .filter_map(|&(id, _)| tab.get(id).snapshot_due())
            .min()
    }

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
        launch: super::Launch,
        watches: &mut impl PaneWatches,
    ) -> Result<PaneState> {
        let cwd = self
            .active()
            .directory
            .path
            .as_deref()
            .filter(|p| p.is_dir())
            .unwrap_or(&self.home);
        let pane = self.spawn.open(
            (self.next_id, rect),
            &self.view,
            config,
            cwd,
            launch,
            watches,
        )?;
        self.next_id += 1;
        Ok(pane)
    }

    fn split(
        &mut self,
        axis: Axis,
        window: &mut impl WindowOps,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let tab = self.tabs.active().unwrap();
        let old = tab.tree.focused();
        let rect = tab.tree.split_preview(
            axis,
            self.view.content_rect(),
            self.view.cell,
            self.view.pane_padding(),
        );
        let pane = match self.spawn_pane(rect, config, super::Launch::Shell, window) {
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
        self.focus_changed(
            Some((self.tabs.active_index().unwrap(), old)),
            window,
            false,
        )
    }

    /// Returns whether focus moved.
    pub(super) fn focus_pane(&mut self, id: u64, window: &mut impl WindowOps) -> Result<bool> {
        let tab = self.tabs.active_mut().unwrap();
        let old = tab.tree.focused();
        let moved = tab.tree.focus(id);
        if moved {
            self.focus_changed(
                Some((self.tabs.active_index().unwrap(), old)),
                window,
                false,
            )?;
        }
        Ok(moved)
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
        previous: u64,
        window: &mut impl WindowOps,
        config: &sumie_config::Config,
    ) -> Result<()> {
        let tab = self.tabs.active_mut().unwrap();
        match command {
            PaneAction::Split(axis) => return self.split(axis, window, config),
            PaneAction::Focus(dir) => {
                if !tab.tree.focus_neighbor(dir, &tab.layout) {
                    return Ok(());
                }
                if !tab.layout.hidden.is_empty() {
                    tab.resize_grid(&self.view)?;
                }
                return self.focus_changed(
                    Some((self.tabs.active_index().unwrap(), previous)),
                    window,
                    false,
                );
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
