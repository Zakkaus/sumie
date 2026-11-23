use std::{sync::Arc, time::Duration};

use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, Proxy, QueueHandle, globals::GlobalList, protocol::wl_surface::WlSurface,
};
use wayland_backend::client::ObjectData;
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};

use crate::{Event, platform::State};

/// The compositor's one-shot result for a submitted frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationFeedback {
    Presented {
        /// POSIX clock ID for `timestamp`, as advertised by the compositor.
        clock_id: u32,
        timestamp: Duration,
        /// Predicted refresh interval in nanoseconds, or zero if unknown.
        refresh_ns: u32,
        /// Bit mask from `wp_presentation_feedback.kind`, including unknown bits.
        flags: u32,
    },
    Discarded,
}

pub(crate) struct Presentation {
    proxy: WpPresentation,
    feedback_data: Arc<dyn ObjectData>,
    clock_id: u32,
    pending: usize,
    deferred_surface_commit: bool,
}

impl Presentation {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Self> {
        Some(Self {
            proxy: globals.bind(qh, 1..=2, ()).ok()?,
            feedback_data: qh.make_data::<WpPresentationFeedback, _>(()),
            clock_id: 0,
            pending: 0,
            deferred_surface_commit: false,
        })
    }

    pub(crate) fn request(&mut self, surface: &WlSurface) {
        // The protocol requires a fresh object for each commit. Reuse its queue data.
        let request = self.proxy.send_constructor::<WpPresentationFeedback>(
            wp_presentation::Request::Feedback {
                surface: surface.clone(),
            },
            Arc::clone(&self.feedback_data),
        );
        self.pending += usize::from(request.is_ok());
        self.deferred_surface_commit = false;
    }

    pub(crate) fn defer_surface_commit(&mut self) -> bool {
        // A state-only commit can replace the frame waiting to be shown.
        self.deferred_surface_commit = self.pending != 0;
        self.deferred_surface_commit
    }

    fn complete(&mut self) -> bool {
        self.pending -= 1;
        self.pending == 0 && std::mem::take(&mut self.deferred_surface_commit)
    }
}

impl Drop for Presentation {
    fn drop(&mut self) {
        self.proxy.destroy();
    }
}

impl Dispatch<WpPresentation, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event
            && let Some(presentation) = &mut state.presentation
        {
            presentation.clock_id = clk_id;
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for State {
    fn event(
        state: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let feedback = match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => PresentationFeedback::Presented {
                clock_id: state
                    .presentation
                    .as_ref()
                    .expect("bound presentation")
                    .clock_id,
                timestamp: Duration::new(
                    u64::from(tv_sec_hi) << 32 | u64::from(tv_sec_lo),
                    tv_nsec,
                ),
                refresh_ns: refresh,
                flags: flags.into(),
            },
            wp_presentation_feedback::Event::Discarded => PresentationFeedback::Discarded,
            _ => return,
        };
        #[cfg(debug_assertions)]
        if let Some(log) = &mut state.present_log {
            log.event(match feedback {
                PresentationFeedback::Presented { .. } => "presented",
                PresentationFeedback::Discarded => "discarded",
            });
        }
        state.events.push_back(Event::Presentation(feedback));
        if state
            .presentation
            .as_mut()
            .expect("bound presentation")
            .complete()
        {
            state.wl_surface().commit();
        }
    }
}

#[cfg(debug_assertions)]
pub(crate) struct PresentLog {
    file: Option<std::fs::File>,
    hidden: bool,
}

#[cfg(debug_assertions)]
impl PresentLog {
    pub(crate) fn open(supported: bool) -> std::io::Result<Option<Self>> {
        use std::io::Write;

        let Some(path) = std::env::var_os("SUMIE_PRESENT_LOG") else {
            return Ok(None);
        };
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(if supported {
            b"ready\n"
        } else {
            b"unsupported\n"
        })?;
        Ok(Some(Self {
            file: Some(file),
            hidden: false,
        }))
    }

    pub(crate) fn visibility(&mut self, hidden: bool) {
        if self.hidden != hidden {
            self.hidden = hidden;
            self.event(if hidden { "hidden" } else { "visible" });
        }
    }

    fn event(&mut self, event: &str) {
        use std::io::Write;

        if let Some(file) = &mut self.file {
            // Use the watchdog's clock, not the compositor's presentation clock.
            let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
            let ns = i128::from(now.tv_sec) * 1_000_000_000 + i128::from(now.tv_nsec);
            let mut bytes = [0; 64];
            let mut remaining = bytes.as_mut_slice();
            writeln!(remaining, "{event} {ns}").expect("presentation log line fits");
            let len = 64 - remaining.len();
            if let Err(error) = file.write_all(&bytes[..len]) {
                tracing::warn!(%error, "presentation log stopped");
                self.file = None;
            }
        }
    }
}
