use std::path::PathBuf;

use sumie_config::{Category, EditValues, Value, WriteError, load, save, settings};
use sumie_locale::Locale;

use crate::{
    input::{Key, Line},
    log::Log,
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
}

impl Mode {
    pub(crate) fn line(&self) -> Option<&Line> {
        match self {
            Self::Browse => None,
            Self::Edit(line) | Self::Search { line, .. } => Some(line),
        }
    }
}

pub(crate) struct Model {
    main: PathBuf,
    values: EditValues,
    invalid_config: bool,
    pub(crate) log: Log,
    pub(crate) locale: Locale,
    pub(crate) options: Vec<OptionInfo>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) tab: usize,
    pub(crate) selected: usize,
    pub(crate) mode: Mode,
    matches: Vec<usize>,
    error: Option<&'static str>,
}

impl Model {
    pub(crate) fn new(main: PathBuf, log: Log) -> Self {
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
            log,
            locale: Locale::En,
            options,
            tabs,
            tab: 0,
            selected: 0,
            mode: Mode::Browse,
            matches: Vec::new(),
            error: None,
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
        self.invalid_config = load(&self.main)
            .inspect_err(|error| self.log.error(error))
            .is_err();
    }

    pub(crate) fn notice(&self) -> Option<&'static str> {
        self.error
            .or(self.invalid_config.then_some("ui.config-error"))
            .map(|key| self.locale.text(key))
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

    /// Return false when the session should exit.
    pub(crate) fn key(&mut self, key: Key, page: usize) -> bool {
        if key == Key::Interrupt {
            return false;
        }
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
            Key::Left | Key::Char('h') => self.step(false),
            Key::Right | Key::Char('l') => self.step(true),
            Key::Enter => {
                let option = &self.options[self.visible()[self.selected]];
                if let Some(text) = option.edit_text(self.value(option)) {
                    self.mode = Mode::Edit(Line::new(text));
                    self.error = None;
                }
            }
            Key::Char('j') => self.navigate(Key::Down, page),
            Key::Char('k') => self.navigate(Key::Up, page),
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
        if let Some(value) = option
            .step(current, right)
            .filter(|value| !same(Some(value), current))
        {
            self.write(index, &value);
        }
    }

    fn reset(&mut self) {
        let index = self.visible()[self.selected];
        let option = &self.options[index];
        if self.values.get(&option.info.path).is_none() {
            return;
        }
        if let Some(value) = option
            .default
            .clone()
            .filter(|_| option.info.default_from.is_none())
        {
            self.write(index, &value);
        } else {
            // Removing only the winner would expose a lower-priority override instead of the default.
            let key = &option.info.path;
            let result = self
                .values
                .definitions(key)
                .try_for_each(|source| save(&self.main, source, key, None).map(|_| ()));
            self.reload();
            self.error = result.err().map(|error| self.write_error(&error));
        }
    }

    fn submit(&mut self) {
        let Mode::Edit(line) = &self.mode else { return };
        let index = self.visible()[self.selected];
        let option = &self.options[index];
        if let Some(value) = option.parse(&line.text) {
            if (!self.invalid_config && same(self.value(option), Some(&value)))
                || self.write(index, &value)
            {
                self.mode = Mode::Browse;
                self.error = None;
            }
        } else {
            self.error = Some("ui.rejected");
        }
    }

    fn write(&mut self, index: usize, value: &Value) -> bool {
        self.error = None;
        let option = &self.options[index];
        if !self.invalid_config && same(self.values.get(&option.info.path), Some(value)) {
            return true;
        }
        let Some(source) = self.values.source(&option.info.path) else {
            self.error = Some("ui.config-blocked");
            return false;
        };
        match save(&self.main, source, &option.info.path, Some(value)) {
            Ok(_) => {
                self.reload();
                true
            }
            Err(error) => {
                self.error = Some(self.write_error(&error));
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

    fn write_error(&self, error: &WriteError) -> &'static str {
        self.log.error(error);
        match error {
            WriteError::Conflict { .. } => "ui.save-conflict",
            WriteError::Unconfirmed { .. } => "ui.unconfirmed",
            WriteError::Invalid(_) if self.invalid_config => "ui.config-blocked",
            WriteError::Invalid(_) => "ui.rejected",
            _ => "ui.save-error",
        }
    }
}
