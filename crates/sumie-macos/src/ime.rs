//! Input-method state between winit and the binary: which recipient owns a composition, and
//! the key releases winit swallows while one runs.
//!
//! A composition belongs to the input generation that was current at its first non-empty
//! preedit. Every event of it carries that generation until it ends, its preedits, the empty
//! one and its one commit alike, so the session drops all of them once the recipient changed,
//! in whatever order they arrive. It ends at its commit, when winit reports it disabled, at
//! focus loss, or at its first event under another input source. A new generation does not
//! end it: the window cancels it natively, and winit's `Disabled` for that cancel follows
//! every event of it that winit delivered. A commit without an owner belongs to no running
//! composition. After a cancel and before the next composition or forwarded key press, it
//! answers the cancelled composition, so it is stale and dropped. Otherwise the input method
//! inserted it without marked text, as Pinyin does with the punctuation that ends a
//! composition, and it goes to the current recipient. The window converts events in the order
//! it delivers them, so a new generation applies from the event that caused it, and a
//! composition the next key starts belongs to the new recipient.
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
        generation: u64,
    },
}

#[derive(Default)]
pub struct Composition {
    generation: u64,
    /// The generation and input source of the running composition, from its first non-empty
    /// preedit until it ends; a newer generation leaves it in place.
    owner: Option<(u64, Option<String>)>,
    /// A composition was cancelled, and its input method may still commit it.
    cancelled: bool,
    /// Presses forwarded to the session whose release has not arrived.
    held: Vec<(PhysicalKey, Symbol)>,
    modifiers: KeyModifiers,
}

impl Composition {
    /// Start a new input generation. Returns whether a composition of an older one runs: the
    /// window then cancels it natively, and its events keep their generation until it ends.
    pub fn set_generation(&mut self, generation: u64) -> bool {
        self.generation = generation;
        self.owner
            .as_ref()
            .is_some_and(|&(owner, _)| owner != generation)
    }

    /// End the composition without a commit, as on focus loss: its overlay clears, and its
    /// late events have no owner.
    pub fn cancel(&mut self) -> Option<Input> {
        let (generation, _) = self.owner.take()?;
        self.cancelled = true;
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
                self.cancelled = false;
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
            .is_some_and(|(_, owner)| *owner != source)
        {
            out.extend(self.cancel());
        }
        match ime {
            Ime::Preedit { text, cursor } if !text.is_empty() => {
                let generation = self.owner.get_or_insert((self.generation, source)).0;
                self.cancelled = false;
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
                } else if std::mem::take(&mut self.cancelled) {
                    // The composition was cancelled (natively at a recipient change or focus
                    // loss, or by another input source), and this is its late commit. A
                    // cancelled composition commits nothing; Ghostty and iTerm2 likewise never
                    // commit marked text on focus loss.
                    tracing::debug!("dropped the commit of a cancelled composition");
                } else {
                    out.push(Input::Commit {
                        text,
                        generation: self.generation,
                    });
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
