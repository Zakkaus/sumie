use std::{collections::VecDeque, io, mem, rc::Rc, time::Duration};

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
    reexports::{
        calloop::{
            Interest, Mode, PostAction, RegistrationToken,
            generic::Generic,
            timer::{TimeoutAction, Timer},
        },
        client::{
            Connection, QueueHandle,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_data_source::WlDataSource, wl_surface::WlSurface,
            },
        },
    },
};

use crate::{Event, platform::State};

const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];
const CHUNK_BYTES: usize = 8192;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(crate) struct Clipboard {
    source: Option<ClipboardSource>,
    reads: PasteQueue<Transfer>,
    writes: Vec<Option<Transfer>>,
}

struct ClipboardSource {
    source: CopyPasteSource,
    bytes: Rc<Vec<u8>>,
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

impl State {
    pub(crate) fn write_clipboard(&mut self, serial: u32, bytes: Option<Vec<u8>>) {
        let Some(manager) = &self.data_device_manager else {
            return;
        };
        let Some(device) = self
            .keyboard
            .as_ref()
            .and_then(|keyboard| keyboard.data_device.as_ref())
        else {
            return;
        };
        let source = if let Some(bytes) = bytes {
            let source = manager.create_copy_paste_source(&self.qh, TEXT_TYPES);
            source.set_selection(device, serial);
            Some(ClipboardSource {
                source,
                bytes: Rc::new(bytes),
            })
        } else {
            device.unset_selection(serial);
            None
        };
        self.cancel_clipboard_writes();
        self.clipboard.source = source;
    }

    pub(crate) fn close_clipboard(&mut self) {
        self.cancel_clipboard_writes();
        for paste in self.clipboard.reads.slots.drain(..) {
            if let Paste::Pending(transfer) = paste.state {
                self.loop_handle.remove(transfer.pipe);
                self.loop_handle.remove(transfer.timer);
            }
        }
    }

    fn cancel_clipboard_writes(&mut self) {
        self.clipboard.source = None;
        for transfer in self.clipboard.writes.drain(..).flatten() {
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

    fn queue_clipboard_write(&mut self, fd: WritePipe, bytes: Rc<Vec<u8>>) -> io::Result<()> {
        fcntl_setfl(&fd, fcntl_getfl(&fd)? | OFlags::NONBLOCK)?;
        let mut offset = 0;
        let slot = self
            .clipboard
            .writes
            .iter()
            .position(Option::is_none)
            .unwrap_or(self.clipboard.writes.len());
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
                let transfer = state.clipboard.writes[slot]
                    .take()
                    .expect("active clipboard write");
                state.loop_handle.remove(transfer.timer);
                PostAction::Remove
            })
            .map_err(|e| e.error)?;
        let transfer = self.clipboard_deadline(pipe, move |state| {
            state.clipboard.writes[slot] = None;
            tracing::warn!("clipboard write timed out");
        })?;
        if let Some(entry) = self.clipboard.writes.get_mut(slot) {
            *entry = Some(transfer);
        } else {
            self.clipboard.writes.push(Some(transfer));
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
        let Some(mime) = offer.with_mime_types(|types| {
            TEXT_TYPES
                .into_iter()
                .find(|mime| types.is_empty() || types.iter().any(|t| t == mime))
        }) else {
            return Ok(());
        };
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
        receive_to_fd(offer.inner(), mime.to_owned(), writer);
        Ok(())
    }
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
        let Some(clipboard) = &self.clipboard.source else {
            return;
        };
        if clipboard.source.inner() != source
            || clipboard.bytes.is_empty()
            || !TEXT_TYPES.contains(&mime.as_str())
        {
            return;
        }
        let bytes = Rc::clone(&clipboard.bytes);
        if let Err(error) = self.queue_clipboard_write(fd, bytes) {
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
            self.cancel_clipboard_writes();
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

#[cfg(test)]
mod tests {
    use super::{Event, PasteQueue, VecDeque};

    #[test]
    fn paste_results_keep_request_order_after_failure() {
        let mut queue = PasteQueue::default();
        let mut events = VecDeque::new();
        let [first, failed, last] = [0, 1, 2].map(|generation| {
            let sequence = queue.next_sequence();
            queue.push(generation, ());
            sequence
        });

        queue.finish(last, Some("last".into()), &mut events);
        assert!(events.is_empty());
        queue.finish(first, Some("first".into()), &mut events);
        assert!(matches!(
            events.pop_front(),
            Some(Event::ClipboardText { text, generation: 0 }) if text == "first"
        ));
        assert!(events.is_empty());
        queue.finish(failed, None, &mut events);
        assert!(matches!(
            events.pop_front(),
            Some(Event::ClipboardText { text, generation: 2 }) if text == "last"
        ));
        assert!(events.is_empty());
    }
}
