#!/usr/bin/env python3
"""U8 frame-slot checks on the terminal driver, run by check-gate.py.

usage: frames.py {resize,idle,flood,timeout,throughput,loading} BINARY [EVIDENCE_DIR]

Each case starts `SUMIE_MACOS_CI_RUNNER=terminal` with frame-hook.dylib injected, a throwaway
HOME, cursor blink off and /bin/sh as the login shell, and pings the main thread every 10 ms
through the runner's proxy wake. Exit 0 on a pass, 1 on a violation, 2 if it cannot check.
With SUMIE_TIMING=record (hosted runners), ping, acquire-time and throughput limits are reported, not
enforced.
  resize   A wgpu submission is held on an MTLSharedEvent while the window is dragged: no
           configure runs on a non-empty queue, a configure waits for it, pings stay <= 100 ms.
  idle     60 s with nothing changing: no present.
  flood    `seq` output, continuous and then in short bursts from idle: at most one present
           per refresh, median acquire <= 2 ms.
  timeout  Two nil drawables: no acquire for 1 s, then 2 s; pings stay <= 100 ms.
  throughput  `head -c 64M /dev/zero | base64` through the pane reaches 20 MB/s.
  loading  The settings include a FIFO with no writer: the window presents the defaults under
           a loading banner within 1 s (the driver waits 500 ms). Once a file replaces the
           FIFO and a writer releases the child, the settings apply and the banner clears.
"""
import json
import os
from pathlib import Path
import shlex
import statistics
import subprocess
import sys
import tempfile
import threading
import time

SCRIPTS = Path(__file__).resolve().parent
PING_PERIOD = 0.010
PING_LIMIT_MS = 100.0
IDLE_SECONDS = 60.0
THROUGHPUT_INPUT = 64 * 1024 * 1024
THROUGHPUT_FLOOR_MB = 20.0
THROUGHPUT_RECORD_SECONDS = 120.0
CONFIG = '[font]\nfamily = "Menlo"\n\n[cursor]\nblink = false\n\n[window]\nconfirm-close = false\n'
# The include blocks the configuration child until the check replaces it; its larger font
# shows when the real settings replace the defaults.
LOADING_CONFIG = 'include = ["slow.toml"]\n'
LOADING_INCLUDE = CONFIG.replace('"Menlo"\n', '"Menlo"\nsize = 24\n')
LOADING_BLANK_MS = 1000.0
LOADING_WAIT = 5.0
RECORD_TIMING = os.environ.get("SUMIE_TIMING") == "record"


class CannotCheck(Exception):
    pass


def build_tools(scratch, cg):
    hook = scratch / "frame-hook.dylib"
    subprocess.run(["xcrun", "clang", "-arch", "arm64", "-fobjc-arc", "-fblocks", "-Wall", "-Wextra",
                    "-Werror", "-dynamiclib", "-framework", "Foundation", "-framework", "Metal",
                    "-framework", "QuartzCore", SCRIPTS / "frame-hook.m", "-o", hook], check=True)
    tool = None
    if cg:
        tool = scratch / "cgtool"
        subprocess.run(["xcrun", "--sdk", "macosx", "swiftc", "-O", SCRIPTS.parent / "macos-spike/cgtool.swift",
                        "-o", tool], check=True, capture_output=True)
    return hook, tool


class Runner:
    def __init__(self, binary, hook, home, loading=False):
        self.home = Path(home)
        config = Path(home) / "config"
        (config / "sumie").mkdir(parents=True)
        state = Path(home) / "Library/Application Support/sumie"  # the first-run hint takes a key
        state.mkdir(parents=True)
        (state / "first-run").touch()
        self.slow = config / "sumie/slow.toml"
        if loading:
            os.mkfifo(self.slow)
        (config / "sumie/sumie.toml").write_text(LOADING_CONFIG if loading else CONFIG)
        control, self.hook = os.pipe()
        env = {**os.environ, "SUMIE_MACOS_CI_RUNNER": "terminal", "HOME": home, "XDG_CONFIG_HOME": str(config),
               "SHELL": "/bin/sh", "DYLD_INSERT_LIBRARIES": str(hook), "SUMIE_FRAME_HOOK_FD": str(control)}
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0,
                                        env=env, pass_fds=(control,))
        os.close(control)
        self.lock = threading.Lock()
        self.lines = []
        self.sent = {}
        self.acked = {}
        self.first = threading.Event()
        self.pinging = False
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for raw in self.process.stdout:
            now = time.monotonic()
            line = raw.decode(errors="replace").strip()
            fields = line.split()
            with self.lock:
                if fields[:2] == ["runner", "ack"]:
                    self.acked[int(fields[2])] = now
                    continue
                self.lines.append((now, fields))
            if fields[:2] == ["runner", "first"]:
                self.first.set()

    def send(self, command):
        self.process.stdin.write((command + "\n").encode())

    def poke(self, command):
        os.write(self.hook, command.encode())

    def start_pings(self):
        self.pinging = True

        def loop():
            number = 0
            while self.pinging:
                number += 1
                with self.lock:
                    self.sent[number] = time.monotonic()
                self.send(f"ping {number}")
                time.sleep(PING_PERIOD)
        threading.Thread(target=loop, daemon=True).start()

    def worst_ping_ms(self, start, end):
        with self.lock:
            rtts = [(self.acked.get(n, time.monotonic()) - sent) * 1000
                    for n, sent in self.sent.items() if start <= sent <= end]
        return max(rtts, default=None), len(rtts)

    def matching(self, *prefix, since=0.0):
        with self.lock:
            return [(at, fields) for at, fields in self.lines if at >= since and fields[:len(prefix)] == list(prefix)]

    def wait_for_file(self, path, timeout=10.0):
        end = time.monotonic() + timeout
        while not path.exists():
            if time.monotonic() > end:
                raise CannotCheck(f"the shell did not create {path.name} within {timeout} s")
            time.sleep(0.01)
        return time.monotonic()

    def wait_for(self, *prefix, since=0.0, timeout=5.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            found = self.matching(*prefix, since=since)
            if found:
                return found[0]
            time.sleep(0.01)
        raise CannotCheck(f"no `{' '.join(prefix)}` line within {timeout} s")

    def report(self):
        asked = time.monotonic()
        self.send("report")
        _, fields = self.wait_for("runner", "report", since=asked)
        names = ("presents", "configures", "busy_configures", "deferred_configures", "timeouts", "columns",
                 "rows", "banner", "prompt")
        report = dict(zip(names, map(int, fields[2:2 + len(names)])))
        report["pwd"] = " ".join(fields[2 + len(names):])
        return report

    def close(self):
        self.pinging = False
        time.sleep(0.05)
        try:
            self.send("close")
            self.process.wait(15)
        except (BrokenPipeError, subprocess.TimeoutExpired):
            self.process.kill()
            self.process.wait()
        os.close(self.hook)
        return self.process.returncode


def timing(errors):
    """Timing limits hold on the owner's Mac; hosted runs only record them."""
    for error in errors if RECORD_TIMING else ():
        print(f"{error} (recorded only)")
    return [] if RECORD_TIMING else errors


def ping_errors(runner, start, end, name):
    worst, count = runner.worst_ping_ms(start, end)
    print(f"{name}: worst ping {worst:.1f} ms over {count} pings")
    return timing([f"{name}: a ping took {worst:.1f} ms (limit {PING_LIMIT_MS:.0f} ms)"]
                  if worst > PING_LIMIT_MS else [])


def case_resize(runner, cg, result):
    runner.start_pings()
    time.sleep(1.0)
    before = runner.report()
    start = time.monotonic()
    runner.poke("H")
    runner.send("type echo held")
    gated, _ = runner.wait_for("hook", "gated", since=start)
    x, y, w, h, _ = map(int, cg("bounds").split())
    drag = threading.Thread(target=cg, args=("drag", x + w - 2, y + h - 2, x + w + 158, y + h + 98, 1200))
    drag.start()
    time.sleep(0.8)
    runner.poke("R")
    drag.join()
    time.sleep(1.0)
    end = time.monotonic()
    after = runner.report()
    result.update(before=before, after=after, held_from=gated - start, bounds_after=cg("bounds"))
    print(f"resize: {after['deferred_configures'] - before['deferred_configures']} deferred, "
          f"{after['configures'] - before['configures']} configures, {after['busy_configures']} on a busy queue")
    errors = ping_errors(runner, start, end, "resize")
    if after["busy_configures"]:
        errors.append(f"resize: {after['busy_configures']} configures ran while GPU work was pending")
    if after["configures"] == before["configures"]:
        errors.append("resize: the window resized but the surface was never configured")
    if not errors and after["deferred_configures"] == before["deferred_configures"]:
        raise CannotCheck("no configure met the held queue; the drag did not overlap the hold")
    return errors


def unexplained(runner, since):
    """Presents not within 200 ms after a focus, occlusion or size change, which do change the
    window. Another application can take focus during the minute on a shared desktop."""
    changes = [at for at, fields in runner.matching("runner", since=since)
               if fields[1] in ("focused", "occluded", "resized")]
    return [at for at, _ in runner.matching("runner", "present", since=since)
            if not any(0 <= at - change <= 0.2 for change in changes)]


def case_idle(runner, _cg, result):
    time.sleep(3.0)
    before = runner.report()
    start = time.monotonic()
    while time.monotonic() < start + IDLE_SECONDS:
        presents = unexplained(runner, start)
        if presents:
            result["lines"] = [fields for _, fields in runner.matching("runner", since=start)][:20]
            return [f"idle: {len(presents)} presents within {time.monotonic() - start:.1f} s of idle"]
        time.sleep(0.25)
    after = runner.report()
    explained = after["presents"] - before["presents"]
    result.update(before=before, after=after, explained=explained)
    print(f"idle: no presents in {IDLE_SECONDS:.0f} s without a window change ({explained} after one)")
    return []


BURSTS = "i=0; while [ $i -lt 15 ]; do seq 1 5000; sleep 0.2; i=$((i+1)); done"


def flood_phase(runner, command, seconds):
    """Type `command` and return the window it ran in; Ctrl-C ends it."""
    start = time.monotonic()
    runner.send(f"type {command}")
    time.sleep(seconds + 0.1)
    runner.send("type \x03")
    time.sleep(0.5)
    return start, start + seconds


def presents_in(runner, window):
    return [int(fields[3]) / 1e6 for at, fields in runner.matching("runner", "present")
            if window[0] <= at <= window[1]]


def spans(presents):
    """Gaps, and spans of two gaps. The slot keeps its cadence, so a present after a late one
    can come sooner, but two presents apart still span more than a refresh."""
    return ([b - a for a, b in zip(presents, presents[1:])],
            [b - a for a, b in zip(presents, presents[2:])])


def case_flood(runner, _cg, result):
    refresh_ms = int(runner.wait_for("runner", "window")[1][3]) / 1e6
    time.sleep(1.0)
    runner.start_pings()
    # Once drawables are in flight, the display throttles any slot (a composited layer releases
    # them once per refresh), so a slot that runs ahead shows as acquire time. A burst from idle
    # finds free drawables, and there a slot freed early presents back to back.
    continuous = flood_phase(runner, "seq 1 999999999", 3.0)
    bursts = flood_phase(runner, BURSTS, 3.5)
    presents = presents_in(runner, continuous)
    burst_presents = presents_in(runner, bursts)
    steady = (continuous[0] + 0.5, continuous[1])
    waits = [int(fields[3]) / 1000 for at, fields in runner.matching("hook", "acquire")
             if steady[0] <= at <= steady[1] and fields[4] == "drawable"]
    if len(presents) < 10 or len(burst_presents) < 10 or not waits:
        raise CannotCheck(f"the flood drew only {len(presents)} and {len(burst_presents)} frames")
    gaps, pairs = spans(presents)
    _, burst_pairs = spans(burst_presents)
    # Turn-end timestamps add up to a few ms of jitter.
    shortest = min(pairs + burst_pairs)
    seconds = (presents[-1] - presents[0]) / 1000
    per_refresh = (len(presents) - 1) * refresh_ms / (presents[-1] - presents[0])
    median = statistics.median(waits)
    result.update(presents=len(presents), burst_presents=len(burst_presents), seconds=seconds,
                  refresh_ms=refresh_ms, min_gap_ms=min(gaps), min_pair_ms=min(pairs),
                  min_burst_pair_ms=min(burst_pairs), median_gap_ms=statistics.median(gaps),
                  acquires=len(waits), median_acquire_ms=median, max_acquire_ms=max(waits))
    print(f"flood: {len(presents)} presents in {seconds:.2f} s ({per_refresh:.2f} per {refresh_ms:.2f} ms "
          f"refresh), gaps min {min(gaps):.2f} median {statistics.median(gaps):.2f} ms, two apart min "
          f"{min(pairs):.2f} ms, in bursts {min(burst_pairs):.2f} ms over {len(burst_presents)}; "
          f"acquire median {median:.2f} max {max(waits):.2f} ms over {len(waits)}")
    errors = ping_errors(runner, continuous[0], bursts[1], "flood")
    if per_refresh > 1.02 or shortest < 0.85 * refresh_ms:
        errors.append(f"flood: more than one present per refresh ({per_refresh:.2f} per refresh, "
                      f"shortest span of two gaps {shortest:.2f} ms)")
    if median > 2.0:
        errors += timing([f"flood: median acquire {median:.2f} ms exceeds 2 ms"])
    return errors


def case_timeout(runner, _cg, result):
    runner.start_pings()
    time.sleep(1.0)
    start = time.monotonic()
    runner.poke("N")
    runner.send("type echo timeout")
    time.sleep(4.5)
    end = time.monotonic()
    acquires = [(int(fields[2]) / 1e6, fields[4]) for _, fields in runner.matching("hook", "acquire", since=start)]
    report = runner.report()
    result.update(acquires=acquires, report=report)
    kinds = [kind for _, kind in acquires]
    if kinds[:2] != ["nil", "nil"] or "drawable" not in kinds[2:]:
        raise CannotCheck(f"unexpected acquires after the nil drawables: {kinds[:6]}")
    gaps = [b - a for (a, _), (b, _) in zip(acquires, acquires[1:3])]
    print(f"timeout: acquires {kinds[:3]}, gaps {', '.join(f'{gap:.3f} s' for gap in gaps)}, "
          f"{report['timeouts']} timeouts")
    errors = ping_errors(runner, start, end, "timeout")
    for gap, least in zip(gaps, (1.0, 2.0)):
        if gap < least:
            errors.append(f"timeout: acquired again {gap * 1000:.0f} ms after a timeout (at least {least:.0f} s)")
    return errors


def case_throughput(runner, _cg, result):
    # base64 writes whole blocks, so each read returns the PTY's 1 KiB maximum and the rate is
    # sumie's: reader, readiness bridge and VT. A writer that makes one write per line (BSD
    # `head` on a tty) is bound by its own write rate in any reader; see docs/research/m1b1.md.
    start, done = runner.home / "throughput.start", runner.home / "throughput.done"
    output = (THROUGHPUT_INPUT + 2) // 3 * 4
    runner.send(f"type cd {shlex.quote(str(runner.home))}; : >{start.name}; "
                f"head -c {THROUGHPUT_INPUT} /dev/zero | base64; : >{done.name}")
    began = runner.wait_for_file(start)
    limit = THROUGHPUT_RECORD_SECONDS if RECORD_TIMING else output / (THROUGHPUT_FLOOR_MB * 1e6)
    while not done.exists() and time.monotonic() < began + limit + 0.5:
        time.sleep(0.01)
    if not done.exists():
        runner.send("type \x03")
        if RECORD_TIMING:
            raise CannotCheck(f"base64 did not finish within {limit:.0f} s")
        return [f"throughput: {output} bytes of base64 took over {limit:.1f} s "
                f"(below {THROUGHPUT_FLOOR_MB:.0f} MB/s)"]
    seconds = (done.stat().st_mtime_ns - start.stat().st_mtime_ns) / 1e9
    rate = output / seconds / 1e6
    result.update(bytes=output, seconds=seconds, mb_per_s=rate)
    print(f"throughput: {output} bytes of base64 in {seconds:.2f} s ({rate:.1f} MB/s)")
    if rate < THROUGHPUT_FLOOR_MB:
        return timing([f"throughput: {rate:.1f} MB/s is below {THROUGHPUT_FLOOR_MB:.0f} MB/s"])
    return []


def case_loading(runner, _cg, result):
    window = int(runner.wait_for("runner", "window", timeout=30)[1][2]) / 1e6
    if not runner.first.wait(LOADING_WAIT):
        return [f"loading: no present within {LOADING_WAIT:.0f} s while an include blocks the settings"]
    blank = int(runner.wait_for("runner", "first")[1][2]) / 1e6 - window
    loading = runner.report()
    # The child opens the include to watch it and again to read it, so a file takes the FIFO's
    # path before a writer, through a second link, releases the first open.
    fifo = runner.slow.with_name("fifo")
    os.link(runner.slow, fifo)
    written = runner.slow.with_name("written.toml")
    written.write_text(LOADING_INCLUDE)
    written.replace(runner.slow)
    try:
        # The child is blocked opening the FIFO, so a non-blocking open for writing succeeds.
        os.close(os.open(fifo, os.O_WRONLY | os.O_NONBLOCK))
    except OSError as error:
        raise CannotCheck(f"the configuration child is not opening the include: {error}") from error
    end = time.monotonic() + LOADING_WAIT
    loaded = runner.report()
    while loaded["banner"] and time.monotonic() < end:
        time.sleep(0.05)
        loaded = runner.report()
    result.update(blank_ms=blank, loading=loading, loaded=loaded)
    print(f"loading: blank for {blank:.0f} ms, then {loading['columns']} columns under the banner; "
          f"{loaded['columns']} columns once the include was written")
    errors = timing([f"loading: the window stayed blank for {blank:.0f} ms (limit {LOADING_BLANK_MS:.0f} ms)"]
                    if blank > LOADING_BLANK_MS else [])
    if not loading["banner"]:
        errors.append("loading: the defaults presented without the loading banner")
    if loaded["banner"]:
        errors.append(f"loading: the banner stayed {LOADING_WAIT:.0f} s after the include was written")
    elif loaded["columns"] >= loading["columns"]:
        errors.append(f"loading: the written settings did not apply ({loaded['columns']} columns, "
                      f"{loading['columns']} under the defaults)")
    return errors


CASES = {"resize": case_resize, "idle": case_idle, "flood": case_flood, "timeout": case_timeout,
         "throughput": case_throughput, "loading": case_loading}


def main():
    if len(sys.argv) not in (3, 4) or sys.argv[1] not in CASES:
        print(__doc__, file=sys.stderr)
        return 2
    name, binary = sys.argv[1], sys.argv[2]
    evidence = Path(sys.argv[3]) if len(sys.argv) == 4 else None
    result = {"case": name, "load": os.getloadavg()}
    with tempfile.TemporaryDirectory(prefix="sumie-frames.") as scratch:
        try:
            hook, tool = build_tools(Path(scratch), name == "resize")
            runner = Runner(binary, hook, str(Path(scratch) / "home"), loading=name == "loading")
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"frames: could not start: {error}", file=sys.stderr)
            return 2

        def cg(*args):
            pid = [runner.process.pid] if args[0] == "bounds" else []
            out = subprocess.run([tool, *map(str, (*args, *pid))], capture_output=True, text=True,
                                 check=False).stdout.strip()
            if args[0] == "bounds" and len(out.split()) != 5:
                raise CannotCheck("the runner window is not on screen")
            return out
        try:
            # The loading case times the first frame itself.
            if name != "loading" and not runner.first.wait(30):
                raise CannotCheck("no first frame")
            errors = CASES[name](runner, cg, result)
        except CannotCheck as error:
            print(f"frames: {name}: could not check: {error}", file=sys.stderr)
            return 2
        finally:
            result["exit"] = runner.close()
    if result["exit"] != 0:
        errors.append(f"{name}: the runner exited {result['exit']}")
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / f"frames-{name}.json").write_text(json.dumps(result, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
