use std::fmt::Write as _;

use sumie_vt::{GridAnchor, Search, SearchMatch, Terminal};

use super::{Session, highlights::visible_spans};
use crate::{Result, input::SearchAction};

/// The last immutable row before a write. Its new coordinate measures how many rows Ghostty
/// actually pruned, even if the history limit hid growth in the scrollbar's total.
struct History {
    screen: i32,
    rows: u32,
    last: Option<GridAnchor>,
}

impl History {
    fn capture(vt: &Terminal, rows: u32) -> Result<Self> {
        Ok(Self {
            screen: vt.screen_id()?,
            rows,
            last: rows
                .checked_sub(1)
                .map(|y| vt.track_cell(y, 0))
                .transpose()?,
        })
    }
}

struct Scan {
    search: Search,
    history: History,
}

/// More than one substring can start in the same cell (e.g. repeated combining marks).
struct Selected {
    pin: GridAnchor,
    occurrence: usize,
}

pub(super) struct Bar {
    query: String,
    scan: Option<Scan>,
    complete: bool,
    selected: Option<Selected>,
    steps: isize,
    reveal: bool,
    opening_screen: i32,
    opening_top: Option<GridAnchor>,
}

impl Bar {
    fn open(vt: &Terminal) -> Result<Self> {
        Ok(Self {
            query: String::new(),
            scan: None,
            complete: true,
            selected: None,
            steps: 0,
            reveal: false,
            opening_screen: vt.screen_id()?,
            opening_top: if vt.at_bottom()? {
                None
            } else {
                Some(vt.track_cell(vt.viewport_top()?, 0)?)
            },
        })
    }

    pub(super) fn resized(&mut self) {
        self.scan = None;
        self.complete = self.query.is_empty();
        self.reveal = true;
    }

    fn edited(&mut self) {
        self.resized();
        self.selected = None;
        self.steps = 0;
    }

    pub(super) fn output(&mut self, vt: &Terminal) -> Result<()> {
        let Some(scan) = &mut self.scan else {
            return Ok(());
        };
        let old = &scan.history;
        if old.screen != vt.screen_id()? {
            self.edited();
            return Ok(());
        }
        let retained = if let Some(pin) = &old.last {
            pin.position()?.map_or(0, |(y, _)| y + 1).min(old.rows)
        } else {
            0
        };
        scan.search.refresh(vt, old.rows - retained, retained)?;
        let rows = vt.history_rows()?;
        if rows != old.rows || retained != old.rows {
            scan.history = History::capture(vt, rows)?;
        }
        self.complete = false;
        Ok(())
    }

    fn matches(&self) -> &[SearchMatch] {
        self.scan.as_ref().map_or(&[], |s| s.search.matches())
    }

    fn selected_index(&self, pos: (u32, u16)) -> usize {
        let hits = self.matches();
        let first = hits.partition_point(|hit| hit.start < pos);
        let occurrence = self.selected.as_ref().map_or(0, |s| s.occurrence);
        first
            + occurrence.min(
                hits[first..]
                    .partition_point(|m| m.start == pos)
                    .saturating_sub(1),
            )
    }

    fn position(&self) -> Result<Option<(u32, u16)>> {
        Ok(self
            .selected
            .as_ref()
            .map(|s| s.pin.position())
            .transpose()?
            .flatten())
    }

    fn advance(&mut self, vt: &Terminal) -> Result<bool> {
        let scan = match &mut self.scan {
            Some(scan) => scan,
            slot @ None => slot.insert(Scan {
                search: Search::new(&self.query),
                history: History::capture(vt, vt.history_rows()?)?,
            }),
        };
        let scan = &mut scan.search;
        let old_count = scan.matches().len();
        self.complete = scan.advance(vt)?;
        Ok(self.complete || scan.has_full_results() || scan.matches().len() != old_count)
    }

    fn select(&mut self, vt: &mut Terminal) -> Result<bool> {
        let matches = self.matches();
        if matches.is_empty()
            || !self
                .scan
                .as_ref()
                .is_some_and(|s| s.search.has_full_results())
        {
            return Ok(false);
        }
        let old = self.position()?;
        let index = if let Some(pos) = old {
            self.selected_index(pos).min(matches.len() - 1)
        } else if self.selected.is_some() {
            0
        } else {
            matches.len() - 1
        };
        let count = isize::try_from(matches.len())?;
        let index = (isize::try_from(index)? + self.steps.rem_euclid(count)) % count;
        let index = usize::try_from(index)?;
        let hit = matches[index];
        let occurrence = index - matches[..index].partition_point(|m| m.start < hit.start);
        if let Some(selected) = self.selected.as_mut().filter(|_| old == Some(hit.start)) {
            selected.occurrence = occurrence;
        } else {
            self.selected = Some(Selected {
                pin: vt.track_cell(hit.start.0, hit.start.1)?,
                occurrence,
            });
        }
        self.steps = 0;
        if std::mem::take(&mut self.reveal) {
            vt.reveal_search(hit.start.0, hit.end.0)?;
            return Ok(true);
        }
        Ok(false)
    }
}

impl Session {
    pub(super) fn search_pending(&self) -> bool {
        self.search.as_ref().is_some_and(|bar| !bar.complete)
    }

    pub(super) fn search_action(&mut self, action: SearchAction) -> Result<()> {
        match action {
            SearchAction::Open => {
                self.search = Some(Bar::open(&self.pane.terminal)?);
                self.view.preedit.clear();
                self.view.search_bar = Some(String::new());
                self.resize_grid()?;
            }
            SearchAction::Close => {
                let bar = self.search.take().expect("search bar is open");
                self.view.search_bar = None;
                self.view.search_highlights.clear();
                self.view.preedit.clear();
                self.resize_grid()?;
                let vt = &mut self.pane.terminal;
                if bar.opening_screen == vt.screen_id()? {
                    if let Some(pin) = bar.opening_top {
                        vt.restore_viewport(pin.position()?.map_or(0, |(y, _)| y))?;
                    } else {
                        vt.scroll_bottom()?;
                    }
                }
            }
            SearchAction::Insert(text) => {
                let bar = self.search.as_mut().expect("search bar is open");
                bar.query.extend(text.chars().filter(|ch| !ch.is_control()));
                bar.edited();
            }
            SearchAction::Backspace => {
                let bar = self.search.as_mut().expect("search bar is open");
                bar.query.pop();
                bar.edited();
            }
            SearchAction::Navigate { newer } => {
                let bar = self.search.as_mut().expect("search bar is open");
                bar.steps = bar.steps.wrapping_add(if newer { 1 } else { -1 });
                bar.reveal = true;
                self.select_match()?;
            }
        }
        self.frame_dirty = true;
        Ok(())
    }

    fn select_match(&mut self) -> Result<()> {
        if self
            .search
            .as_mut()
            .expect("search bar is open")
            .select(&mut self.pane.terminal)?
        {
            self.snapshot_dirty = true;
            self.selection = sumie_core::Selection::default();
        }
        Ok(())
    }

    pub(super) fn advance_search(&mut self) -> Result<()> {
        if let Some(bar) = self.search.as_mut().filter(|bar| !bar.complete)
            && bar.advance(&self.pane.terminal)?
        {
            self.frame_dirty = true;
            self.select_match()?;
        }
        Ok(())
    }

    pub(super) fn prepare_search_view(&mut self) -> Result<()> {
        let Some(bar) = &self.search else {
            return Ok(());
        };
        let size = self.view.size();
        let label = self.view.search_bar.as_mut().expect("search bar is open");
        let matches = bar.matches();
        let current = bar.position()?.and_then(|pos| {
            let i = bar.selected_index(pos);
            matches.get(i).filter(|m| m.start == pos).map(|_| i)
        });
        label.clear();
        write!(
            label,
            "Search: {}\t{}/{}",
            bar.query,
            current.map_or(0, |i| i + 1),
            matches.len()
        )?;
        if !bar.complete {
            label.push('…');
        }
        visible_spans(
            &mut self.view.search_highlights,
            matches,
            current.map(|i| matches[i]),
            self.pane.terminal.viewport_top()?,
            size.rows,
            size.cols,
        );
        Ok(())
    }
}
