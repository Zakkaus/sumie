import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from session import artifact_dir, stop, sumie_binary


CYCLES = int(os.environ.get('SUMIE_PANES_CYCLES', '200'))
CHECKPOINT = 50
SETTLE_S = 6
REAP_S = 6
GAP_LIMIT_NS = 100_000_000
# Memory is not a handle: the allocator and the atlas keep freed pages, so it gets a bound.
RSS_LIMIT_KIB = 12 * 1024
RSS_SLOPE_KIB = 4 * 1024
DRM_LIMIT_KIB = 4 * 1024
MIB = 1024


def children(pid):
    """Live direct children of the process, from every thread: a spawn runs on a pane thread."""
    found = set()
    for task in Path(f'/proc/{pid}/task').iterdir():
        try:
            found.update(int(child) for child in (task / 'children').read_text().split())
        except OSError:
            pass
    return found


def state(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().rpartition(')')[2].split()[0]
    except (OSError, IndexError):
        return None


def drm_kib(pid):
    """DRM client memory in KiB, or None where the driver exposes none (NVIDIA, software GPUs)."""
    total = None
    fdinfo = Path(f'/proc/{pid}/fdinfo')
    for entry in fdinfo.iterdir():
        try:
            lines = entry.read_text().splitlines()
        except OSError:
            continue
        if any(line.startswith('drm-driver:') for line in lines):
            for line in lines:
                name, _, value = line.partition(':')
                if name.startswith(('drm-resident-', 'drm-memory-')) and value.split()[1:2] == ['KiB']:
                    total = (total or 0) + int(value.split()[0])
    return total


def sample(pid):
    status = dict(line.split(':', 1) for line in Path(f'/proc/{pid}/status').read_text().splitlines())
    kids = children(pid)
    return {
        'fds': len(os.listdir(f'/proc/{pid}/fd')),
        'threads': len(os.listdir(f'/proc/{pid}/task')),
        'children': len(kids),
        'zombies': sum(state(kid) == 'Z' for kid in kids),
        'rss_kib': int(status['VmRSS'].split()[0]),
        'drm_kib': drm_kib(pid),
    }


def wait_for(condition, timeout):
    """Seconds until the condition held, or None."""
    start = time.monotonic()
    while True:
        if condition():
            return time.monotonic() - start
        if time.monotonic() - start >= timeout:
            return None
        time.sleep(0.005)


class Run:
    def __init__(self, client, injector, present_log):
        self.client = client
        self.injector = injector
        self.present_log = present_log
        self.errors = []

    def chord(self, chord):
        subprocess.run([self.injector, '--keys', chord], check=True, timeout=10)

    def type(self, text):
        subprocess.run([self.injector, text], check=True, timeout=10)

    def alive(self):
        if self.client.poll() is not None:
            raise RuntimeError(f'sumie exited with status {self.client.returncode}')

    def settled(self, baseline):
        now = sample(self.client.pid)
        return (now['fds'] <= baseline['fds'] and now['threads'] <= baseline['threads']
                and now['children'] <= baseline['children'] and now['zombies'] == 0)

    def settle(self, baseline):
        self.alive()
        return wait_for(lambda: self.settled(baseline), SETTLE_S)

    def pane_cycle(self):
        base = len(children(self.client.pid))
        self.chord('ctrl+shift+e')
        if wait_for(lambda: len(children(self.client.pid)) == base + 1, 5) is None:
            raise RuntimeError('a split did not start a shell within 5 s')
        self.chord('ctrl+shift+w')
        if wait_for(lambda: len(children(self.client.pid)) == base, 5) is None:
            raise RuntimeError('closing a pane did not reap its shell within 5 s')

    def tab_cycle(self):
        base = len(children(self.client.pid))
        self.chord('ctrl+shift+t')
        if wait_for(lambda: len(children(self.client.pid)) == base + 1, 5) is None:
            raise RuntimeError('a new tab did not start a shell within 5 s')
        self.chord('ctrl+shift+w')
        if wait_for(lambda: len(children(self.client.pid)) == base, 5) is None:
            raise RuntimeError('closing a tab did not reap its shell within 5 s')

    def cycles(self, name, cycle, baseline, rows):
        first = len(rows)
        for number in range(1, CYCLES + 1):
            cycle()
            if number % CHECKPOINT == 0 or number == CYCLES:
                took = self.settle(baseline)
                rows.append((f'{name} {number}', sample(self.client.pid), took))
        for label, row, took in rows[first:]:
            for key, noun in (('fds', 'fd'), ('threads', 'thread'), ('children', 'child')):
                if row[key] > baseline[key]:
                    self.errors.append(f'{label}: {noun} count above baseline: {row[key]} > '
                                       f'{baseline[key]} after {SETTLE_S} s of quiescence')
            if row['zombies']:
                self.errors.append(f'{label}: {row["zombies"]} zombie children after quiescence')
        last = rows[-1][1]
        growth = last['rss_kib'] - baseline['rss_kib']
        slope = last['rss_kib'] - rows[first][1]['rss_kib']
        if growth > RSS_LIMIT_KIB or slope > RSS_SLOPE_KIB:
            self.errors.append(f'{name}: RSS grew {growth / MIB:.1f} MiB over the baseline and '
                               f'{slope / MIB:.1f} MiB after the first checkpoint '
                               f'(limits {RSS_LIMIT_KIB // MIB} and {RSS_SLOPE_KIB // MIB} MiB)')
        drm = (last['drm_kib'], baseline['drm_kib'])
        if None not in drm and drm[0] - drm[1] > DRM_LIMIT_KIB:
            self.errors.append(f'{name}: DRM memory grew {(drm[0] - drm[1]) / MIB:.1f} MiB '
                               f'(limit {DRM_LIMIT_KIB // MIB} MiB)')

    def presented(self):
        stamps = []
        for line in self.present_log.read_text(errors='replace').splitlines():
            event, _, stamp = line.partition(' ')
            if event == 'presented':
                stamps.append(int(stamp))
        return stamps

    def ignoring_hup(self):
        """A pane whose shell and child ignore SIGHUP is closed while another pane draws."""
        self.type('while :; do echo $RANDOM; sleep 0.01; done\n')
        if wait_for(lambda: len(self.presented()) > 20, 5) is None:
            raise RuntimeError('the drawing pane produced no frames')
        # A gap on a shared host may be noise; a blocked event loop repeats on the second close.
        for attempt in range(2):
            late, gap = self.close_ignoring_hup()
            if late or gap <= GAP_LIMIT_NS:
                break
            print('hup: gap over the limit; closing a second HUP-ignoring pane to see whether it repeats')
        if late:
            self.errors.append(f'hup: the shell was not reaped within {REAP_S} s of the close')
        if gap > GAP_LIMIT_NS:
            self.errors.append(f'hup: present gap {gap / 1e6:.1f} ms exceeds 100 ms while closing')

    def close_ignoring_hup(self):
        pid = self.client.pid
        base = children(pid)
        self.chord('ctrl+shift+e')
        if wait_for(lambda: len(children(pid) - base) == 1, 5) is None:
            raise RuntimeError('a split did not start the HUP-ignoring shell within 5 s')
        (shell,) = children(pid) - base
        try:
            self.type("trap '' HUP; sleep 1000\n")
            if wait_for(lambda: children_of(shell, 'sleep'), 5) is None:
                raise RuntimeError('the HUP-ignoring shell did not start sleep within 5 s')
            time.sleep(0.5)
            start = time.monotonic_ns()
            self.chord('ctrl+shift+w')
            time.sleep(0.5)
            if state(shell) is None:
                raise RuntimeError('the shell died at once, so the test never reached the escalation')
            gone = wait_for(lambda: state(shell) is None, REAP_S)
            end = time.monotonic_ns()
        finally:
            # The control with the escalation removed leaves the group alive; never keep it.
            if state(shell) is not None:
                os.killpg(shell, signal.SIGKILL)
                wait_for(lambda: state(shell) is None, 2)
        late = gone is None
        # The workload pane keeps drawing, so any gap is the event loop blocking.
        every = self.presented()
        stamps = [t for t in every if start < t <= end]
        edges = [max([t for t in every if t <= start], default=start), *stamps, end]
        gap = max(b - a for a, b in zip(edges, edges[1:]))
        for a, b in zip(edges, edges[1:]):
            if b - a > GAP_LIMIT_NS:
                print(f'  gap {(b - a) / 1e6:.1f} ms, {(a - start) / 1e9:+.2f} s from the close')
        reaped = 'not reaped' if late else f'reaped {(end - start) / 1e9:.2f} s after the close'
        print(f'hup: shell {shell} {reaped}; {len(stamps)} frames in the window; '
              f'max present gap {gap / 1e6:.1f} ms')
        return late, gap


def children_of(pid, name):
    try:
        kids = [int(kid) for task in Path(f'/proc/{pid}/task').iterdir()
                for kid in (task / 'children').read_text().split()]
        return any(Path(f'/proc/{kid}/comm').read_text().strip() == name for kid in kids)
    except OSError:
        return False


def table(baseline, rows):
    print(f'{"":14}{"fds":>6}{"threads":>9}{"children":>10}{"zombies":>9}{"RSS MiB":>10}{"DRM MiB":>9}{"settle s":>10}')
    for label, now, took in [('baseline', baseline, None), *rows]:
        settle = '' if took is None and label == 'baseline' else ('none' if took is None else f'{took:.2f}')
        drm = 'n/a' if now['drm_kib'] is None else f'{now["drm_kib"] / MIB:.1f}'
        print(f'{label:14}{now["fds"]:>6}{now["threads"]:>9}{now["children"]:>10}{now["zombies"]:>9}'
              f'{now["rss_kib"] / MIB:>10.1f}{drm:>9}{settle:>10}')


def main():
    parent = sys.argv[1]
    artifacts = artifact_dir(parent, 'panes.')
    print(f'Artifacts: {artifacts}', file=sys.stderr, flush=True)
    injector = os.environ.get('SUMIE_NESTED_TYPE')
    if not injector:
        raise RuntimeError('start with panes.sh, which asks the runner for the key injector')
    home = artifacts / 'home'
    home.mkdir()
    present_log = artifacts / 'present.log'
    present_log.touch()
    binary = sumie_binary('debug')
    # A private HOME and XDG tree keep every user rc file and the real configuration out of the run.
    env = dict(os.environ, HOME=str(home), SHELL='/bin/bash', LC_ALL='C.UTF-8',
               SUMIE_PRESENT_LOG=str(present_log))
    for name in ('WAYLAND_DEBUG', 'ENV', 'BASH_ENV'):
        env.pop(name, None)
    print(f'Cycles: {CYCLES}; sumie: {binary}; load: {os.getloadavg()}', flush=True)
    with (artifacts / 'sumie.log').open('w') as output:
        client = subprocess.Popen([binary], env=env, stdout=output, stderr=subprocess.STDOUT)
    run = Run(client, injector, present_log)
    rows = []
    try:
        if wait_for(lambda: client.poll() is not None or 'ready' in present_log.read_text(), 10) is None:
            raise RuntimeError('sumie wrote no presentation log header; build the debug profile')
        run.alive()
        if wait_for(lambda: 'presented' in present_log.read_text(), 10) is None:
            raise RuntimeError('sumie never presented a frame')
        if wait_for(lambda: len(children(client.pid)) == 1, 10) is None:
            raise RuntimeError('sumie did not start its first shell')
        time.sleep(1.0)
        # Lazily started threads and the first allocator growth belong to the baseline.
        for _ in range(5):
            run.pane_cycle()
        for _ in range(3):
            run.tab_cycle()
        time.sleep(1.0)
        baseline = sample(client.pid)
        run.cycles('panes', run.pane_cycle, baseline, rows)
        run.cycles('tabs', run.tab_cycle, baseline, rows)
        table(baseline, rows)
        run.ignoring_hup()
        if run.settle(baseline) is None:
            now = sample(client.pid)
            run.errors.append(
                f'hup: not quiescent after the close: fds {now["fds"]} (baseline {baseline["fds"]}), '
                f'threads {now["threads"]} ({baseline["threads"]}), children {now["children"]} '
                f'({baseline["children"]}), zombies {now["zombies"]}')
    finally:
        stop(client)
    for error in run.errors:
        print(f'FAIL: {error}')
    if run.errors:
        return 1
    print(f'PASS: {CYCLES} pane and {CYCLES} tab cycles left no fd, thread, child or zombie above the '
          'baseline, and closing a HUP-ignoring pane never stalled drawing')
    return 0


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        # Exit 2 means the gate could not run; exit 1 is reserved for a measured violation.
        print(f'panes: {error}', file=sys.stderr)
        sys.exit(2)
