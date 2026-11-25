//! Runs the actual smoke window with a fixed fixture and an external controller.

use std::{
    env,
    ops::ControlFlow,
    time::{Duration, Instant},
};

use sumie_macos::{Event, Platform, WindowContext, readiness::Readiness, runner as native};
use sumie_render::wgpu;

use super::Smoke;
use crate::{Result, capture};

mod terminal;
mod wake;

const LENGTH: Duration = Duration::from_secs(60);
const FIXTURE: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 0.0,
    b: 1.0,
    a: 1.0,
};

pub(super) enum Mode {
    Timing(Option<Sample>),
    Capabilities,
    Wake(Option<wake::Wake>),
    /// One frame, then pings and window steps while `AppKit` animates.
    Idle,
    /// The terminal driver, with typed text, counters and one line per present.
    Terminal,
}

impl Mode {
    pub(super) fn from_env() -> Result<Option<Self>> {
        match env::var("SUMIE_MACOS_CI_RUNNER") {
            Ok(value) if value == "timing" => Ok(Some(Self::Timing(None))),
            Ok(value) if value == "capabilities" => Ok(Some(Self::Capabilities)),
            Ok(value) if value == "wake" => Ok(Some(Self::Wake(None))),
            Ok(value) if value == "idle" => Ok(Some(Self::Idle)),
            Ok(value) if value == "terminal" => Ok(Some(Self::Terminal)),
            Err(env::VarError::NotPresent) => Ok(None),
            _ => Err("invalid SUMIE_MACOS_CI_RUNNER mode".into()),
        }
    }
}

pub(super) fn run(mode: Mode, options: capture::Options) -> Result<()> {
    if options.frames.is_some() || options.screenshot.is_some() {
        return Err("the runner probe cannot be combined with frame limits or GPU readback".into());
    }
    if let Mode::Terminal = mode {
        return terminal::run(options);
    }
    let mut driver = Driver {
        mode,
        state: None,
        started: Instant::now(),
        close_requested: false,
        closed: false,
    };
    let result =
        native::run(|context, readiness, event| driver.dispatch(context, readiness, event));
    drop(driver.state.take());
    result?;
    if !driver.closed {
        return Err("winit exited without handling the native close request".into());
    }
    println!("runner dropped");
    Ok(())
}

struct Driver {
    mode: Mode,
    state: Option<Smoke>,
    started: Instant,
    close_requested: bool,
    closed: bool,
}

impl Driver {
    fn dispatch(
        &mut self,
        context: &WindowContext,
        readiness: &Readiness,
        event: Event,
    ) -> Result<ControlFlow<()>> {
        match event {
            Event::Resumed if matches!(self.mode, Mode::Wake(None)) => {
                let window = Platform::create(context, "sumie", super::SMOKE_SIZE)?;
                native::name_window(&window);
                self.mode = Mode::Wake(Some(wake::Wake::start(context, readiness, window)?));
            }
            Event::Resumed if self.state.is_none() && !matches!(self.mode, Mode::Wake(_)) => {
                let window = Platform::create(context, "sumie", super::SMOKE_SIZE)?;
                native::name_window(&window);
                println!("runner window {}", self.started.elapsed().as_nanos());
                window.request_redraw();
                self.state = Some(Smoke::new(window, readiness, false)?);
            }
            Event::Ready => {
                if let Mode::Wake(Some(wake)) = &mut self.mode
                    && wake.ready()?.is_break()
                {
                    self.closed = true;
                    return Ok(ControlFlow::Break(()));
                }
                // The runner closes through its controller, never through `Smoke::close`.
                if let Some(smoke) = &mut self.state {
                    let _ = smoke.ready()?;
                }
            }
            Event::Resized | Event::Occluded(false) => {
                if let Some(smoke) = &self.state {
                    smoke.window.request_redraw();
                }
            }
            Event::RedrawRequested => {
                if let Some(smoke) = &mut self.state {
                    repaint(&mut self.mode, smoke, self.started)?;
                }
            }
            Event::RunnerCommand(command) => self.command(context, command)?,
            Event::RunnerKey(key) if matches!(self.mode, Mode::Capabilities) => {
                let direction = if key.pressed { "down" } else { "up" };
                println!("runner key {} {direction}", key.route.label());
            }
            Event::CloseRequested if self.close_requested => {
                self.closed = true;
                println!("runner close");
                return Ok(ControlFlow::Break(()));
            }
            Event::CloseRequested => return Err("unexpected native close".into()),
            _ => (),
        }
        Ok(ControlFlow::Continue(()))
    }

    fn command(&mut self, context: &WindowContext, command: native::Command) -> Result<()> {
        use native::Command;
        match (&self.mode, command) {
            (_, Command::Close) if !self.close_requested => {
                self.close_requested = true;
                if !native::perform_close() {
                    return Err("native probe window is missing at close".into());
                }
            }
            (Mode::Timing(_) | Mode::Idle, Command::Ping(sequence)) => {
                println!("runner ack {sequence}");
            }
            (Mode::Timing(Some(sample)), Command::Finish) => sample.report()?,
            (Mode::Idle, Command::Step(step)) => {
                let window = &self.state.as_ref().ok_or("no window for the step")?.window;
                println!("runner step {step:?} {}", self.started.elapsed().as_nanos());
                native::step(context, window, step);
            }
            (Mode::Capabilities, Command::Inspect) => report_capabilities(),
            (Mode::Capabilities, Command::AppKitKeys) => {
                println!("runner posted appkit {}", native::post_appkit_keys());
            }
            (Mode::Capabilities, Command::CgKeys) => {
                println!("runner posted cg {}", native::post_cg_keys());
            }
            _ => return Err("unexpected runner controller command".into()),
        }
        Ok(())
    }
}

fn report_capabilities() {
    let result = native::inspect();
    println!(
        "runner capabilities {} {} {} {} {} {} {}",
        result.window.id,
        result.window.window_server,
        result.window.key,
        result.window.responder,
        result.pasteboard,
        result.screen_capture_access,
        result.post_event_access,
    );
}

fn repaint(mode: &mut Mode, smoke: &mut Smoke, started: Instant) -> Result<()> {
    if let Mode::Timing(Some(sample)) = mode
        && Instant::now() >= sample.end()
    {
        return Ok(());
    }
    let count = smoke.surface.present_count();
    let clear = match mode {
        Mode::Capabilities | Mode::Wake(_) | Mode::Idle | Mode::Terminal => FIXTURE,
        Mode::Timing(_) if count.is_multiple_of(2) => wgpu::Color::RED,
        Mode::Timing(_) => wgpu::Color::BLUE,
    };
    let mut presented = None;
    let _ = smoke.draw_with(&capture::Options::default(), Some(clear), || {
        presented = Some(Instant::now());
    })?;
    let Some(at) = presented else {
        return Ok(());
    };
    if count == 0 {
        println!("runner first {}", at.duration_since(started).as_nanos());
    }
    if let Mode::Timing(sample) = mode {
        let sample = sample.get_or_insert_with(|| Sample::new(at));
        if count != 0 && at <= sample.end() {
            sample.record(at);
        }
        if at < sample.end() {
            smoke.window.request_redraw();
        }
    }
    Ok(())
}

pub(super) struct Sample {
    first: Instant,
    last: Instant,
    total: u64,
    max_interval: Duration,
}

impl Sample {
    const fn new(first: Instant) -> Self {
        Self {
            first,
            last: first,
            total: 1,
            max_interval: Duration::ZERO,
        }
    }

    fn end(&self) -> Instant {
        self.first + LENGTH
    }

    fn record(&mut self, at: Instant) {
        self.max_interval = self.max_interval.max(at.duration_since(self.last));
        self.last = at;
        self.total += 1;
    }

    fn intervals(&self) -> (Duration, Duration) {
        let tail = self.end().duration_since(self.last);
        (self.max_interval.max(tail), tail)
    }

    fn report(&self) -> Result<()> {
        if Instant::now() < self.end() {
            return Err("controller ended the timing sample before 60 seconds".into());
        }
        let (max_interval, tail) = self.intervals();
        println!(
            "runner sample {} {} {} {}",
            self.total,
            max_interval.as_nanos(),
            tail.as_nanos(),
            LENGTH.as_nanos(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_gap_includes_the_tail() {
        let start = Instant::now();
        let mut sample = Sample::new(start);
        sample.record(start + Duration::from_secs(25));
        let tail = Duration::from_secs(35);
        assert_eq!(sample.intervals(), (tail, tail));
    }
}
