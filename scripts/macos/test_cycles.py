"""Platform-independent regressions for the macOS cycle sampler."""
import subprocess
import unittest
from unittest.mock import patch

import cycles


class CycleResources(unittest.TestCase):
    def sample(self, baseline, readings, *, now=0.0):
        values = {key: iter(series) for key, series in readings.items()}

        def resources(_pid, pending, _deadline):
            for key in values:
                if key in pending:
                    yield key, next(values[key])

        def sleep(delay):
            nonlocal now
            now += delay

        with (patch.object(cycles, "COUNTS", tuple(values)),
              patch.object(cycles, "resources", resources),
              patch.object(cycles.time, "monotonic", lambda: now),
              patch.object(cycles.time, "sleep", sleep)):
            return cycles.settled(42, baseline, 0.0)

    def test_lsof_counts_only_numeric_fds_and_rejects_failed_output(self):
        command = ["lsof", "-n", "-P", "-p", "42", "-Ff"]
        output = "p42\nfcwd\nftxt\nfmem\nf0\nf3\n"
        with patch.object(cycles.subprocess, "run", side_effect=[
            subprocess.CompletedProcess(command, 1, output),
            subprocess.CompletedProcess(command, 0, output),
        ]) as run:
            deadline = cycles.time.monotonic() + 6
            self.assertEqual(dict(cycles.resources(42, {"fds"}, deadline)), {"fds": None})
            self.assertEqual(dict(cycles.resources(42, {"fds"}, deadline)), {"fds": {0, 3}})
        self.assertEqual(list(run.call_args.args[0]), command)

    def test_baseline_waits_for_each_metric_to_agree_not_its_minimum(self):
        sample, pending, _, _ = self.sample(None, {"fds": [None, {0}, {0}], "threads": [2, 3, 3]})
        self.assertEqual(sample, {"fds": 1, "threads": 3})
        self.assertFalse(pending)

    def test_transient_region_spike_does_not_require_resampling_settled_fds(self):
        baseline = {"fds": 1, "iosurface_regions": 1}
        sample, pending, _, regions = self.sample(baseline, {
            "fds": [{0}, {0}], "iosurface_regions": [3, 1, None, 1, 0],
        })
        self.assertFalse(pending)
        self.assertEqual(sample, {"fds": 1, "iosurface_regions": 0})
        self.assertEqual(regions[-1], (1.0, "iosurface_regions", 0))

    def test_fd_and_surface_leaks_do_not_get_a_new_deadline_after_reap(self):
        baseline = {"fds": 1, "iosurface_regions": 1}
        sample, pending, fds, regions = self.sample(baseline, {
            "fds": [{0, 8}], "iosurface_regions": [2],
        }, now=5.75)
        self.assertEqual(pending, set(baseline))
        self.assertEqual(sample, {"fds": 2, "iosurface_regions": 2})
        self.assertEqual(fds, {0, 8})
        self.assertEqual(regions, [(5.75, "iosurface_regions", 2)])


if __name__ == "__main__":
    unittest.main()
