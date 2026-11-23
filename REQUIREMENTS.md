# sumie requirements

## Ghostty performance baseline

Startup time and process memory must not exceed Ghostty on the same host. With
cursor blink off, the terminal must produce no idle frames in 60 seconds. This
baseline records those metrics for manual comparison; it is not a merge gate.

### Reproduce

On the baseline host, run from the repository root. The harness requires the
[private KWin runner](scripts/nested/README.md), libwayland 1.26 timestamp format,
Ghostty at `/scratch/ssd/sumie-p0/run-ghostty.sh`, and an existing artifact
directory outside the checkout. Build before starting the benchmark so the build
cannot affect the measurements.

```sh
CARGO_TARGET_DIR=/scratch/ssd/sumie-target-baseline cargo build --release --locked
scripts/perf/baseline.sh /absolute/path/to/artifacts
```

The harness prints a Markdown table and keeps the protocol traces, per-process
`smaps_rollup` snapshots, and virtual output mode in a unique artifact directory.
It warms both clients once, then alternates ten fresh processes per app in the
same private KWin instance without clearing caches. Each client uses the
comparison font and palette, prints one line, and sleeps. Ghostty sets
`cursor-style-blink=false` and disables shell integration; the workload selects
a steady block cursor in both terminals. sumie does not implement cursor blink.

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

### Recorded run

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
