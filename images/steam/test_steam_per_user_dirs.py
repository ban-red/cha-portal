"""Tests for steam-per-user-dirs: `python3 -m unittest discover -s images/steam`.

It reads a mountinfo file (`CHA_MOUNTINFO`) that the tests write.
"""

import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-per-user-dirs")

LIB = "/mnt/games/steam"
COMPAT = f"{LIB}/steamapps/compatdata"
SHADER = f"{LIB}/steamapps/shadercache"


def mountinfo(*points):
    lines = [
        "1065 1020 0:52 / / rw,relatime - overlay overlay rw,lowerdir=/x",
        "1066 1065 0:55 / /proc rw,nosuid - proc proc rw",
    ]
    for n, point in enumerate(points):
        lines.append(f"{1100 + n} 1065 0:{70 + n} / {point} rw,relatime - nfs4 nas:/s rw")
    return "\n".join(lines) + "\n"


def check(dirs, info):
    """Runs the script: its exit code and what it said."""
    with tempfile.NamedTemporaryFile("w", suffix=".mountinfo") as f:
        f.write(info)
        f.flush()
        env = {"PATH": os.environ["PATH"], "CHA_MOUNTINFO": f.name}
        if dirs is not None:
            env["CHA_PER_USER_DIRS"] = dirs
        done = subprocess.run(
            ["sh", SCRIPT], env=env, capture_output=True, text=True, check=False
        )
    return done.returncode, done.stderr


class PerUserDirs(unittest.TestCase):
    def test_nothing_to_check_without_any(self):
        self.assertEqual(check(None, mountinfo()), (0, ""))
        self.assertEqual(check("", mountinfo()), (0, ""))

    def test_all_mounted(self):
        info = mountinfo(LIB, COMPAT, SHADER)
        self.assertEqual(check(f"{COMPAT}:{SHADER}", info), (0, ""))

    def test_one_missing_stops_the_start_and_says_which(self):
        code, said = check(f"{COMPAT}:{SHADER}", mountinfo(LIB, SHADER))
        self.assertEqual(code, 1)
        self.assertIn(f"({COMPAT})", said)
        self.assertNotIn(SHADER, said)
        self.assertIn("Stop this app and start it again", said)

    def test_all_missing_lists_all(self):
        code, said = check(f"{COMPAT}:{SHADER}", mountinfo(LIB))
        self.assertEqual(code, 1)
        self.assertIn(f"({COMPAT}, {SHADER})", said)

    def test_the_shared_directory_itself_isnt_enough(self):
        self.assertEqual(check(COMPAT, mountinfo(LIB))[0], 1)

    def test_only_the_mount_point_field_counts(self):
        # The path as a source, in another field, isn't a mount.
        info = mountinfo(LIB).replace("nas:/s", COMPAT)
        self.assertEqual(check(COMPAT, info)[0], 1)

    def test_a_prefix_or_a_longer_path_isnt_the_mount(self):
        info = mountinfo(f"{COMPAT}/appid", f"{COMPAT}2")
        self.assertEqual(check(COMPAT, info)[0], 1)

    def test_escaped_spaces(self):
        spaced = "/mnt/my games/steam/steamapps/compatdata"
        info = mountinfo(spaced.replace(" ", "\\040"))
        self.assertEqual(check(spaced, info), (0, ""))

    def test_an_unreadable_mountinfo_stops_the_start_too(self):
        code, _ = check(COMPAT, "")
        self.assertEqual(code, 1)


if __name__ == "__main__":
    unittest.main()
