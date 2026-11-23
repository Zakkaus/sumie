use sumie_core::{
    CellPosition, CellRange, LinkUri,
    mouse::{Button, MousePhase},
};
use sumie_vt::Terminal;
use sumie_wayland::{Event, PointerPhase};

use super::{Gpu, Result, Session, Tab, View};

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

    pub(super) fn hit(&self, view: &View, searching: bool, banner: bool) -> Option<CellPosition> {
        if !self.control || searching {
            return None;
        }
        view.link_cell(self.position?, banner)
    }
}

#[derive(Default)]
pub(super) struct Hover {
    checked: Option<CellPosition>,
    link: Option<(CellRange, LinkUri)>,
}

impl Hover {
    pub(super) fn invalidate(&mut self) {
        self.checked = None;
    }

    fn refresh(&mut self, terminal: &Terminal, hit: Option<CellPosition>) -> Result<bool> {
        let previous = self.range();
        if let Some(hit) = hit {
            if self.checked != Some(hit) {
                self.link = terminal.link_at(hit)?;
                self.checked = Some(hit);
            }
        } else {
            self.link = None;
            self.checked = None;
        }
        Ok(previous != self.range())
    }

    pub(super) fn range(&self) -> Option<CellRange> {
        self.link.as_ref().map(|(range, _)| *range)
    }

    pub(super) fn open(&self) {
        if let Some((_, uri)) = &self.link
            && let Err(error) = sumie_pty::open_url(uri.clone())
        {
            tracing::warn!(%error, "could not start desktop opener worker");
        }
    }
}

impl Tab {
    pub(super) fn prepare_pointer_press(
        &mut self,
        phase: MousePhase,
        hit: impl FnOnce() -> Option<CellPosition>,
    ) -> Result<bool> {
        let MousePhase::Press(button, _) = phase else {
            return Ok(false);
        };
        self.update_snapshot()?;
        Ok(button == Button::Left && self.probe_link(hit())?)
    }

    pub(super) fn probe_link(&mut self, hit: Option<CellPosition>) -> Result<bool> {
        self.frame_dirty |= self.hover.refresh(&self.pane.terminal, hit)?;
        Ok(self.hover.range().is_some())
    }
}

impl Session {
    pub(super) fn update_link_hover(&mut self, gpu: &mut Gpu, banner: bool) -> Result<()> {
        let hit = self
            .link_pointer
            .hit(&self.view, self.active().search.is_some(), banner);
        let tab = self.active_mut();
        if hit.is_none() || !tab.snapshot_dirty {
            tab.probe_link(hit)?;
        }
        gpu.set_link_cursor(tab.hover.range().is_some());
        Ok(())
    }
}
