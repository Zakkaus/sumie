#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
use std::fmt;

#[cfg(target_os = "linux")]
use smithay_client_toolkit::{
    reexports::{calloop::EventLoop, client::protocol::wl_shm},
    shm::slot::SlotPool,
};
#[cfg(target_os = "linux")]
use sumie_wayland::{CursorRect, Event, Platform};

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let mut event_loop = EventLoop::try_new()?;
    let mut platform = Platform::connect(&event_loop, "sumie Wayland example")?;
    platform.set_cursor_rectangle(CursorRect {
        x: 8,
        y: 8,
        width: 1,
        height: 18,
    });
    let mut pool = SlotPool::new(1, platform.shm())?;
    let mut redraw = None;

    loop {
        event_loop.dispatch(None, &mut platform)?;
        for event in platform.drain_events() {
            log_event(&event);
            match event {
                Event::Resized {
                    logical, device, ..
                } => redraw = Some((logical, device)),
                Event::CloseRequested => return Ok(()),
                _ => {}
            }
        }

        if platform.can_present()
            && let Some((logical, device)) = redraw.take()
        {
            present_color(&mut platform, &mut pool, logical, device)?;
        }
    }
}

#[cfg(target_os = "linux")]
fn log_event(event: &Event) {
    tracing::info!(event = ?EventMetadata(event));
}

#[cfg(target_os = "linux")]
struct EventMetadata<'a>(&'a Event);

#[cfg(target_os = "linux")]
impl fmt::Debug for EventMetadata<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Event::Configured {
                logical,
                activated,
                maximized,
                fullscreen,
                server_decorated,
            } => f
                .debug_struct("Configured")
                .field("logical", logical)
                .field("activated", activated)
                .field("maximized", maximized)
                .field("fullscreen", fullscreen)
                .field("server_decorated", server_decorated)
                .finish(),
            Event::Resized {
                logical,
                device,
                scale,
            } => f
                .debug_struct("Resized")
                .field("logical", logical)
                .field("device", device)
                .field("scale", scale)
                .finish(),
            Event::FrameReady => f.write_str("FrameReady"),
            Event::CloseRequested => f.write_str("CloseRequested"),
            Event::Key {
                pressed,
                repeat,
                utf8,
                ..
            } => f
                .debug_struct("Key")
                .field("pressed", pressed)
                .field("repeat", repeat)
                .field("bytes", &utf8.as_ref().map_or(0, String::len))
                .finish(),
            Event::ImePreedit { text, .. } => write!(f, "ImePreedit {{ bytes: {} }}", text.len()),
            Event::ImeCommit { text } => write!(f, "ImeCommit {{ bytes: {} }}", text.len()),
        }
    }
}

#[cfg(target_os = "linux")]
fn present_color(
    platform: &mut Platform,
    pool: &mut SlotPool,
    (logical_width, logical_height): (u32, u32),
    (width, height): (u32, u32),
) -> Result<(), Box<dyn std::error::Error>> {
    let stride = i32::try_from(width.checked_mul(4).ok_or("buffer stride overflow")?)?;
    let (buffer, pixels) = pool.create_buffer(
        i32::try_from(width)?,
        i32::try_from(height)?,
        stride,
        wl_shm::Format::Xrgb8888,
    )?;
    pixels
        .as_chunks_mut::<4>()
        .0
        .fill(0x0024_5464_u32.to_ne_bytes());
    if platform.begin_frame() {
        let surface = platform.wl_surface();
        buffer.attach_to(surface)?;
        surface.damage(
            0,
            0,
            i32::try_from(logical_width)?,
            i32::try_from(logical_height)?,
        );
        surface.commit();
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("The Wayland window example requires Linux.");
    std::process::ExitCode::FAILURE
}
