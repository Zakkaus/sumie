//! The terminal driver under the runner's controller: pings, typed text, window steps and
//! counters, and one line per present for the frame-slot checks.

use std::{ops::ControlFlow, time::Instant};

use sumie_macos::{
    Event, Platform, WindowContext,
    runner::{self as native, Command},
};

use super::super::driver::{Driver, State};
use crate::{Result, capture};

/// The probe window's size in points: small enough to drag on the smallest panel.
const SIZE: [u32; 2] = [640, 400];

pub(in crate::macos) fn run(capture: capture::Options) -> Result<()> {
    let mut driver = Driver::new(capture);
    let started = Instant::now();
    let mut presented = 0;
    let result = native::run(|context, readiness, event| match event {
        Event::Resumed if matches!(driver.state, State::Starting) => {
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
            Ok(ControlFlow::Continue(()))
        }
        Event::RunnerCommand(command) => {
            self::command(&mut driver, context, command)?;
            Ok(ControlFlow::Continue(()))
        }
        event => {
            match &event {
                Event::Focused(focused) => println!("runner focused {focused}"),
                Event::Occluded(occluded) => println!("runner occluded {occluded}"),
                Event::Resized => println!("runner resized"),
                _ => (),
            }
            let flow = driver.handle(context, readiness, event)?;
            report_presents(&driver, started, &mut presented);
            Ok(flow)
        }
    });
    result?;
    println!("runner dropped");
    Ok(())
}

fn command(driver: &mut Driver, context: &WindowContext, command: Command) -> Result<()> {
    match command {
        Command::Ping(sequence) => println!("runner ack {sequence}"),
        Command::Close => {
            if !native::perform_close() {
                return Err("the terminal window is missing at close".into());
            }
        }
        command => {
            let State::Open(open) = &mut driver.state else {
                return Err("the terminal window is not open".into());
            };
            match command {
                Command::Type(text) => open.window.commit_text(text),
                Command::Report => {
                    let app = open.app.as_ref().ok_or("the settings have not arrived")?;
                    let gpu = app.gpu();
                    let stats = open.window.stats;
                    let [columns, rows] = app.snapshot_grid().unwrap_or_default();
                    println!(
                        "runner report {} {} {} {} {} {columns} {rows}",
                        gpu.present_count(),
                        gpu.configures.0,
                        gpu.configures.1,
                        stats.deferred_configures,
                        stats.timeouts,
                    );
                }
                Command::Step(step) => native::step(context, open.window.platform(), step),
                _ => return Err("unexpected runner controller command".into()),
            }
        }
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
