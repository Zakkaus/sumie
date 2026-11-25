mod driver;
mod events;
mod notify;
pub(crate) mod sources;
pub(crate) use driver::run;
use sources::Watches;

use std::{collections::VecDeque, time::Duration};

use sumie_render::{backends, instance_descriptor, wgpu};
use sumie_wayland::Platform;

use sumie_core::notify::Notification;

use crate::{
    Result,
    frame_window::FrameWindow,
    gpu::Gpu,
    platform::{CaretBounds, CursorIcon, Event, LogicalRect, WindowOps},
    view,
};

pub(crate) struct LinuxWindow {
    pub(crate) watches: Watches,
    platform: Platform,
    batch: VecDeque<sumie_wayland::Event>,
    copy_serial: Option<u32>,
    notifier: notify::Notifier,
}

impl LinuxWindow {
    pub(crate) fn open(title: &str) -> Result<Self> {
        let platform = Platform::connect(title)?;
        Ok(Self {
            watches: Watches::new(platform.loop_handle()),
            platform,
            batch: VecDeque::new(),
            copy_serial: None,
            notifier: notify::Notifier::new(),
        })
    }

    pub(crate) fn create_gpu(&self, capture: bool, minimum_contrast: f32) -> Result<Gpu> {
        let instance = wgpu::Instance::new(instance_descriptor(
            wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
                self.platform.display_handle_source(),
            )),
            backends(),
        ));
        let surface = instance.create_surface(self.platform.window_owner())?;
        Gpu::new(instance, surface, capture, minimum_contrast)
    }

    pub(crate) fn dispatch(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.platform.dispatch(timeout)?;
        self.platform.swap_events(&mut self.batch);
        Ok(())
    }

    pub(crate) fn deliver_events(
        &mut self,
        mut deliver: impl FnMut(Event, &mut Self) -> Result<bool>,
    ) -> Result<bool> {
        while let Some(raw) = self.batch.pop_front() {
            if let Some((event, serial)) = events::convert(raw) {
                self.copy_serial = serial;
                let result = deliver(event, self);
                self.copy_serial = None;
                if !matches!(result, Ok(false)) {
                    self.batch.clear();
                    return result;
                }
            }
        }
        Ok(false)
    }
}

impl FrameWindow for LinuxWindow {
    fn frame_slot_available(&self) -> bool {
        self.platform.can_present()
    }

    fn arm_frame_callback(&mut self) {
        assert!(self.platform.begin_frame());
    }
}

impl WindowOps for LinuxWindow {
    fn click_interval() -> Option<u32> {
        sumie_wayland::double_click_interval()
    }

    fn hostname() -> Result<String> {
        Ok(std::fs::read_to_string("/proc/sys/kernel/hostname")?
            .trim_end()
            .to_owned())
    }

    fn logical_rect(bounds: CaretBounds) -> Option<LogicalRect> {
        view::surface_logical_rect(bounds)
    }

    fn recover_frame_slot(&mut self) {
        self.platform.recover_frame_slot();
    }

    fn set_cursor_rectangle(&mut self, rect: LogicalRect) {
        self.platform
            .set_cursor_rectangle(sumie_wayland::CursorRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            });
    }

    fn set_pointer_icon(&mut self, icon: CursorIcon) {
        self.platform.set_pointer_icon(match icon {
            CursorIcon::Default => sumie_wayland::CursorIcon::Default,
            CursorIcon::Text => sumie_wayland::CursorIcon::Text,
            CursorIcon::Pointer => sumie_wayland::CursorIcon::Pointer,
        });
    }

    fn set_input_generation(&mut self, generation: u64) {
        self.platform.set_input_generation(generation);
    }

    fn read_clipboard(&mut self, recipient: u64) {
        if let Err(error) = self
            .platform
            .read_clipboard(sumie_pty::INPUT_LIMIT, recipient)
        {
            tracing::warn!(%error, "could not request clipboard text");
        }
    }

    fn write_clipboard(&mut self, text: String) {
        if let Some(serial) = self.copy_serial {
            self.platform.write_clipboard(serial, text);
        } else {
            tracing::warn!("cannot copy to the clipboard without an input serial");
        }
    }

    fn write_terminal_clipboard(&mut self, bytes: Option<Vec<u8>>) {
        self.platform.write_terminal_clipboard(bytes);
    }

    fn queue_title(&mut self, title: String) {
        self.platform.queue_title(title);
    }

    fn notify(&mut self, request: Notification) {
        self.notifier.send(request);
    }

    fn set_background_effects(&mut self, transparent: bool, blur: bool) -> bool {
        self.platform.set_background_effects(transparent, blur)
    }

    fn background_commit_needed(&self) -> bool {
        self.platform.background_commit_needed()
    }
}
