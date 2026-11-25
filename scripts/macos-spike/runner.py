#!/usr/bin/env python3
"""External controller for the opt-in macOS smoke probe."""

import collections
import json
import os
from pathlib import Path
import platform
import selectors
import subprocess
import sys
import time


SECOND = 1_000_000_000
PERIOD = 50_000_000
DURATION = 60 * SECOND
WATCHDOG = 10 * SECOND
STARTUP_TIMEOUT = 60 * SECOND
CLOSE_TIMEOUT = 15 * SECOND
KEY_TIMEOUT = 2 * SECOND


class ProbeError(Exception):
    pass


def require(condition, reason):
    if not condition:
        raise ProbeError(reason)


def numbers(fields, count):
    require(len(fields) == count and all(field.isdecimal() for field in fields),
            "invalid numeric probe record")
    return [int(field) for field in fields]


def flags(fields, count):
    require(len(fields) == count and all(field in ("true", "false") for field in fields),
            "invalid native inspection flags")
    return [field == "true" for field in fields]


class Smoke:
    def __init__(self, binary, mode, scratch):
        home = scratch / mode
        home.mkdir()
        config = home / "config"
        config.mkdir()
        self.started = time.monotonic_ns()
        self.process = subprocess.Popen(
            [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0,
            env={**os.environ, "SUMIE_MACOS_CI_RUNNER": mode, "HOME": str(home),
                 "XDG_CONFIG_HOME": str(config)},
        )
        os.set_blocking(self.process.stdin.fileno(), False)
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.buffer = b""
        self.records = collections.deque()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.selector.close()
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()

    def send(self, command):
        line = (command + "\n").encode("ascii")
        sent = time.monotonic_ns()
        require(os.write(self.process.stdin.fileno(), line) == len(line),
                "incomplete controller command")
        return sent

    def receive(self, deadline):
        while not self.records:
            now = time.monotonic_ns()
            if now >= deadline or not self.selector.select((deadline - now) / SECOND):
                return None
            chunk = os.read(self.process.stdout.fileno(), 16 * 1024)
            require(chunk, "sumie ended before the expected protocol record")
            received = time.monotonic_ns()
            *lines, self.buffer = (self.buffer + chunk).split(b"\n")
            require(len(self.buffer) <= 4096, "overlong probe record")
            for line in lines:
                if line.startswith(b"runner "):
                    self.records.append((received, line.decode("ascii").split()[1:]))
        return self.records.popleft()

    def expect(self, kind, deadline):
        reply = self.receive(deadline)
        require(reply is not None, f"timed out waiting for {kind}")
        received, fields = reply
        require(fields[:1] == [kind], f"out-of-order record while waiting for {kind}")
        return received, fields[1:]

    def startup(self, result):
        deadline = self.started + STARTUP_TIMEOUT
        opened, values = self.expect("window", deadline)
        result["window_open_ns"], = numbers(values, 1)
        result["launch_to_window_notice_ns"] = opened - self.started
        first, values = self.expect("first", deadline)
        result["first_submission_ns"], = numbers(values, 1)
        result["launch_to_first_submission_notice_ns"] = first - self.started
        require(result["first_submission_ns"] >= result["window_open_ns"],
                "first submission preceded opening the window")
        return first

    def close(self, result):
        sent = self.send("close")
        deadline = sent + CLOSE_TIMEOUT
        received, fields = self.expect("close", deadline)
        require(not fields, "invalid native close record")
        result["request_to_close_callback_notice_ns"] = received - sent
        dropped, fields = self.expect("dropped", deadline)
        require(not fields, "invalid smoke teardown record")
        result["request_to_smoke_drop_notice_ns"] = dropped - sent
        remaining = max(0, deadline - time.monotonic_ns()) / SECOND
        require(self.process.wait(timeout=remaining) == 0, "sumie did not exit successfully")
        result["request_to_reaped_exit_ns"] = time.monotonic_ns() - sent
        require(not self.records and not self.process.stdout.read().strip(),
                "unexpected protocol output after teardown")


def heartbeat_sample(smoke, first, result):
    end = first + DURATION
    next_ping = first
    sent = collections.deque()
    total = 0
    rows = result["heartbeats"] = []
    schedule_lateness = 0
    while next_ping <= end or sent:
        now = time.monotonic_ns()
        require(not sent or now - sent[0][1] < WATCHDOG, "main-thread heartbeat timed out")
        if next_ping <= end and now >= next_ping:
            total += 1
            ts = smoke.send(f"ping {total}")
            sent.append((total, ts))
            schedule_lateness = max(schedule_lateness, ts - next_ping)
            next_ping += PERIOD
        deadline = next_ping if next_ping <= end else sent[0][1] + WATCHDOG
        if sent:
            deadline = min(deadline, sent[0][1] + WATCHDOG)
        reply = smoke.receive(deadline)
        if reply is None:
            continue
        received, fields = reply
        require(fields[:1] == ["ack"], "unexpected record during main-thread heartbeat sample")
        sequence, = numbers(fields[1:], 1)
        require(sent and sequence == sent[0][0], "missing, duplicate, or out-of-order heartbeat")
        _, ts = sent.popleft()
        require(ts <= received < ts + WATCHDOG, "main-thread heartbeat timed out")
        rows.append([sequence, ts - first, received - ts])
    require(len(rows) == DURATION // PERIOD + 1, "incomplete heartbeat sample")
    result["max_heartbeat_rtt_ns"] = max(row[2] for row in rows)
    result["max_controller_ping_lateness_ns"] = schedule_lateness


def timing(binary, scratch, result):
    with Smoke(binary, "timing", scratch) as smoke:
        first = smoke.startup(result)
        heartbeat_sample(smoke, first, result)
        sent = smoke.send("finish")
        _, values = smoke.expect("sample", sent + WATCHDOG)
        total, maximum, tail, duration = numbers(values, 4)
        result.update(submissions=total, max_submission_interval_ns=maximum,
                      submission_tail_ns=tail, submission_sample_ns=duration)
        require(total >= 2, "fixture did not submit changing frames")
        require(duration == DURATION and 0 <= tail <= maximum <= duration,
                "invalid submission interval or unmeasured tail")
        close = result["close"] = {}
        smoke.close(close)
    result["targets_met"] = {
        "submission_interval_at_most_100_ms": maximum <= 100_000_000,
        "heartbeat_rtt_at_most_100_ms": result["max_heartbeat_rtt_ns"] <= 100_000_000,
        "close_at_most_6_s": close["request_to_reaped_exit_ns"] <= 6 * SECOND,
    }
    result["status"] = "measured"


def key_delivery(smoke, route, observations):
    sent = smoke.send(route)
    _, fields = smoke.expect("posted", sent + WATCHDOG)
    require(fields[:1] == [route], "unexpected native event route")
    if not flags(fields[1:], 1)[0]:
        return "unsupported:native_event_unavailable_or_focus_lost"
    observation = observations[route] = []
    for direction in ("down", "up"):
        reply = smoke.receive(sent + KEY_TIMEOUT)
        if reply is None:
            require(not observation, "native key down arrived without the matching key up")
            return "unsupported:no_winit_delivery"
        _, fields = reply
        require(fields == ["key", route, direction], "native key delivery was out of order")
        observation.append(direction)
    return "supported"


def known_pixel(scratch, window_id):
    fixture = scratch / "fixture.png"
    try:
        capture = subprocess.run(
            ["/usr/sbin/screencapture", "-x", "-o", "-l", str(window_id), fixture],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False,
        )
        if capture.returncode != 0 or not fixture.is_file():
            return "unsupported:window_capture_failed"
        verified = subprocess.run([scratch / "check-pixel", fixture], timeout=5, check=False)
        if verified.returncode != 0:
            return "unsupported:known_pixel_not_observed"
        return "supported"
    except subprocess.TimeoutExpired:
        return "unsupported:window_capture_timed_out"
    finally:
        fixture.unlink(missing_ok=True)


def capabilities(binary, scratch, result):
    status = result["capabilities"]
    with Smoke(binary, "capabilities", scratch) as smoke:
        smoke.startup(result)
        # Presentation has returned; give AppKit activation and the compositor time to settle.
        time.sleep(0.25)
        sent = smoke.send("inspect")
        _, fields = smoke.expect("capabilities", sent + WATCHDOG)
        window_id, = numbers(fields[:1], 1)
        window_server, key, responder, board, screen_access, post_access = flags(fields[1:], 6)
        result["screen_capture_preflight"] = screen_access
        result["cg_event_post_preflight"] = post_access
        status.update(
            window_server="supported" if window_server else "unsupported:no_window_server_record",
            key_window="supported" if key else "unsupported:probe_window_not_key",
            first_responder="supported" if responder else "unsupported:content_view_not_responder",
            named_pasteboard="supported" if board else "unsupported:sentinel_round_trip_failed",
        )
        observations = result["winit_key_observations"] = {}
        if key and responder:
            status["appkit_key_delivery"] = key_delivery(smoke, "appkit", observations)
        else:
            status["appkit_key_delivery"] = "unsupported:focus_or_responder_unavailable"
        if post_access and key and responder:
            status["cg_event_delivery"] = key_delivery(smoke, "cg", observations)
        else:
            status["cg_event_delivery"] = (
                "unsupported:focus_or_responder_unavailable" if post_access else "unsupported:tcc"
            )
        if screen_access and window_server and window_id > 0:
            status["known_pixel_capture"] = known_pixel(scratch, window_id)
        else:
            status["known_pixel_capture"] = (
                "unsupported:window_unavailable" if screen_access else "unsupported:tcc"
            )
        close = result["close"] = {}
        smoke.close(close)
    required = {"window_server"}
    required.update(filter(None, os.environ.get("SUMIE_MACOS_SPIKE_REQUIRED", "").split(",")))
    require(required <= status.keys() - {"native_quit"}, "unknown required capability name")
    require(all(status[name] == "supported" for name in required), "required capability is missing")
    measured = all(value == "supported" for name, value in status.items() if name != "native_quit")
    result["status"] = "measured" if measured else "partial"


def capture_metadata():
    return {
        "image_os": os.environ.get("ImageOS"),
        "image_version": os.environ.get("ImageVersion"),
        "runner_arch": os.environ.get("RUNNER_ARCH"),
        "machine": platform.machine(),
        "macos_version": platform.mac_ver()[0],
        "macos_build": subprocess.check_output(["/usr/bin/sw_vers", "-buildVersion"], text=True).strip(),
        "kernel_release": platform.release(),
        "logical_cpus": os.cpu_count(),
    }


def write_json(evidence, name, report):
    (evidence / name).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")


def main():
    binary, evidence, scratch = (Path(arg).resolve() for arg in sys.argv[1:])
    repo = Path(__file__).resolve().parents[2]
    require(not evidence.is_relative_to(repo) and not scratch.is_relative_to(repo),
            "runner evidence and scratch space must be outside the repository")
    evidence.mkdir(parents=True, exist_ok=True)
    metadata = capture_metadata()
    capabilities_report = {
        "capabilities": dict.fromkeys((
            "window_server", "key_window", "first_responder", "named_pasteboard",
            "appkit_key_delivery", "cg_event_delivery", "known_pixel_capture",
        ), "not_run:inspection_incomplete"),
        "screen_capture_preflight": None,
        "cg_event_post_preflight": None,
    }
    capabilities_report["capabilities"]["native_quit"] = "separate:see native Quit probe evidence"
    failed = False
    for measure, report in ((timing, {}), (capabilities, capabilities_report)):
        metadata[f"load_before_{measure.__name__}"] = os.getloadavg()
        try:
            measure(binary, scratch, report)
        except (ProbeError, OSError, subprocess.TimeoutExpired, UnicodeError) as error:
            failed = True
            report["status"] = "failed"
            report["error"] = str(error) if isinstance(error, ProbeError) else type(error).__name__
            print(f"{measure.__name__}: {report['error']}", file=sys.stderr)
        metadata[f"load_after_{measure.__name__}"] = os.getloadavg()
        write_json(evidence, f"runner-{measure.__name__}.json", report)
    write_json(evidence, "runner-environment.json", metadata)
    return int(failed)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ProbeError as error:
        sys.exit(str(error))
