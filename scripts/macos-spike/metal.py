import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
from time import monotonic_ns


PING_NS = 50_000_000
HOLD_NS = 400_000_000
DEADLINE_NS = 30_000_000_000


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


# scenario: (marker of the call that may block, releases that should unblock it)
BLOCKING = {
    "held": ("ACQUIRE_ENTER 4 ", 2),
    "fifo": (None, 0),
    "occluded": (None, 0),
    "minimized": (None, 0),
    "busy-resize": ("CONFIGURE_ENTER ", 1),
    "deferred-resize": ("CONFIGURE_DEFERRED ", 1),
}


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(fraction * len(ordered)))]


class Trial:
    def __init__(self, scenario, trace):
        self.scenario = scenario
        self.marker, self.needed = BLOCKING[scenario]
        self.trace = trace
        self.start = monotonic_ns()
        self.events = []
        self.sent = {}
        self.replied = {}
        self.next_ping = 0
        self.begin = None
        self.enter = None
        self.releases = []
        self.done = None
        self.quit = False

    def record(self, line):
        stamp = monotonic_ns() - self.start
        print(f"{stamp:12d}\t{line}", file=self.trace, flush=True)
        self.events.append((stamp, line))
        return stamp

    def receive(self, line):
        stamp = self.record(line)
        key, _, detail = line.partition(" ")
        if key == "NATIVE_ERROR":
            raise RuntimeError(detail)
        if key == "PONG":
            seq = int(detail)
            require(seq in self.sent and seq not in self.replied, f"unexpected PONG {seq}")
            self.replied[seq] = stamp
        if self.marker and line.startswith(self.marker):
            require(self.enter is None, "multiple blocking calls")
            self.enter = stamp
        if key == "DONE":
            self.done = stamp

    def send(self, child, line):
        stamp = self.record(f"SEND {line}")
        child.stdin.write(f"{line}\n".encode())
        return stamp

    def release(self, release_fd):
        self.releases.append(self.record(f"SEND RELEASE {len(self.releases) + 1}"))
        os.write(release_fd, b"R")

    def drive(self, child, release_fd):
        now = monotonic_ns() - self.start
        require(now < DEADLINE_NS, "watchdog deadline expired; see the trace and stderr")
        if self.quit or not self.matches("READY"):
            return
        if self.begin is None and len(self.replied) >= 3:
            self.begin = self.send(child, "BEGIN")
        # Stage each release after a hold with at least three numbered pings in flight.
        # Frame 1's GPU work completes before the second release, so frame 1 is then on screen.
        held = self.releases[-1] if self.releases else self.enter
        if self.enter is not None and self.done is None and len(self.releases) < self.needed and (
            now - held >= HOLD_NS and sum(t > held for t in self.sent.values()) >= 3
            and (not self.releases or self.matches("GPU_DONE 1"))
        ):
            self.release(release_fd)
        gated = len(self.matches("GATE_ARMED"))
        if self.done is not None and len(self.releases) < gated:
            self.release(release_fd)
        if self.done is not None and self.sent.keys() == self.replied.keys():
            if any(t > self.done for t in self.sent.values()) and len(self.matches("GPU_DONE")) == gated:
                self.send(child, "QUIT")
                self.quit = True
                return
        if now >= self.next_ping:
            seq = len(self.sent) + 1
            self.sent[seq] = self.send(child, f"PING {seq}")
            self.next_ping = now + PING_NS

    def matches(self, prefix):
        return [(t, line) for t, line in self.events if line == prefix or line.startswith(prefix + " ")]

    def one(self, line):
        events = [(t, text) for t, text in self.matches(line) if text == line]
        require(len(events) == 1, f"expected one {line!r}, found {len(events)}")
        return events[0][0]

    def chain(self, *lines):
        times = [self.one(line) for line in lines]
        require(all(a < b for a, b in zip(times, times[1:])), f"wrong event order: {lines}")

    def line(self, prefix):
        found = self.matches(prefix)
        require(len(found) == 1, f"expected one {prefix!r}, found {len(found)}")
        return found[0][1]

    def rtt(self, sequences=None):
        """Worst ping round trip, by default over pings sent after BEGIN (launch excluded)."""
        sequences = sequences or [seq for seq, t in self.sent.items() if t > self.begin]
        return max(self.replied[seq] - self.sent[seq] for seq in sequences) / 1e6

    def acquires(self):
        rows = [line.split() for _, line in self.matches("ACQUIRE_RETURN")]
        return [(int(row[3]), int(row[5])) for row in rows]

    def held_until(self, unblocked):
        """Pings sent while the call was entered; they must all be answered after `unblocked`."""
        blocked = [seq for seq, t in self.sent.items() if self.enter < t < unblocked]
        require(len(blocked) >= 3, "fewer than three pings during the hold")
        require(all(self.replied[seq] > unblocked for seq in blocked), "main replied during the hold")
        return blocked

    def check(self):
        require(self.quit and self.sent.keys() == self.replied.keys(), "incomplete watchdog exchange")
        return getattr(self, "check_" + self.scenario.replace("-", "_"))()

    def check_held(self):
        sequence = ["SEND BEGIN", "QUEUE_IDLE"]
        for n in range(1, 4):
            sequence.extend((f"ACQUIRE_ENTER {n} 3 {n - 1} 1", self.line(f"ACQUIRE_RETURN {n}"),
                             f"GATE_ARMED {n}"))
        returned = self.line("ACQUIRE_RETURN 4")
        surface = int(returned.split()[3])
        sequence.extend(("ACQUIRE_ENTER 4 3 3 1", returned, "DONE", "SEND QUIT"))
        self.chain(*sequence)
        surfaces = sorted({s for s, _ in self.acquires()})
        returned_at = self.one(returned)
        if not self.releases or returned_at < self.releases[0]:
            require(surface == 4 and surfaces == [1, 2, 3, 4], "extra acquire neither blocked nor grew")
            return {"result": "not-reproduced", "surfaces": surfaces,
                    "extra_acquire_us": self.acquires()[3][1], "max_ping_rtt_ms": self.rtt(),
                    "finding": "With all three pool drawables presented behind pending GPU work, the "
                               "layer returned a fourth IOSurface instead of blocking."}
        require(len(self.releases) >= 2 and returned_at > self.releases[1] and surface <= 3,
                "extra acquire returned before frame 2 was released")
        self.chain("SEND RELEASE 1", "GPU_DONE 1 4", "SEND RELEASE 2", returned)
        return {"result": "reproduced", "blocked_ms": (returned_at - self.enter) / 1e6,
                "pings_during_hold": self.held_until(self.releases[1]), "max_ping_rtt_ms": self.rtt()}

    def check_fifo(self):
        self.chain("SEND BEGIN", "QUEUE_IDLE", "DONE", "SEND QUIT")
        rows = self.acquires()
        require(len(rows) == 240 and not self.matches("WGPU_OCCLUDED"), "incomplete FIFO run")
        waits = [us for _, us in rows[3:]]
        return {"result": "measured", "surfaces": sorted({s for s, _ in rows}),
                "acquires": len(rows), "acquire_us_median": percentile(waits, 0.5),
                "acquire_us_p95": percentile(waits, 0.95), "acquire_us_max": max(waits),
                "max_ping_rtt_ms": self.rtt()}

    def check_occluded(self):
        self.chain("SEND BEGIN", "QUEUE_IDLE", "COVER", self.line("RACE_ACQUIRE 1"), "RACE_DONE",
                   "DONE", "SEND QUIT")
        require(self.matches("WGPU_OCCLUDED"), "wgpu never reported the covered window as occluded")
        races = [line.split() for _, line in self.matches("RACE_ACQUIRE")]
        waits = [int(row[3]) for row in races]
        return {"result": "blocked" if max(waits) >= 500_000 else "returned", "race_acquire_us": waits,
                "race_drawables": [row[4] for row in races],
                "presents_pending": [int(row[6]) for row in races],
                "frames_before_wgpu_occluded": int(self.line("WGPU_OCCLUDED").split()[1]),
                "max_ping_rtt_ms": self.rtt()}

    check_minimized = check_occluded

    def resize_chain(self, configure_marker):
        [(start, request)] = self.matches("RESIZE_REQUEST")
        [(initial, ready)] = self.matches("READY")
        target = request.removeprefix("RESIZE_REQUEST ")
        require(initial < start and target != ready.removeprefix("READY "), "window did not change size")
        release = self.line("GPU_RELEASE 1")
        require(release in {"GPU_RELEASE 1 2", "GPU_RELEASE 1 3"},
                "GPU buffer not pending on an unsignaled event")
        self.chain("SEND BEGIN", "QUEUE_IDLE", "ACQUIRE_ENTER 1 3 0 1", self.line("ACQUIRE_RETURN 1"),
                   "GATE_ARMED 1", request, f"{configure_marker} {target}", "SEND RELEASE 1", release,
                   "DONE", "SEND QUIT")
        entered, configured = self.one(f"CONFIGURE_ENTER {target}"), self.one("CONFIGURE_RETURN")
        require(any(entered < t < configured for t, _ in self.matches(f"LAYER_SIZE {target}")),
                "Metal layer did not reach the requested size during configure")
        return self.one(release), entered, configured

    def check_busy_resize(self):
        released, _, configured = self.resize_chain("CONFIGURE_ENTER")
        require(configured > released, "configure returned before the GPU release")
        self.chain("GPU_DONE 1 4", "CONFIGURE_RETURN")
        return {"result": "reproduced", "blocked_ms": (configured - self.enter) / 1e6,
                "pings_during_hold": self.held_until(configured), "max_ping_rtt_ms": self.rtt(),
                "finding": "Main-thread configure waited for the pending GPU submission."}

    def check_deferred_resize(self):
        released, entered, configured = self.resize_chain("CONFIGURE_DEFERRED")
        self.chain("GPU_DONE 1 4", "CONFIGURE_RETURN")
        during = [seq for seq, t in self.sent.items() if self.enter < t < released - PING_NS]
        require(len(during) >= 3 and all(self.replied[seq] < released for seq in during),
                "main thread did not answer pings while the resize was deferred")
        return {"result": "responsive", "configure_ms": (configured - entered) / 1e6,
                "deferred_ms": (entered - self.enter) / 1e6, "pings_during_hold": during,
                "max_ping_rtt_during_hold_ms": self.rtt(during), "max_ping_rtt_ms": self.rtt()}


def exchange(trial, child, release_fd):
    pending = b""
    with selectors.DefaultSelector() as selector:
        selector.register(child.stdout, selectors.EVENT_READ)
        while True:
            if selector.select(0.01):
                data = os.read(child.stdout.fileno(), 4096)
                if not data:
                    require(not pending, "incomplete probe record")
                    break
                lines = (pending + data).split(b"\n")
                pending = lines.pop()
                for line in lines:
                    trial.receive(line.decode("utf-8"))
            trial.drive(child, release_fd)
    code = child.wait(timeout=2)
    trial.record(f"EXIT {code}")
    require(code == 0, f"probe exited {code}; see stderr")
    return trial.check()


def run(binary, evidence, scenario):
    with (
        (evidence / f"metal-{scenario}.trace").open("w", encoding="utf-8") as trace,
        (evidence / f"metal-{scenario}.stderr").open("wb") as stderr,
    ):
        reader, writer = os.pipe()
        trial = Trial(scenario, trace)
        child = None
        try:
            try:
                child = subprocess.Popen(
                    [binary, scenario], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr,
                    bufsize=0, pass_fds=(reader,),
                    env={**os.environ, "SUMIE_METAL_SCENARIO": scenario, "SUMIE_METAL_RELEASE_FD": str(reader)},
                )
            finally:
                os.close(reader)
            return exchange(trial, child, writer)
        except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as exc:
            trial.record(f"WATCHDOG_ERROR {exc}")
            return {"result": "not-proven", "error": str(exc)}
        finally:
            if child is not None:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
                child.stdin.close()
                child.stdout.close()
            os.close(writer)


def main():
    binary, evidence = map(Path, sys.argv[1:3])
    scenarios = sys.argv[3:] or list(BLOCKING)
    require(set(scenarios) <= BLOCKING.keys(), f"unknown scenario in {scenarios}")
    result = {scenario: run(binary, evidence, scenario) for scenario in scenarios}
    result["clock"] = "Trace timestamps are watchdog send/receipt times in monotonic nanoseconds since launch."
    result["scope"] = (
        "GPU holds are test-owned event waits on real wgpu submissions; FIFO and occlusion use the "
        "compositor's own pacing. The pinned wgpu-hal fork enables Apple's drawable timeout, which "
        "bounds the race acquire. "
        "None of these establishes worker safety or safe teardown ownership."
    )
    text = json.dumps(result, indent=2) + "\n"
    (evidence / "metal.json").write_text(text, encoding="utf-8")
    print(text, end="")
    return int(any("error" in result[case] for case in scenarios))


if __name__ == "__main__":
    sys.exit(main())
