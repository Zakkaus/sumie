use std::time::Duration;
use std::{collections::VecDeque, fmt::Debug, ptr::NonNull, sync::Arc};

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WaylandWindowHandle,
    WindowHandle,
};
use smithay_client_toolkit::reexports::csd_frame::WindowState;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::DataDeviceManagerState,
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{self, EventLoop, EventSource, LoopHandle, RegistrationToken},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Dispatch, Proxy, QueueHandle,
            globals::{self, BindError},
            protocol::{wl_callback::WlCallback, wl_output, wl_shm, wl_surface::WlSurface},
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::SeatState,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{
        CreatePoolError, Shm, ShmHandler,
        slot::{ActivateSlotError, CreateBufferError, SlotPool},
    },
};
use wayland_backend::client::Backend;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

#[cfg(debug_assertions)]
use crate::presentation::PresentLog;
use crate::{
    CursorRect, Event, clipboard::Clipboard, keyboard::Keyboard, pointer::Pointer,
    presentation::Presentation, scale::Scale,
};

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("connect to Wayland: {0}")]
    Connection(#[from] smithay_client_toolkit::reexports::client::ConnectError),
    #[error("initialize Wayland registry: {0}")]
    Registry(#[from] globals::GlobalError),
    #[error("bind {interface}: {source}")]
    Global {
        interface: &'static str,
        source: BindError,
    },
    #[error("set up Wayland event loop: {0}")]
    Register(#[from] calloop::Error),
    #[cfg(debug_assertions)]
    #[error("open presentation log: {0}")]
    PresentLog(#[source] std::io::Error),
}

#[derive(Clone, Debug)]
struct DisplayOwner(Backend);

impl HasDisplayHandle for DisplayOwner {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

/// Owns the native window and its connection. Share it with the graphics surface.
pub struct WindowOwner {
    window: Window,
    display: DisplayOwner,
}

impl WindowOwner {
    pub(crate) fn wl_surface(&self) -> &WlSurface {
        self.window.wl_surface()
    }
}

impl HasDisplayHandle for WindowOwner {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.display.display_handle()
    }
}

impl HasWindowHandle for WindowOwner {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let ptr =
            NonNull::new(self.wl_surface().id().as_ptr().cast()).ok_or(HandleError::Unavailable)?;
        let raw = WaylandWindowHandle::new(ptr).into();
        // SAFETY: WindowOwner keeps the SCTK window and display alive for the borrow and
        // exposes neither proxy. wgpu keeps an Arc<WindowOwner> until its surface drops.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

/// Register non-Wayland sources on the platform's event-loop thread.
#[derive(Clone)]
pub struct SourceHandle(LoopHandle<'static, State>);

impl SourceHandle {
    /// Register a source with a callback that does not access the Wayland dispatch state.
    ///
    /// # Errors
    /// Returns an error if calloop cannot register the source.
    pub fn insert<S, F>(&self, source: S, mut callback: F) -> calloop::Result<RegistrationToken>
    where
        S: EventSource + 'static,
        F: FnMut(S::Event, &mut S::Metadata) -> S::Ret + 'static,
    {
        Ok(self
            .0
            .insert_source(source, move |event, metadata, _| callback(event, metadata))?)
    }

    /// Unregister a previously inserted source.
    pub fn remove(&self, token: RegistrationToken) {
        self.0.remove(token);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShmPresentError {
    #[error("window dimensions exceed the wl_shm buffer limit")]
    Size,
    #[error("create shared memory pool: {0}")]
    Pool(#[from] CreatePoolError),
    #[error("create shared memory buffer: {0}")]
    Buffer(#[from] CreateBufferError),
    #[error("attach shared memory buffer: {0}")]
    Attach(#[from] ActivateSlotError),
}

/// One xdg-toplevel. Connect, dispatch and present on the same thread.
pub struct Platform {
    shm_pool: Option<SlotPool>,
    state: State,
    event_loop: EventLoop<'static, State>,
}

impl Platform {
    /// Connect, register the Wayland source, and request the initial configure.
    /// This performs a registry round trip; subsequent dispatch is driven by calloop.
    ///
    /// # Errors
    /// Returns an error if the connection, required globals, or event-loop setup fail.
    pub fn connect(title: &str) -> Result<Self, ConnectError> {
        let event_loop = EventLoop::try_new()?;
        let state = State::connect(event_loop.handle(), title)?;
        Ok(Self {
            shm_pool: None,
            state,
            event_loop,
        })
    }

    /// Wait up to `timeout` and dispatch ready sources. `None` waits indefinitely.
    ///
    /// # Errors
    /// Returns an error from calloop or the Wayland event source.
    pub fn dispatch(&mut self, timeout: Option<Duration>) -> calloop::Result<()> {
        self.event_loop.dispatch(timeout, &mut self.state)
    }

    /// Drain events before the next dispatch or presentation.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.state.events.drain(..)
    }

    /// Update the cursor for the focused pointer; repeated requests are ignored.
    pub fn set_pointer_icon(&mut self, icon: crate::CursorIcon) {
        if let Some(pointer) = &mut self.state.pointer {
            let connection = Connection::from_backend(self.state.window.display.0.clone());
            pointer.set_icon(&connection, icon);
        }
    }

    /// Change the focused input recipient; restart IME composition on the same surface.
    /// IME text events carry this generation until the next focus change.
    /// Call outside the interval between `begin_frame` and the buffer commit.
    pub fn set_input_generation(&mut self, generation: u64) {
        self.state.set_input_generation(generation);
    }

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

    /// Publish text from a recent key press or pointer release on the active seat.
    /// Does nothing without a clipboard data device. Transfers run through calloop.
    pub fn write_clipboard(&mut self, serial: u32, text: String) {
        self.state.write_clipboard(serial, Some(text.into_bytes()));
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
            self.state.write_clipboard(serial, bytes);
        }
    }

    /// Stage the latest title for the next committed frame.
    pub fn queue_title(&mut self, mut title: String) {
        // Wayland strings cannot contain embedded NULs.
        if let Some(nul) = title.find('\0') {
            title.truncate(nul);
        }
        self.state.pending_title = Some(title);
    }

    #[must_use]
    pub fn loop_handle(&self) -> SourceHandle {
        SourceHandle(self.event_loop.handle())
    }

    /// Share native window ownership with a graphics surface.
    #[must_use]
    pub fn window_owner(&self) -> Arc<WindowOwner> {
        Arc::clone(&self.state.window)
    }

    /// Retain the display without retaining the window, for graphics instance creation.
    #[must_use]
    pub fn display_handle_source(&self) -> impl HasDisplayHandle + Debug + Send + Sync + use<> {
        self.state.window.display.clone()
    }

    /// Whether the surface is configured and the compositor has released the frame slot.
    #[must_use]
    pub fn can_present(&self) -> bool {
        self.state.can_present()
    }

    /// Stop waiting for a callback that may have been withheld while the surface was hidden.
    /// Call on visibility recovery or when input needs a fresh layout.
    pub fn recover_frame_slot(&mut self) {
        self.state.frame_pending = None;
    }

    /// Arm the frame callback and optional presentation feedback, and stage buffer geometry.
    /// Call only when a buffer is ready, immediately before presenting it. A successful
    /// call must be followed by a buffer commit before dispatching or changing IME state.
    #[must_use]
    pub fn begin_frame(&mut self) -> bool {
        if !self.can_present() {
            return false;
        }
        self.state.arm_frame();
        true
    }

    /// Update the IME anchor. The latest rectangle is retained across focus changes.
    /// Call outside the interval between `begin_frame` and the buffer commit.
    pub fn set_cursor_rectangle(&mut self, rect: CursorRect) {
        self.state.set_cursor_rectangle(rect);
    }

    /// Fill and commit a tightly packed XRGB8888 buffer at the configured device size.
    /// Returns `false` without calling `fill` if there is no frame slot.
    ///
    /// # Errors
    /// Returns an error if the buffer size is invalid or the SHM buffer cannot be created or attached.
    pub fn present_shm(&mut self, fill: impl FnOnce(&mut [u8])) -> Result<bool, ShmPresentError> {
        if !self.can_present() {
            return Ok(false);
        }
        let (width, height) = self.state.scale.device_size(self.state.logical);
        let (Ok(width), Ok(height)) = (i32::try_from(width), i32::try_from(height)) else {
            return Err(ShmPresentError::Size);
        };
        let stride = width.checked_mul(4).ok_or(ShmPresentError::Size)?;
        stride.checked_mul(height).ok_or(ShmPresentError::Size)?;
        let pool = match &mut self.shm_pool {
            Some(pool) => pool,
            None => self.shm_pool.insert(SlotPool::new(1, &self.state.shm)?),
        };
        let (buffer, pixels) =
            pool.create_buffer(width, height, stride, wl_shm::Format::Xrgb8888)?;
        fill(pixels);
        buffer.attach_to(self.state.wl_surface())?;
        self.state.arm_frame();
        let surface = self.state.wl_surface();
        surface.damage(0, 0, i32::MAX, i32::MAX);
        surface.commit();
        Ok(true)
    }
}

// SCTK's handler traits return proxies. Keep their implementor out of the public API.
pub(crate) struct State {
    scale: Scale,
    pub(crate) presentation: Option<Presentation>,
    #[cfg(debug_assertions)]
    pub(crate) present_log: Option<PresentLog>,
    pub(crate) window: Arc<WindowOwner>,
    pub(crate) qh: QueueHandle<Self>,
    pub(crate) compositor: CompositorState,
    pub(crate) loop_handle: LoopHandle<'static, Self>,
    source: RegistrationToken,
    registry: RegistryState,
    output: OutputState,
    pub(crate) seats: SeatState,
    pub(crate) shm: Shm,
    pub(crate) data_device_manager: Option<DataDeviceManagerState>,
    pub(crate) clipboard: Clipboard,
    pub(crate) keyboard: Option<Keyboard>,
    pub(crate) pointer: Option<Pointer>,
    pub(crate) text_input_manager: Option<ZwpTextInputManagerV3>,
    pub(crate) input_generation: u64,
    pub(crate) cursor_rect: Option<CursorRect>,
    pub(crate) events: VecDeque<Event>,
    logical: (u32, u32),
    configured: bool,
    frame_pending: Option<WlCallback>,
    pending_title: Option<String>,
    geometry_dirty: bool,
}

impl State {
    fn connect(loop_handle: LoopHandle<'static, Self>, title: &str) -> Result<Self, ConnectError> {
        let conn = Connection::connect_to_env()?;
        let (globals, queue) = globals::registry_queue_init::<Self>(&conn)?;
        let qh = queue.handle();
        let compositor =
            CompositorState::bind(&globals, &qh).map_err(|source| ConnectError::Global {
                interface: "wl_compositor",
                source,
            })?;
        let shell = XdgShell::bind(&globals, &qh).map_err(|source| ConnectError::Global {
            interface: "xdg_wm_base",
            source,
        })?;
        let shm = Shm::bind(&globals, &qh).map_err(|source| ConnectError::Global {
            interface: "wl_shm",
            source,
        })?;
        let surface = compositor.create_surface(&qh);
        let scale = Scale::bind(&globals, &qh, &surface);
        let window = shell.create_window(surface, WindowDecorations::RequestServer, &qh);
        window.set_title(title);
        window.set_app_id("sumie");
        window.commit();

        let presentation = Presentation::bind(&globals, &qh);
        #[cfg(debug_assertions)]
        let present_log =
            PresentLog::open(presentation.is_some()).map_err(ConnectError::PresentLog)?;
        let text_input_manager = globals.bind(&qh, 1..=2, ()).ok();
        if text_input_manager.is_none() {
            tracing::info!("text-input-v3 unavailable; keyboard input only");
        }
        let window = Arc::new(WindowOwner {
            window,
            display: DisplayOwner(conn.backend()),
        });
        let source = WaylandSource::new(conn, queue)
            .insert(loop_handle.clone())
            .map_err(|err| err.error)?;
        Ok(Self {
            scale,
            presentation,
            compositor,
            #[cfg(debug_assertions)]
            present_log,
            window,
            registry: RegistryState::new(&globals),
            output: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            data_device_manager: DataDeviceManagerState::bind(&globals, &qh).ok(),
            shm,
            qh,
            loop_handle,
            source,
            clipboard: Clipboard::default(),
            keyboard: None,
            pointer: None,
            text_input_manager,
            input_generation: 0,
            cursor_rect: None,
            events: VecDeque::new(),
            logical: (800, 600),
            configured: false,
            frame_pending: None,
            pending_title: None,
            geometry_dirty: true,
        })
    }

    pub(crate) fn wl_surface(&self) -> &WlSurface {
        self.window.wl_surface()
    }

    fn can_present(&self) -> bool {
        self.configured && self.frame_pending.is_none()
    }

    fn arm_frame(&mut self) {
        if let Some(title) = self.pending_title.take() {
            self.window.window.set_title(title);
        }
        if self.geometry_dirty {
            self.scale.apply(self.wl_surface(), self.logical);
            self.geometry_dirty = false;
        }
        let surface = self.window.wl_surface();
        self.frame_pending = Some(surface.frame(&self.qh, ()));
        if let Some(presentation) = &mut self.presentation {
            presentation.request(surface);
        }
    }

    pub(crate) fn emit_resize(&mut self) {
        self.geometry_dirty = true;
        if self.configured {
            self.events.push_back(Event::Resized {
                logical: self.logical,
                device: self.scale.device_size(self.logical),
                scale: self.scale.units(),
            });
        }
    }

    pub(crate) fn fractional_scale_changed(&mut self, scale: u32) {
        if self.scale.set_fractional(scale) {
            self.emit_resize();
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.loop_handle.remove(self.source);
        self.close_clipboard();
        drop(self.pointer.take());
        drop(self.keyboard.take());
        if let Some(manager) = &self.text_input_manager {
            manager.destroy();
        }
        // The Scale field destroys its extensions before Window destroys the xdg role and wl_surface.
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.events.push_back(Event::CloseRequested);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let first = !self.configured;
        let logical = (
            configure
                .new_size
                .0
                .map_or(self.logical.0, std::num::NonZeroU32::get),
            configure
                .new_size
                .1
                .map_or(self.logical.1, std::num::NonZeroU32::get),
        );
        let server_decorated = configure.decoration_mode == DecorationMode::Server;
        if first && !server_decorated {
            tracing::info!(
                "server-side decorations unavailable; P0 window has no client-side decorations"
            );
        }
        let suspended = configure.state.contains(WindowState::SUSPENDED);
        #[cfg(debug_assertions)]
        if let Some(log) = &mut self.present_log {
            log.visibility(suspended);
        }
        self.events.push_back(Event::Configured {
            logical,
            activated: configure.is_activated(),
            suspended,
            maximized: configure.is_maximized(),
            fullscreen: configure.is_fullscreen(),
            server_decorated,
        });
        self.configured = true;
        if first || self.logical != logical {
            self.logical = logical;
            self.emit_resize();
        }
        if first {
            self.events.push_back(Event::FrameReady);
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        if self.scale.set_integer(surface, factor) {
            self.emit_resize();
        }
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {}

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl Dispatch<WlCallback, ()> for State {
    fn event(
        state: &mut Self,
        callback: &WlCallback,
        _: <WlCallback as Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if state.frame_pending.as_ref() == Some(callback) {
            state.frame_pending = None;
            state.events.push_back(Event::FrameReady);
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
delegate_dispatch2!(State);
