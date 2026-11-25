//! Quit requests from the runner's controller: the Quit menu item's `terminate:`, or the quit
//! Apple event loginwindow sends.

use std::process;

use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventSendOptions};
use winit::platform::macos::ActiveEventLoopExtMacOS as _;

use super::{Quit, Result, WindowContext, app};

/// Ask `AppKit` to quit once no winit handler is running. A system quit sends this process the
/// quit Apple event with `keyAEQuitReason` = `kAEReallyLogOut`, as loginwindow does; `AppKit`'s
/// wait for the reply then runs inside this closure.
pub fn request_quit(context: &WindowContext, quit: Quit) {
    context.run_after_handler(move || match quit {
        Quit::Ordinary => {
            if let Some(app) = app() {
                app.terminate(None);
            }
        }
        Quit::System => {
            if let Err(error) = send_system_quit() {
                tracing::error!(%error, "could not send the quit Apple event");
            }
        }
    });
}

fn send_system_quit() -> Result<()> {
    const fn code(bytes: [u8; 4]) -> u32 {
        u32::from_be_bytes(bytes)
    }
    let pid = i32::try_from(process::id())?;
    let target = NSAppleEventDescriptor::descriptorWithProcessIdentifier(pid);
    let event =
        NSAppleEventDescriptor::appleEventWithEventClass_eventID_targetDescriptor_returnID_transactionID(
            code(*b"aevt"),
            code(*b"quit"),
            Some(&target),
            -1, // kAutoGenerateReturnID
            0,  // kAnyTransactionID
        );
    event.setAttributeDescriptor_forKeyword(
        &NSAppleEventDescriptor::descriptorWithTypeCode(code(*b"rlgo")),
        code(*b"why?"),
    );
    let options = NSAppleEventSendOptions::NoReply | NSAppleEventSendOptions::NeverInteract;
    event
        .sendEventWithOptions_timeout_error(options, 10.0)
        .map_err(|error| error.localizedDescription().to_string())?;
    Ok(())
}
