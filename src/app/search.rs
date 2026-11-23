use std::fmt::Write as _;

use sumie_vt::{GridAnchor, Search, SearchMatch, Terminal};

use super::{PaneState, View, highlights::visible_spans};
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
    // None selects the entire query; otherwise this is a UTF-8 byte boundary.
    caret: Option<usize>,
    scan: Option<Scan>,
    complete: bool,
    selected: Option<Selected>,
    steps: isize,
    reveal: bool,
}

impl Bar {
    fn open() -> Self {
        Self {
            query: String::new(),
            caret: Some(0),
            scan: None,
            complete: true,
            selected: None,
            steps: 0,
            reveal: false,
        }
    }

    fn apply(search: &mut Option<Self>, action: SearchAction, vt: &mut Terminal) -> Result<bool> {
        let bar = match action {
            SearchAction::Open => {
                *search = Some(Self::open());
                return Ok(false);
            }
            SearchAction::Close => {
                *search = None;
                return Ok(false);
            }
            _ => search.as_mut().expect("search bar is open"),
        };
        match action {
            SearchAction::SelectAll => bar.caret = None,
            SearchAction::Home => bar.caret = Some(0),
            SearchAction::End => bar.caret = Some(bar.query.len()),
            SearchAction::Insert(mut text) => {
                text.retain(|ch| !ch.is_control());
                if !text.is_empty() {
                    if let Some(caret) = bar.caret {
                        bar.query.insert_str(caret, &text);
                        bar.caret = Some(caret + text.len());
                    } else {
                        bar.caret = Some(text.len());
                        bar.query = text;
                    }
                    bar.edited();
                }
            }
            SearchAction::Backspace { word } => {
                let end = bar.caret.unwrap_or(bar.query.len());
                let start = match bar.caret {
                    None => 0,
                    Some(_) if word => previous_word(&bar.query[..end]),
                    Some(_) => bar.query[..end]
                        .char_indices()
                        .next_back()
                        .map_or(0, |(i, _)| i),
                };
                bar.caret = Some(start);
                if start != end {
                    bar.query.replace_range(start..end, "");
                    bar.edited();
                }
            }
            SearchAction::Navigate { newer } => {
                bar.steps = bar.steps.wrapping_add(if newer { 1 } else { -1 });
                bar.reveal = true;
                return bar.select(vt);
            }
            SearchAction::Open | SearchAction::Close => unreachable!(),
        }
        Ok(false)
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

fn previous_word(text: &str) -> usize {
    let word = text.trim_end_matches(|ch: char| !ch.is_alphanumeric());
    word.trim_end_matches(char::is_alphanumeric).len()
}

impl PaneState {
    pub(super) fn search_pending(&self) -> bool {
        self.search.as_ref().is_some_and(|bar| !bar.complete)
    }

    pub(super) fn search_action(&mut self, action: SearchAction, view: &mut View) -> Result<()> {
        match &action {
            SearchAction::Open => {
                view.preedit.clear();
                view.search_bar = Some(String::new());
            }
            SearchAction::Close => {
                view.search_bar = None;
                view.search_highlights.clear();
                view.preedit.clear();
                self.clear_selection();
            }
            _ => (),
        }
        if Bar::apply(&mut self.search, action, &mut self.pane.terminal)? {
            self.snapshot_dirty = true;
            self.clear_selection();
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
            self.clear_selection();
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

    pub(super) fn prepare_search_view(&self, view: &mut View) -> Result<()> {
        const PREFIX: &str = "Search: ";
        let Some(bar) = &self.search else {
            return Ok(());
        };
        let size = view.pane_size(self.rect);
        let label = view.search_bar.as_mut().expect("search bar is open");
        let matches = bar.matches();
        let current = bar.position()?.and_then(|pos| {
            let i = bar.selected_index(pos);
            matches.get(i).filter(|m| m.start == pos).map(|_| i)
        });
        label.clear();
        let end = PREFIX.len() + bar.caret.unwrap_or(bar.query.len());
        let start = bar.caret.map_or(PREFIX.len(), |_| end);
        view.search_selection = start..end;
        write!(
            label,
            "{PREFIX}{}\t{}/{}",
            bar.query,
            current.map_or(0, |i| i + 1),
            matches.len()
        )?;
        if !bar.complete {
            label.push('…');
        }
        visible_spans(
            &mut view.search_highlights,
            matches,
            current.map(|i| matches[i]),
            self.pane.terminal.viewport_top()?,
            size.rows,
            size.cols,
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sumie_core::{KeyAction::Press, Pane, PaneCommand};
    use sumie_wayland::{Event, Modifiers};
    use xkeysym::key;

    use super::*;
    use crate::{
        input,
        pane::request_environment,
        sources,
        tests::{key_event, ready_pane},
    };

    #[test]
    fn closing_search_keeps_the_match_in_view() {
        let mut vt = Terminal::new(40, 5).unwrap();
        vt.write(b"needle\r\n");
        for _ in 0..20 {
            vt.write(b"filler\r\n");
        }
        let mut search = None;
        assert!(vt.at_bottom().unwrap());
        Bar::apply(&mut search, SearchAction::Open, &mut vt).unwrap();
        Bar::apply(&mut search, SearchAction::Insert("needle".into()), &mut vt).unwrap();
        let bar = search.as_mut().unwrap();
        while !bar.complete {
            bar.advance(&vt).unwrap();
        }
        Bar::apply(
            &mut search,
            SearchAction::Navigate { newer: false },
            &mut vt,
        )
        .unwrap();
        assert_eq!(vt.viewport_top().unwrap(), 0);

        Bar::apply(&mut search, SearchAction::Close, &mut vt).unwrap();
        assert_eq!(vt.viewport_top().unwrap(), 0);
        vt.write(b"more output\r\n");
        assert_eq!(vt.viewport_top().unwrap(), 0);
    }

    #[test]
    fn search_query_editing_never_writes_to_the_pty() {
        let mut pane = ready_pane("printf 'READY\\n'; exec cat", &request_environment());
        let mut search = Some(Bar::open());
        let mut routing = input::Routing::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let chord = |physical, symbol| key_event(physical, symbol, Press, ctrl);
        let plain = |physical, symbol| key_event(physical, symbol, Press, Modifiers::default());
        let commit = |text: &str| Event::ImeCommit {
            text: text.into(),
            generation: 0,
        };
        for (event, expected) in [
            (commit("replace-me"), "replace-me"),
            (chord(30, key::a), "replace-me"),
            (plain(19, key::r), "r"),
            (plain(102, key::Home), "r"),
            (commit("水"), "水r"),
            (plain(107, key::End), "水r"),
            (commit(" word"), "水r word"),
            (chord(14, key::BackSpace), "水r "),
            (chord(30, key::a), "水r "),
            (plain(14, key::BackSpace), ""),
            (chord(47, key::v), "pasted"),
        ] {
            let mut route =
                |event| input::route((0, 0), &mut pane, &mut routing, true, event, false).unwrap();
            let input::Effect::Search(action) = (match route(event) {
                input::Effect::ReadClipboard => route(Event::ClipboardText {
                    text: "pasted".into(),
                    generation: 0,
                }),
                effect => effect,
            }) else {
                panic!("search did not consume the edit");
            };
            Bar::apply(&mut search, action, &mut pane.terminal).unwrap();
            assert_eq!(search.as_ref().unwrap().query, expected);
            assert!(pane.pty.input_fd().is_none());
        }
        pane.command(PaneCommand::Close).unwrap();
        sources::reap(&mut pane.pty).unwrap();
    }
}
