# sumie requirements

## Ghostty performance baseline

Startup time and process memory must not exceed Ghostty on the same host. With
cursor blink off, the terminal must produce no idle frames in 60 seconds. This
baseline records those metrics for manual comparison; it is not a merge gate.

### Reproduce

On the baseline host, run from the repository root. The harness uses virtual
KWin at scale 1.25 in a private runtime without a D-Bus session. It requires
`/scratch/ram`, KWin, `kscreen-doctor`, Python 3.11+, GNU `timeout`, libwayland
1.26 timestamp format, Ghostty at `/scratch/ssd/sumie-p0/run-ghostty.sh`, and an
existing artifact directory outside the checkout. Build before the benchmark so
the build cannot affect the measurements.

```sh
export CARGO_TARGET_DIR=/scratch/ssd/sumie-target-baseline
cargo build --release --locked
scripts/perf/baseline.sh /absolute/path/to/artifacts
```

The harness prints a Markdown table and keeps the protocol traces, per-process
`smaps_rollup` snapshots, and virtual output mode in a unique artifact directory.
It warms both clients once, then alternates ten fresh processes per app in the
same private KWin instance without clearing caches. Each client uses the
comparison font and palette, prints one line, and sleeps. Ghostty sets
`cursor-style-blink=false` and disables shell integration; the workload selects
a steady block cursor in both terminals.

Pass `--startup-only` after the artifact directory to repeat the ten startup
samples without collecting idle or memory results.

### Measurement

- **Startup:** Timestamp immediately before exec of each launch command (the
  Ghostty command is a shell launcher). Use libwayland's `WAYLAND_DEBUG=1`
  timestamps and identify the terminal surface through `xdg_surface.get_toplevel`.
  Use the first `wp_presentation_feedback.presented` on that surface when both
  clients provide feedback. Otherwise use the first `wl_surface.commit` carrying
  a non-null buffer after configure acknowledgment for both. A commit is a
  submission, not proof of display.
- **Idle:** Start the 60-second window five seconds after the startup event.
  Count presents or, in commit mode, all commits on the terminal surface. Exclude
  other surfaces, protocol objects and feedback discards.
- **Memory:** Read `Pss` and `Rss` from the main terminal process's
  `/proc/<pid>/smaps_rollup` at the start of that window. Exclude the shell,
  compositor and GPU allocations not accounted for in the process rollup.

Report arithmetic means and Ghostty's sample standard deviation. The two-sided
reference band is Ghostty's mean ± the larger of twice that deviation and the
measurement resolution: 0.001 ms (client timestamp), 1 KiB (PSS report), and
4 KiB (host page size for RSS). Values beyond the lower edge also require a
baseline update when enforced. Idle is an exact zero-count requirement, not a
statistical band. The virtual refresh period is not the resolution of the
protocol timestamps.

### Original commit-based run

- Date: 2026-11-23. sumie commit: `797f9581467a24da98fecfbedd2bf0f41ac03738`.
- Host: AMD Ryzen 9 9950X3D; NVIDIA GeForce RTX 4080 SUPER, driver `620.73.04`;
  Radeon 610M also installed (`amdgpu`). Linux `7.3.5-gentoo-cjk-zakk-scx`.
- KWin 6.8.3 virtual output: 1600 × 1000 at 60 Hz, 1280 × 800 logical, scale 1.25.
  libwayland 1.26.0. CPU governor `powersave` (0.624–5.756 GHz policy limits).
- Ghostty `1.4.0-dev+0000000` (GTK 4.22.5, OpenGL/NVIDIA); binary SHA-256
  `b43927ff732f3db526cc5a1043b5241acedef7904cf3646f16a55e6742acf94e`.
- Both clients submitted 1000 × 750 buffers with NVIDIA DMA-BUF modifiers.
- One-minute host load average at sample end: 14.48–47.61 (32 logical CPUs).

Ghostty requested and received presentation feedback on its terminal surface;
sumie did not. The comparison uses commits for both clients. The table also
reports Ghostty's first `presented` separately. With cursor blink disabled,
Ghostty still submitted buffers roughly every 600 ms. Its renderer timer wakes
the renderer on that interval independently of the cursor's blinking state.
The idle count measures submissions, not visible changes.

| Metric | Ghostty mean | sumie mean | Ghostty sample SD | Two-sided tolerance |
| --- | ---: | ---: | ---: | ---: |
| Startup to first buffered commit (ms) | 372.264 | 263.396 | 237.861 | ±475.722 |
| Idle commits / 60 s | 99.900 | 0.000 | 0.316 | 0 (exact) |
| PSS (KiB) | 100904.400 | 71236.800 | 5307.205 | ±10614.411 |
| RSS (KiB) | 153636.800 | 117236.400 | 2188.625 | ±4377.250 |
| Startup to first presented (ms; not comparable) | 397.570 | n/a | 230.395 | ±460.790 |

The startup difference is within the reference band. Both sumie memory means
are below the lower edge. All ten sumie idle counts were zero; Ghostty recorded
99–100 commits per run.

### Presentation-based startup run

On 2026-11-23, both clients received `presented` on the terminal surface in
all ten measured launches. The run used the same host and virtual output as
above, with sumie built in release mode with debug assertions and no debug
presentation log. The one-minute host load average at sample end was
31.54–33.76. This run measured startup only.

| Metric | Ghostty mean | sumie mean | Ghostty sample SD | Two-sided tolerance |
| --- | ---: | ---: | ---: | ---: |
| Startup to first presented (ms) | 171.363 | 163.374 | 11.495 | ±22.990 |

## Presentation stall watchdog

From the repository root, build an optimized binary with debug assertions and
run the check in the same private KWin setup as the startup baseline:

```sh
export CARGO_TARGET_DIR=/scratch/ssd/sumie-target-present
CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true cargo build --release --locked
SUMIE_BIN="$CARGO_TARGET_DIR/release/sumie" scripts/perf/stall.sh /absolute/path/to/artifacts
```

The check defaults to 60 seconds. It launches one sumie window with a pane that
prints a changing line every 10 ms. The independent watchdog starts before sumie
launches and reports the wait for the first visible `presented` as startup time.
The stall check passes only if it observes a visible `presented` and no visible
gap after it exceeds 100 ms, including the tail at test end. A `discarded`
does not reset the clock. The watchdog reports suspended intervals separately
and starts a new visible interval on restore.

`SUMIE_PRESENT_LOG` appends feedback receipt and visibility transitions with
`CLOCK_MONOTONIC` timestamps in nanoseconds. The log is available only in
builds with debug assertions. The watchdog keeps it, the client log, and the
virtual output mode under the supplied artifact directory.

The full gate is four hours (`14400` seconds) with 20 panes producing output.
Pass `14400` as the second argument to extend the single-pane check to four
hours; that run does not cover the full gate's 20-pane workload.

### Recorded 60-second check

On 2026-11-23, virtual KWin 6.8.3 at 60 Hz and scale 1.25 reported 3,586
`presented`, zero `discarded`, and no suspended intervals. The one-minute host
load average at launch was 31.54. The original watchdog failed because it counted
the 194.098 ms startup against the stall limit. The maximum after the first
`presented` was 52.826 ms, within the stall limit. This was not the four-hour gate.

## Pane open/close leak gate

`scripts/perf/panes.sh /absolute/path/to/artifacts` (the `linux-panes` gate)
launches one debug sumie window in the private KWin at scale 1.25 with a
private `HOME`, so no user rc file or configuration is read. It opens and
closes a split pane 200 times, then a tab 200 times, and samples
`/proc/<pid>` at every 50 cycles after waiting up to 6 s for quiescence. It
then closes a pane whose shell runs `trap '' HUP; sleep 1000` while a sibling
pane draws and checks the escalation: the shell must be reaped within 6 s (the
default grace is 3 s) and the longest gap between `presented` entries in the
close window must stay within 100 ms.

The gate fails when a count exceeds the baseline taken after a warm-up of five
pane and three tab cycles, or when RSS grows more than 12 MiB over it. DRM
client memory is compared only where the driver exposes `drm-resident-*` or
`drm-memory-*` in fdinfo. NVIDIA's proprietary driver on the development host
does not, so that column reads `n/a` there.
