"""steam-gaming's gamescope command line, run against a fake dbus-run-session."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


class GamescopeCommandLine(unittest.TestCase):
    def run_script(self, env):
        with tempfile.TemporaryDirectory() as tmp:
            bin_dir = Path(tmp)
            args_file = bin_dir / "args"
            fake = bin_dir / "dbus-run-session"
            fake.write_text(f'#!/bin/sh\nfor a in "$@"; do printf "%s\\n" "$a"; done > "{args_file}"\n')
            fake.chmod(0o755)
            full_env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", **env}
            subprocess.run(["sh", str(HERE / "steam-gaming")], env=full_env, check=True, timeout=10)
            return args_file.read_text().splitlines()

    def test_gamescope_paints_at_the_games_cadence_at_the_streams_size_and_rate(self):
        args = self.run_script({"CHA_WIDTH": "1920", "CHA_HEIGHT": "1080", "CHA_REFRESH": "120"})
        gamescope = args[args.index("gamescope") :]
        self.assertEqual(gamescope[1:3], ["--backend", "wayland"])
        flag = lambda name: gamescope[gamescope.index(name) + 1]
        self.assertEqual((flag("-W"), flag("-H"), flag("-w"), flag("-h")), ("1920", "1080", "1920", "1080"))
        self.assertEqual(flag("-r"), "120")
        # VRR: the compositor reports a zero refresh for gamescope's window.
        self.assertIn("--adaptive-sync", gamescope)
        self.assertIn("--mangoapp", gamescope)
        # Steam is what gamescope runs.
        self.assertIn("steam -steamos3 -gamepadui", gamescope[-1])

    def test_the_defaults_are_1440p60(self):
        args = self.run_script({})
        gamescope = args[args.index("gamescope") :]
        self.assertEqual(gamescope[gamescope.index("-W") + 1], "2560")
        self.assertEqual(gamescope[gamescope.index("-r") + 1], "60")


if __name__ == "__main__":
    unittest.main()
