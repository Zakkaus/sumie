//! `sumie config import ghostty`: copy the settings sumie has from a Ghostty file into the sumie
//! config. A key the sumie config already sets is left alone.

mod ghostty;
mod keybinds;

use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use sumie_config::{Config, EditValues, Value, WriteError, format_list, save, user_config_path};
use sumie_locale::Locale;

use crate::Result;
use ghostty::Target;

const USAGE: &str = "usage: sumie config import ghostty [PATH]";

pub(crate) fn run(mut args: impl Iterator<Item = OsString>) -> Result<()> {
    if args.next().is_none_or(|source| source != "ghostty") {
        return Err(USAGE.into());
    }
    let source = args.next().map(PathBuf::from);
    if args.next().is_some() {
        return Err(USAGE.into());
    }
    let destination = user_config_path().ok_or_else(|| {
        Locale::from_language(sumie_config::Language::default()).text("ui.no-config-path")
    })?;
    import(source, &destination, io::stdout().lock())
}

fn import(source: Option<PathBuf>, destination: &Path, mut out: impl io::Write) -> Result<()> {
    let config = sumie_config::load(destination)?;
    let locale = Locale::from_language(config.language());
    let (path, text) = source
        .map_or_else(default_sources, |path| vec![path])
        .into_iter()
        .find_map(|path| fs::read_to_string(&path).ok().map(|text| (path, text)))
        .ok_or_else(|| locale.text("import.no-source"))?;
    let configured = EditValues::read(destination);
    let skipped = |out: &mut dyn io::Write, line, key: &str| {
        let (path, label) = (path.display(), locale.text("import.skipped"));
        writeln!(out, "{path}:{line}: {key} ({label})")
    };
    let mut seen = BTreeSet::new();
    let mut palette: Vec<Value> = Config::default()
        .colors
        .palette()
        .into_iter()
        .map(|[r, g, b]| Value::String(format!("#{r:02x}{g:02x}{b:02x}")))
        .collect();
    let mut palette_seen = BTreeSet::new();
    for setting in ghostty::parse(&text) {
        match setting.value {
            Some((Target::Key(target), value))
                if configured.get(&target).is_none() && seen.insert(target.clone()) =>
            {
                let snapshot = configured.source(&target).ok_or("no settings snapshot")?;
                match save(destination, snapshot, &target, Some(&value)) {
                    Ok(_) => writeln!(out, "{target} = {}", format_list(&value))?,
                    Err(WriteError::Invalid(_)) => skipped(&mut out, setting.line, &setting.key)?,
                    Err(error) => return Err(error.into()),
                }
            }
            Some((Target::Palette(index), value))
                if configured.get("colors.palette").is_none() && palette_seen.insert(index) =>
            {
                palette[index] = value;
            }
            _ => skipped(&mut out, setting.line, &setting.key)?,
        }
    }
    if !palette_seen.is_empty() {
        let value = Value::Array(palette);
        let snapshot = configured
            .source("colors.palette")
            .ok_or("no settings snapshot")?;
        save(destination, snapshot, "colors.palette", Some(&value))?;
        writeln!(out, "colors.palette = {}", format_list(&value))?;
    }
    Ok(())
}

/// Ghostty's own file locations, in the order it prefers them.
fn default_sources() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let config = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|home| home.join(".config")));
    let mac = home
        .filter(|_| cfg!(target_os = "macos"))
        .map(|home| home.join("Library/Application Support/com.mitchellh.ghostty"));
    [config.map(|dir| dir.join("ghostty")), mac]
        .into_iter()
        .flatten()
        .flat_map(|dir| ["config.ghostty", "config"].map(|name| dir.join(name)))
        .collect()
}
