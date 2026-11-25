# macOS runner baseline

From the repository root, build a native arm64 release `sumie` with `--features macos-ci` before running the probe. On a hosted runner, use a fresh `macos-15` arm64 job for each sample. On an owner's Mac, `python3 scripts/macos-spike/baseline.py 5 "$EVIDENCE"` runs five samples in sequence and writes `baseline.json` with the median, minimum and maximum of launch to first present, the longest main-thread ping, the longest submission gap and close-to-reaped exit. Each sample opens and closes one window. The script expects `$CARGO_TARGET_DIR/release/sumie`, or `target/release/sumie` if the variable is unset. Set `SUMIE_MACOS_SPIKE_BIN` to override the path. Pass an evidence directory outside the checkout:

```sh
bash scripts/macos-spike/runner.sh "$EVIDENCE"
```

The script builds the Swift pixel verifier before starting either process. It writes three files:

| File | Evidence |
|---|---|
| `runner-environment.json` | Runner image and version, macOS build, architecture, logical CPU count, and 1-, 5-, and 15-minute load averages before and after each process. |
| `runner-timing.json` | Window startup, first submission, the longest submission interval over 60 seconds including the tail, numbered heartbeats, and native close through observed process exit. All times use nanoseconds. |
| `runner-capabilities.json` | WindowServer, key window, content-view first responder, named pasteboard, separate TCC preflights, and observed native event delivery and pixel capture. |

## Interpret the timing sample

Timing uses the same winit window and Metal renderer as the default smoke page. The clear color alternates on each submission, without GPU readback, PNG work, or terminal output. The measured interval starts just after the renderer's first successful presentation call and ends 60 seconds later. The maximum includes the time from the last in-range submission to that endpoint. The renderer's presentation call is not proof of when a frame reached the display.

The controller sends a ping every 50 ms through the end of the sample, for 1,201 scheduled pings. Only the winit main-thread callback can acknowledge it. Each `heartbeats` row records `[sequence, controller_send_ns_since_first_notice, round_trip_ns]`. Controller scheduling lateness is separate from round-trip time, which can include controller descheduling. The startup fields without `launch_to_` start at the smoke probe's Rust entry, while `launch_to_*` fields include process launch and controller receipt.

Close starts when the controller sends the native window action. The controller requires winit `CloseRequested`, return from the event loop, drop of the actual smoke state, and successful process reap. This is not a shell, PTY, or child-reaping test. Native Quit is separate: see `quit-deferred.log` and `quit-synchronous.log` in the `m1b0-quit-1` artifact.

The 100 ms submission and heartbeat targets and six-second close target are report fields, not hosted CI thresholds. Missing or out-of-order replies, a stalled process, missing teardown, or a nonzero exit fail the sample. Compare the three fresh jobs before changing a target into a hosted requirement.

## Interpret the capability matrix

Use the reported image version when comparing capabilities. The window is the actual smoke window created from the process main thread. The input probe requires that window to be key and its winit content view to be first responder; it records the known F13 down/up pair only after AppKit's `postEvent` reaches the winit handler. The separate Core Graphics test sends only an F14 pair to the smoke process PID, only if event posting is already permitted, and looks for that pair in the same handler. Neither path proves physical input or IME behavior.

The pasteboard test writes and clears a fixed sentinel on a unique named board. It never reads the general board. The screen capture test runs `screencapture -x -o -l` only after the screen capture preflight permits it, targets only the fixture window, and validates a magenta patch after color-space conversion. It preserves no captured PNG. Neither preflight requests access or changes TCC. A preflight success or successful API return alone is not delivery evidence.

`partial` explicitly identifies an unsupported capability; it is not native acceptance. WindowServer is required. Once the baseline establishes more capabilities for an image, set `SUMIE_MACOS_SPIKE_REQUIRED` to their comma-separated matrix keys so regressions fail instead of passing as `partial`. Run the same unsupported native scenarios on the owner's machine before relying on them. This probe stores no pasteboard contents, typed text, or desktop images, and does not establish macOS 13 compatibility.
