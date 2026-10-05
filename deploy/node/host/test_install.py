"""Tests for install.sh: `python3 -m unittest discover -s deploy/node/host`.

The script runs for real against a temp directory (CHA_HOST_ROOT is the prefix
of every destination), with udevadm, apparmor_parser and modprobe stubbed on
PATH to log their calls.
"""

import os
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "install.sh")

RULES = "etc/udev/rules.d/72-cha-virtual-pads.rules"
PROFILE = "etc/apparmor.d/cha-sandbox"
MODULES = "etc/modules-load.d/cha.conf"


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.root = os.path.join(self.tmp, "root")
        self.bin = os.path.join(self.tmp, "bin")
        self.log = os.path.join(self.tmp, "calls")
        os.makedirs(os.path.join(self.root, "etc/apparmor.d"))
        os.makedirs(self.bin)
        for tool in ("udevadm", "apparmor_parser", "modprobe"):
            self.stub(tool)

    def stub(self, tool):
        path = os.path.join(self.bin, tool)
        with open(path, "w") as f:
            f.write(f'#!/bin/sh\necho "{tool} $*" >> "{self.log}"\n')
        os.chmod(path, 0o755)

    def run_script(self, *args, path=None):
        env = dict(os.environ, CHA_HOST_ROOT=self.root)
        env["PATH"] = path or self.bin + os.pathsep + os.environ["PATH"]
        return subprocess.run(
            ["sh", SCRIPT, *args], env=env, capture_output=True, text=True
        )

    def calls(self):
        if not os.path.exists(self.log):
            return []
        with open(self.log) as f:
            return f.read().splitlines()

    def dest(self, rel):
        return os.path.join(self.root, rel)

    def test_installs_everything_then_changes_nothing(self):
        first = self.run_script()
        self.assertEqual(first.returncode, 0, first.stderr)
        for rel in (RULES, PROFILE, MODULES):
            self.assertTrue(os.path.exists(self.dest(rel)), rel)
            self.assertEqual(os.stat(self.dest(rel)).st_mode & 0o777, 0o644)
        self.assertEqual(first.stdout.count("installed"), 3)
        self.assertEqual(
            self.calls(),
            [
                "udevadm control --reload",
                "udevadm trigger --subsystem-match=input --subsystem-match=hidraw",
                f"apparmor_parser -r -W {self.dest(PROFILE)}",
                "modprobe uinput",
                "modprobe uhid",
            ],
        )
        with open(self.dest(MODULES)) as f:
            modules = f.read().split()
        self.assertIn("uinput", modules)
        self.assertIn("uhid", modules)

        os.remove(self.log)
        again = self.run_script()
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(again.stdout.count("unchanged"), 3)
        # Nothing to reload; the modules are loaded each time (cheap).
        self.assertEqual(self.calls(), ["modprobe uinput", "modprobe uhid"])

    def test_updates_only_what_differs(self):
        self.run_script()
        os.remove(self.log)
        with open(self.dest(RULES), "w") as f:
            f.write("old\n")
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("updated", result.stdout)
        self.assertEqual(result.stdout.count("unchanged"), 2)
        calls = self.calls()
        self.assertIn("udevadm control --reload", calls)
        self.assertFalse(any(c.startswith("apparmor_parser") for c in calls))
        with open(self.dest(RULES)) as f, open(os.path.join(HERE, "72-cha-virtual-pads.rules")) as g:
            self.assertEqual(f.read(), g.read())

    def test_check_reports_without_changing(self):
        result = self.run_script("--check")
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing", result.stdout)
        self.assertIn("deploy/node/host/install.sh", result.stdout)
        self.assertFalse(os.path.exists(self.dest(RULES)))
        self.assertEqual(self.calls(), [])

        self.run_script()
        os.remove(self.log)
        current = self.run_script("--check")
        self.assertEqual(current.returncode, 0, current.stdout)
        self.assertEqual(current.stdout.count("unchanged"), 3)
        self.assertEqual(self.calls(), [])

        with open(self.dest(PROFILE), "w") as f:
            f.write("old\n")
        stale = self.run_script("--check")
        self.assertEqual(stale.returncode, 1)
        self.assertIn("outdated", stale.stdout)

    def test_skips_apparmor_on_a_host_without_it(self):
        os.rmdir(self.dest("etc/apparmor.d"))
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("skipped", result.stdout)
        self.assertIn("no AppArmor", result.stdout)
        self.assertFalse(os.path.exists(self.dest(PROFILE)))
        self.assertTrue(os.path.exists(self.dest(RULES)))
        check = self.run_script("--check")
        self.assertEqual(check.returncode, 0, check.stdout)

    def test_a_failing_tool_fails_the_run_after_the_rest_is_done(self):
        with open(os.path.join(self.bin, "modprobe"), "w") as f:
            f.write("#!/bin/sh\nexit 1\n")
        result = self.run_script()
        self.assertEqual(result.returncode, 1)
        self.assertIn("failed: modprobe", result.stderr)
        self.assertTrue(os.path.exists(self.dest(MODULES)))

    def test_refuses_without_root(self):
        if os.geteuid() == 0:
            self.skipTest("running as root")
        env = {k: v for k, v in os.environ.items() if k != "CHA_HOST_ROOT"}
        result = subprocess.run(
            ["sh", SCRIPT], env=env, capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("as root", result.stderr)

    def test_help_and_unknown_arguments(self):
        self.assertEqual(self.run_script("--help").returncode, 0)
        self.assertIn("--check", self.run_script("--help").stdout)
        self.assertEqual(self.run_script("--nope").returncode, 2)


if __name__ == "__main__":
    unittest.main()
