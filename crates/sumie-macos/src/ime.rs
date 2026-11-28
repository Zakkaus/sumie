//! Input-method state between winit and the binary: which recipient owns a composition, and
//! the key releases winit swallows while one runs.
//!
//! A composition belongs to the input generation that was current at its first non-empty
//! preedit. Its preedits, the empty one that ends it and its one commit carry that generation,
//! so the session drops all of them once the recipient changed. A new generation, focus loss
//! or an input-source change ends the owner first: an event of the old composition that
//! arrives afterwards has none, and a commit without an owner is dropped. On a new generation
//! or focus loss the window then cancels the native marked text.
//!
//! winit forwards no key-up while the input method composes, so a press reported before the
//! composition took over would never end. Its release is sent when the composition starts,
//! with the modifiers held then; the session drops the real one if winit forwards it later,
//! since it no longer holds the press.

use sumie_core::{KeyAction, KeyModifiers};

use crate::Key;

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
        generation: u64,
    },
}

#[derive(Default)]
pub struct Composition {
    generation: u64,
    /// The generation and input source of the running composition, from its first non-empty
    /// preedit until its commit or cancellation.
    owner: Option<(u64, Option<String>)>,
    /// Presses forwarded to the session whose release has not arrived.
    held: Vec<Key>,
    modifiers: KeyModifiers,
}

impl Composition {
    /// Start a new input generation. Returns whether it changed; the window then cancels the
    /// native composition, whose late events no longer have an owner.
    pub fn set_generation(&mut self, generation: u64) -> bool {
        if generation == self.generation {
            return false;
        }
        self.owner = None;
        self.generation = generation;
        true
    }

    /// End the composition without a commit, as on focus loss: its overlay clears, and its
    /// late events have no owner.
    pub fn cancel(&mut self) -> Option<Input> {
        let (generation, _) = self.owner.take()?;
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
                self.held.retain(|held| held.physical != key.physical);
                self.held.push(key.clone());
            }
            KeyAction::Repeat => (),
            KeyAction::Release => self.held.retain(|held| held.physical != key.physical),
        }
    }

    /// Convert an input-method event under the input source selected now. A switch of input
    /// source ends the composition, so the old input method's commit on deactivation has no
    /// owner.
    pub fn ime(&mut self, ime: Ime, source: Option<String>, out: &mut Vec<Input>) {
        if self
            .owner
            .as_ref()
            .is_some_and(|(_, owner)| *owner != source)
        {
            out.extend(self.cancel());
        }
        match ime {
            Ime::Preedit { text, cursor } if !text.is_empty() => {
                let generation = self.owner.get_or_insert((self.generation, source)).0;
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
                if let Some((generation, _)) = self.owner {
                    out.push(Input::Preedit {
                        text: String::new(),
                        cursor: None,
                        generation,
                    });
                }
            }
            Ime::Commit(text) => {
                if let Some((generation, _)) = self.owner.take() {
                    out.push(Input::Commit { text, generation });
                } else {
                    tracing::debug!("dropped an input-method commit without an owner");
                }
            }
            Ime::Disabled => out.extend(self.cancel()),
        }
    }

    /// Release every forwarded press: winit swallows key-ups from now on.
    fn release_held(&mut self) -> impl Iterator<Item = Input> {
        let modifiers = self.modifiers;
        self.held.drain(..).map(move |held| {
            Input::Key(Key {
                text: None,
                modifiers,
                action: KeyAction::Release,
                ..held
            })
        })
    }
}

#[cfg(test)]
mod tests;
