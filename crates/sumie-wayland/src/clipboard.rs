use std::{io, mem, rc::Rc};

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
        calloop::{Interest, Mode, PostAction, RegistrationToken, generic::Generic},
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

#[derive(Default)]
pub(crate) struct Clipboard {
    source: Option<ClipboardSource>,
    writes: Vec<Option<RegistrationToken>>,
}

struct ClipboardSource {
    source: CopyPasteSource,
    text: Rc<String>,
}

impl State {
    pub(crate) fn write_clipboard(&mut self, serial: u32, text: String) {
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
        let source = manager.create_copy_paste_source(&self.qh, TEXT_TYPES);
        source.set_selection(device, serial);
        self.clipboard.source = Some(ClipboardSource {
            source,
            text: Rc::new(text),
        });
    }

    pub(crate) fn close_clipboard(&mut self) {
        self.clipboard.source = None;
        for token in self.clipboard.writes.drain(..).flatten() {
            self.loop_handle.remove(token);
        }
    }

    fn queue_clipboard_write(&mut self, fd: WritePipe, text: Rc<String>) -> io::Result<()> {
        fcntl_setfl(&fd, fcntl_getfl(&fd)? | OFlags::NONBLOCK)?;
        // Each requested fd keeps its own offset and the source text, even after replacement.
        let mut offset = 0;
        let writes = &mut self.clipboard.writes;
        let slot = writes
            .iter()
            .position(Option::is_none)
            .unwrap_or(writes.len());
        let token = self
            .loop_handle
            .insert_source(fd, move |(), fd, state| {
                let remaining = &text.as_bytes()[offset..];
                let action = match write(fd, &remaining[..remaining.len().min(CHUNK_BYTES)]) {
                    Ok(0) | Err(Errno::PIPE) => PostAction::Remove,
                    Ok(n) => {
                        offset += n;
                        if offset == text.len() {
                            PostAction::Remove
                        } else {
                            PostAction::Continue
                        }
                    }
                    Err(Errno::AGAIN | Errno::INTR) => PostAction::Continue,
                    Err(error) => {
                        tracing::warn!(%error, "clipboard write failed");
                        PostAction::Remove
                    }
                };
                if action == PostAction::Remove {
                    state.clipboard.writes[slot] = None;
                }
                action
            })
            .map_err(|e| e.error)?;
        if let Some(entry) = writes.get_mut(slot) {
            *entry = Some(token);
        } else {
            writes.push(Some(token));
        }
        Ok(())
    }

    pub(crate) fn read_clipboard(&self, max_bytes: usize) -> io::Result<()> {
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
        receive_to_fd(offer.inner(), mime.to_owned(), writer);
        let source = Generic::new(reader, Interest::READ, Mode::Level);
        let mut data = Vec::new();
        let mut bytes = 0usize;
        self.loop_handle
            .insert_source(source, move |_, fd, state| {
                let mut chunk = [0; CHUNK_BYTES];
                match read(fd, &mut chunk) {
                    Ok(0) => {
                        state.finish_clipboard(mem::take(&mut data), bytes);
                        Ok(PostAction::Remove)
                    }
                    Ok(n) => {
                        bytes = bytes.saturating_add(n);
                        if bytes <= max_bytes {
                            data.extend_from_slice(&chunk[..n]);
                        }
                        Ok(PostAction::Continue)
                    }
                    Err(Errno::AGAIN | Errno::INTR) => Ok(PostAction::Continue),
                    Err(error) => {
                        tracing::warn!(%error, "clipboard read failed");
                        Ok(PostAction::Remove)
                    }
                }
            })
            .map_err(|e| e.error)?;
        Ok(())
    }

    fn finish_clipboard(&mut self, data: Vec<u8>, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let reason = if bytes != data.len() {
            "oversized"
        } else if let Ok(text) = String::from_utf8(data) {
            self.events.push_back(Event::ClipboardText(text));
            return;
        } else {
            "invalid UTF-8"
        };
        tracing::warn!(bytes, reason, "discarded clipboard transfer");
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
            || clipboard.text.is_empty()
            || !TEXT_TYPES.contains(&mime.as_str())
        {
            return;
        }
        let text = Rc::clone(&clipboard.text);
        if let Err(error) = self.queue_clipboard_write(fd, text) {
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
            self.clipboard.source = None;
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
