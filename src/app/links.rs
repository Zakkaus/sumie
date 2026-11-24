use sumie_core::{
    CellPosition, CellRange, LinkUri, SelectionPoint,
    mouse::{Button, MousePhase},
};
use sumie_vt::Terminal;

use super::{PaneState, Result, Session, WindowOps};
use crate::platform::{CursorIcon, Event, Modifiers, PointerPhase};
use crate::view::LinkHighlight;

#[derive(Default)]
pub(super) struct Pointer {
    position: Option<(f64, f64)>,
    link_enabled: bool,
    pub(super) modifiers: Modifiers,
}

impl Pointer {
    pub(super) fn clear_link(&mut self) {
        self.link_enabled = false;
    }

    pub(super) fn leave_thumbnail(&mut self, view: &super::View, pane: u64) {
        if self
            .position
            .is_some_and(|pos| view.thumbnail_hit(pos, pane).is_some())
        {
            self.position = None;
        }
    }

    pub(super) fn record(&mut self, event: &Event) {
        match *event {
            Event::Pointer(pointer) if matches!(pointer.phase, PointerPhase::Cancel) => {
                self.position = None;
                self.clear_link();
            }
            Event::PointerEntered {
                position,
                modifiers,
            } => {
                self.position = Some(position);
                self.modifiers = modifiers;
                self.link_enabled = true;
            }
            Event::Pointer(pointer) => {
                self.position = Some(pointer.position);
                self.modifiers = pointer.modifiers;
                match pointer.phase {
                    PointerPhase::Motion => self.link_enabled = true,
                    PointerPhase::Wheel(_) => self.clear_link(),
                    _ => (),
                }
            }
            Event::ModifiersChanged(modifiers) => self.modifiers = modifiers,
            _ => (),
        }
    }
}

struct Checked {
    hit: CellPosition,
    // Keep the stamp anchored to the original lookup across soft wraps.
    origin: CellPosition,
    stamp: u64,
}

#[derive(Default)]
pub(super) struct Hover {
    checked: Option<Checked>,
    ranges: Vec<CellRange>,
    uri: Option<LinkUri>,
    control: bool,
}

impl Hover {
    pub(super) fn invalidate(&mut self) {
        self.checked = None;
    }

    pub(super) fn before_snapshot(&mut self, terminal: &Terminal) {
        if let Some(checked) = &self.checked
            && terminal.link_stamp(checked.origin) != (checked.stamp, false)
        {
            self.checked = None;
        }
    }

    fn clear(&mut self) -> bool {
        self.checked = None;
        self.ranges.clear();
        self.uri.take().is_some()
    }

    fn refresh(
        &mut self,
        terminal: &Terminal,
        hit: Option<CellPosition>,
        host: &str,
    ) -> Result<bool> {
        let was_link = self.uri.is_some();
        if let Some(hit) = hit {
            if let Some(checked) = &mut self.checked
                && (checked.hit == hit
                    || (matches!(self.ranges.as_slice(), [range]
                        if (range.start..=range.end).contains(&SelectionPoint::from(hit)))
                        && terminal.plain_link_cell(checked.origin)?
                        && terminal.plain_link_cell(hit)?))
            {
                checked.hit = hit;
                return Ok(false);
            }
            self.uri = terminal.link_at(hit, host, &mut self.ranges)?;
            self.checked = Some(Checked {
                hit,
                origin: hit,
                stamp: terminal.link_stamp(hit).0,
            });
        } else {
            return Ok(self.clear());
        }
        Ok(was_link || self.uri.is_some())
    }

    fn set_control(&mut self, control: bool) -> bool {
        std::mem::replace(&mut self.control, control) != control && self.uri.is_some()
    }

    pub(super) fn highlight(&self) -> LinkHighlight<'_> {
        LinkHighlight {
            ranges: &self.ranges,
            control: self.control,
        }
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

    pub(super) fn clear_link_hover(&mut self) {
        self.frame_dirty |= self.hover.clear();
    }

    pub(super) fn probe_link(&mut self, hit: Option<CellPosition>, host: &str) -> Result<()> {
        self.frame_dirty |= self.hover.refresh(&self.pane.terminal, hit, host)?;
        Ok(())
    }
}

impl Session {
    pub(super) fn update_link_hover(
        &mut self,
        window: &mut impl WindowOps,
        banner: bool,
        window_focused: bool,
    ) -> Result<()> {
        let target = self
            .link_pointer
            .position
            .and_then(|position| self.pane_under_pointer(position).map(|id| (id, position)));
        let tab = self.tabs.active_mut().expect("live window");
        let focused = tab.tree.focused();
        let modifiers = self.link_pointer.modifiers;
        let thumbnail = self
            .view
            .hover_thumbnail(target, |id| tab.get(id).can_hover_thumbnail(modifiers));
        let link_target =
            target.filter(|_| window_focused && self.link_pointer.link_enabled && !thumbnail);
        let mut link = false;
        for pane in &mut tab.panes {
            let active = pane.id == focused;
            let hit = link_target
                .filter(|(id, _)| {
                    *id == pane.id && pane.viewer.is_none() && !(active && pane.search.is_some())
                })
                .and_then(|(_, position)| {
                    self.view.link_cell(position, pane.rect, banner && active)
                });
            if hit.is_none() || !pane.snapshot_dirty {
                pane.probe_link(hit, &self.host)?;
            }
            pane.frame_dirty |= pane.hover.set_control(modifiers.control);
            link |= hit.is_some_and(|hit| {
                pane.hover.uri().is_some()
                    && pane
                        .hover
                        .checked
                        .as_ref()
                        .is_some_and(|checked| checked.hit == hit)
            });
        }
        window.set_pointer_icon(if thumbnail || (link && modifiers.control) {
            CursorIcon::Pointer
        } else if link {
            CursorIcon::Text
        } else {
            CursorIcon::Default
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_reuses_wrapped_url_but_not_over_an_osc8_label() {
        let mut terminal = Terminal::new(12, 3).unwrap();
        terminal.write(b"https://ex.io/a\x1b]8;;https://other.test\x1b\\b\x1b]8;;\x1b\\c");
        let mut hover = Hover::default();
        for (row, column, control, redraw, uri) in [
            (0, 0, false, true, "https://ex.io/abc"),
            (1, 1, false, false, "https://ex.io/abc"),
            (1, 3, false, true, "https://other.test"),
            (1, 3, true, true, "https://other.test"),
        ] {
            let hit = CellPosition { row, column };
            let moved = hover.refresh(&terminal, Some(hit), "").unwrap();
            assert_eq!(moved | hover.set_control(control), redraw);
            assert_eq!(hover.highlight().control, control);
            assert_eq!(hover.uri().unwrap().as_str(), uri);
        }
    }
}
