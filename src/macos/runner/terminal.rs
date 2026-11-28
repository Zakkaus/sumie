//! The terminal driver under the runner's controller: pings, typed text, window steps and
//! counters, and one line per present for the frame-slot checks. For the close and Quit
//! checks it also requests Quit, injects a fatal error or a stalled main thread, and closes
//! and reopens its window in one process.

use std::{
    ops::ControlFlow,
    thread,
    time::{Duration, Instant},
};

use sumie_macos::{
    Event, Platform, WindowContext,
    readiness::Readiness,
    runner::{self as native, Command},
};

use super::super::driver::{Driver, Open, State};
use crate::{Result, capture};

/// The probe window's size in points: small enough to drag on the smallest panel.
const SIZE: [u32; 2] = [640, 400];
/// How long a report waits for the session; the driver starts it by its first-result
/// deadline, so only a stuck start runs out.
const REPORT_WAIT: Duration = Duration::from_secs(5);
/// How long `stall` blocks the main thread: well past every drain deadline.
const STALL: Duration = Duration::from_secs(60);

/// What the controller asked for besides the driver's own state.
#[derive(Default)]
struct Control {
    /// Set while a report waits for the session.
    report: Option<Instant>,
    /// The closing window reopens once drained.
    cycle: bool,
    cycles: u64,
    presented: u64,
    /// Injected input-method events, reported once the batch that carries them is delivered.
    injected: Vec<String>,
}

pub(in crate::macos) fn run(capture: capture::Options) -> Result<()> {
    let mut driver = Driver::new(capture)?;
    let started = Instant::now();
    let mut control = Control::default();
    let result = native::run(|context, readiness, event| {
        let flow = match event {
            Event::Resumed if matches!(driver.state, State::Starting) => {
                open(&mut driver, context, readiness, started)?;
                return Ok(ControlFlow::Continue(()));
            }
            Event::RunnerCommand(command) => {
                let result = self::command(&mut driver, context, command, &mut control);
                driver.settle(context, readiness, result.map(ControlFlow::Continue))?
            }
            event => {
                let delivered = matches!(event, Event::AboutToWait);
                match &event {
                    Event::Focused(focused) => println!("runner focused {focused}"),
                    Event::Ime(ime) => println!("runner ime {}", native::describe(ime)),
                    Event::Occluded(occluded) => println!("runner occluded {occluded}"),
                    Event::Resized => println!("runner resized"),
                    _ => (),
                }
                let flow = driver.handle(context, readiness, event)?;
                if delivered {
                    for injected in control.injected.drain(..) {
                        println!("runner injected {injected}");
                    }
                }
                report_presents(&driver, started, &mut control.presented);
                let pending = pending_report(&driver, &mut control.report);
                driver.settle(context, readiness, pending.map(|()| flow))?
            }
        };
        if flow.is_break() && std::mem::take(&mut control.cycle) {
            driver.termination.cancel();
            driver = Driver::new(capture::Options::default())?;
            control.presented = 0;
            control.cycles += 1;
            println!("runner cycled {}", control.cycles);
            open(&mut driver, context, readiness, started)?;
            return Ok(ControlFlow::Continue(()));
        }
        Ok(flow)
    });
    result?;
    println!("runner dropped");
    Ok(())
}

fn open(
    driver: &mut Driver,
    context: &WindowContext,
    readiness: &Readiness,
    started: Instant,
) -> Result<()> {
    let platform = Platform::create(context, "sumie", SIZE)?;
    native::name_window(&platform);
    driver.open(readiness, platform)?;
    if let State::Open(open) = &mut driver.state {
        open.window.fixed_title = true;
        println!(
            "runner window {} {}",
            started.elapsed().as_nanos(),
            open.window.refresh().as_nanos()
        );
    }
    Ok(())
}

/// The key a person presses to answer the close confirmation: `y` confirms, Return cancels.
fn close_answer(yes: bool) -> Event {
    use sumie_core::{KeyAction, KeyCode, PhysicalKey};
    let (code, symbol) = if yes {
        (KeyCode::KeyY, sumie_macos::Symbol::Character('y'))
    } else {
        (KeyCode::Enter, sumie_macos::Symbol::Enter)
    };
    Event::Key(sumie_macos::Key {
        physical: PhysicalKey::Code(code),
        symbol,
        text: None,
        modifiers: sumie_core::KeyModifiers::default(),
        action: KeyAction::Press,
    })
}

fn command(
    driver: &mut Driver,
    context: &WindowContext,
    command: Command,
    control: &mut Control,
) -> Result<()> {
    match command {
        Command::Ping(sequence) => println!("runner ack {sequence}"),
        Command::Close | Command::Cycle => {
            control.cycle = command == Command::Cycle;
            if !native::perform_close() {
                return Err("the terminal window is missing at close".into());
            }
        }
        Command::RequestQuit(quit) => native::request_quit(context, quit),
        Command::Fail => return Err("injected fault".into()),
        Command::Stall => {
            println!("runner stalling");
            thread::sleep(STALL);
        }
        command => {
            let State::Open(open) = &mut driver.state else {
                return Err("the terminal window is not open".into());
            };
            match command {
                Command::Type(text) => open.window.commit_text(text),
                Command::CloseAnswer(yes) => open.window.queue(close_answer(yes)),
                Command::Report => {
                    if !print_report(open)? {
                        control
                            .report
                            .get_or_insert_with(|| Instant::now() + REPORT_WAIT);
                    }
                }
                Command::Step(step) => native::step(context, open.window.platform(), step)?,
                Command::Key(key) => println!("runner posted key {}", native::post_key(key)),
                Command::ImeAllowed(enabled) => open.window.set_ime(enabled),
                Command::InjectIme(ime) => {
                    control.injected.push(native::describe(&ime));
                    open.window.ime(ime);
                }
                Command::SearchThenPreedit(text) => {
                    control.injected.push("search".into());
                    open.window.search_then_preedit(text);
                }
                Command::Overlay => {
                    let preedit = open.app.as_ref().and_then(|app| app.preedit());
                    let (length, cursor) = preedit
                        .filter(|(text, _)| !text.is_empty())
                        .map_or((0, None), |(text, cursor)| (text.len(), cursor));
                    let (start, end) = cursor.map_or(("-".into(), "-".into()), |(start, end)| {
                        (start.to_string(), end.to_string())
                    });
                    println!(
                        "runner overlay {length} {start} {end} {}",
                        open.window.generation()
                    );
                }
                Command::InputSource => println!(
                    "runner source {}",
                    sumie_macos::Platform::input_source().unwrap_or_else(|| "-".into())
                ),
                Command::Mouse(mouse) => {
                    let scale = open.window.platform().scale();
                    let point = open
                        .app
                        .as_ref()
                        .and_then(|app| app.cell_center(mouse.cell))
                        .map(|backing| backing.map(|px| px / scale));
                    let posted = point.is_some_and(|point| native::post_mouse(mouse, point));
                    println!("runner posted mouse {posted}");
                }
                _ => return Err("unexpected runner controller command".into()),
            }
        }
    }
    Ok(())
}

/// Print the counters and the focused pane's shell state once the session exists; returns
/// whether it did. The OSC 7 URI is last, `-` before the shell reports one.
fn print_report(open: &Open) -> Result<bool> {
    let Some(app) = &open.app else {
        return Ok(false);
    };
    let Some((prompt, pwd)) = app.shell_state()? else {
        return Ok(false);
    };
    let gpu = app.gpu();
    let stats = open.window.stats;
    let [columns, rows] = app.snapshot_grid().unwrap_or_default();
    println!(
        "runner report {} {} {} {} {} {columns} {rows} {} {} {}",
        gpu.present_count(),
        gpu.configures.0,
        gpu.configures.1,
        stats.deferred_configures,
        stats.timeouts,
        u8::from(app.banner().is_some()),
        u8::from(prompt),
        if pwd.is_empty() { "-" } else { &pwd },
    );
    Ok(true)
}

/// Answer a report that waited for the session, or fail once it waited too long.
fn pending_report(driver: &Driver, report: &mut Option<Instant>) -> Result<()> {
    let Some(until) = *report else {
        return Ok(());
    };
    if let State::Open(open) = &driver.state
        && print_report(open)?
    {
        *report = None;
    } else if Instant::now() >= until {
        return Err(format!("the session did not start within {REPORT_WAIT:?} of a report").into());
    }
    Ok(())
}

fn report_presents(driver: &Driver, started: Instant, presented: &mut u64) {
    let State::Open(open) = &driver.state else {
        return;
    };
    let Some(app) = &open.app else {
        return;
    };
    let count = app.gpu().present_count();
    if count != *presented {
        *presented = count;
        let at = started.elapsed().as_nanos();
        if count == 1 {
            println!("runner first {at}");
        }
        println!("runner present {count} {at}");
    }
}
