use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::sync::mpsc::TryRecvError;

use sumie_core::{KeyAction, KeyModifiers, Pane, PaneCommand};
use sumie_pty::{Command, Pty, Size, SpawnOptions};
use sumie_vt::{KeyEncoder, KeyEvent, Rgb, Snapshot, Terminal};

use crate::{Result, keycodes};

const FOREGROUND: Rgb = [0xcc, 0xcc, 0xcc];
const BACKGROUND: Rgb = [0x1e, 0x1e, 0x1e];
const PALETTE: [Rgb; 16] = [
    [0x00, 0x00, 0x00],
    [0xcd, 0x31, 0x31],
    [0x0d, 0xbc, 0x79],
    [0xe5, 0xe5, 0x10],
    [0x24, 0x72, 0xc8],
    [0xbc, 0x3f, 0xbc],
    [0x11, 0xa8, 0xcd],
    [0xe5, 0xe5, 0xe5],
    [0x66, 0x66, 0x66],
    [0xf1, 0x4c, 0x4c],
    [0x23, 0xd1, 0x8b],
    [0xf5, 0xf5, 0x43],
    [0x3b, 0x8e, 0xea],
    [0xd6, 0x70, 0xd6],
    [0x29, 0xb8, 0xdb],
    [0xe5, 0xe5, 0xe5],
];

pub(crate) struct LocalPane {
    terminal: Terminal,
    encoder: KeyEncoder,
    pub(crate) pty: Pty,
}

impl LocalPane {
    pub(crate) fn login(size: Size, cell: [u32; 2]) -> Result<Self> {
        Self::spawn(Command::LoginShell, &request_environment(), size, cell)
    }

    pub(crate) fn spawn(
        command: Command<'_>,
        env: &BTreeMap<OsString, OsString>,
        size: Size,
        cell: [u32; 2],
    ) -> Result<Self> {
        let mut terminal = Terminal::new(size.cols, size.rows)?;
        terminal.set_colors(FOREGROUND, BACKGROUND, &PALETTE)?;
        terminal.resize_pixels(size.cols, size.rows, cell)?;
        Ok(Self {
            terminal,
            encoder: KeyEncoder::new()?,
            pty: Pty::spawn(SpawnOptions::new(command, env, size))?,
        })
    }

    /// Returns whether the snapshot needs refreshing and whether output reached EOF.
    pub(crate) fn drain_output(&mut self) -> io::Result<(bool, bool)> {
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
}

impl Pane for LocalPane {
    type Snapshot = Snapshot;
    type Error = crate::Error;

    fn command(&mut self, command: PaneCommand<'_>) -> Result<()> {
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
                }
            }
            PaneCommand::Text(bytes) => self.pty.enqueue_input(bytes)?,
            PaneCommand::Paste(text) => {
                self.pty.enqueue_input(&self.terminal.encode_paste(text)?)?;
            }
            PaneCommand::Resize {
                cols,
                rows,
                px: [width_px, height_px],
                cell_px,
            } => {
                self.terminal.resize_pixels(cols, rows, cell_px)?;
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
        Ok(())
    }

    fn snapshot(&mut self) -> Result<Snapshot> {
        Ok(self.terminal.render_snapshot()?)
    }
}

pub(crate) fn request_environment() -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("COLORTERM".into(), "truecolor".into());
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
