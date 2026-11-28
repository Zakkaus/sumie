"""The window-cycle sampler for lifetime.py's cycles case: descriptor, thread, child and
GPU-region counts of the runner after each close, until they settle or the cleanup bound ends."""
import re
import subprocess
import time

# The design book's cleanup bound: a closed window's resources are back within 6 s.
CLEANUP = 6.0
REGIONS = {"iosurface_regions": "IOSurface", "ioaccelerator_regions": "IOAccelerator (graphics)"}
COUNTS = ("fds", "threads", "children", "zombies", *REGIONS)


def resources(pid, pending, deadline):
    """Sample only the counts that have not settled; a failed probe yields None."""
    def read(*command):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=remaining)
        except (OSError, subprocess.TimeoutExpired):
            return None
        return result.stdout.splitlines() if result.returncode == 0 else None

    if "fds" in pending:
        rows = read("lsof", "-n", "-P", "-p", str(pid), "-Ff")
        yield "fds", None if rows is None else {int(row[1:]) for row in rows if re.fullmatch(r"f[0-9]+", row)}
    if "threads" in pending:
        rows = read("ps", "-M", "-p", str(pid))
        yield "threads", len(rows) - 1 if rows and len(rows) > 1 else None
    if pending & {"children", "zombies"}:
        rows = read("ps", "-A", "-o", "ppid=,stat=")
        children = None if not rows else [stat for ppid, stat in map(str.split, rows) if int(ppid) == pid]
        if "children" in pending:
            yield "children", None if children is None else len(children)
        if "zombies" in pending:
            yield "zombies", None if children is None else sum(stat.startswith("Z") for stat in children)
    if pending & REGIONS.keys():
        rows = read("vmmap", "-summary", str(pid))
        for key, name in REGIONS.items():
            if key in pending:
                # A type with no regions has no row.
                yield key, None if not rows else next((int(row.split()[-1]) for row in rows
                                                       if re.match(rf"{re.escape(name)}\s+\d", row)), 0)


def settled(pid, baseline, closed):
    """Freeze each count after two valid samples agree, or both meet its baseline."""
    deadline = closed + CLEANUP
    pending = set(COUNTS)
    previous, sample, regions, fds = {}, {}, [], None
    while pending and time.monotonic() < deadline:
        for key, value in resources(pid, pending, deadline):
            at = time.monotonic()
            if key in REGIONS:
                regions.append((round(at - closed, 3), key, value))
            if value is None or at > deadline:
                previous.pop(key, None)
                continue
            if key == "fds":
                fds, value = value, len(value)
            sample[key] = value
            prior = previous.get(key)
            if prior is not None and (value == prior if baseline is None else max(prior, value) <= baseline[key]):
                pending.remove(key)
            previous[key] = value
        if pending:
            time.sleep(max(0, min(0.25, deadline - time.monotonic())))
    return sample, pending, fds, regions
