#![forbid(unsafe_code)]

#[cfg(target_os = "linux")]
use std::fmt;

#[cfg(target_os = "linux")]
use sumie_wayland::{CursorRect, Event, Platform};

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let mut platform = Platform::connect("sumie Wayland example")?;
    platform.set_cursor_rectangle(CursorRect {
        x: 8,
        y: 8,
        width: 1,
        height: 18,
    });
    let mut redraw = false;

    loop {
        platform.dispatch(None)?;
        for event in platform.drain_events() {
            log_event(&event);
            match event {
                Event::Resized { .. } => redraw = true,
                Event::CloseRequested => return Ok(()),
                _ => {}
            }
        }

        if redraw {
            redraw = !platform.present_shm(|pixels| {
                pixels
                    .as_chunks_mut::<4>()
                    .0
                    .fill(0x0024_5464_u32.to_ne_bytes());
            })?;
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
            Event::ClipboardText(text) => write!(f, "ClipboardText {{ bytes: {} }}", text.len()),
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
            Event::Pointer {
                phase, position, ..
            } => f
                .debug_struct("Pointer")
                .field("phase", phase)
                .field("position", position)
                .finish(),
            Event::ImePreedit { text, .. } => write!(f, "ImePreedit {{ bytes: {} }}", text.len()),
            Event::ImeCommit { text } => write!(f, "ImeCommit {{ bytes: {} }}", text.len()),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("The Wayland window example requires Linux.");
    std::process::ExitCode::FAILURE
}
