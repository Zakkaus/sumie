"""Exercise gate verdicts without launching a compositor or a macOS runner."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
GATE = runpy.run_path(str(SCRIPTS / "check-gate.py"))["main"]


class GateTests(unittest.TestCase):
    def test_pasteboard_rejects_a_duplicate_in_the_final_record(self):
        pasteboard = runpy.run_path(str(SCRIPTS / "macos/pasteboard.py"))
        plain = pasteboard["CONTENT"].encode().hex()
        framed = pasteboard["framed"](pasteboard["CONTENT"])
        check = Mock()
        check.keys.await_recorded.side_effect = [plain, framed]
        check.keys.stop.side_effect = [plain, framed * 2]
        with contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(pasteboard["Violation"], "pasteboard: framed: recorded"):
                pasteboard["case_paste"](check)

    def test_panes_cannot_check_without_kscreen_doctor(self):
        with patch.object(sys, "platform", "linux"), patch.object(sys, "argv", ["check-gate.py", "panes"]), \
             patch("gates.panes.shutil.which", side_effect=lambda tool: None if tool == "kscreen-doctor" else tool), \
             patch("gates.panes.built_binary", side_effect=AssertionError("fixture cannot run")), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(GATE(), 2)
        self.assertIn("kscreen-doctor", stderr.getvalue())

    def test_panes_distinguishes_setup_errors_from_stalls(self):
        for stdout, expected in (("", 2), ("FAIL: pane 7 starved: a line waited\n", 1)):
            with self.subTest(expected=expected), tempfile.TemporaryDirectory() as artifacts, \
                 patch.object(sys, "platform", "linux"), patch.object(sys, "argv", ["check-gate.py", "panes"]), \
                 patch("gates.panes.shutil.which", side_effect=lambda tool: tool), \
                 patch("gates.panes.built_binary", return_value="sumie"), \
                 patch("gates.panes.tempfile.mkdtemp", return_value=artifacts), \
                 patch("gates.panes.subprocess.run", return_value=subprocess.CompletedProcess([], 1, stdout)), \
                 contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(GATE(), expected)
                if stdout:
                    self.assertIn(stdout, output.getvalue())

    def test_macos_quit_preserves_fixture_exit_status(self):
        for status in (0, 1, 2):
            with self.subTest(status=status), patch.object(sys, "argv", ["check-gate.py", "macos-quit"]), \
                 patch.dict(os.environ, SUMIE_SPIKE_EVIDENCE="unused"), \
                 patch.object(subprocess, "call", return_value=status), contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(GATE(), status)

    def test_contrast_probe_requires_the_relevant_assertion_only(self):
        controls = runpy.run_path(str(SCRIPTS / "negative-controls.py"))["controls"]
        from negative import probe

        record = Mock()
        with patch.dict(controls.__globals__, probe=record, input_controls=Mock(), macos_controls=Mock()), \
             patch("shutil.which", return_value=None), contextlib.redirect_stdout(io.StringIO()):
            controls()
        args, kwargs = next(call for call in record.call_args_list if call.args[0] == "contrast above the supported range")
        _, _, file, *_ = args
        for message, failures, accepted in (
            ("File exists (os error 17)", 1, False),
            ("called `Result::unwrap_err()` on an `Ok` value: Config { minimum_contrast: Some(21.00000001) }", 2, False),
            ("called `Result::unwrap_err()` on an `Ok` value: Config { minimum_contrast: Some(21.00000001) }", 1, True),
        ):
            with self.subTest(message=message, failures=failures), tempfile.TemporaryDirectory() as scratch:
                path = Path(scratch) / file
                path.parent.mkdir(parents=True)
                original = "(1.0..=21.0).contains(&ratio)"
                path.write_text(original)
                output = (f"test tests::load_configuration ... FAILED\n{message}\n"
                          f"test result: FAILED. 0 passed; {failures} failed; 0 ignored;\n")
                with patch.object(probe, "ROOT", Path(scratch)), \
                     patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 1, output)), \
                     contextlib.redirect_stdout(io.StringIO()):
                    if accepted:
                        probe.probe(*args, **kwargs)
                    else:
                        with self.assertRaises(RuntimeError):
                            probe.probe(*args, **kwargs)

    def test_darwin_fast_does_not_run_window_scenarios(self):
        with tempfile.TemporaryDirectory() as scratch:
            bin_dir = Path(scratch)
            for name, script in {
                "uname": "echo Darwin\n",
                "python3": 'case "${2-}" in macos-*) exit 2;; clippy-macos-ci) echo "$2";; esac\n',
            }.items():
                tool = bin_dir / name
                tool.write_text(f"#!/bin/sh\n{script}")
                tool.chmod(0o700)
            result = subprocess.run(["sh", SCRIPTS / "check-all.sh", "--fast"],
                                    env={**os.environ, "PATH": f"{scratch}{os.pathsep}{os.defpath}"},
                                    capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "clippy-macos-ci")


if __name__ == "__main__":
    unittest.main()
