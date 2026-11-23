use sumie_core::{
    CellPosition, CellRange, LinkUri,
    mouse::{Button, MousePhase},
};
use sumie_vt::Terminal;
use sumie_wayland::{Event, PointerPhase};

use super::{Gpu, PaneState, Result, Session};

#[derive(Default)]
pub(super) struct Pointer {
    position: Option<(f64, f64)>,
    control: bool,
}

impl Pointer {
    pub(super) fn record(&mut self, event: &Event) {
        match *event {
            Event::Pointer {
                phase: PointerPhase::Cancel,
                ..
            } => self.position = None,
            Event::PointerEntered {
                position,
                modifiers,
            }
            | Event::Pointer {
                position,
                modifiers,
                ..
            } => {
                self.position = Some(position);
                self.control = modifiers.ctrl;
            }
            Event::ModifiersChanged(modifiers) => self.control = modifiers.ctrl,
            _ => (),
        }
    }

    fn position(&self) -> Option<(f64, f64)> {
        self.position.filter(|_| self.control)
    }
}

#[derive(Default)]
pub(super) struct Hover {
    checked: Option<(CellPosition, u64)>,
    ranges: Vec<CellRange>,
    uri: Option<LinkUri>,
}

impl Hover {
    pub(super) fn invalidate(&mut self) {
        self.checked = None;
    }

    pub(super) fn before_snapshot(&mut self, terminal: &Terminal) {
        if let Some((hit, stamp)) = self.checked
            && terminal.link_stamp(hit) != (stamp, false)
        {
            self.checked = None;
        }
    }

    fn refresh(
        &mut self,
        terminal: &Terminal,
        hit: Option<CellPosition>,
        host: &str,
    ) -> Result<bool> {
        let was_link = self.uri.is_some();
        if let Some(hit) = hit {
            if self.checked.is_some_and(|(pos, _)| pos == hit) {
                return Ok(false);
            }
            self.uri = terminal.link_at(hit, host, &mut self.ranges)?;
            self.checked = Some((hit, terminal.link_stamp(hit).0));
        } else {
            self.uri = None;
            self.checked = None;
            self.ranges.clear();
        }
        Ok(was_link || self.uri.is_some())
    }

    pub(super) fn ranges(&self) -> &[CellRange] {
        &self.ranges
    }

    pub(super) fn uri(&self) -> Option<&LinkUri> {
        self.uri.as_ref()
    }

    pub(super) fn open(uri: LinkUri) {
        if let Err(error) = sumie_pty::open_url(uri) {
            tracing::warn!(%error, "could not start desktop opener worker");
        }
    }
}

impl PaneState {
    pub(super) fn prepare_pointer_link(
        &mut self,
        mouse: &mut sumie_core::mouse::MouseEvent,
        hit: impl FnOnce() -> Option<CellPosition>,
        host: &str,
    ) -> Result<Option<LinkUri>> {
        if self.search.is_some() {
            return Ok(None);
        }
        match mouse.phase {
            MousePhase::Press(Button::Left, _) if mouse.modifiers.control => {
                self.update_snapshot()?;
                self.probe_link(hit(), host)?;
                let uri = self.hover.uri();
                mouse.link = uri.is_some();
                self.pane.capture_link(uri);
            }
            MousePhase::Release(Button::Left) => {
                if let Some(uri) = self.pane.take_pressed_link()
                    && let Some(hit) = hit()
                {
                    self.update_snapshot()?;
                    // Revalidate even if the hover cache still covers the release cell.
                    self.hover.invalidate();
                    self.probe_link(Some(hit), host)?;
                    mouse.link = self.hover.uri() == Some(&uri);
                    return Ok(mouse.link.then_some(uri));
                }
            }
            _ => (),
        }
        Ok(None)
    }

    pub(super) fn probe_link(&mut self, hit: Option<CellPosition>, host: &str) -> Result<()> {
        self.frame_dirty |= self.hover.refresh(&self.pane.terminal, hit, host)?;
        Ok(())
    }
}

impl Session {
    pub(super) fn update_link_hover(&mut self, gpu: &mut Gpu, banner: bool) -> Result<()> {
        let target = self
            .link_pointer
            .position()
            .and_then(|position| self.pane_under_pointer(position).map(|id| (id, position)));
        let tab = self.tabs.active_mut().expect("live window");
        let focused = tab.tree.focused();
        let mut link = false;
        for pane in &mut tab.panes {
            let active = pane.id == focused;
            let hit = target
                .filter(|(id, _)| *id == pane.id && !(active && pane.search.is_some()))
                .and_then(|(_, position)| {
                    self.view.link_cell(position, pane.rect, banner && active)
                });
            if hit.is_none() || !pane.snapshot_dirty {
                pane.probe_link(hit, &self.host)?;
            }
            link |= hit.is_some_and(|hit| {
                pane.hover.uri().is_some() && pane.hover.checked.is_some_and(|(pos, _)| pos == hit)
            });
        }
        gpu.set_link_cursor(link);
        Ok(())
    }
}
