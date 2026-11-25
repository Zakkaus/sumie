#!/usr/bin/env python3
"""U4: ping the runner's main thread through its proxy wake while AppKit animates.

usage: SUMIE_CGTOOL=PATH animations.py BINARY OUT_DIR [--sample] (see animations.sh)
Pings every 10 ms; per phase, reports the worst round trip and pings over 100 ms.
With --sample, attaches `sample` to the process for each phase (diagnostic reruns).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

CG = os.environ["SUMIE_CGTOOL"]
PERIOD = 0.010
LIMIT_NS = 100_000_000


def cg(*args):
    return subprocess.run([CG, *map(str, args)], capture_output=True, text=True, check=False).stdout.strip()


class Runner:
    def __init__(self, binary, home):
        env = {**os.environ, "SUMIE_MACOS_CI_RUNNER": "idle", "HOME": home,
               "XDG_CONFIG_HOME": os.path.join(home, "config")}
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        bufsize=0, env=env)
        self.lock = threading.Lock()
        self.sent = {}
        self.acked = {}
        self.lines = []
        self.window = threading.Event()
        self.running = True
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for raw in self.process.stdout:
            now = time.monotonic_ns()
            line = raw.decode().strip()
            fields = line.split()
            if fields[:2] == ["runner", "ack"]:
                with self.lock:
                    self.acked[int(fields[2])] = now
            else:
                self.lines.append((now, line))
                if line.startswith("runner first"):
                    self.window.set()

    def send(self, command):
        self.process.stdin.write((command + "\n").encode())

    def ping_loop(self):
        number = 0
        while self.running:
            number += 1
            with self.lock:
                self.sent[number] = time.monotonic_ns()
            self.send(f"ping {number}")
            time.sleep(PERIOD)

    def window_pings(self, start, end):
        with self.lock:
            rtts = []
            for number, sent in self.sent.items():
                if start <= sent <= end:
                    acked = self.acked.get(number)
                    rtts.append((acked or time.monotonic_ns()) - sent)
        return rtts


def phase(runner, results, name, action, length, sampled):
    sampler = None
    if sampled:
        out = results["dir"] / f"sample-{name}.txt"
        sampler = subprocess.Popen(["sample", str(runner.process.pid), str(int(length + 1)), "1",
                                    "-mayDie", "-file", str(out)],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(0.3)
    start = time.monotonic_ns()
    detail = action()
    time.sleep(length)
    end = time.monotonic_ns()
    time.sleep(0.3)
    rtts = sorted(runner.window_pings(start, end))
    entry = {
        "phase": name,
        "pings": len(rtts),
        "max_ms": round(rtts[-1] / 1e6, 2) if rtts else None,
        "p50_ms": round(rtts[len(rtts) // 2] / 1e6, 2) if rtts else None,
        "over_100ms": sum(rtt > LIMIT_NS for rtt in rtts),
        "detail": detail,
    }
    results["phases"].append(entry)
    print(json.dumps(entry), flush=True)
    if sampler:
        sampler.wait()
    time.sleep(1.0)


def main():
    binary, out = sys.argv[1], Path(sys.argv[2])
    sampled = "--sample" in sys.argv[3:]
    out.mkdir(parents=True, exist_ok=True)
    results = {"dir": out, "phases": [], "load": os.getloadavg()}
    with tempfile.TemporaryDirectory() as home:
        runner = Runner(binary, home)
        if not runner.window.wait(30):
            sys.exit("no first frame")
        pid = runner.process.pid
        threading.Thread(target=runner.ping_loop, daemon=True).start()
        time.sleep(1.0)
        results["front"] = cg("front", pid)

        def bounds():
            return cg("bounds", pid)

        def step(command):
            def run():
                runner.send(command)
                return None
            return run

        def cmd_m():
            front = cg("front", pid)
            cg("cmdm")
            return {"front": front}

        def minimize_button():
            front = cg("front", pid)
            x, y = cg("minbutton", pid).split()
            cg("click", x, y)
            return {"front": front, "button": [x, y]}

        def resize():
            x, y, w, h, _ = map(int, bounds().split())
            cg("drag", x + w - 2, y + h - 2, x + w + 198, y + h + 118, 1500)
            return {"before": [w, h], "after": bounds()}

        def menu():
            front = cg("front", pid)
            item = cg("menuitem", pid).split()
            mx, my, mw, mh = map(int, item)
            cg("hold", mx + mw // 2, my + mh // 2, 2000, 1000, 700)
            return {"front": front, "item": item}

        def check(after):
            def run():
                return {"window": bounds(), **({} if after is None else after())}
            return run

        phase(runner, results, "idle", lambda: None, 2.0, sampled)
        phase(runner, results, "cmd-m", cmd_m, 1.5, sampled)
        results["phases"][-1]["detail"]["after"] = bounds()
        phase(runner, results, "restore-0", step("restore"), 1.5, sampled)
        phase(runner, results, "minimize-button", minimize_button, 1.5, sampled)
        results["phases"][-1]["detail"]["after"] = bounds()
        phase(runner, results, "restore-1", step("restore"), 1.5, sampled)
        phase(runner, results, "minimize-after-handler", step("minimize-after"), 1.5, sampled)
        results["phases"][-1]["detail"] = {"after": bounds()}
        phase(runner, results, "restore-2", step("restore"), 1.5, sampled)
        phase(runner, results, "minimize-in-handler", step("minimize-in"), 1.5, sampled)
        results["phases"][-1]["detail"] = {"after": bounds()}
        phase(runner, results, "restore-3", step("restore"), 1.5, sampled)
        phase(runner, results, "live-resize", resize, 0.3, sampled)
        phase(runner, results, "menu-2s", menu, 0.3, sampled)
        phase(runner, results, "fullscreen-in", step("fullscreen"), 2.5, sampled)
        results["phases"][-1]["detail"] = {"after": bounds()}
        phase(runner, results, "fullscreen-out", step("windowed"), 2.5, sampled)
        results["phases"][-1]["detail"] = {"after": bounds()}
        runner.running = False
        time.sleep(0.2)
        runner.send("close")
        try:
            runner.process.wait(15)
        except subprocess.TimeoutExpired:
            runner.process.kill()
        results["exit"] = runner.process.returncode
        results["lines"] = [line for _, line in runner.lines]
    results["dir"] = str(out)
    (out / "animations.json").write_text(json.dumps(results, indent=1))
    print("exit", results["exit"])


if __name__ == "__main__":
    main()
