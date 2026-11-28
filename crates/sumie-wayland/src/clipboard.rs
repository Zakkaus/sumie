use std::{collections::VecDeque, io, mem, os::fd::OwnedFd, rc::Rc, time::Duration};

use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::{Errno, read, write},
    pipe::{PipeFlags, pipe_with},
};
use smithay_client_toolkit::{
    data_device_manager::{
        WritePipe,
        data_device::DataDeviceHandler,
        data_offer::{DataOfferHandler, DragOffer, receive_to_fd},
        data_source::{CopyPasteSource, DataSourceHandler},
    },
    primary_selection::{
        PrimarySelectionManagerState,
        device::PrimarySelectionDeviceHandler,
        selection::{PrimarySelectionSource, PrimarySelectionSourceHandler},
    },
    reexports::{
        calloop::{
            Interest, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        client::{
            Connection, QueueHandle,
            globals::GlobalList,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_data_source::WlDataSource, wl_surface::WlSurface,
            },
        },
    },
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};

use crate::{
    Event,
    platform::{Platform, State},
};

const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const CHUNK_BYTES: usize = 8192;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct Clipboard {
    source: Option<ClipboardSource<CopyPasteSource>>,
    primary: Option<ClipboardSource<PrimarySelectionSource>>,
    reads: PasteQueue<Transfer>,
    writes: [Vec<Option<Transfer>>; 2],
}

#[derive(Clone, Copy)]
enum Selection {
    Clipboard,
    Primary,
}

struct ClipboardSource<S> {
    source: S,
    bytes: Rc<Vec<u8>>,
}

impl<S> ClipboardSource<S> {
    fn request_bytes(&self, matches: bool, mime: &str) -> Option<Rc<Vec<u8>>> {
        (matches && !self.bytes.is_empty() && TEXT_TYPES.contains(&mime))
            .then(|| Rc::clone(&self.bytes))
    }
}

struct Transfer {
    pipe: RegistrationToken,
    timer: RegistrationToken,
}

enum Paste<T> {
    Pending(T),
    Ready(Option<String>),
}

struct PasteSlot<T> {
    generation: u64,
    state: Paste<T>,
}

struct PasteQueue<T> {
    first: usize,
    slots: VecDeque<PasteSlot<T>>,
}

impl<T> Default for PasteQueue<T> {
    fn default() -> Self {
        Self {
            first: 0,
            slots: VecDeque::new(),
        }
    }
}

impl<T> PasteQueue<T> {
    fn next_sequence(&self) -> usize {
        self.first.wrapping_add(self.slots.len())
    }

    fn push(&mut self, generation: u64, transfer: T) {
        self.slots.push_back(PasteSlot {
            generation,
            state: Paste::Pending(transfer),
        });
    }

    fn finish(&mut self, sequence: usize, text: Option<String>, events: &mut VecDeque<Event>) -> T {
        let index = sequence.wrapping_sub(self.first);
        let Paste::Pending(transfer) =
            mem::replace(&mut self.slots[index].state, Paste::Ready(text))
        else {
            unreachable!("clipboard read completes once");
        };
        while self
            .slots
            .front()
            .is_some_and(|slot| matches!(slot.state, Paste::Ready(_)))
        {
            if let Some(PasteSlot {
                generation,
                state: Paste::Ready(Some(text)),
            }) = self.slots.pop_front()
            {
                events.push_back(Event::ClipboardText { text, generation });
            }
            self.first = self.first.wrapping_add(1);
        }
        transfer
    }
}

impl Platform {
    /// Request text from the keyboard seat's clipboard. Non-empty UTF-8 reads
    /// smaller than `max_bytes` arrive in request order as `Event::ClipboardText`.
    /// The reply carries `recipient`, even if keyboard focus changes during the read.
    /// Reads that reach the size limit or take more than five seconds are discarded.
    ///
    /// # Errors
    /// Returns an error if pipe creation or event source registration fails.
    pub fn read_clipboard(&mut self, max_bytes: usize, recipient: u64) -> std::io::Result<()> {
        self.state.read_clipboard(max_bytes, recipient)
    }

    #[must_use]
    pub fn has_primary_selection(&self) -> bool {
        self.state.primary_selection_manager.is_some()
    }

    /// Request the keyboard seat's primary selection under the same limits and ordering
    /// as [`Self::read_clipboard`]. With no primary offer, there is no reply.
    ///
    /// # Errors
    /// Returns an error if pipe creation or event source registration fails.
    pub fn read_primary(&mut self, max_bytes: usize, recipient: u64) -> std::io::Result<()> {
        self.state.read_primary(max_bytes, recipient)
    }

    /// Publish text from a recent key press or pointer release on the active seat.
    /// On selection release, publish the same text to primary selection if available.
    /// Transfers run through calloop.
    pub fn write_clipboard(&mut self, serial: u32, text: String, on_release: bool) {
        self.state
            .write_clipboard(serial, Some(text.into_bytes()), on_release);
    }

    /// Publish a terminal write using the focused seat's serial, or clear on `None`.
    /// Without keyboard focus, there is no serial and the request is discarded.
    pub fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        if let Some(serial) = self
            .state
            .keyboard
            .as_ref()
            .and_then(|key| key.selection_serial)
        {
            self.state.write_clipboard(serial, bytes, false);
        }
    }
}

impl State {
    pub(crate) fn bind_primary_selection(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
    ) -> Option<PrimarySelectionManagerState> {
        let manager = PrimarySelectionManagerState::bind(globals, qh).ok();
        if manager.is_none() {
            tracing::debug!("primary selection unavailable; middle-click paste disabled");
        }
        manager
    }

    pub(crate) fn write_clipboard(
        &mut self,
        serial: u32,
        bytes: Option<Vec<u8>>,
        on_release: bool,
    ) {
        let bytes = bytes.map(Rc::new);
        if on_release && let Some(bytes) = &bytes {
            self.write_primary(serial, Rc::clone(bytes));
        }
        let Some((manager, device)) = self.data_device_manager.as_ref().zip(
            self.keyboard
                .as_ref()
                .and_then(|key| key.data_device.as_ref()),
        ) else {
            return;
        };
        let source = if let Some(bytes) = bytes {
            let source = manager.create_copy_paste_source(&self.qh, TEXT_TYPES);
            source.set_selection(device, serial);
            Some(ClipboardSource { source, bytes })
        } else {
            device.unset_selection(serial);
            None
        };
        self.cancel_clipboard_writes(Selection::Clipboard);
        self.clipboard.source = source;
    }

    fn write_primary(&mut self, serial: u32, bytes: Rc<Vec<u8>>) {
        let Some((manager, device)) = self.primary_selection_manager.as_ref().zip(
            self.keyboard
                .as_ref()
                .and_then(|key| key.primary_selection.as_ref()),
        ) else {
            return;
        };
        let source = manager.create_selection_source(&self.qh, TEXT_TYPES);
        source.set_selection(device, serial);
        self.cancel_primary_writes();
        self.clipboard.primary = Some(ClipboardSource { source, bytes });
    }

    pub(crate) fn close_clipboard(&mut self) {
        self.cancel_clipboard_writes(Selection::Clipboard);
        self.cancel_primary_writes();
        for paste in self.clipboard.reads.slots.drain(..) {
            if let Paste::Pending(transfer) = paste.state {
                self.loop_handle.remove(transfer.pipe);
                self.loop_handle.remove(transfer.timer);
            }
        }
    }

    pub(crate) fn cancel_primary_writes(&mut self) {
        self.cancel_clipboard_writes(Selection::Primary);
    }

    fn cancel_clipboard_writes(&mut self, selection: Selection) {
        match selection {
            Selection::Clipboard => self.clipboard.source = None,
            Selection::Primary => self.clipboard.primary = None,
        }
        for transfer in self.clipboard.writes[selection as usize]
            .drain(..)
            .flatten()
        {
            self.loop_handle.remove(transfer.pipe);
            self.loop_handle.remove(transfer.timer);
        }
    }

    fn clipboard_deadline(
        &self,
        pipe: RegistrationToken,
        mut expire: impl FnMut(&mut Self) + 'static,
    ) -> io::Result<Transfer> {
        match self.loop_handle.insert_source(
            Timer::from_duration(TRANSFER_TIMEOUT),
            move |_, (), state| {
                state.loop_handle.remove(pipe);
                expire(state);
                TimeoutAction::Drop
            },
        ) {
            Ok(timer) => Ok(Transfer { pipe, timer }),
            Err(error) => {
                self.loop_handle.remove(pipe);
                Err(error.error.into())
            }
        }
    }

    fn queue_clipboard_write(
        &mut self,
        selection: Selection,
        fd: WritePipe,
        bytes: Rc<Vec<u8>>,
    ) -> io::Result<()> {
        fcntl_setfl(&fd, fcntl_getfl(&fd)? | OFlags::NONBLOCK)?;
        let mut offset = 0;
        let kind = selection as usize;
        let writes = &mut self.clipboard.writes[kind];
        let slot = writes
            .iter()
            .position(Option::is_none)
            .unwrap_or(writes.len());
        let pipe = self
            .loop_handle
            .insert_source(fd, move |(), fd, state| {
                let remaining = &bytes[offset..];
                match write(fd, &remaining[..remaining.len().min(CHUNK_BYTES)]) {
                    Ok(0) | Err(Errno::PIPE) => (),
                    Ok(n) => {
                        offset += n;
                        if offset < bytes.len() {
                            return PostAction::Continue;
                        }
                    }
                    Err(Errno::AGAIN | Errno::INTR) => return PostAction::Continue,
                    Err(error) => tracing::warn!(%error, "clipboard write failed"),
                }
                let transfer = state.clipboard.writes[kind][slot]
                    .take()
                    .expect("active clipboard write");
                state.loop_handle.remove(transfer.timer);
                PostAction::Remove
            })
            .map_err(|e| e.error)?;
        let transfer = self.clipboard_deadline(pipe, move |state| {
            state.clipboard.writes[kind][slot] = None;
            tracing::warn!("clipboard write timed out");
        })?;
        let writes = &mut self.clipboard.writes[kind];
        if let Some(entry) = writes.get_mut(slot) {
            *entry = Some(transfer);
        } else {
            writes.push(Some(transfer));
        }
        Ok(())
    }

    pub(crate) fn read_clipboard(&mut self, max_bytes: usize, recipient: u64) -> io::Result<()> {
        let Some(offer) = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.data_device.as_ref()?.data().selection_offer())
        else {
            return Ok(());
        };
        let Some(mime) = offer.with_mime_types(text_mime) else {
            return Ok(());
        };
        self.queue_clipboard_read(max_bytes, recipient, |writer| {
            receive_to_fd(offer.inner(), mime.to_owned(), writer);
        })
    }

    pub(crate) fn read_primary(&mut self, max_bytes: usize, recipient: u64) -> io::Result<()> {
        let Some(offer) = self
            .keyboard
            .as_ref()
            .and_then(|key| key.primary_selection.as_ref()?.data().selection_offer())
        else {
            return Ok(());
        };
        let Some(mime) = offer.with_mime_types(text_mime) else {
            return Ok(());
        };
        self.queue_clipboard_read(max_bytes, recipient, |writer| {
            offer.receive_to_fd(mime.to_owned(), writer);
        })
    }

    fn queue_clipboard_read(
        &mut self,
        max_bytes: usize,
        recipient: u64,
        receive: impl FnOnce(OwnedFd),
    ) -> io::Result<()> {
        let (reader, writer) = pipe_with(PipeFlags::CLOEXEC)?;
        // The selection owner may expect blocking writes; change only our end.
        fcntl_setfl(&reader, fcntl_getfl(&reader)? | OFlags::NONBLOCK)?;
        let source = Generic::new(reader, Interest::READ, Mode::Level);
        let sequence = self.clipboard.reads.next_sequence();
        let mut data = Vec::new();
        let pipe = self
            .loop_handle
            .insert_source(source, move |_, fd, state| {
                let mut chunk = [0; CHUNK_BYTES];
                let remaining = max_bytes - data.len();
                let text = match read(fd, &mut chunk[..remaining.min(CHUNK_BYTES)]) {
                    Ok(0) => clipboard_text(mem::take(&mut data)),
                    Ok(n) if n == remaining => {
                        tracing::warn!(
                            bytes = max_bytes,
                            reason = "oversized",
                            "discarded clipboard transfer"
                        );
                        None
                    }
                    Ok(n) => {
                        data.extend_from_slice(&chunk[..n]);
                        return Ok(PostAction::Continue);
                    }
                    Err(Errno::AGAIN | Errno::INTR) => return Ok(PostAction::Continue),
                    Err(error) => {
                        tracing::warn!(%error, "clipboard read failed");
                        None
                    }
                };
                let transfer = state
                    .clipboard
                    .reads
                    .finish(sequence, text, &mut state.events);
                state.loop_handle.remove(transfer.timer);
                Ok(PostAction::Remove)
            })
            .map_err(|e| e.error)?;
        let transfer = self.clipboard_deadline(pipe, move |state| {
            tracing::warn!("clipboard read timed out");
            state
                .clipboard
                .reads
                .finish(sequence, None, &mut state.events);
        })?;
        self.clipboard.reads.push(recipient, transfer);
        receive(writer);
        Ok(())
    }
}

fn text_mime(types: &[String]) -> Option<&'static str> {
    TEXT_TYPES
        .into_iter()
        .find(|mime| types.is_empty() || types.iter().any(|t| t == mime))
}

fn clipboard_text(data: Vec<u8>) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let bytes = data.len();
    if let Ok(text) = String::from_utf8(data) {
        Some(text)
    } else {
        tracing::warn!(
            bytes,
            reason = "invalid UTF-8",
            "discarded clipboard transfer"
        );
        None
    }
}

// SCTK tracks selection offers on DataDeviceData; we do not accept drag offers.
impl DataDeviceHandler for State {
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        fd: WritePipe,
    ) {
        let bytes = self
            .clipboard
            .source
            .as_ref()
            .and_then(|entry| entry.request_bytes(entry.source.inner() == source, &mime));
        if let Some(bytes) = bytes
            && let Err(error) = self.queue_clipboard_write(Selection::Clipboard, fd, bytes)
        {
            tracing::warn!(%error, "could not start clipboard write");
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .clipboard
            .source
            .as_ref()
            .is_some_and(|clipboard| clipboard.source.inner() == source)
        {
            self.cancel_clipboard_writes(Selection::Clipboard);
        }
    }

    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl PrimarySelectionDeviceHandler for State {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        mime: String,
        fd: WritePipe,
    ) {
        let bytes = self
            .clipboard
            .primary
            .as_ref()
            .and_then(|entry| entry.request_bytes(entry.source.inner() == source, &mime));
        if let Some(bytes) = bytes
            && let Err(error) = self.queue_clipboard_write(Selection::Primary, fd, bytes)
        {
            tracing::warn!(%error, "could not start primary selection write");
        }
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        if self
            .clipboard
            .primary
            .as_ref()
            .is_some_and(|entry| entry.source.inner() == source)
        {
            self.cancel_primary_writes();
        }
    }
}

#[cfg(test)]
mod tests;
