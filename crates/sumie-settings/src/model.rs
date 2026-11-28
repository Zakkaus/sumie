use std::{borrow::Cow, path::PathBuf};

use sumie_config::{
    Category, EditValues, Error, Kind, Problem, Value, WriteError, load, save, settings,
};
use sumie_core::{Chord, Platform, binding_conflict};
use sumie_locale::Locale;

use crate::{
    input::{Key, Line},
    value::{OptionInfo, same},
};

pub(crate) struct Tab {
    pub(crate) digit: usize,
    pub(crate) label: String,
    options: Vec<usize>,
}

#[derive(Default)]
pub(crate) enum Mode {
    #[default]
    Browse,
    Search {
        line: Line,
        prior: usize,
    },
    Edit(Line),
    Record,
}

impl Mode {
    pub(crate) fn line(&self) -> Option<&Line> {
        match self {
            Self::Browse | Self::Record => None,
            Self::Edit(line) | Self::Search { line, .. } => Some(line),
        }
    }
}

enum Notice {
    Message(&'static str),
    Allowed(usize),
    BindingConflict { chord: Chord, option: usize },
}

pub(crate) struct Model {
    main: PathBuf,
    values: EditValues,
    invalid_config: bool,
    pub(crate) locale: Locale,
    pub(crate) options: Vec<OptionInfo>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) tab: usize,
    pub(crate) selected: usize,
    pub(crate) mode: Mode,
    matches: Vec<usize>,
    clicked: Option<usize>,
    error: Option<Notice>,
    vim_keys: bool,
}

impl Model {
    pub(crate) fn new(main: PathBuf) -> Self {
        let options: Vec<_> = settings().into_iter().map(OptionInfo::new).collect();
        let tabs = Category::ALL
            .into_iter()
            .enumerate()
            .filter_map(|(index, category)| {
                let options: Vec<_> = options
                    .iter()
                    .enumerate()
                    .filter_map(|(i, option)| (option.info.category == category).then_some(i))
                    .collect();
                (!options.is_empty()).then(|| Tab {
                    label: format!("tab.{}", category.id()),
                    digit: index + 1,
                    options,
                })
            })
            .collect();
        let mut model = Self {
            main,
            values: EditValues::default(),
            invalid_config: false,
            locale: Locale::En,
            options,
            tabs,
            tab: 0,
            selected: 0,
            mode: Mode::Browse,
            matches: Vec::new(),
            clicked: None,
            error: None,
            vim_keys: false,
        };
        model.reload();
        model
    }

    fn reload(&mut self) {
        self.values = EditValues::read(&self.main);
        let language = self
            .values
            .get("language")
            .cloned()
            .and_then(|value| value.try_into().ok())
            .unwrap_or_default();
        self.locale = Locale::from_language(language);
        self.invalid_config = load(&self.main).is_err();
        let vim_keys = self
            .options
            .iter()
            .find(|option| option.info.path == "settings-vim-keys")
            .expect("Vim key setting is in the schema");
        self.vim_keys = self
            .value(vim_keys)
            .and_then(Value::as_bool)
            .or_else(|| vim_keys.default.as_ref().and_then(Value::as_bool))
            .expect("Vim key setting has a boolean default");
    }

    pub(crate) fn notice(&self) -> Option<Cow<'static, str>> {
        let locale = self.locale;
        match self.error {
            Some(Notice::Message(key)) => Some(locale.text(key).into()),
            Some(Notice::Allowed(index)) => Some(
                locale
                    .text("ui.expected")
                    .replace("{value}", &self.options[index].allowed(locale))
                    .into(),
            ),
            Some(Notice::BindingConflict { chord, option }) => Some(
                locale
                    .text("ui.binding-conflict")
                    .replace("{chord}", &chord.display(Platform::current()))
                    .replace("{action}", self.options[option].label(locale))
                    .into(),
            ),
            None => self
                .invalid_config
                .then(|| locale.text("ui.config-error").into()),
        }
    }

    pub(crate) fn visible(&self) -> &[usize] {
        if matches!(self.mode, Mode::Search { .. }) {
            &self.matches
        } else {
            &self.tabs[self.tab].options
        }
    }

    pub(crate) fn current(&self) -> Option<usize> {
        self.visible().get(self.selected).copied()
    }

    pub(crate) fn default<'a>(&'a self, option: &'a OptionInfo) -> Option<&'a Value> {
        option
            .info
            .default_from
            .as_deref()
            .and_then(|key| self.values.get(key))
            .or(option.default.as_ref())
    }

    pub(crate) fn value<'a>(&'a self, option: &'a OptionInfo) -> Option<&'a Value> {
        self.values
            .get(&option.info.path)
            .or_else(|| self.default(option))
    }

    pub(crate) fn changed(&self, option: &OptionInfo) -> bool {
        !same(self.value(option), self.default(option))
    }

    /// The list scrolls a page at a time, so the first row follows from the selection.
    pub(crate) fn first_row(&self, page: usize) -> usize {
        self.selected - self.selected % page
    }

    pub(crate) fn click_tab(&mut self, tab: usize) {
        if matches!(self.mode, Mode::Browse) {
            self.clicked = None;
            self.switch(tab);
        }
    }

    pub(crate) fn click_option(&mut self, selected: usize) {
        if !matches!(self.mode, Mode::Browse | Mode::Search { .. }) {
            return;
        }
        let second = self.clicked.replace(selected) == Some(selected) && self.selected == selected;
        self.selected = selected;
        if second {
            self.clicked = None;
            if matches!(self.mode, Mode::Search { .. }) {
                self.accept_match();
            }
            let option = &self.options[self.visible()[self.selected]];
            if matches!(option.info.kind, Kind::Bool | Kind::Choice(_)) {
                self.step(true);
            } else {
                self.begin_edit();
            }
        }
    }

    fn begin_edit(&mut self) {
        let option = &self.options[self.visible()[self.selected]];
        self.mode = if matches!(option.info.kind, Kind::Binding(_)) {
            Mode::Record
        } else if let Some(text) = option.edit_text(self.value(option)) {
            Mode::Edit(Line::new(text))
        } else {
            return;
        };
        self.error = None;
    }

    fn record(&mut self, chord: Chord) {
        let index = self.visible()[self.selected];
        let Kind::Binding(target) = self.options[index].info.kind else {
            unreachable!("recording requires a binding");
        };
        let find =
            |id| {
                self.options.iter().position(|option| {
            matches!(option.info.kind, Kind::Binding(binding) if binding.id == id)
        }).expect("every binding has an option")
            };
        if let Some(other) = binding_conflict(Platform::current(), chord, target, |binding| {
            self.values
                .get(&self.options[find(binding.id)].info.path)
                .and_then(Value::as_str)
                .and_then(Chord::parse)
        }) {
            self.error = Some(Notice::BindingConflict {
                chord,
                option: find(other.id),
            });
        } else if self.write(index, &Value::String(chord.to_string())) {
            self.mode = Mode::Browse;
        }
    }

    /// Return false when the session should exit.
    pub(crate) fn key(&mut self, key: Key, page: usize) -> bool {
        self.clicked = None;
        match &mut self.mode {
            Mode::Edit(line) => match key {
                Key::Esc => {
                    self.mode = Mode::Browse;
                    self.error = None;
                }
                Key::Enter => self.submit(),
                _ => {
                    if line.key(key) {
                        self.error = None;
                    }
                }
            },
            Mode::Search { line, prior } => match key {
                Key::Esc => {
                    self.selected = *prior;
                    self.mode = Mode::Browse;
                }
                Key::Enter => self.accept_match(),
                _ => {
                    if line.key(key) {
                        self.update_matches();
                    } else {
                        self.navigate(key, page);
                    }
                }
            },
            Mode::Record => match key {
                Key::Esc => {
                    self.mode = Mode::Browse;
                    self.error = None;
                }
                Key::Chord(chord) => self.record(chord),
                _ => self.error = Some(Notice::Message("ui.unbindable")),
            },
            Mode::Browse => return self.browse(key, page),
        }
        true
    }

    fn browse(&mut self, key: Key, page: usize) -> bool {
        match key {
            Key::Esc | Key::Char('q') => return false,
            Key::Tab => self.switch((self.tab + 1) % self.tabs.len()),
            Key::Char(ch @ '1'..='7') => {
                let digit = "01234567".find(ch).expect("digit from match");
                if let Some(tab) = self.tabs.iter().position(|tab| tab.digit == digit) {
                    self.switch(tab);
                }
            }
            Key::Char('/') => {
                self.mode = Mode::Search {
                    line: Line::default(),
                    prior: self.selected,
                };
                self.update_matches();
            }
            Key::Char('r') => self.reset(),
            Key::Left => self.step(false),
            Key::Right => self.step(true),
            Key::Char('h') if self.vim_keys => self.step(false),
            Key::Char('l') if self.vim_keys => self.step(true),
            Key::Enter => self.begin_edit(),
            Key::Char('j') if self.vim_keys => self.navigate(Key::Down, page),
            Key::Char('k') if self.vim_keys => self.navigate(Key::Up, page),
            _ => self.navigate(key, page),
        }
        true
    }

    fn switch(&mut self, tab: usize) {
        if self.tab != tab {
            self.tab = tab;
            self.selected = 0;
        }
    }

    fn navigate(&mut self, key: Key, page: usize) {
        self.selected = match key {
            Key::Up => self.selected.saturating_sub(1),
            Key::PageUp => self.selected.saturating_sub(page),
            Key::Down => self.selected + 1,
            Key::PageDown => self.selected + page,
            Key::Home => 0,
            Key::End => self.visible().len(),
            _ => self.selected,
        }
        .min(self.visible().len().saturating_sub(1));
    }

    fn update_matches(&mut self) {
        let Mode::Search { line, .. } = &self.mode else {
            return;
        };
        let query = line.text.trim().to_lowercase();
        self.matches.clear();
        self.matches.extend(
            self.options
                .iter()
                .enumerate()
                .filter_map(|(index, option)| {
                    (query.is_empty()
                        || option.label(self.locale).to_lowercase().contains(&query)
                        || option.help(self.locale).to_lowercase().contains(&query))
                    .then_some(index)
                }),
        );
        self.selected = 0;
    }

    fn accept_match(&mut self) {
        let Some(index) = self.current() else { return };
        if let Some((tab, selected)) = self.tabs.iter().enumerate().find_map(|(tab, entry)| {
            entry
                .options
                .iter()
                .position(|&option| option == index)
                .map(|selected| (tab, selected))
        }) {
            self.tab = tab;
            self.selected = selected;
            self.mode = Mode::Browse;
        }
    }

    fn step(&mut self, right: bool) {
        let index = self.visible()[self.selected];
        let option = &self.options[index];
        let current = self.value(option);
        if let Some(value) = option.step(current, right) {
            self.write(index, &value);
        }
    }

    fn reset(&mut self) {
        let index = self.visible()[self.selected];
        let option = &self.options[index];
        let key = &option.info.path;
        if self.values.get(key).is_some()
            && let Some(source) = self.values.source(key)
        {
            let result = save(&self.main, source, key, None);
            self.reload();
            self.error = result.err().map(|error| self.write_error(index, &error));
        }
    }

    fn submit(&mut self) {
        let Mode::Edit(line) = &self.mode else { return };
        let index = self.visible()[self.selected];
        let option = &self.options[index];
        if let Some(value) = option.parse(&line.text) {
            if self.write(index, &value) {
                self.mode = Mode::Browse;
            }
        } else {
            self.error = Some(Notice::Allowed(index));
        }
    }

    fn write(&mut self, index: usize, value: &Value) -> bool {
        self.error = None;
        let option = &self.options[index];
        if !self.invalid_config && same(self.value(option), Some(value)) {
            return true;
        }
        let Some(source) = self.values.source(&option.info.path) else {
            self.error = Some(Notice::Message("ui.config-blocked"));
            return false;
        };
        match save(&self.main, source, &option.info.path, Some(value)) {
            Ok(_) => {
                self.reload();
                true
            }
            Err(error) => {
                self.error = Some(self.write_error(index, &error));
                if matches!(
                    error,
                    WriteError::Conflict { .. } | WriteError::Unconfirmed { .. }
                ) {
                    self.reload();
                    self.mode = Mode::Browse;
                }
                false
            }
        }
    }

    fn write_error(&self, index: usize, error: &WriteError) -> Notice {
        let key = match error {
            WriteError::Conflict { .. } => "ui.save-conflict",
            WriteError::Unconfirmed { .. } => "ui.unconfirmed",
            WriteError::Invalid(problems)
                if problems.iter().any(|error| {
                    let Error::Invalid { message, .. } = error else {
                        return false;
                    };
                    matches!(message, Problem::Range { .. } | Problem::FontSize)
                        && message.key() == Some(self.options[index].info.path.as_str())
                }) =>
            {
                return Notice::Allowed(index);
            }
            WriteError::Invalid(_) if self.invalid_config => "ui.config-blocked",
            WriteError::Invalid(_) => "ui.rejected",
            _ => "ui.save-error",
        };
        Notice::Message(key)
    }
}
