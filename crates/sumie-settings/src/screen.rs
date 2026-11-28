use std::{borrow::Cow, fmt::Write as _, ops::Range};

use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

use crate::{
    input::{Key, Line},
    model::{Mode, Model},
};

const SIDE_BY_SIDE: usize = 72;
const HELP_ROWS: usize = 3;

pub(crate) struct Layout {
    list: Range<usize>,
    help: Range<usize>,
    side_by_side: bool,
    notice: Option<usize>,
    keys: Option<usize>,
    prompt: Option<usize>,
}

impl Layout {
    pub(crate) fn new(model: &Model, width: usize, height: usize) -> Self {
        let mut bottom = height.saturating_sub(1);
        let mut footer = || {
            if bottom == 0 {
                return None;
            }
            let row = bottom;
            bottom -= 1;
            Some(row)
        };
        let prompt = model.mode.line().and_then(|_| footer());
        let notice = model.notice().and_then(|_| footer());
        let keys = footer();
        let end = bottom + 1;
        let side_by_side = width >= SIDE_BY_SIDE;
        let help_height = if side_by_side {
            0
        } else {
            HELP_ROWS.min(end.saturating_sub(2))
        };
        let list = 1..end - help_height;
        let help = if side_by_side {
            list.clone()
        } else {
            list.end..end
        };
        Self {
            list,
            help,
            side_by_side,
            notice,
            keys,
            prompt,
        }
    }

    pub(crate) fn page(&self) -> usize {
        self.list.len().max(1)
    }

    pub(crate) fn mouse(&self, model: &mut Model, width: usize, key: Key) -> bool {
        let list_width = if self.side_by_side {
            width / 2 - 2
        } else {
            width
        };
        match key {
            Key::Click { column, row: 0 } if width != 0 => {
                let (first, visible, tab_width) = tab_columns(model, width);
                let offset = column / tab_width;
                if offset < visible {
                    model.click_tab(first + offset);
                }
            }
            Key::Click { column, row } => {
                if self.list.contains(&row) && column < list_width {
                    let selected = model.first_row(self.page()) + row - self.list.start;
                    if selected < model.visible().len() {
                        model.click_option(selected);
                    }
                }
            }
            _ => return false,
        }
        true
    }
}

/// Human-readable rows in terminal cells. Output escapes are added only by the terminal adapter.
#[derive(Default)]
pub(crate) struct Screen {
    pub(crate) rows: Vec<String>,
    pub(crate) caret: Option<(usize, usize)>,
}

impl Screen {
    pub(crate) fn render(&mut self, model: &Model, width: usize, height: usize, layout: &Layout) {
        self.rows.resize_with(height, String::new);
        self.rows.iter_mut().for_each(String::clear);
        self.caret = None;
        if width == 0 || height == 0 {
            return;
        }
        self.tabs(model, width);
        self.options(model, width, layout);
        self.help(model, width, layout);
        if let (Some(row), Some(message)) = (layout.notice, model.notice()) {
            slot(&mut self.rows[row], &message, width, false);
        }
        let (keys, prompt) = match &model.mode {
            Mode::Browse => ("ui.keys", ""),
            Mode::Search { .. } => ("ui.search-keys", model.locale.text("ui.search")),
            Mode::Record => ("ui.record-keys", ""),
            Mode::Edit(_) => (
                "ui.edit-keys",
                model
                    .current()
                    .map_or("", |index| model.options[index].label(model.locale)),
            ),
        };
        if let Some(row) = layout.keys {
            slot(&mut self.rows[row], model.locale.text(keys), width, false);
        }
        if let (Some(row), Some(line)) = (layout.prompt, model.mode.line()) {
            let col = prompt_row(&mut self.rows[row], prompt, line, width);
            self.caret = Some((row, col));
        }
    }

    fn tabs(&mut self, model: &Model, width: usize) {
        let (first, visible, tab_width) = tab_columns(model, width);
        let row = &mut self.rows[0];
        for (index, tab) in model.tabs.iter().enumerate().skip(first).take(visible) {
            let label = model.locale.text(&tab.label);
            let title = if index == model.tab {
                format!("{} [{label}]", tab.digit)
            } else {
                format!("{} {label}", tab.digit)
            };
            let divided = index + 1 != first + visible;
            slot(row, &title, tab_width - usize::from(divided), divided);
            if divided {
                row.push(' ');
            }
        }
    }

    fn options(&mut self, model: &Model, width: usize, layout: &Layout) {
        let width = if layout.side_by_side {
            width / 2 - 2
        } else {
            width
        };
        let page = layout.page();
        let first = model.first_row(page);
        let visible = model.visible();
        if visible.is_empty() {
            if let Some(row) = layout.list.clone().next() {
                slot(
                    &mut self.rows[row],
                    model.locale.text("ui.no-results"),
                    width,
                    false,
                );
            }
            return;
        }
        for (offset, (row, &index)) in layout
            .list
            .clone()
            .zip(visible.iter().skip(first))
            .enumerate()
        {
            let option = &model.options[index];
            let value = option.display(model.locale, model.value(option));
            let mark = if model.changed(option) { '*' } else { ' ' };
            let pointer = if model.selected == first + offset {
                '>'
            } else {
                ' '
            };
            let title_width = 3.max(width * 3 / 5).min(width);
            let title = format!("{pointer}{mark} {}", option.label(model.locale));
            let row = &mut self.rows[row];
            slot(row, &title, title_width, title_width < width);
            slot(row, &value, width - title_width, false);
        }
    }

    fn help(&mut self, model: &Model, width: usize, layout: &Layout) {
        let Some(index) = model.current() else { return };
        let option = &model.options[index];
        let locale = model.locale;
        let default = option.display(locale, model.default(option));
        let default = format!("{}: {default}", locale.text("ui.default"));
        let allowed = option.allowed(locale);
        let allowed = format!("{}: {allowed}", locale.text("ui.allowed"));
        let start = if layout.side_by_side {
            width / 2 + 1
        } else {
            0
        };
        for (row, text) in layout
            .help
            .clone()
            .zip([option.help(locale), &default, &allowed])
        {
            let row = &mut self.rows[row];
            let fill = start.saturating_sub(row.width());
            row.extend(std::iter::repeat_n(' ', fill));
            slot(row, text, width - start, false);
        }
    }
}

fn tab_columns(model: &Model, width: usize) -> (usize, usize, usize) {
    // Keep the active tab visible at every width. The digit stays before the label.
    let visible = model.tabs.len().min((width / 6).max(1));
    let first = model.tab.saturating_add(1).saturating_sub(visible);
    (first, visible, width / visible)
}

fn prompt_row(row: &mut String, label: &str, line: &Line, width: usize) -> usize {
    let label_width = label.width().min(width / 3);
    let label_width = slot(row, label, label_width, false);
    if label_width != 0 {
        row.push_str(": ");
    }
    let before = &line.text[..line.cursor];
    let after = &line.text[line.cursor..];
    let start = row.width();
    // Reserve a real cell for the cursor so the host terminal can anchor its IME there.
    let available = width.saturating_sub(start + 1);
    let before = clean(before);
    let left = if before.width() > available && available > 0 {
        row.push('…');
        let mut cells = 0;
        let suffix = before
            .graphemes(true)
            .rev()
            .take_while(|part| {
                cells += part.width();
                cells < available
            })
            .map(str::len)
            .sum::<usize>();
        row.push_str(&before[before.len() - suffix..]);
        row.width() - start
    } else {
        slot(row, &before, available, false)
    };
    slot(row, after, available - left, false);
    (start + left).min(width - 1)
}

/// No text slot wraps. Strip terminal controls before measuring and reserve a cell for ….
fn slot(row: &mut String, text: &str, width: usize, pad: bool) -> usize {
    if width == 0 {
        return 0;
    }
    let text = clean(text);
    let cells = text.width();
    let used = if cells <= width {
        row.push_str(&text);
        cells
    } else {
        let mut used = 0;
        for part in text.graphemes(true) {
            let next = used + part.width();
            if next >= width {
                break;
            }
            row.push_str(part);
            used = next;
        }
        row.push('…');
        used + 1
    };
    if pad {
        row.extend(std::iter::repeat_n(' ', width - used));
    }
    used
}

fn clean(text: &str) -> Cow<'_, str> {
    if text.chars().any(char::is_control) {
        text.chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect::<String>()
            .into()
    } else {
        text.into()
    }
}

pub(crate) fn frame(output: &mut String, screen: &Screen) {
    output.clear();
    output.push_str("\x1b[?25l\x1b[H");
    for (index, row) in screen.rows.iter().enumerate() {
        if index != 0 {
            output.push_str("\r\n");
        }
        output.push_str("\x1b[K");
        output.push_str(row);
    }
    if let Some((row, column)) = screen.caret {
        let _ = write!(output, "\x1b[{};{}H\x1b[?25h", row + 1, column + 1);
    }
}
