import argparse
import bisect
import math
import os
import re
import shlex
import subprocess
import sys
import time

from session import artifact_dir, stop, sumie_binary


NS_PER_MS = 1_000_000
NS_PER_S = 1_000_000_000
LIMIT_NS = 100 * NS_PER_MS
# One writer period plus the stall limit.
REPAINT_LIMIT_NS = 200 * NS_PER_MS
WARMUP_NS = NS_PER_S
FOCUS_NS = NS_PER_S // 2
# The frame log flushes once per second; keep sumie alive long enough for the last batch.
SETTLE_S = 2.5
DAY_US = 86_400_000_000
STAMP = re.compile(r'\[(\d\d):(\d\d):(\d\d)\.(\d{6})\] (.*)')
PRESENTATION_DONE = re.compile(r'wp_presentation_feedback#\d+\.(presented|discarded)\(')
ANSI = re.compile(r'\x1b\[[0-9;]*m')
CURRENT_MODE = re.compile(r'\d+:(\d+)x(\d+)@([\d.]+)\*')
FRAME_HEADER = 'sumie-frame-log 1 monotonic-ns'
SPLIT_RIGHT, SPLIT_DOWN = 'ctrl+shift+e', 'ctrl+shift+o'

# Each pane's login shell. It claims the next free index and prints one changing line
# every 100 ms (the design book's soak workload), stamping the clock before and after it.
# A loaded host can preempt the writer for 100 ms inside that span, so the bytes reached
# the PTY somewhere between the stamps: a frame counts only if it snapshotted after the
# first, and a wait starts at the second.
WRITER = '''\
import itertools, os, signal, time
directory = os.environ['SUMIE_STALL_WRITERS']
for index in itertools.count():
    try:
        fd = os.open(os.path.join(directory, str(index)), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        break
    except FileExistsError:
        pass
stop = os.path.join(directory, 'stop')
with os.fdopen(fd, 'w', buffering=1) as stamps:
    due = time.monotonic()
    for line in itertools.count():
        if os.path.exists(stop):
            break
        before = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
        print(f'writer {index} line {line}', flush=True)
        stamps.write(f'{line} {before} {time.clock_gettime_ns(time.CLOCK_MONOTONIC)}\\n')
        due += 0.1
        time.sleep(max(0, due - time.monotonic()))
# Exiting would close the pane.
while True:
    signal.pause()
'''


def parse_args():
    parser = argparse.ArgumentParser(usage='stall.py ARTIFACT_DIR SECONDS [options]')
    parser.add_argument('parent')
    parser.add_argument('seconds', type=int)
    parser.add_argument('--panes', type=int, default=1, help='split the window into N writer panes')
    parser.add_argument('--refresh-hz', type=int, help='set the virtual output refresh rate first')
    parser.add_argument('--present-log', action='store_true',
                        help='read SUMIE_PRESENT_LOG (debug assertions) instead of WAYLAND_DEBUG')
    options = parser.parse_args()
    if options.seconds <= 0 or options.panes <= 0 or (options.refresh_hz or 1) <= 0:
        parser.error('SECONDS, --panes and --refresh-hz must be positive')
    return options


def grow(count, back, forward, split, steps):
    """Split one pane into COUNT slices along an axis, always halving the largest slice.
    Appends one chord list per split to STEPS; the new slice takes focus after a split."""
    sizes, focus = [1.0], 0
    while len(sizes) < count:
        largest = sizes.index(max(sizes))
        move = back if largest < focus else forward
        steps.append([move] * abs(largest - focus) + [split])
        sizes[largest] /= 2
        sizes.insert(largest + 1, sizes[largest])
        focus = largest + 1
    return focus


def layout_steps(panes):
    """Chords that build a grid of PANES panes in pane ID order, one list per split."""
    columns = max(1, round(math.sqrt(panes)))
    steps = []
    focus = grow(columns, 'ctrl+alt+left', 'ctrl+alt+right', SPLIT_RIGHT, steps)
    pending = ['ctrl+alt+left'] * focus
    for column in range(columns):
        if column:
            # Only the unsplit column lies to the right of the previous column's panes.
            pending.append('ctrl+alt+right')
        rows = panes // columns + (column < panes % columns)
        first = len(steps)
        grow(rows, 'ctrl+alt+up', 'ctrl+alt+down', SPLIT_DOWN, steps)
        if len(steps) > first:
            steps[first] = pending + steps[first]
            pending = []
    assert len(steps) == panes - 1
    return [','.join(step) for step in steps]


def configure_output(refresh_hz, output):
    env = dict(os.environ, QT_QPA_PLATFORM='wayland')

    def doctor(*args):
        text = subprocess.run(['kscreen-doctor', *args], check=True, timeout=10, env=env,
                              capture_output=True, text=True).stdout
        return ANSI.sub('', text)

    if refresh_hz:
        width, height, _ = CURRENT_MODE.search(doctor('-o')).groups()
        doctor(f'output.Virtual-0.addCustomMode.{width}.{height}.{refresh_hz * 1000}.full')
        doctor(f'output.Virtual-0.mode.{width}x{height}@{refresh_hz}')
    text = doctor('-o')
    output.write_text(text)
    mode = CURRENT_MODE.search(text)
    if not mode or (refresh_hz and abs(float(mode[3]) - refresh_hz) > 1):
        raise RuntimeError(f'virtual output is not at {refresh_hz} Hz; see {output}')
    return f'{mode[1]}x{mode[2]}@{mode[3]}'


def trace_events(log, launch_us, start):
    # Release builds have no presentation log. Their WAYLAND_DEBUG trace carries the
    # same feedback events, stamped in UTC, so map them onto the watchdog's clock.
    while True:
        offset = log.tell()
        line = log.readline()
        if not line.endswith('\n'):
            log.seek(offset)
            return
        stamp = STAMP.match(line)
        if not stamp or not (done := PRESENTATION_DONE.search(stamp[5])):
            continue
        h, m, s, us = map(int, stamp.groups()[:4])
        elapsed_us = (((h * 60 + m) * 60 + s) * 1_000_000 + us - launch_us) % DAY_US
        yield f'{done[1]} {start + elapsed_us * 1000}'


class Watchdog:
    """Visible presentations from the compositor's feedback, independent of sumie's own log."""

    def __init__(self, start):
        self.start = start
        self.hidden_since = None
        self.hidden = []
        self.presents = []
        self.presented = 0
        self.discarded = 0
        self.ready = False

    @property
    def first_present_ns(self):
        return self.presents[0] - self.start if self.presents else None

    def read(self, log):
        for line in log:
            line = line.strip()
            if line == 'ready':
                self.ready = True
                continue
            if line == 'unsupported':
                raise RuntimeError('KWin does not advertise wp_presentation')
            event, stamp = line.split()
            stamp = int(stamp)
            if event == 'discarded':
                self.discarded += 1
            elif event == 'hidden':
                self.hidden_since = stamp
            elif event == 'visible':
                self.hidden.append((self.hidden_since, stamp))
                self.hidden_since = None
            elif event == 'presented':
                self.presented += 1
                if self.hidden_since is None:
                    self.presents.append(stamp)
            else:
                raise RuntimeError(f'unknown presentation log event: {event}')

    def finish(self, lines, window):
        """Each line written while the window was visible must be followed by a visible
        presentation within 100 ms. Returns the failure messages."""
        if not self.ready:
            raise RuntimeError('no presentation log header; build sumie with debug assertions')
        if self.hidden_since is not None:
            self.hidden.append((self.hidden_since, math.inf))
        if not self.presents:
            raise RuntimeError('no visible presentation')
        begin, end = window
        print(f'First visible presented: {self.first_present_ns / NS_PER_MS:.3f} ms')
        print(f'Presented: {self.presented}; discarded: {self.discarded}')
        hidden_ns = sum(min(b, end) - a for a, b in self.hidden if a < end)
        print(f'Invisible: {len(self.hidden)} periods, {hidden_ns / NS_PER_MS:.3f} ms')
        shown = [begin, *(t for t in self.presents if begin < t < end), end]
        print(f'Max visible presentation gap in window: '
              f'{max(b - a for a, b in zip(shown, shown[1:])) / NS_PER_MS:.3f} ms')
        wait = 0
        for before, after in lines:
            if begin <= before < end and not any(a <= before < b for a, b in self.hidden):
                at = bisect.bisect_right(self.presents, before)
                wait = max(wait, self.presents[at] - after if at < len(self.presents) else math.inf)
        print(f'Max output-to-presentation wait: {wait / NS_PER_MS:.3f} ms')
        if wait > LIMIT_NS:
            return [f'FAIL: output waited {wait / NS_PER_MS:.1f} ms for a visible presentation '
                    '(stall gap over 100 ms)']
        print('PASS: every line was presented within 100 ms')
        return []


class Run:
    """sumie under the watchdog, with writer files and a liveness check."""

    def __init__(self, client, watch, read_events, writers, client_log):
        self.client = client
        self.watch = watch
        self.read_events = read_events
        self.writers = writers
        self.client_log = client_log

    def wait_until(self, deadline_ns, ready=lambda: False):
        while not ready():
            self.read_events()
            left = deadline_ns - time.monotonic_ns()
            if left <= 0:
                return False
            try:
                status = self.client.wait(timeout=min(left / NS_PER_S, 0.02))
            except subprocess.TimeoutExpired:
                continue
            raise RuntimeError(f'sumie exited with status {status}; see {self.client_log}')
        return True

    def wait_for_writer(self, index, seconds):
        path = self.writers / str(index)
        if not self.wait_until(time.monotonic_ns() + seconds * NS_PER_S, path.exists):
            raise RuntimeError(f'pane {index} started no writer within {seconds} s')


def quantile(values, q):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(q * len(ordered)) - 1)] if ordered else 0


def read_frames(path):
    if not path.exists():
        return None
    lines = path.read_text().splitlines()
    if not lines or lines[0] != FRAME_HEADER:
        raise RuntimeError(f'{path}: unexpected frame log header')
    frames = []
    for line in lines[1:]:
        fields = line.split()
        if fields[0] != 'frame' or len(fields) < 7:
            raise RuntimeError(f'{path}: malformed record {line!r}')
        stamps = tuple(map(int, fields[1:6]))
        frames.append((stamps, int(fields[6]), {int(id) for id in fields[7:]}))
    return frames


def read_writer(path):
    """Each line's clock before and after its write."""
    lines = path.read_text().splitlines() if path.exists() else []
    return [tuple(map(int, line.split()[1:])) for line in lines]


def check_frames(frames, panes, writers, window):
    """Report frame CPU and per-pane repaint latency; return the failure messages."""
    if not frames:
        return ['FAIL: no frame log records; sumie ignored SUMIE_FRAME_LOG']
    begin, end = window
    failures = []
    measured = [f for f in frames if begin <= f[0][0] < end]
    cpu = [(s[4] - s[0]) / NS_PER_MS for s, _, _ in measured]
    stages = [[s[i + 1] - s[i] for s, _, _ in measured] for i in range(4)]
    print(f'Frames in window: {len(measured)} ({len(measured) / ((end - begin) / NS_PER_S):.1f}/s); '
          f'dirty panes per frame: {sum(f[1] for f in measured) / max(1, len(measured)):.2f}; '
          f'repainted per frame: {sum(len(f[2]) for f in measured) / max(1, len(measured)):.2f}')
    print(f'Frame CPU (start to present returned): p50 {quantile(cpu, .5):.3f} ms, '
          f'p99 {quantile(cpu, .99):.3f} ms, max {max(cpu, default=0):.3f} ms')
    print('Stage p50/p99 (ms): ' + ', '.join(
        f'{name} {quantile(values, .5) / NS_PER_MS:.3f}/{quantile(values, .99) / NS_PER_MS:.3f}'
        for name, values in zip(('snapshots', 'paint', 'acquire+submit', 'present'), stages)))

    ids = set().union(*(f[2] for f in frames))
    if ids != set(range(panes)):
        failures.append(f'FAIL: frame log pane IDs {sorted(ids)} are not the writers 0..{panes - 1}')
    advanced = [index for index in range(panes)
                if sum(begin <= t < end for t, _ in writers[index]) >= 2]
    print(f'Writers advanced: {len(advanced)} of {panes}')
    if len(advanced) < panes:
        failures.append(f'FAIL: {len(advanced)} of {panes} panes advanced')
    # Each pane's repaints, by the frame's snapshot time and its present time.
    repaints = {index: [(s[1], s[4]) for s, _, painted in frames if index in painted]
                for index in range(panes)}
    worst_latency, worst_gap = (0, None), (0, None)
    for index in advanced:
        lines = [line for line in writers[index] if line[0] >= begin]
        snaps = [snap for snap, _ in repaints[index]]
        latency = 0
        for before, after in lines:
            at = bisect.bisect_right(snaps, before)
            latency = max(latency, repaints[index][at][1] - after if at < len(snaps) else math.inf)
        shown = [done for _, done in repaints[index] if lines[0][0] <= done <= lines[-1][0]]
        gap = max((b - a for a, b in zip(shown, shown[1:])), default=math.inf)
        worst_latency = max(worst_latency, (latency, index))
        worst_gap = max(worst_gap, (gap, index))
        if latency > REPAINT_LIMIT_NS:
            failures.append(f'FAIL: pane {index} starved: a line waited '
                            f'{latency / NS_PER_MS:.1f} ms for its repaint')
    for name, (value, index) in (('line-to-repaint latency', worst_latency),
                                 ('repaint gap while writing', worst_gap)):
        print(f'Max per-pane {name}: {value / NS_PER_MS:.3f} ms (pane {index})')
    return failures


def main():
    options = parse_args()
    artifacts = artifact_dir(options.parent, 'stall.')
    print(f'Artifacts: {artifacts}', file=sys.stderr, flush=True)
    mode = configure_output(options.refresh_hz, artifacts / 'output.txt')
    injector = os.environ.get('SUMIE_NESTED_TYPE')
    steps = layout_steps(options.panes)
    if steps and not injector:
        raise RuntimeError('splitting panes needs the input helper; start with stall.sh')
    writers = artifacts / 'writers'
    writers.mkdir()
    workload = artifacts / 'writer'
    workload.write_text(f'#!{sys.executable} -B\n{WRITER}')
    workload.chmod(0o700)
    present_log = artifacts / 'present.log'
    client_log = artifacts / 'sumie.log'
    frame_log = artifacts / 'frames.log'
    env = dict(os.environ, SHELL=str(workload), SUMIE_STALL_WRITERS=str(writers),
               SUMIE_FRAME_LOG=str(frame_log))
    binary = sumie_binary('release')
    if options.present_log:
        env['SUMIE_PRESENT_LOG'] = str(present_log)
    else:
        # The trace has no visibility events, so hidden periods are not counted.
        env['WAYLAND_DEBUG'] = '1'
    source = 'presentation log' if options.present_log else 'WAYLAND_DEBUG trace (no visibility)'
    print(f'Duration: {options.seconds} s; panes: {options.panes}; output: {mode}; '
          f'sumie: {shlex.quote(str(binary))}; source: {source}; load: {os.getloadavg()}',
          flush=True)
    present_log.touch()
    with client_log.open('w') as output, (present_log if options.present_log else client_log).open(
            errors='replace') as log:
        start = time.monotonic_ns()
        launch_us = time.time_ns() // 1000
        watch = Watchdog(start)
        watch.ready = not options.present_log
        client = subprocess.Popen([binary], env=env, stdout=output, stderr=subprocess.STDOUT)

        def read_events():
            watch.read(log if options.present_log else trace_events(log, launch_us, start))

        run = Run(client, watch, read_events, writers, client_log)
        try:
            run.wait_for_writer(0, 15)
            # Chords reach only the focused window, which KWin maps with the first frame.
            run.wait_until(time.monotonic_ns() + 15 * NS_PER_S,
                           lambda: watch.first_present_ns is not None)
            run.wait_until(time.monotonic_ns() + FOCUS_NS)
            for index, chords in enumerate(steps, 1):
                subprocess.run([injector, '--keys', chords], check=True, timeout=10)
                run.wait_for_writer(index, 5)
            begin = time.monotonic_ns() + WARMUP_NS
            end = begin + options.seconds * NS_PER_S
            run.wait_until(end)
            (writers / 'stop').touch()
            run.wait_until(time.monotonic_ns() + int(SETTLE_S * NS_PER_S))
        finally:
            stop(client)
            read_events()
    stamps = [read_writer(writers / str(i)) for i in range(options.panes)]
    spans = [after - before for pane in stamps for before, after in pane if begin <= before < end]
    print(f'Writer write spans over 10 ms: {sum(span > 10 * NS_PER_MS for span in spans)} of '
          f'{len(spans)}; max {max(spans, default=0) / NS_PER_MS:.3f} ms')
    failures = watch.finish(sorted(line for pane in stamps for line in pane), (begin, end))
    frames = read_frames(frame_log)
    failures += ([f'FAIL: no frame log at {frame_log}'] if frames is None
                 else check_frames(frames, options.panes, stamps, (begin, end)))
    for failure in failures:
        print(failure)
    if not failures:
        print(f'PASS: {options.panes} panes advanced, and each repainted within '
              f'{REPAINT_LIMIT_NS // NS_PER_MS} ms of every line')
    return int(bool(failures))


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f'stall: {error}')
