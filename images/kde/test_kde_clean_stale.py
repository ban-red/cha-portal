"""Tests for kde-clean-stale: what it clears from a persisted home, and what it leaves."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("kde-clean-stale")
# What Qt's QLockFile writes: pid, application, host name, host id.
LOCK = "4242\nkonsole\nabc123container\n0a1b2c3d\n"


class CleanStale(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.home = Path(self._tmp.name) / "home"
        self.home.mkdir()

    def put(self, rel, text="x"):
        path = self.home / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
        return path

    def run_script(self, *args):
        return subprocess.run(
            ["sh", str(SCRIPT), *(args or [str(self.home)])],
            check=True,
            capture_output=True,
            text=True,
        )

    def test_clears_qt_lock_files(self):
        locks = [
            self.put(".config/kwinrc.lock", LOCK),
            self.put(".config/plasma-org.kde.plasma.desktop-appletsrc.lock", LOCK),
            self.put(".local/share/kded6/x.lock", LOCK),
            self.put(".cache/some/deeper/thing.lock", LOCK),
        ]
        self.run_script()
        for lock in locks:
            self.assertFalse(lock.exists(), lock)

    def test_leaves_files_the_user_made(self):
        keep = [
            self.put("Desktop/notes.lock", "mine"),  # not a pid
            self.put("Documents/big.lock", "1\n" * 1000),  # too big
            self.put(".config/konsolerc", "[General]\nx=1\n"),
            self.put(".config/kwinrc", "[Compositing]\nx=1\n"),
            self.put("Desktop/wallpaper.png", "png"),
            self.put(".config/session/ksmserver_x", "restore"),
            self.put(".local/share/konsole/Shell.profile", "p"),
            self.put(".config/odd.lock", "7\n"),  # a pid and no host
            self.put("Documents/todo.lock", LOCK),  # outside the KDE dirs
        ]
        self.run_script()
        for path in keep:
            self.assertTrue(path.exists(), path)

    def test_clears_caches_and_ice_state(self):
        gone = [
            self.put(".cache/ksycoca6_en_abc", "cache"),
            self.put(".ICEauthority", "auth"),
            self.put(".ICEauthority-c", ""),
            self.put(".ICEauthority-l", ""),
            self.put(".dbus/session-bus/machine-0", "DBUS_SESSION_BUS_ADDRESS=x"),
        ]
        keep = [self.put(".cache/thumbnails/a.png", "t"), self.put(".Xauthority", "x")]
        self.run_script()
        for path in gone:
            self.assertFalse(path.exists(), path)
        for path in keep:
            self.assertTrue(path.exists(), path)

    def test_never_follows_a_link_out_of_the_home(self):
        outside = Path(self._tmp.name) / "outside"
        outside.mkdir()
        target = outside / "victim.lock"
        target.write_text(LOCK)
        (self.home / ".config").mkdir()
        os.symlink(target, self.home / ".config" / "link.lock")
        os.symlink(outside, self.home / ".cache")
        self.put(".local/share/a/b.lock", LOCK)
        self.run_script()
        self.assertTrue(target.exists())
        self.assertTrue((self.home / ".config" / "link.lock").is_symlink())

    def test_an_empty_or_missing_home_is_fine(self):
        self.run_script()
        self.run_script(str(self.home / "nope"))

    def test_running_twice_changes_nothing_more(self):
        self.put(".config/kwinrc", "[x]\n")
        self.put(".config/kwinrc.lock", LOCK)
        self.run_script()
        self.run_script()
        self.assertEqual((self.home / ".config/kwinrc").read_text(), "[x]\n")
        self.assertFalse((self.home / ".config/kwinrc.lock").exists())


if __name__ == "__main__":
    unittest.main()
