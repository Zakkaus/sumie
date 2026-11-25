import os
import re
import shlex
import subprocess
import sys
import time

from session import artifact_dir, stop, sumie_binary


NS_PER_MS = 1_000_000
LIMIT_NS = 100 * NS_PER_MS
DAY_US = 86_400_000_000
STAMP = re.compile(r'\[(\d\d):(\d\d):(\d\d)\.(\d{6})\] (.*)')
PRESENTATION_DONE = re.compile(r'wp_presentation_feedback#\d+\.(presented|discarded)\(')


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
    def __init__(self, start, end):
        self.start = start
        self.end = end
        self.anchor = start
        self.hidden_since = None
        self.hidden_ns = 0
        self.hidden_periods = 0
        self.presented = 0
        self.discarded = 0
        self.first_present_ns = None
        self.max_gap_ns = 0
        self.ready = False

    def gap(self, timestamp):
        if self.first_present_ns is not None:
            self.max_gap_ns = max(self.max_gap_ns, timestamp - self.anchor)

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
            if stamp >= self.end:
                break
            if event == 'discarded':
                self.discarded += 1
            elif event == 'hidden':
                self.gap(stamp)
                self.hidden_since = stamp
                self.hidden_periods += 1
            elif event == 'visible':
                self.hidden_ns += stamp - self.hidden_since
                self.hidden_since = None
                self.anchor = stamp
            elif event == 'presented':
                self.presented += 1
                if self.hidden_since is None:
                    self.gap(stamp)
                    self.anchor = stamp
                    if self.first_present_ns is None:
                        self.first_present_ns = stamp - self.start
            else:
                raise RuntimeError(f'unknown presentation log event: {event}')

    def finish(self):
        if not self.ready:
            raise RuntimeError('no presentation log header; build sumie with debug assertions')
        if self.hidden_since is None:
            self.gap(self.end)
        else:
            self.hidden_ns += self.end - self.hidden_since
        first = ('none' if self.first_present_ns is None
                 else f'{self.first_present_ns / NS_PER_MS:.3f} ms')
        print(f'First visible presented: {first}')
        print(f'Presented: {self.presented}; discarded: {self.discarded}')
        print(f'Invisible: {self.hidden_periods} periods, {self.hidden_ns / NS_PER_MS:.3f} ms')
        print(f'Max visible gap after first presented: {self.max_gap_ns / NS_PER_MS:.3f} ms')
        if self.max_gap_ns > LIMIT_NS:
            print('FAIL: visible presentation gap exceeded 100 ms')
            return 1
        if self.first_present_ns is None:
            raise RuntimeError('no visible presentation in the test window')
        print('PASS: no visible presentation gap exceeded 100 ms')
        return 0


def main():
    parent, seconds = sys.argv[1:]
    seconds = int(seconds)
    artifacts = artifact_dir(parent, 'stall.')
    print(f'Artifacts: {artifacts}', file=sys.stderr, flush=True)
    with (artifacts / 'output.txt').open('w') as output:
        subprocess.run(['kscreen-doctor', '-o'], check=True, timeout=10, stdout=output,
                       env=dict(os.environ, QT_QPA_PLATFORM='wayland'))
    workload = artifacts / 'stall-shell'
    workload.write_text(f'#!{sys.executable}\n'
                        'import itertools, time\n'
                        'for number in itertools.count():\n'
                        '    print(number, flush=True)\n'
                        '    time.sleep(0.01)\n')
    workload.chmod(0o700)
    present_log = artifacts / 'present.log'
    client_log = artifacts / 'sumie.log'
    env = dict(os.environ, SHELL=str(workload))
    binary = sumie_binary('debug')
    # Only debug builds write the presentation log. Release builds fall back to the
    # WAYLAND_DEBUG trace, which has no visibility events, so hidden periods are not counted.
    release = not os.access(binary, os.X_OK)
    if release:
        binary = sumie_binary('release')
        env['WAYLAND_DEBUG'] = '1'
    else:
        env['SUMIE_PRESENT_LOG'] = str(present_log)
    source = 'WAYLAND_DEBUG trace (release; no visibility)' if release else 'presentation log'
    print(f'Duration: {seconds} s; sumie: {shlex.quote(str(binary))}; source: {source}; '
          f'load: {os.getloadavg()}', flush=True)
    present_log.touch()
    with client_log.open('w') as output, (client_log if release else present_log).open(
            errors='replace') as log:
        start = time.monotonic_ns()
        launch_us = time.time_ns() // 1000
        watch = Watchdog(start, start + seconds * 1_000_000_000)
        watch.ready = release
        client = subprocess.Popen([binary], env=env, stdout=output, stderr=subprocess.STDOUT)

        def read_events():
            watch.read(trace_events(log, launch_us, start) if release else log)

        try:
            while (left := watch.end - time.monotonic_ns()) > 0:
                read_events()
                try:
                    status = client.wait(timeout=min(left / 1_000_000_000, 0.02))
                except subprocess.TimeoutExpired:
                    continue
                raise RuntimeError(f'sumie exited with status {status}; see {client_log}')
            read_events()
        finally:
            stop(client)
    return watch.finish()


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f'stall: {error}')
