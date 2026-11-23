import os
import shlex
import subprocess
import sys
import time

from session import artifact_dir, stop, sumie_binary


NS_PER_MS = 1_000_000
LIMIT_NS = 100 * NS_PER_MS


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
        self.max_after_first_ns = 0
        self.ready = False

    def gap(self, timestamp):
        gap = timestamp - self.anchor
        self.max_gap_ns = max(self.max_gap_ns, gap)
        if self.first_present_ns is not None:
            self.max_after_first_ns = max(self.max_after_first_ns, gap)

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
        print(f'Max visible gap: {self.max_gap_ns / NS_PER_MS:.3f} ms '
              f'(after first visible presented: {self.max_after_first_ns / NS_PER_MS:.3f} ms)')
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
    env = dict(os.environ, SHELL=str(workload), SUMIE_PRESENT_LOG=str(present_log))
    binary = sumie_binary('debug')
    print(f'Duration: {seconds} s; sumie: {shlex.quote(str(binary))}; load: {os.getloadavg()}',
          flush=True)
    present_log.touch()
    with client_log.open('w') as output, present_log.open() as log:
        start = time.monotonic_ns()
        watch = Watchdog(start, start + seconds * 1_000_000_000)
        client = subprocess.Popen([binary], env=env, stdout=output, stderr=subprocess.STDOUT)
        try:
            while (left := watch.end - time.monotonic_ns()) > 0:
                watch.read(log)
                try:
                    status = client.wait(timeout=min(left / 1_000_000_000, 0.02))
                except subprocess.TimeoutExpired:
                    continue
                raise RuntimeError(f'sumie exited with status {status}; see {client_log}')
            watch.read(log)
        finally:
            stop(client)
    return watch.finish()


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(f'stall: {error}')
