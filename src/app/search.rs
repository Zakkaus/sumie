use std::fmt::Write as _;

use sumie_vt::Search;

use super::Session;
use crate::{Result, input::SearchAction};

#[derive(Default)]
pub(super) struct Bar {
    query: String,
    scan: Option<Search>,
    complete: bool,
    // Distance from the newest match; navigation can arrive before the scan finishes.
    offset: isize,
    pub(super) restore_top: Option<u32>,
}

impl Bar {
    pub(super) fn restart(&mut self) {
        self.scan = None;
        self.complete = self.query.is_empty();
        self.offset = 0;
    }

    fn index(&self, count: usize) -> usize {
        let count = isize::try_from(count).expect("matches fit in an allocation");
        usize::try_from(count - 1 - self.offset.rem_euclid(count)).unwrap()
    }
}

impl Session {
    pub(super) fn search_pending(&self) -> bool {
        self.search.as_ref().is_some_and(|bar| !bar.complete)
    }

    pub(super) fn search_action(&mut self, action: SearchAction) -> Result<()> {
        if matches!(action, SearchAction::Open) {
            self.search = Some(Bar {
                restore_top: if self.pane.terminal.at_bottom()? {
                    None
                } else {
                    Some(self.pane.terminal.viewport_top()?)
                },
                ..Bar::default()
            });
            self.view.preedit.clear();
            self.view.search_bar = Some(String::new());
            return self.resize_grid();
        }
        let Some(bar) = &mut self.search else {
            return Ok(());
        };
        match action {
            SearchAction::Open => unreachable!(),
            SearchAction::Close => {
                let restore_top = bar.restore_top;
                self.search = None;
                self.view.search_bar = None;
                self.view.search_highlights.clear();
                self.view.preedit.clear();
                self.resize_grid()?;
                if let Some(top) = restore_top {
                    let delta = i64::from(top) - i64::from(self.pane.terminal.viewport_top()?);
                    self.pane.terminal.scroll_rows(i32::try_from(delta)?)?;
                } else {
                    self.pane.terminal.scroll_bottom()?;
                }
            }
            SearchAction::Insert(text) => {
                bar.query.extend(text.chars().filter(|ch| !ch.is_control()));
                bar.restart();
            }
            SearchAction::Backspace => {
                bar.query.pop();
                bar.restart();
            }
            SearchAction::Navigate { newer } => {
                bar.offset = bar.offset.wrapping_add(if newer { -1 } else { 1 });
                self.reveal_match()?;
            }
        }
        self.frame_dirty = true;
        Ok(())
    }

    pub(super) fn advance_search(&mut self) -> Result<()> {
        let Some(bar) = self.search.as_mut().filter(|bar| !bar.complete) else {
            return Ok(());
        };
        let scan = bar.scan.get_or_insert_with(|| Search::new(&bar.query));
        let old_count = scan.matches().len();
        bar.complete = scan.advance(&self.pane.terminal)?;
        self.frame_dirty |= bar.complete || scan.matches().len() != old_count;
        if bar.complete {
            self.reveal_match()?;
        }
        Ok(())
    }

    fn reveal_match(&mut self) -> Result<()> {
        let Some(bar) = &self.search else {
            return Ok(());
        };
        if let Some(matches) = bar.scan.as_ref().map(Search::matches)
            && bar.complete
            && !matches.is_empty()
        {
            self.pane
                .terminal
                .reveal_search(matches[bar.index(matches.len())].start.0)?;
            self.snapshot_dirty = true;
            self.selection = sumie_core::Selection::default();
        }
        Ok(())
    }

    pub(super) fn prepare_search_view(&mut self) -> Result<()> {
        let Some(bar) = &self.search else {
            return Ok(());
        };
        let size = self.view.size();
        let label = self.view.search_bar.as_mut().expect("search bar is open");
        let matches = bar.scan.as_ref().map_or(&[][..], Search::matches);
        let current = (bar.complete && !matches.is_empty()).then(|| bar.index(matches.len()));
        label.clear();
        write!(
            label,
            "Search: {}\t{}/{}",
            bar.query,
            current.map_or(0, |i| matches.len() - i),
            matches.len()
        )?;
        if !bar.complete {
            label.push('…');
        }
        let spans = &mut self.view.search_highlights;
        spans.clear();
        let top = self.pane.terminal.viewport_top()?;
        let bottom = top.saturating_add(u32::from(size.rows) - 1);
        let first = matches.partition_point(|m| m.end.0 < top);
        for (i, m) in matches[first..]
            .iter()
            .take_while(|m| m.start.0 <= bottom)
            .enumerate()
        {
            for row in m.start.0.max(top)..=m.end.0.min(bottom) {
                let left = if row == m.start.0 { m.start.1 } else { 0 };
                let right = if row == m.end.0 {
                    m.end.1
                } else {
                    size.cols - 1
                };
                spans.push((
                    u16::try_from(row - top).unwrap(),
                    left,
                    right,
                    Some(first + i) == current,
                ));
            }
        }
        Ok(())
    }
}
