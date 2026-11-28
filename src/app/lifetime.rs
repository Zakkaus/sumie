use std::{os::fd::BorrowedFd, time::Instant};

use super::{PaneState, Session};
use crate::{
    Result,
    platform::{ExitWatches, PaneWatches},
};

impl Session {
    pub(super) fn sync_input(&self, watches: &mut impl PaneWatches) -> Result<()> {
        for tab in self.tabs.as_slice() {
            for pane in &tab.panes {
                watches.watch_input(pane.id, pane.pane.pty.input_fd())?;
            }
        }
        Ok(())
    }

    pub(super) fn watch_blink(
        &self,
        watches: &mut impl PaneWatches,
        eligible: bool,
        banner: Option<&str>,
    ) -> Result<()> {
        let tab = self.active();
        let cursor = tab.snapshot.as_ref().and_then(|snapshot| snapshot.cursor);
        let enabled = eligible
            && tab.viewer.is_none()
            && tab.search.is_none()
            && self.view.preedit.is_empty()
            && cursor.is_some_and(|cursor| {
                cursor.visible && cursor.blink && (cursor.row != 0 || banner.is_none())
            });
        watches.watch_blink(tab.id, enabled)
    }

    pub(super) fn retire_tab(
        &mut self,
        index: usize,
        watches: &mut impl PaneWatches,
    ) -> Result<()> {
        let tab = self.tabs.close(index).expect("existing tab");
        let mut result = Ok(());
        for pane in tab.panes {
            result = result.and(self.retire(pane, watches));
        }
        result
    }

    pub(super) fn retire(
        &mut self,
        mut pane: PaneState,
        watches: &mut impl PaneWatches,
    ) -> Result<()> {
        let id = pane.id;
        if self
            .confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.targets(&pane))
        {
            self.dismiss_confirmation();
        }
        self.routing.forget_pane(id);
        let close = pane.pane.pty.close();
        if pane.pane.pty.exit_fd().is_some() {
            tracing::debug!(
                pane = id,
                "sent SIGHUP; watching the child until it is reaped"
            );
            watches.retire_pane(id);
            self.closing.push((id, pane.pane.pty));
        } else {
            watches.unregister(id);
        }
        Ok(close?)
    }

    pub(super) fn close_all(&mut self, watches: &mut impl PaneWatches) -> Result<()> {
        let mut result = Ok(());
        while let Some(index) = self.tabs.active_index() {
            result = result.and(self.retire_tab(index, watches));
        }
        result
    }

    #[cfg_attr(
        target_os = "macos",
        expect(
            dead_code,
            reason = "macOS keeps reaping through the window's exit watches"
        )
    )]
    pub(super) fn closing_fds(&self) -> impl Iterator<Item = (u64, BorrowedFd<'_>)> {
        self.closing
            .iter()
            .filter_map(|(id, pty)| pty.exit_fd().map(|fd| (*id, fd)))
    }

    pub(crate) fn closing_count(&self) -> usize {
        self.closing.len()
    }

    pub(crate) fn closing_deadline(&self) -> Option<Instant> {
        self.closing
            .iter()
            .filter_map(|(_, pty)| pty.close_deadline())
            .min()
    }

    pub(crate) fn service_closing(
        &mut self,
        now: Instant,
        watches: &mut impl ExitWatches,
    ) -> Result<()> {
        let mut index = 0;
        while let Some((id, pty)) = self.closing.get_mut(index) {
            if watches.take_exit_ready(*id) || pty.close_deadline().is_some_and(|at| at <= now) {
                pty.service_child()?;
            }
            if pty.exit_fd().is_none() {
                watches.unregister(*id);
                tracing::debug!(
                    pane = *id,
                    "reaped a closing child and removed its exit watch"
                );
                self.closing.swap_remove(index);
            } else {
                index += 1;
            }
        }
        Ok(())
    }
}
