use std::{collections::VecDeque, ptr::NonNull};

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, RawWindowHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{self, EventLoop, LoopHandle, RegistrationToken},
        calloop_wayland_source::WaylandSource,
        client::{
            Connection, Proxy, QueueHandle,
            globals::{self, BindError},
            protocol::{wl_output, wl_surface::WlSurface},
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
    shm::{Shm, ShmHandler},
};
use wayland_backend::client::Backend;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

use crate::{CursorRect, Event, keyboard::Keyboard, scale::Scale};

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
    #[error("register Wayland event source: {0}")]
    Register(#[from] calloop::Error),
}

/// One xdg-toplevel dispatched on the caller's event-loop thread.
/// Release GPU resources and the wgpu surface before dropping this object.
pub struct Platform {
    scale: Scale,
    pub(crate) window: Window,
    backend: Backend,
    qh: QueueHandle<Self>,
    pub(crate) loop_handle: LoopHandle<'static, Self>,
    source: RegistrationToken,
    registry: RegistryState,
    output: OutputState,
    pub(crate) seats: SeatState,
    shm: Shm,
    pub(crate) keyboard: Option<Keyboard>,
    pub(crate) text_input_manager: Option<ZwpTextInputManagerV3>,
    pub(crate) cursor_rect: Option<CursorRect>,
    pub(crate) events: VecDeque<Event>,
    logical: (u32, u32),
    configured: bool,
    frame_pending: bool,
    geometry_dirty: bool,
}

impl Platform {
    /// Connect, register the Wayland source, and request the initial configure.
    /// This performs a registry round trip; subsequent dispatch is driven by calloop.
    ///
    /// # Errors
    /// Returns an error if the connection, required globals, or source registration fail.
    pub fn connect(
        event_loop: &EventLoop<'static, Self>,
        title: &str,
    ) -> Result<Self, ConnectError> {
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

        let text_input_manager = globals.bind(&qh, 1..=2, ()).ok();
        if text_input_manager.is_none() {
            tracing::info!("text-input-v3 unavailable; keyboard input only");
        }
        let backend = conn.backend();
        let loop_handle = event_loop.handle();
        let source = WaylandSource::new(conn, queue)
            .insert(loop_handle.clone())
            .map_err(|err| err.error)?;
        Ok(Self {
            scale,
            window,
            backend,
            registry: RegistryState::new(&globals),
            output: OutputState::new(&globals, &qh),
            seats: SeatState::new(&globals, &qh),
            shm,
            qh,
            loop_handle,
            source,
            keyboard: None,
            text_input_manager,
            cursor_rect: None,
            events: VecDeque::new(),
            logical: (800, 600),
            configured: false,
            frame_pending: false,
            geometry_dirty: true,
        })
    }

    /// Drain events before the next dispatch or presentation.
    pub fn drain_events(&mut self) -> impl Iterator<Item = Event> + '_ {
        self.events.drain(..)
    }

    /// `wl_surface` used by the renderer. Do not destroy it or assign another role.
    #[must_use]
    pub fn wl_surface(&self) -> &WlSurface {
        self.window.wl_surface()
    }

    /// Shared-memory global for clients that attach their own buffers.
    #[must_use]
    pub fn shm(&self) -> &Shm {
        &self.shm
    }

    /// Raw handle for wgpu's raw surface target. It is valid only while this platform lives.
    ///
    /// # Errors
    /// Returns `HandleError::Unavailable` if the native surface is gone.
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, HandleError> {
        let ptr =
            NonNull::new(self.wl_surface().id().as_ptr().cast()).ok_or(HandleError::Unavailable)?;
        Ok(RawWindowHandle::Wayland(WaylandWindowHandle::new(ptr)))
    }

    /// Whether the surface is configured and the compositor has released the frame slot.
    #[must_use]
    pub fn can_present(&self) -> bool {
        self.configured && !self.frame_pending
    }

    /// Arm the next frame callback and stage buffer geometry for presentation.
    /// Call only when a buffer is ready, immediately before presenting it. A successful
    /// call must be followed by a buffer commit before dispatching or changing IME state.
    #[must_use]
    pub fn begin_frame(&mut self) -> bool {
        if !self.can_present() {
            return false;
        }
        if self.geometry_dirty {
            self.scale.apply(self.wl_surface(), self.logical);
            self.geometry_dirty = false;
        }
        let surface = self.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
        true
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

impl HasDisplayHandle for Platform {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.backend.display_handle()
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        self.loop_handle.remove(self.source);
        drop(self.keyboard.take());
        if let Some(manager) = &self.text_input_manager {
            manager.destroy();
        }
        // The Scale field destroys its extensions before Window destroys the xdg role and wl_surface.
    }
}

impl WindowHandler for Platform {
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
        self.events.push_back(Event::Configured {
            logical,
            activated: configure.is_activated(),
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

impl CompositorHandler for Platform {
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

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlSurface, _: u32) {
        self.frame_pending = false;
        self.events.push_back(Event::FrameReady);
    }

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

impl OutputHandler for Platform {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for Platform {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Platform {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(Platform);
delegate_dispatch2!(Platform);
