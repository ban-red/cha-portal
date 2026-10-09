"""Tests for steam-drives: `python3 -m unittest discover -s images/steam`."""

import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-drives")


class Drives(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        self.shared = os.path.join(self.tmp, "games")
        os.makedirs(self.shared)
        self.library = os.path.join(self.home, ".local/share/Steam")

    def prefix(self, root, name, dosdevices=True):
        """A compatdata entry; its dosdevices has c: and z: as Wine makes them."""
        path = os.path.join(root, "steamapps/compatdata", name, "pfx")
        if dosdevices:
            path = os.path.join(path, "dosdevices")
            os.makedirs(path)
            os.symlink("../drive_c", os.path.join(path, "c:"))
            os.symlink("/", os.path.join(path, "z:"))
        else:
            os.makedirs(path)
        return path

    def run_once(self, directory="default"):
        directory = self.shared if directory == "default" else directory
        env = dict(os.environ, HOME=self.home)
        args = [sys.executable, SCRIPT, "--once"] + ([directory] if directory is not None else [])
        return subprocess.run(args, env=env, capture_output=True, text=True, timeout=30)

    def letters(self, dosdevices):
        return sorted(n for n in os.listdir(dosdevices) if not n.startswith("."))

    def test_adds_d_to_a_prefix_without_it(self):
        dd = self.prefix(self.library, "1")
        done = self.run_once()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(os.readlink(os.path.join(dd, "d:")), self.shared)
        self.assertEqual(self.letters(dd), ["c:", "d:", "z:"])
        self.assertEqual(os.readlink(os.path.join(dd, "z:")), "/")
        self.assertIn("steam-drives: d: ->", done.stderr)

    def test_leaves_a_prefix_alone_when_a_letter_points_at_the_directory(self):
        dd = self.prefix(self.library, "1")
        alias = os.path.join(self.tmp, "alias")
        os.symlink(self.shared, alias)  # the same directory by another path
        os.symlink(alias, os.path.join(dd, "g:"))
        before = os.stat(dd).st_mtime_ns
        done = self.run_once()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.letters(dd), ["c:", "g:", "z:"])
        self.assertEqual(os.stat(dd).st_mtime_ns, before)
        self.assertEqual(done.stderr, "")

    def test_uses_e_when_d_points_elsewhere(self):
        dd = self.prefix(self.library, "1")
        other = os.path.join(self.tmp, "other")
        os.makedirs(other)
        os.symlink(other, os.path.join(dd, "d:"))
        self.run_once()
        self.assertEqual(os.readlink(os.path.join(dd, "d:")), other)
        self.assertEqual(os.readlink(os.path.join(dd, "e:")), self.shared)

    def test_skips_to_the_first_free_letter(self):
        dd = self.prefix(self.library, "1")
        for letter in "de":
            os.symlink(self.tmp, os.path.join(dd, f"{letter}:"))
        self.run_once()
        self.assertEqual(os.readlink(os.path.join(dd, "f:")), self.shared)

    def test_skips_an_entry_without_dosdevices(self):
        bare = self.prefix(self.library, "1", dosdevices=False)
        good = self.prefix(self.library, "2")
        done = self.run_once()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(os.listdir(bare), [])
        self.assertTrue(os.path.islink(os.path.join(good, "d:")))

    def test_finds_prefixes_in_the_home_and_in_the_shared_directorys_compatdata(self):
        home = self.prefix(self.library, "1")
        shared = self.prefix(self.shared, "2")
        self.run_once()
        self.assertTrue(os.path.islink(os.path.join(home, "d:")))
        self.assertTrue(os.path.islink(os.path.join(shared, "d:")))

    def test_finds_prefixes_in_another_library_of_the_list(self):
        other = os.path.join(self.tmp, "disk2")
        dd = self.prefix(other, "3")
        os.makedirs(os.path.join(self.library, "steamapps"))
        with open(os.path.join(self.library, "steamapps/libraryfolders.vdf"), "w") as f:
            f.write(
                '"libraryfolders"\n{\n\t"0"\n\t{\n\t\t"path"\t\t"%s"\n\t}\n'
                '\t"1"\n\t{\n\t\t"path"\t\t"%s"\n\t}\n}\n' % (self.library, other)
            )
        self.run_once()
        self.assertTrue(os.path.islink(os.path.join(dd, "d:")))

    def test_no_directory_is_a_no_op(self):
        dd = self.prefix(self.library, "1")
        for directory in (None, ""):
            done = self.run_once(directory)
            self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.letters(dd), ["c:", "z:"])

    def test_the_shared_directory_is_never_written(self):
        self.prefix(self.shared, "2")
        before = os.stat(self.shared).st_mtime_ns
        self.run_once()
        self.assertEqual(os.stat(self.shared).st_mtime_ns, before)
        self.assertEqual(os.listdir(self.shared), ["steamapps"])

    @unittest.skipIf(os.geteuid() == 0, "root can write anywhere")
    def test_an_unwritable_prefix_doesnt_stop_the_others(self):
        first = self.prefix(self.library, "1")
        locked = self.prefix(self.library, "2")
        last = self.prefix(self.library, "3")
        os.chmod(locked, 0o555)
        self.addCleanup(os.chmod, locked, 0o755)
        done = self.run_once()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertTrue(os.path.islink(os.path.join(first, "d:")))
        self.assertTrue(os.path.islink(os.path.join(last, "d:")))
        self.assertFalse(os.path.lexists(os.path.join(locked, "d:")))
        self.assertIn(locked, done.stderr)
        self.assertEqual([n for n in os.listdir(locked) if n.startswith(".")], [])

    def test_the_polling_loop_picks_up_a_prefix_made_later(self):
        early = self.prefix(self.library, "1")
        env = dict(os.environ, HOME=self.home, CHA_DRIVES_INTERVAL="0.05")
        proc = subprocess.Popen(
            [sys.executable, SCRIPT, self.shared], env=env, stderr=subprocess.PIPE, text=True
        )
        self.addCleanup(proc.stderr.close)
        self.addCleanup(proc.wait)
        self.addCleanup(proc.terminate)

        def wait_for(path):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if os.path.islink(path):
                    return True
                self.assertIsNone(proc.poll(), "steam-drives ended")
                time.sleep(0.02)
            return False

        self.assertTrue(wait_for(os.path.join(early, "d:")))
        late = self.prefix(self.library, "2")
        self.assertTrue(wait_for(os.path.join(late, "d:")))


if __name__ == "__main__":
    unittest.main()
