import os
from pathlib import Path
import re
import statistics
import subprocess
import sys
import tempfile
import time


REPO = Path(__file__).resolve().parents[2]
GHOSTTY = Path('/scratch/ssd/sumie-p0/run-ghostty.sh')
SUMIE = Path('/scratch/ssd/sumie-target-baseline/release/sumie')
RUNS = 10
SETTLE = 5
IDLE = 60
DAY_US = 86_400_000_000
STAMP = re.compile(r'\[(\d\d):(\d\d):(\d\d)\.(\d{6})\] (.*)')
GET_SURFACE = re.compile(r' -> xdg_wm_base#\d+\.get_xdg_surface\(new id (xdg_surface#\d+), (wl_surface#\d+)\)')
GET_TOPLEVEL = re.compile(r' -> (xdg_surface#\d+)\.get_toplevel\(')
FEEDBACK = re.compile(r' -> wp_presentation#\d+\.feedback\((wl_surface#\d+), new id (wp_presentation_feedback#\d+)\)')
FEEDBACK_DONE = re.compile(r'(wp_presentation_feedback#\d+)\.(presented|discarded)\(')


class Trace:
    def __init__(self):
        self.exec_us = None
        self.surfaces = {}
        self.xdg_surface = self.surface = None
        self.configured = self.attached = self.scaled = False
        self.first_buffer = None
        self.commits = []
        self.feedback = set()
        self.presents = []

    def line(self, line):
        if line.startswith('exec_us '):
            self.exec_us = int(line.split()[1]) % DAY_US
            return
        stamp = STAMP.match(line)
        if not stamp:
            return
        message = stamp[5]
        if match := GET_SURFACE.search(message):
            self.surfaces[match[1]] = match[2]
        elif (match := GET_TOPLEVEL.search(message)) and self.xdg_surface is None:
            self.xdg_surface = match[1]
            self.surface = self.surfaces[match[1]]
        elif f' -> {self.xdg_surface}.ack_configure(' in message:
            self.configured = True
        elif re.search(r'wp_fractional_scale_v1#\d+\.preferred_scale\(150\)', message):
            self.scaled = True
        elif f' -> {self.surface}.attach(' in message:
            self.attached = f'{self.surface}.attach(wl_buffer#' in message
        elif f' -> {self.surface}.commit()' in message:
            if self.first_buffer is not None:
                self.commits.append(self.elapsed(stamp))
            elif self.configured and self.attached:
                self.first_buffer = self.elapsed(stamp)
            self.attached = False
        elif (match := FEEDBACK.search(message)) and match[1] == self.surface:
            self.feedback.add(match[2])
        elif (match := FEEDBACK_DONE.search(message)) and match[1] in self.feedback:
            self.feedback.remove(match[1])
            if match[2] == 'presented':
                self.presents.append(self.elapsed(stamp))

    def elapsed(self, stamp):
        h, m, s, us = map(int, stamp.groups()[:4])
        return (((h * 60 + m) * 60 + s) * 1_000_000 + us - self.exec_us) % DAY_US

    def read(self, log):
        while True:
            offset = log.tell()
            line = log.readline()
            if not line.endswith('\n'):
                log.seek(offset)
                return
            self.line(line)


def mark_exec():
    os.write(2, f'exec_us {time.time_ns() // 1000}\n'.encode())


def wait_alive(client, seconds):
    try:
        status = client.wait(timeout=seconds)
    except subprocess.TimeoutExpired:
        return
    raise RuntimeError(f'client {client.pid} exited early with status {status}')


def measure(command, env, log_path, method=None):
    trace = Trace()
    with log_path.open('w') as output, log_path.open(errors='replace') as log:
        client = subprocess.Popen(command, env=env, stdout=output, stderr=subprocess.STDOUT,
                                  preexec_fn=mark_exec)
        try:
            deadline = time.monotonic() + 20
            while True:
                trace.read(log)
                if trace.first_buffer is not None and (method != 'present' or trace.presents):
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f'no startup event; see {log_path}')
                wait_alive(client, 0.01)
            if not trace.scaled:
                raise RuntimeError(f'no fractional scale 150 event; see {log_path}')

            startup = trace.presents[0] if method == 'present' else trace.first_buffer
            idle_start = startup + SETTLE * 1_000_000
            idle_end = idle_start + IDLE * 1_000_000
            now = lambda: (time.time_ns() // 1000 - trace.exec_us) % DAY_US
            wait_alive(client, max(0, idle_start - now()) / 1_000_000)
            if method is None:
                trace.read(log)
                return bool(trace.presents)

            rollup = Path(f'/proc/{client.pid}/smaps_rollup').read_text()
            log_path.with_suffix('.smaps_rollup').write_text(rollup)
            pss, rss = (int(re.search(rf'^{key}:\s+(\d+) kB$', rollup, re.M)[1])
                        for key in ('Pss', 'Rss'))
            wait_alive(client, max(0, idle_end - now()) / 1_000_000)
            trace.read(log)
            events = trace.presents if method == 'present' else trace.commits
            idle = sum(idle_start <= event < idle_end for event in events)
            first_present = trace.presents[0] / 1000 if trace.presents else None
            return startup / 1000, idle, pss, rss, first_present
        finally:
            if client.poll() is None:
                client.terminate()
                try:
                    client.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait()


def table(results, method):
    event = 'presented' if method == 'present' else 'buffered commit'
    idle = 'presents' if method == 'present' else 'commits'
    metrics = [(f'Startup to first {event} (ms)', 0.001), (f'Idle {idle} / 60 s', None),
               ('PSS (KiB)', 1), ('RSS (KiB)', os.sysconf('SC_PAGE_SIZE') / 1024)]
    if method == 'commit':
        metrics.append(('Startup to first presented (ms; not comparable)', 0.001))
    print('| Metric | Ghostty mean | sumie mean | Ghostty sample SD | Two-sided tolerance |')
    print('| --- | ---: | ---: | ---: | ---: |')
    for (label, resolution), ghostty, sumie in zip(metrics, zip(*results['Ghostty']),
                                                  zip(*results['sumie'])):
        means = [f'{statistics.mean(samples):.3f}' if None not in samples else 'n/a'
                 for samples in (ghostty, sumie)]
        sd = statistics.stdev(ghostty) if None not in ghostty else None
        deviation = f'{sd:.3f}' if sd is not None else 'n/a'
        tolerance = '0 (exact)' if resolution is None else (
            f'±{max(2 * sd, resolution):.3f}' if sd is not None else 'n/a'
        )
        print(f'| {label} | {means[0]} | {means[1]} | {deviation} | {tolerance} |')


def main():
    runtime = Path(os.environ['XDG_RUNTIME_DIR'])
    bus = os.environ['DBUS_SESSION_BUS_ADDRESS'].partition(',')[0]
    if (runtime.parent != Path('/scratch/ram') or not runtime.name.startswith('sumie-rt.')
            or bus != f'unix:path={runtime}/bus' or os.environ['WAYLAND_DISPLAY'] != 'wl-sumie'
            or 'DISPLAY' in os.environ or 'WAYLAND_SOCKET' in os.environ):
        raise RuntimeError('start with baseline.sh in the private nested runner')
    parent = Path(sys.argv[1]).resolve(strict=True)
    if parent.is_relative_to(REPO):
        raise RuntimeError('artifact directory must be outside the repository')
    for executable in (GHOSTTY, SUMIE):
        if not os.access(executable, os.X_OK):
            raise RuntimeError(f'executable not found: {executable}')

    artifacts = Path(tempfile.mkdtemp(prefix='baseline.', dir=parent))
    print(f'Artifacts: {artifacts}', file=sys.stderr, flush=True)
    workload = artifacts / 'idle-shell'
    workload.write_text("#!/bin/sh\nprintf '\\033[2 qsumie baseline\\n'\nexec /bin/sleep infinity\n")
    workload.chmod(0o700)
    commands = {
        'Ghostty': [GHOSTTY, '--cursor-style-blink=false', '--shell-integration=none', '-e', workload],
        'sumie': [SUMIE],
    }
    env = dict(os.environ, WAYLAND_DEBUG='1', GDK_BACKEND='wayland', SHELL=str(workload))
    with (artifacts / 'output.txt').open('w') as output:
        subprocess.run(['kscreen-doctor', '-o'], check=True, timeout=10, stdout=output,
                       env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
    print(f'Load before warm-up: {os.getloadavg()}', file=sys.stderr, flush=True)
    warm = {name: measure(command, env, artifacts / f'{name}-warmup.log')
            for name, command in commands.items()}
    method = 'present' if all(warm.values()) else 'commit'
    print(f'Warm-up presentation feedback: {warm}; comparison uses {method}',
          file=sys.stderr, flush=True)
    results = {name: [] for name in commands}
    for run in range(1, RUNS + 1):
        for name, command in commands.items():
            values = measure(command, env, artifacts / f'{name}-{run:02}.log', method)
            results[name].append(values)
            print(f'{name} #{run}: {values}; load={os.getloadavg()}', file=sys.stderr, flush=True)
    table(results, method)


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError) as error:
        sys.exit(f'baseline: {error}')
