"""Tests for steam-run: `python3 -m unittest discover -s images/steam`.

The command it runs is a stand-in shell script; steam-status is a stand-in
that records that it ran and clears the status when it is ended, as the real
one does.
"""

import json
import os
import signal
import subprocess
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-run")

FAKE_STATUS = """#!/bin/sh
touch "$XDG_RUNTIME_DIR/status-ran"
trap 'rm -f "$XDG_RUNTIME_DIR/status"; exit 0' TERM
while :; do sleep 0.1; done
"""


class Rig(tempfile.TemporaryDirectory):
    """A home, a runtime dir and a bin dir with the fake steam-status."""

    def __enter__(self):
        root = super().__enter__()
        self.home = os.path.join(root, "home")
        self.run = os.path.join(root, "run")
        self.bin = os.path.join(root, "bin")
        for d in (self.home, self.run, self.bin):
            os.mkdir(d)
        with open(os.path.join(self.bin, "steam-status"), "w") as f:
            f.write(FAKE_STATUS)
        os.chmod(os.path.join(self.bin, "steam-status"), 0o755)
        return self

    @property
    def log(self):
        return os.path.join(self.home, ".local/state/cha/gamescope.log")

    def env(self):
        return {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "HOME": self.home,
            "XDG_RUNTIME_DIR": self.run,
            "CHA_STEAM_STATUS_HOLD": "0",
        }

    def run_script(self, script, **kw):
        return subprocess.run(
            [SCRIPT, "sh", "-c", script],
            env=self.env(),
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
            **kw,
        )

    def status(self):
        try:
            with open(os.path.join(self.run, "status")) as f:
                return json.load(f)
        except FileNotFoundError:
            return None

    def read(self, path):
        with open(path) as f:
            return f.read()


class SteamRun(unittest.TestCase):
    def test_a_clean_exit_is_quiet_and_logged(self):
        with Rig() as rig:
            done = rig.run_script("echo hello; echo oops >&2; sleep 0.5; exit 0")
            self.assertEqual(done.returncode, 0)
            # stdout and stderr both reach the container's output.
            self.assertIn("hello", done.stdout)
            self.assertIn("oops", done.stdout)
            self.assertNotIn("crashed", done.stderr)
            text = rig.read(rig.log)
            self.assertIn("hello", text)
            self.assertIn("oops", text)
            self.assertIsNone(rig.status())
            self.assertTrue(os.path.exists(os.path.join(rig.run, "status-ran")))

    def test_an_abort_is_named_in_the_log_and_the_status(self):
        with Rig() as rig:
            done = rig.run_script("echo before; kill -ABRT $$")
            self.assertEqual(done.returncode, 134)
            self.assertIn("gamescope crashed (SIGABRT), exit code 134", done.stderr)
            self.assertIn("before", done.stdout)
            self.assertIn("gamescope crashed (SIGABRT)", rig.read(rig.log))
            self.assertEqual(rig.status(), {"label": "gamescope crashed (SIGABRT)"})

    def test_a_shell_style_134_is_an_abort_too(self):
        # dbus-run-session reports a signalled child as 128 + the signal.
        with Rig() as rig:
            done = rig.run_script("exit 134")
            self.assertEqual(done.returncode, 134)
            self.assertEqual(rig.status(), {"label": "gamescope crashed (SIGABRT)"})

    def test_another_failure_keeps_its_code(self):
        with Rig() as rig:
            done = rig.run_script("exit 3")
            self.assertEqual(done.returncode, 3)
            self.assertIn("exited with code 3", done.stderr)
            self.assertEqual(
                rig.status(), {"label": "gamescope or Steam exited with code 3"}
            )

    def test_the_status_helper_does_not_wipe_the_crash(self):
        # steam-status clears the status as it ends: steam-run waits for that
        # before it writes its own.
        with Rig() as rig:
            rig.run_script("exit 3")
            self.assertIsNotNone(rig.status())

    def test_earlier_runs_are_kept_up_to_a_limit(self):
        with Rig() as rig:
            for n in range(7):
                rig.run_script(f"echo run{n}")
            directory = os.path.dirname(rig.log)
            names = sorted(os.listdir(directory))
            self.assertEqual(
                names,
                ["gamescope.log", "gamescope.log.1", "gamescope.log.2",
                 "gamescope.log.3", "gamescope.log.4"],
            )
            self.assertIn("run6", rig.read(rig.log))
            self.assertIn("run5", rig.read(rig.log + ".1"))
            self.assertIn("run2", rig.read(rig.log + ".4"))

    def test_a_run_log_is_bounded_and_keeps_the_end(self):
        with Rig() as rig:
            done = rig.run_script(
                "i=0; while [ $i -lt 40000 ]; do echo line-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done; kill -ABRT $$"
            )
            self.assertEqual(done.returncode, 134)
            self.assertIn("line-39999-", done.stdout)
            size = os.path.getsize(rig.log)
            self.assertLessEqual(size, 1 << 20)
            text = rig.read(rig.log)
            self.assertIn("earlier output of this run dropped", text)
            self.assertIn("line-39999-", text)
            self.assertIn("gamescope crashed (SIGABRT)", text)
            self.assertNotIn("line-0-", text)

    def test_a_home_that_cant_be_written_still_runs(self):
        with Rig() as rig:
            # HOME is a file: no state directory can be made.
            bad = os.path.join(rig.home, "file")
            open(bad, "w").close()
            env = rig.env()
            env["HOME"] = bad
            done = subprocess.run(
                [SCRIPT, "sh", "-c", "echo still; exit 0"],
                env=env, capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 0)
            self.assertIn("still", done.stdout)

    def test_output_left_by_orphans_holding_the_pipe_doesnt_hang_it(self):
        with Rig() as rig:
            started = time.monotonic()
            done = rig.run_script("(sleep 30 &) ; exit 0")
            self.assertEqual(done.returncode, 0)
            self.assertLess(time.monotonic() - started, 10)

    def test_a_stop_is_passed_on_and_is_clean(self):
        with Rig() as rig:
            proc = subprocess.Popen(
                [SCRIPT, "sh", "-c", "trap 'exit 143' TERM; echo up; while :; do sleep 0.1; done"],
                env=rig.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            )
            self.assertEqual(proc.stdout.readline().strip(), "up")
            proc.send_signal(signal.SIGTERM)
            out, err = proc.communicate(timeout=30)
            self.assertEqual(proc.returncode, 0)
            self.assertNotIn("crashed", err)
            self.assertIsNone(rig.status())

    def test_an_end_after_a_switch_request_is_not_a_crash(self):
        # Nested gamescope segfaults as Steam shuts down for a switch.
        with Rig() as rig:
            with open(os.path.join(rig.run, "steam-session"), "w") as f:
                f.write("desktop\n")
            done = rig.run_script("exit 139")
            self.assertEqual(done.returncode, 0)
            self.assertNotIn("crashed", done.stderr)
            self.assertIsNone(rig.status())
            self.assertIn("ended for a session switch, exit code 139", rig.read(rig.log))

    def test_a_named_session_is_named_in_its_messages(self):
        with Rig() as rig:
            done = subprocess.run(
                [SCRIPT, "--name", "desktop", "sh", "-c", "kill -ABRT $$"],
                env=rig.env(), capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 134)
            self.assertIn("desktop crashed (SIGABRT), exit code 134", done.stderr)
            self.assertEqual(rig.status(), {"label": "desktop crashed (SIGABRT)"})
            text = rig.read(rig.log)
            self.assertIn("starting desktop: sh -c", text)
            done = subprocess.run(
                [SCRIPT, "--name", "desktop", "sh", "-c", "exit 3"],
                env=rig.env(), capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 3)
            self.assertEqual(rig.status(), {"label": "desktop exited with code 3"})
            # Still one log file per run, under the same name.
            self.assertTrue(os.path.exists(rig.log + ".1"))

    def test_the_default_name_is_gamescope(self):
        with Rig() as rig:
            rig.run_script("exit 0")
            self.assertIn("starting gamescope: sh -c", rig.read(rig.log))

    def test_a_name_without_a_command_is_a_usage_error(self):
        with Rig() as rig:
            done = subprocess.run(
                [SCRIPT, "--name", "desktop"],
                env=rig.env(), capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 2)

    def test_a_missing_command_fails_plainly(self):
        with Rig() as rig:
            done = subprocess.run(
                [SCRIPT, "no-such-command-here"],
                env=rig.env(), capture_output=True, text=True, timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 127)
            self.assertIn("can't start", done.stderr)


if __name__ == "__main__":
    unittest.main()
