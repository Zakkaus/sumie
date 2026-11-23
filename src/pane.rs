mod mouse;
#[path = "shell_integration.rs"]
mod shell_integration;
#[path = "terminfo.rs"]
mod terminfo;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::mpsc::TryRecvError;

use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand, PaneEffect};
use sumie_pty::{Command, Pty, Size, SpawnOptions};
use sumie_vt::{KeyEncoder, KeyEvent, Snapshot, Terminal};

use crate::{Result, keycodes};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const XTVERSION: &str = concat!("sumie ", env!("CARGO_PKG_VERSION"));

pub(crate) struct LocalPane {
    pub(crate) terminal: Terminal,
    encoder: KeyEncoder,
    mouse: mouse::Mouse,
    pub(crate) pty: Pty,
}

impl LocalPane {
    pub(crate) fn login(size: Size, cell: [u32; 2], config: &sumie_config::Config) -> Result<Self> {
        let mut env = request_environment();
        let args = if config.shell.integration() {
            shell_integration::inject(&mut env)
        } else {
            None
        };
        let command = args
            .as_ref()
            .map_or(Command::LoginShell, |args| Command::Argv {
                program: &env[std::ffi::OsStr::new("SHELL")],
                args,
            });
        Self::spawn(command, &env, size, cell, config)
    }

    pub(crate) fn spawn(
        command: Command<'_>,
        env: &BTreeMap<OsString, OsString>,
        size: Size,
        cell: [u32; 2],
        config: &sumie_config::Config,
    ) -> Result<Self> {
        let mut terminal = Terminal::new(size.cols, size.rows)?;
        let term = env.get(std::ffi::OsStr::new("TERM"));
        terminal.set_identity(XTVERSION, term.map_or(&[], |term| term.as_encoded_bytes()))?;
        terminal.set_scrollback_bytes(10_000_000)?;
        set_colors(&mut terminal, &config.colors)?;
        set_cursor_defaults(&mut terminal, config.cursor)?;
        terminal.resize_pixels(size.cols, size.rows, cell)?;
        let mouse = mouse::Mouse::new(&terminal, [size.cols, size.rows], cell)?;
        Ok(Self {
            terminal,
            encoder: KeyEncoder::new()?,
            mouse,
            pty: Pty::spawn(SpawnOptions::new(command, env, size))?,
        })
    }

    /// Returns whether the snapshot needs refreshing and whether output reached EOF.
    pub(crate) fn drain_output(&mut self) -> Result<(bool, bool)> {
        self.pty.clear_output_wake()?;
        let mut dirty = false;
        loop {
            match self.pty.try_recv_output() {
                Ok(chunk) => {
                    if !chunk.is_empty() {
                        self.terminal.write(&chunk);
                        dirty = true;
                    }
                }
                Err(end) => {
                    if dirty {
                        self.mouse.sync(&self.terminal)?;
                    }
                    self.drain_replies();
                    return Ok((dirty, end == TryRecvError::Disconnected));
                }
            }
        }
    }

    fn drain_replies(&mut self) {
        self.terminal.drain_replies(|bytes| {
            if let Err(error) = self.pty.enqueue_input(bytes) {
                tracing::warn!(%error, "dropped terminal protocol reply");
            }
        });
    }

    fn follow_input(&mut self) -> Result<PaneEffect> {
        if self.terminal.at_bottom()? {
            return Ok(PaneEffect::None);
        }
        self.terminal.scroll_bottom()?;
        self.mouse.state.cancel_selection();
        Ok(PaneEffect::ViewportChanged)
    }
}

pub(crate) fn set_colors(terminal: &mut Terminal, colors: &sumie_config::Colors) -> Result<()> {
    terminal.set_colors(
        colors.foreground(),
        colors.background(),
        colors.cursor(),
        &colors.palette(),
    )?;
    Ok(())
}

pub(crate) fn set_cursor_defaults(
    terminal: &mut Terminal,
    cursor: sumie_config::Cursor,
) -> Result<()> {
    let style = match cursor.style() {
        sumie_config::CursorStyle::Block => sumie_vt::CursorStyle::Block,
        sumie_config::CursorStyle::Bar => sumie_vt::CursorStyle::Bar,
        sumie_config::CursorStyle::Underline => sumie_vt::CursorStyle::Underline,
    };
    terminal.set_cursor_defaults(style, cursor.blink())?;
    Ok(())
}

impl Pane for LocalPane {
    type Snapshot = Snapshot;
    type Error = crate::Error;

    fn command(&mut self, command: PaneCommand<'_>) -> Result<PaneEffect> {
        match command {
            PaneCommand::Key {
                physical,
                modifiers,
                action,
                text,
                unshifted,
            } => {
                let event = KeyEvent {
                    key: keycodes::ghostty_key(physical),
                    modifiers: modifier_bits(modifiers),
                    consumed_modifiers: 0,
                    action: match action {
                        KeyAction::Press => sumie_vt::KeyAction::Press,
                        KeyAction::Repeat => sumie_vt::KeyAction::Repeat,
                        KeyAction::Release => sumie_vt::KeyAction::Release,
                    },
                    text,
                    unshifted,
                };
                let bytes = self.encoder.encode(&self.terminal, &event)?;
                if !bytes.is_empty() {
                    self.pty.enqueue_input(bytes)?;
                    return self.follow_input();
                }
            }
            PaneCommand::Mouse(event) => return self.pointer(event),
            PaneCommand::Text(bytes) if !bytes.is_empty() => {
                self.pty.enqueue_input(bytes)?;
                return self.follow_input();
            }
            PaneCommand::Text(_) => (),
            PaneCommand::Paste(text) => {
                self.pty.enqueue_input(&self.terminal.encode_paste(text)?)?;
                return self.follow_input();
            }
            PaneCommand::Copy(range) => {
                let point = |p: sumie_core::CellPosition| [p.column, p.row];
                return Ok(PaneEffect::Copied(
                    self.terminal
                        .selection_text(point(range.start), point(range.end))?,
                ));
            }
            PaneCommand::Resize {
                cols,
                rows,
                px: [width_px, height_px],
                cell_px,
            } => {
                self.terminal.resize_pixels(cols, rows, cell_px)?;
                self.mouse.resize([cols, rows], cell_px);
                self.pty.resize(Size {
                    cols,
                    rows,
                    width_px,
                    height_px,
                })?;
                self.drain_replies();
            }
            PaneCommand::Close => self.pty.close()?,
        }
        Ok(PaneEffect::None)
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        Ok(self.terminal.render_snapshot()?)
    }
}

pub(crate) fn request_environment() -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
    let term = if let Some(dir) = terminfo::directory() {
        env.insert("TERMINFO".into(), dir.as_os_str().into());
        terminfo::NAME
    } else {
        env.remove(std::ffi::OsStr::new("TERMINFO"));
        "xterm-256color"
    };
    env.insert("TERM".into(), term.into());
    env.insert("COLORTERM".into(), "truecolor".into());
    env.insert("TERM_PROGRAM".into(), "sumie".into());
    env.insert("TERM_PROGRAM_VERSION".into(), VERSION.into());
    env
}

fn modifier_bits(modifiers: KeyModifiers) -> u16 {
    [
        modifiers.shift,
        modifiers.control,
        modifiers.alt,
        modifiers.super_key,
        modifiers.caps_lock,
        modifiers.num_lock,
    ]
    .into_iter()
    .enumerate()
    .fold(0, |bits, (index, active)| {
        bits | (u16::from(active) << index)
    })
}
