"""Tests for the SteamOS stand-ins (steamos-select-branch, steamos-update,
dbus-send): `python3 -m unittest discover -s images/steam`."""

import os
import subprocess
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def run(name, *args, env=None, err=None):
    return subprocess.run(
        [os.path.join(HERE, name), *args], env=env, stdout=subprocess.PIPE,
        stderr=err if err is not None else subprocess.PIPE, text=True, timeout=30,
        check=False,
    )


class Branch(unittest.TestCase):
    def test_current_and_list_are_rel(self):
        for flag in ("-c", "-l"):
            done = run("steamos-select-branch", flag)
            self.assertEqual((done.returncode, done.stdout), (0, "rel\n"))

    def test_anything_else_changes_nothing_and_succeeds(self):
        done = run("steamos-select-branch", "beta")
        self.assertEqual(done.returncode, 0)
        self.assertEqual(done.stdout, "")
        self.assertIn("no OS branch", done.stderr)


class Update(unittest.TestCase):
    def test_nothing_is_ever_available(self):
        for args in (["check"], ["apply"], []):
            done = run("steamos-update", *args)
            self.assertEqual(done.returncode, 7)
            self.assertEqual(done.stdout, "")


class DbusSend(unittest.TestCase):
    def rig(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.calls = os.path.join(d.name, "calls")
        bin_ = os.path.join(d.name, "bin")
        os.mkdir(bin_)
        for name in ("steam", "steamos-session-select", "real-dbus-send"):
            path = os.path.join(bin_, name)
            with open(path, "w") as f:
                f.write(f'#!/bin/sh\necho "{name} $*" >> "{self.calls}"\n')
                if name == "steam":
                    f.write("sleep 3\n")
            os.chmod(path, 0o755)
        self.run_dir = os.path.join(d.name, "run")
        os.mkdir(self.run_dir)
        self.env = {
            "PATH": f"{bin_}:{os.environ['PATH']}",
            "CHA_REAL_DBUS_SEND": os.path.join(bin_, "real-dbus-send"),
            "XDG_RUNTIME_DIR": self.run_dir,
        }
        self.err = os.path.join(d.name, "err")
        return d.name

    def send(self, *args, env=None):
        # stderr to a file: the detached steam inherits it.
        with open(self.err, "w+") as err:
            started = time.monotonic()
            done = run("dbus-send", *args, env=env or self.env, err=err)
            done.took = time.monotonic() - started
        return done

    def calls_made(self, count=1):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                with open(self.calls) as f:
                    lines = f.read().splitlines()
                if len(lines) >= count:
                    return lines
            except FileNotFoundError:
                pass
            time.sleep(0.05)
        raise AssertionError("expected calls didn't happen")

    def logind(self, method, *extra):
        return [
            "--system", "--print-reply", "--dest=org.freedesktop.login1",
            "/org/freedesktop/login1", f"org.freedesktop.login1.Manager.{method}",
            *extra,
        ]

    def test_power_off_shuts_steam_down_and_returns_at_once(self):
        self.rig()
        done = self.send(*self.logind("PowerOff", "boolean:true"))
        self.assertEqual(done.returncode, 0)
        self.assertLess(done.took, 2)
        self.assertEqual(self.calls_made(), ["steam -shutdown"])
        # The end is asked for, so gamescope's segfault on the way out isn't
        # taken for a crash.
        with open(os.path.join(self.run_dir, "steam-session")) as f:
            self.assertEqual(f.read(), "exit\n")

    def test_power_off_from_the_desktop_logs_out_too(self):
        self.rig()
        env = dict(self.env, CHA_STEAM_SESSION="desktop")
        done = self.send(*self.logind("PowerOff", "boolean:true"), env=env)
        self.assertEqual(done.returncode, 0)
        self.assertEqual(self.calls_made(), ["steamos-session-select --finish-desktop"])

    def test_power_off_that_cant_ask_ends_nothing(self):
        self.rig()
        env = dict(self.env, XDG_RUNTIME_DIR=os.path.join(self.run_dir, "missing"))
        done = self.send(*self.logind("PowerOff", "boolean:true"), env=env)
        self.assertEqual(done.returncode, 1)
        time.sleep(0.3)
        self.assertFalse(os.path.exists(self.calls))

    def test_reboot_restarts_gaming_mode(self):
        self.rig()
        done = self.send(*self.logind("Reboot", "boolean:true"))
        self.assertEqual(done.returncode, 0)
        self.assertEqual(self.calls_made(), ["steamos-session-select gamescope"])

    def test_suspend_and_the_rest_do_nothing(self):
        self.rig()
        for method in ("Suspend", "Hibernate", "SuspendThenHibernate"):
            done = self.send(*self.logind(method, "boolean:true"))
            self.assertEqual(done.returncode, 0)
        time.sleep(0.3)
        self.assertFalse(os.path.exists(self.calls))

    def test_other_calls_go_to_the_real_dbus_send(self):
        self.rig()
        args = ["--session", "--dest=org.freedesktop.Notifications", "/x", "org.x.Y.Z"]
        self.send(*args)
        self.assertEqual(self.calls_made(), ["real-dbus-send " + " ".join(args)])
        # Another system-bus destination is not logind.
        self.send("--system", "--dest=org.freedesktop.systemd1", "/x", "org.x.Manager.PowerOff")
        self.assertEqual(len(self.calls_made(2)), 2)

    def test_a_missing_real_dbus_send_is_127(self):
        self.rig()
        env = dict(self.env, CHA_REAL_DBUS_SEND="/nonexistent/dbus-send")
        done = self.send("--session", "/x", "org.x.Y.Z", env=env)
        self.assertEqual(done.returncode, 127)


if __name__ == "__main__":
    unittest.main()
