"""Tests for steamos-session-select: `python3 -m unittest discover -s images/steam`.

`steam` and `xfce4-session-logout` are stand-ins that record their calls;
CHA_PROC points at a fake /proc.
"""

import os
import subprocess
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steamos-session-select")


class Rig(tempfile.TemporaryDirectory):
    def __enter__(self):
        root = super().__enter__()
        self.run = os.path.join(root, "run")
        self.bin = os.path.join(root, "bin")
        self.proc = os.path.join(root, "proc")
        for d in (self.run, self.bin, self.proc):
            os.mkdir(d)
        self.calls = os.path.join(self.run, "calls")
        self.fake("xfce4-session-logout", f'echo "logout $*" >> "{self.calls}"\n')
        return self

    def fake(self, name, body):
        path = os.path.join(self.bin, name)
        with open(path, "w") as f:
            f.write("#!/bin/sh\n" + body)
        os.chmod(path, 0o755)

    def env(self, **extra):
        env = {
            "PATH": f"{self.bin}:{os.environ['PATH']}",
            "XDG_RUNTIME_DIR": self.run,
            "CHA_PROC": self.proc,
        }
        env.update(extra)
        return env

    def select(self, name, **extra):
        # Our stderr is a file, not a pipe: the detached command inherits it,
        # and a pipe would stay open until that command ended.
        with open(os.path.join(self.run, "stderr"), "w+") as err:
            done = subprocess.run(
                [SCRIPT, name], env=self.env(**extra), stdout=subprocess.PIPE,
                stderr=err, text=True, timeout=30, check=False,
            )
            err.seek(0)
            done.stderr = err.read()
        return done

    def request(self):
        try:
            with open(os.path.join(self.run, "steam-session")) as f:
                return f.read()
        except FileNotFoundError:
            return None

    def wait_calls(self, count):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            try:
                with open(self.calls) as f:
                    lines = f.read().splitlines()
                if len(lines) >= count:
                    return lines
            except FileNotFoundError:
                pass
            time.sleep(0.05)
        raise AssertionError("the background commands didn't run")

    def no_calls(self):
        time.sleep(0.5)
        if os.path.exists(self.calls):
            raise AssertionError("a command ran: " + open(self.calls).read())


class SessionSelect(unittest.TestCase):
    def test_names_map_to_modes(self):
        for name, mode in [
            ("gamescope", "gaming"),
            ("plasma", "desktop"),
            ("plasma-x11", "desktop"),
            ("plasma-wayland", "desktop"),
            ("desktop", "desktop"),
            ("plasma-persistent", "desktop"),
            ("plasma-wayland-persistent", "desktop"),
            ("gamescope-persistent", "gaming"),
        ]:
            with Rig() as rig:
                rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
                done = rig.select(name, CHA_STEAM_SESSION="gaming")
                self.assertEqual(done.returncode, 0, (name, done.stderr))
                self.assertEqual(rig.request(), mode + "\n", name)
                rig.wait_calls(1)

    def test_an_unknown_name_is_refused(self):
        with Rig() as rig:
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
            done = rig.select("weston", CHA_STEAM_SESSION="gaming")
            self.assertEqual(done.returncode, 2)
            self.assertIn("unknown session", done.stderr)
            self.assertIsNone(rig.request())
            rig.no_calls()

    def test_no_name_is_a_usage_error(self):
        with Rig() as rig:
            done = subprocess.run(
                [SCRIPT], env=rig.env(), capture_output=True, text=True,
                timeout=30, check=False,
            )
            self.assertEqual(done.returncode, 2)

    def test_the_request_is_written_whole_with_no_temp_file_left(self):
        with Rig() as rig:
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
            rig.select("plasma", CHA_STEAM_SESSION="gaming")
            self.assertEqual(os.listdir(rig.run).count("steam-session"), 1)
            self.assertEqual(
                [n for n in os.listdir(rig.run) if n.startswith("steam-session.")], []
            )

    def test_gaming_mode_shuts_steam_down_without_waiting_for_it(self):
        with Rig() as rig:
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\nsleep 5\n')
            started = time.monotonic()
            done = rig.select("plasma", CHA_STEAM_SESSION="gaming")
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertLess(time.monotonic() - started, 3)
            self.assertEqual(rig.wait_calls(1), ["steam -shutdown"])

    def test_the_desktop_logs_out_when_steam_isnt_running(self):
        with Rig() as rig:
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
            started = time.monotonic()
            done = rig.select("gamescope", CHA_STEAM_SESSION="desktop")
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertLess(time.monotonic() - started, 3)
            self.assertEqual(rig.request(), "gaming\n")
            self.assertEqual(rig.wait_calls(1), ["logout --logout --fast"])
            time.sleep(0.3)
            self.assertEqual(rig.wait_calls(1), ["logout --logout --fast"])

    def test_the_desktop_shuts_steam_down_first_and_waits_for_it(self):
        with Rig() as rig:
            entry = os.path.join(rig.proc, "4242")
            os.mkdir(entry)
            with open(os.path.join(entry, "comm"), "w") as f:
                f.write("steam\n")
            # steam -shutdown: the client ends a moment later.
            rig.fake(
                "steam",
                f'echo "steam $*" >> "{rig.calls}"\n'
                f'(sleep 0.5; rm -rf "{entry}") &\n',
            )
            done = rig.select("gamescope", CHA_STEAM_SESSION="desktop")
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(
                rig.wait_calls(2), ["steam -shutdown", "logout --logout --fast"]
            )
            self.assertFalse(os.path.exists(entry))

    def test_a_steam_that_wont_end_is_waited_for_only_so_long(self):
        with Rig() as rig:
            entry = os.path.join(rig.proc, "4242")
            os.mkdir(entry)
            with open(os.path.join(entry, "comm"), "w") as f:
                f.write("steam\n")
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
            rig.select("gamescope", CHA_STEAM_SESSION="desktop", CHA_STEAM_SHUTDOWN_WAIT="0.5")
            self.assertEqual(
                rig.wait_calls(2), ["steam -shutdown", "logout --logout --fast"]
            )

    def test_a_request_that_cant_be_written_ends_nothing(self):
        with Rig() as rig:
            rig.fake("steam", f'echo "steam $*" >> "{rig.calls}"\n')
            # A directory where the request file goes.
            os.mkdir(os.path.join(rig.run, "steam-session"))
            for session in ("gaming", "desktop"):
                done = rig.select("plasma", CHA_STEAM_SESSION=session)
                self.assertEqual(done.returncode, 1)
                self.assertIn("can't write", done.stderr)
            rig.no_calls()
            # And no runtime dir at all.
            done = rig.select("plasma", CHA_STEAM_SESSION="gaming", XDG_RUNTIME_DIR=os.path.join(rig.run, "none"))
            self.assertEqual(done.returncode, 1)
            rig.no_calls()


if __name__ == "__main__":
    unittest.main()
