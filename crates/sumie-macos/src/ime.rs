//! Input-method state between winit and the binary: which recipient owns a composition, and
//! the key releases winit swallows while one runs.
//!
//! A composition keeps the generation and text recipient of its first non-empty preedit.
//! Only preedits for the current generation reach the overlay. A commit already queued when
//! focus changes goes to the original recipient. A composition ends at its commit, when winit
//! reports it disabled, at focus loss, or at its first event under another input source.
//! A generation change cancels it natively; winit's `Disabled` follows the events it already
//! delivered. A commit without an owner answers a cancelled composition or repeats a commit.
//! The window converts events in delivery order, so a new generation applies from the event
//! that caused it, and a composition the next key starts belongs to the new recipient.
//!
//! winit forwards no key-up while the input method composes, so a press reported before the
//! composition took over would never end. Its release is sent when the composition starts,
//! with the modifiers held then. The session routes a release to the pane that took its
//! press, whichever recipient is current, and drops the real one if winit forwards it later,
//! since it no longer holds the press.

use sumie_core::{KeyAction, KeyModifiers, PhysicalKey};

use crate::{Key, Symbol};

/// A native input-method event, as winit reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ime {
    /// Replace the marked text; empty when the input method cleared it. `cursor` is the input
    /// method's selection in UTF-8 byte offsets, `None` to hide it.
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    Commit(String),
    /// winit left the composition: input methods were disallowed, or the input source
    /// changed.
    Disabled,
}

/// What the window queues for the session, in order.
pub enum Input {
    Key(Key),
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
        generation: u64,
    },
    Commit {
        text: String,
        recipient: u64,
    },
}

#[derive(Default)]
pub struct Composition {
    generation: u64,
    recipient: u64,
    /// Generation, text recipient, and input source of the running composition.
    owner: Option<(u64, u64, Option<String>)>,
    /// Presses forwarded to the session whose release has not arrived.
    held: Vec<(PhysicalKey, Symbol)>,
    modifiers: KeyModifiers,
}

impl Composition {
    /// Start a new input generation. Returns whether a composition of an older one runs: the
    /// the window then cancels it natively, but already queued commits keep their recipient.
    pub fn set_generation(&mut self, generation: u64, recipient: u64) -> bool {
        self.recipient = recipient;
        self.generation = generation;
        self.owner
            .as_ref()
            .is_some_and(|&(owner, _, _)| owner != generation)
    }

    /// End the composition without a commit, as on focus loss: its overlay clears, and its
    /// late events have no owner.
    pub fn cancel(&mut self) -> Option<Input> {
        let (generation, _, _) = self.owner.take()?;
        Some(Input::Preedit {
            text: String::new(),
            cursor: None,
            generation,
        })
    }

    pub const fn set_modifiers(&mut self, modifiers: KeyModifiers) {
        self.modifiers = modifiers;
    }

    /// Track a key winit forwarded.
    pub fn key(&mut self, key: &Key) {
        match key.action {
            KeyAction::Press => {
                self.held.retain(|&(physical, _)| physical != key.physical);
                self.held.push((key.physical, key.symbol));
            }
            KeyAction::Repeat => (),
            KeyAction::Release => self.held.retain(|&(physical, _)| physical != key.physical),
        }
    }

    /// Convert an input-method event under the input source selected now. A switch of input
    /// source ends the composition, so the old input method's commit on deactivation has no
    /// owner.
    pub fn ime(&mut self, ime: Ime, source: Option<String>, out: &mut Vec<Input>) {
        if self
            .owner
            .as_ref()
            .is_some_and(|(_, _, owner)| *owner != source)
        {
            out.extend(self.cancel());
        }
        match ime {
            Ime::Preedit { text, cursor } if !text.is_empty() => {
                let generation = self
                    .owner
                    .get_or_insert((self.generation, self.recipient, source))
                    .0;
                out.extend(self.release_held());
                let cursor = cursor.filter(|&(start, end)| {
                    start <= end && text.is_char_boundary(start) && text.is_char_boundary(end)
                });
                out.push(Input::Preedit {
                    text,
                    cursor,
                    generation,
                });
            }
            Ime::Preedit { .. } => {
                if let Some((generation, _, _)) = self.owner {
                    out.push(Input::Preedit {
                        text: String::new(),
                        cursor: None,
                        generation,
                    });
                }
            }
            Ime::Commit(text) => {
                if let Some((_, recipient, _)) = self.owner.take() {
                    out.push(Input::Commit { text, recipient });
                } else {
                    // No composition is running: it was cancelled (natively at a recipient
                    // change or focus loss, or by another input source), or this repeats the
                    // commit that ended it. Either way it is stale. A cancelled composition
                    // commits nothing; Ghostty and iTerm2 likewise never commit marked text
                    // on focus loss.
                    tracing::debug!("dropped an input-method commit without an owner");
                }
            }
            Ime::Disabled => out.extend(self.cancel()),
        }
    }

    /// Release every forwarded press: winit swallows key-ups from now on.
    fn release_held(&mut self) -> impl Iterator<Item = Input> {
        let modifiers = self.modifiers;
        self.held.drain(..).map(move |(physical, symbol)| {
            Input::Key(Key {
                physical,
                symbol,
                text: None,
                modifiers,
                action: KeyAction::Release,
            })
        })
    }
}

#[cfg(test)]
mod tests;
