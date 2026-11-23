use std::{io, mem};

use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    io::{Errno, read},
    pipe::{PipeFlags, pipe_with},
};
use smithay_client_toolkit::{
    data_device_manager::{
        data_device::DataDeviceHandler,
        data_offer::{DataOfferHandler, DragOffer, receive_to_fd},
    },
    reexports::{
        calloop::{Interest, Mode, PostAction, generic::Generic},
        client::{
            Connection, QueueHandle,
            protocol::{
                wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
                wl_surface::WlSurface,
            },
        },
    },
};

use crate::{Event, platform::State};

const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

impl State {
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
                let mut chunk = [0; 8192];
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
