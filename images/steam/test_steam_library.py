"""Tests for steam-library: `python3 -m unittest discover -s images/steam`.

The samples are Steam's own: the first is a fresh home's `libraryfolders.vdf`
as Steam wrote it (copied from a Steam home in a Cha environment), the others
are the shapes Steam has written for a second library, and the marker a library
keeps in its own directory.
"""

import importlib.machinery
import importlib.util
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-library")
loader = importlib.machinery.SourceFileLoader("steam_library", SCRIPT)
spec = importlib.util.spec_from_loader("steam_library", loader)
steam_library = importlib.util.module_from_spec(spec)
loader.exec_module(steam_library)

HOME_LIBRARY = "/home/cha/.local/share/Steam"

# A home that has run Steam once: its own folder, with the games it holds.
FRESH = """\
"libraryfolders"
{
\t"0"
\t{
\t\t"path"\t\t"/home/cha/.local/share/Steam"
\t\t"label"\t\t""
\t\t"contentid"\t\t"7775501969565344037"
\t\t"totalsize"\t\t"0"
\t\t"update_clean_bytes_tally"\t\t"2147559708"
\t\t"time_last_update_verified"\t\t"1791157009"
\t\t"apps"
\t\t{
\t\t\t"228980"\t\t"0"
\t\t\t"1493710"\t\t"0"
\t\t\t"2379780"\t\t"0"
\t\t\t"4183110"\t\t"0"
\t\t}
\t}
}
"""

# The same with a second library on another disk, as Steam writes one.
TWO = """\
"libraryfolders"
{
\t"0"
\t{
\t\t"path"\t\t"/home/cha/.local/share/Steam"
\t\t"label"\t\t""
\t\t"contentid"\t\t"7775501969565344037"
\t\t"totalsize"\t\t"0"
\t\t"update_clean_bytes_tally"\t\t"2147559708"
\t\t"time_last_update_verified"\t\t"1791157009"
\t\t"apps"
\t\t{
\t\t\t"228980"\t\t"0"
\t\t}
\t}
\t"1"
\t{
\t\t"path"\t\t"/mnt/disk2/SteamLibrary"
\t\t"label"\t\t"Disk 2"
\t\t"contentid"\t\t"3321447902334442090"
\t\t"totalsize"\t\t"1000204886016"
\t\t"update_clean_bytes_tally"\t\t"88391274"
\t\t"time_last_update_verified"\t\t"1790000000"
\t\t"apps"
\t\t{
\t\t\t"1245620"\t\t"72388331056"
\t\t}
\t}
}
"""

# What Steam wrote before 2021: numbers to paths.
OLD = """\
"LibraryFolders"
{
\t"TimeNextStatsReport"\t\t"1588461238"
\t"ContentStatsID"\t\t"-6125421990219304892"
\t"1"\t\t"/mnt/disk2/SteamLibrary"
}
"""

# A library's own marker, in the library's directory (not under steamapps/).
MARKER = """\
"libraryfolder"
{
\t"contentid"\t\t"6123876543210987654"
\t"label"\t\t"games-remote"
}
"""


class Scratch(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="steam-library-test-")
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        # What the script says it did, collected instead of printed.
        self.stderr = []
        self._say = steam_library.say
        steam_library.say = self.stderr.append
        self.addCleanup(setattr, steam_library, "say", self._say)

    def path(self, *parts):
        return os.path.join(self.dir, *parts)

    def write(self, path, text, mode=0o644):
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(text)
        os.chmod(path, mode)

    def read(self, path):
        with open(path) as f:
            return f.read()


class FormatTests(unittest.TestCase):
    def test_steams_own_files_read_and_write_back_byte_for_byte(self):
        for text in (FRESH, TWO, OLD, MARKER):
            self.assertEqual(steam_library.dump(steam_library.parse(text)), text)

    def test_the_tree_has_what_the_file_says(self):
        tree = steam_library.parse(TWO)
        entries = steam_library.find_root(tree)
        self.assertEqual([k for k, _ in entries], ["0", "1"])
        self.assertEqual(
            steam_library.listed(entries),
            [HOME_LIBRARY, "/mnt/disk2/SteamLibrary"],
        )
        self.assertEqual(steam_library.listed(steam_library.find_root(steam_library.parse(OLD))), ["/mnt/disk2/SteamLibrary"])

    def test_comments_escapes_and_bare_words(self):
        text = '// a comment\n"a" // another\n{\n\t"k" "x\\"y\\\\z\\n"\n\tbare word\n\t"n" { "m" "1" }\n}\n'
        tree = steam_library.parse(text)
        self.assertEqual(tree[0][0], "a")
        inner = dict(tree[0][1])
        self.assertEqual(inner["k"], 'x"y\\z\n')
        self.assertEqual(inner["bare"], "word")
        self.assertEqual(inner["n"], [("m", "1")])
        # And what is written back reads the same.
        self.assertEqual(steam_library.parse(steam_library.dump(tree)), tree)

    def test_a_key_may_repeat(self):
        tree = steam_library.parse('"a"\n{\n"k" "1"\n"k" "2"\n}\n')
        self.assertEqual(tree, [("a", [("k", "1"), ("k", "2")])])

    def test_broken_files_are_refused_not_guessed_at(self):
        for text in ['"a" {', '"a" { "k" }', "}", '{ "a" "b" }', '"a" "b" "c"', '"unterminated']:
            with self.assertRaises(ValueError, msg=text):
                steam_library.parse(text)


SHARED = "/mnt/games/steam"


class AddTests(unittest.TestCase):
    def test_a_new_library_is_the_next_entry_and_the_rest_is_as_it_was(self):
        new = steam_library.add_library(FRESH, SHARED, "games-remote", "6123876543210987654")
        # The old text is all still there, in order, with the entry before the last }.
        self.assertTrue(new.startswith(FRESH[: FRESH.rindex("}")]))
        self.assertEqual(
            new[len(FRESH) - 2 :],
            '\t"1"\n\t{\n'
            '\t\t"path"\t\t"/mnt/games/steam"\n'
            '\t\t"label"\t\t"games-remote"\n'
            '\t\t"contentid"\t\t"6123876543210987654"\n'
            '\t\t"totalsize"\t\t"0"\n'
            '\t\t"apps"\n\t\t{\n\t\t}\n'
            "\t}\n}\n",
        )
        entries = steam_library.find_root(steam_library.parse(new))
        self.assertEqual(steam_library.listed(entries), [HOME_LIBRARY, SHARED])

    def test_it_follows_the_existing_numbers_and_leaves_other_libraries_alone(self):
        new = steam_library.add_library(TWO, SHARED)
        entries = steam_library.find_root(steam_library.parse(new))
        self.assertEqual([k for k, _ in entries], ["0", "1", "2"])
        self.assertEqual(
            steam_library.listed(entries),
            [HOME_LIBRARY, "/mnt/disk2/SteamLibrary", SHARED],
        )
        # The one on disk 2 is exactly as it was, down to its apps.
        old_entries = steam_library.find_root(steam_library.parse(TWO))
        self.assertEqual(entries[:2], old_entries)
        # A gap in the numbers isn't filled: the next after the highest.
        gap = TWO.replace('\t"1"\n', '\t"5"\n')
        new = steam_library.add_library(gap, SHARED)
        keys = [k for k, _ in steam_library.find_root(steam_library.parse(new))]
        self.assertEqual(keys, ["0", "5", "6"])

    def test_without_a_marker_the_entry_is_the_path_alone_for_steam_to_finish(self):
        new = steam_library.add_library(FRESH, SHARED)
        entry = dict(steam_library.find_root(steam_library.parse(new))[1][1])
        self.assertEqual(entry["path"], SHARED)
        self.assertEqual(entry["label"], "")
        self.assertNotIn("contentid", entry)

    def test_a_library_already_listed_changes_nothing(self):
        once = steam_library.add_library(FRESH, SHARED, "x", "1")
        self.assertEqual(steam_library.add_library(once, SHARED, "x", "1"), once)
        self.assertEqual(steam_library.add_library(once, SHARED + "/", "x", "1"), once)
        # Listed already by someone else (the user added it by hand): kept as is.
        self.assertEqual(steam_library.add_library(TWO, "/mnt/disk2/SteamLibrary"), TWO)
        self.assertEqual(steam_library.add_library(OLD, "/mnt/disk2/SteamLibrary"), OLD)
        # The home's own folder.
        self.assertEqual(steam_library.add_library(FRESH, HOME_LIBRARY), FRESH)

    def test_the_old_format_gets_an_old_style_entry(self):
        new = steam_library.add_library(OLD, SHARED, "label", "1")
        self.assertEqual(
            new,
            OLD.replace('"/mnt/disk2/SteamLibrary"\n', '"/mnt/disk2/SteamLibrary"\n\t"2"\t\t"/mnt/games/steam"\n'),
        )

    def test_a_list_that_isnt_one_is_refused(self):
        with self.assertRaises(ValueError):
            steam_library.add_library(MARKER, SHARED)
        with self.assertRaises(ValueError):
            steam_library.add_library("", SHARED)

    def test_an_empty_label_is_named_and_a_set_one_is_kept(self):
        def labels(text):
            return [dict(v)["label"] for _, v in steam_library.find_root(steam_library.parse(text))]

        # Steam's own folder is named, and the new entry takes the label given.
        new = steam_library.add_library(FRESH, SHARED, "Shared library", None, HOME_LIBRARY)
        self.assertEqual(labels(new), ["Personal library", "Shared library"])
        # Already listed with no label (a home from before): named in place.
        old = steam_library.add_library(FRESH, SHARED)
        self.assertEqual(labels(old), ["", ""])
        named = steam_library.add_library(old, SHARED, "Shared library", None, HOME_LIBRARY)
        self.assertEqual(labels(named), ["Personal library", "Shared library"])
        # Then it stays put, and a name the user chose is never replaced.
        self.assertEqual(steam_library.add_library(named, SHARED, "Other", None, HOME_LIBRARY), named)
        mine = named.replace("Shared library", "Games NAS")
        self.assertEqual(steam_library.add_library(mine, SHARED, "Shared library", None, HOME_LIBRARY), mine)

    def test_a_first_launchs_list_has_steams_folder_then_ours(self):
        text = steam_library.first_list(HOME_LIBRARY, SHARED, "games-remote", "6123876543210987654")
        entries = steam_library.find_root(steam_library.parse(text))
        self.assertEqual([k for k, _ in entries], ["0", "1"])
        self.assertEqual(steam_library.listed(entries), [HOME_LIBRARY, SHARED])
        self.assertEqual(steam_library.add_library(text, SHARED), text)
        self.assertEqual(steam_library.dump(steam_library.parse(text)), text)


class MarkerTests(Scratch):
    def test_the_content_id_and_label_come_from_the_librarys_own_file(self):
        self.write(self.path("lib", "libraryfolder.vdf"), MARKER)
        self.assertEqual(steam_library.read_marker(self.path("lib")), ("6123876543210987654", "games-remote"))

    def test_a_library_with_no_marker_or_a_broken_one_has_none(self):
        os.makedirs(self.path("lib"))
        self.assertEqual(steam_library.read_marker(self.path("lib")), (None, ""))
        self.write(self.path("lib", "libraryfolder.vdf"), '"libraryfolder" {')
        self.assertEqual(steam_library.read_marker(self.path("lib")), (None, ""))


def snapshot(directory):
    """Every path under a directory with its mode, size and mtime."""
    seen = {}
    for root, dirs, files in os.walk(directory):
        for name in dirs + files:
            path = os.path.join(root, name)
            st = os.lstat(path)
            seen[path] = (st.st_mode, st.st_size, st.st_mtime_ns)
    st = os.lstat(directory)
    seen[directory] = (st.st_mode, st.st_size, st.st_mtime_ns)
    return seen


class RegisterTests(Scratch):
    def setUp(self):
        super().setUp()
        self.home = self.path("home")
        self.steam = os.path.join(self.home, steam_library.STEAM_DIR)
        self.shared = self.path("nas", "steam")
        os.makedirs(os.path.join(self.shared, "steamapps", "compatdata"))
        self.write(os.path.join(self.shared, "libraryfolder.vdf"), MARKER)
        self.write(os.path.join(self.shared, "steamapps", "appmanifest_1.acf"), '"AppState" { "appid" "1" }')
        self.apps_list = os.path.join(self.steam, "steamapps", "libraryfolders.vdf")
        self.config_list = os.path.join(self.steam, "config", "libraryfolders.vdf")

    def listed(self, path):
        return steam_library.listed(steam_library.find_root(steam_library.parse(self.read(path))))

    def test_both_of_steams_lists_get_it_and_the_librarys_own_marker_is_used(self):
        self.write(self.apps_list, FRESH, 0o755)
        self.write(self.config_list, FRESH, 0o755)
        changed = steam_library.register(self.home, self.shared)
        self.assertEqual(sorted(changed), sorted([self.apps_list, self.config_list]))
        for path in (self.apps_list, self.config_list):
            self.assertEqual(self.listed(path), [HOME_LIBRARY, self.shared])
            entry = dict(steam_library.find_root(steam_library.parse(self.read(path)))[1][1])
            self.assertEqual((entry["contentid"], entry["label"]), ("6123876543210987654", "games-remote"))
            # Steam's files stay as it keeps them.
            self.assertEqual(stat.S_IMODE(os.stat(path).st_mode), 0o755)
        # Two lists, one content.
        self.assertEqual(self.read(self.apps_list), self.read(self.config_list))

    def test_running_it_again_every_launch_leaves_the_files_untouched(self):
        self.write(self.apps_list, FRESH)
        self.write(self.config_list, FRESH)
        steam_library.register(self.home, self.shared)
        before = {p: (self.read(p), os.stat(p).st_mtime_ns) for p in (self.apps_list, self.config_list)}
        self.assertEqual(steam_library.register(self.home, self.shared), [])
        self.assertEqual(steam_library.register(self.home, self.shared), [])
        for path, state in before.items():
            self.assertEqual((self.read(path), os.stat(path).st_mtime_ns), state, "not even rewritten")

    def test_only_the_lists_that_exist_are_changed(self):
        self.write(self.config_list, TWO)
        self.assertEqual(steam_library.register(self.home, self.shared), [self.config_list])
        self.assertFalse(os.path.exists(self.apps_list))
        self.assertEqual(
            self.listed(self.config_list),
            [HOME_LIBRARY, "/mnt/disk2/SteamLibrary", self.shared],
        )

    def test_a_home_that_has_not_run_steam_gets_both_lists(self):
        changed = steam_library.register(self.home, self.shared)
        self.assertEqual(sorted(changed), sorted([self.apps_list, self.config_list]))
        for path in (self.apps_list, self.config_list):
            self.assertEqual(self.listed(path), [self.steam, self.shared])
        # And then Steam's own writing of them is the same shape: a second
        # launch finds the library listed and does nothing.
        self.assertEqual(steam_library.register(self.home, self.shared), [])

    def test_the_shared_directory_is_never_written(self):
        self.write(self.apps_list, FRESH)
        before = snapshot(self.shared)
        steam_library.register(self.home, self.shared)
        steam_library.register(self.home, self.shared)
        self.assertEqual(snapshot(self.shared), before)
        # Without its own marker either: nothing is made there for Steam.
        bare = self.path("nas", "bare")
        os.makedirs(bare)
        before = snapshot(bare)
        steam_library.register(self.home, bare)
        self.assertEqual(snapshot(bare), before)
        self.assertEqual(os.listdir(bare), [])
        self.assertIn(bare, self.listed(self.apps_list))

    def test_a_directory_that_isnt_there_is_not_listed(self):
        self.write(self.apps_list, FRESH)
        self.assertEqual(steam_library.register(self.home, self.path("nowhere")), [])
        self.assertEqual(self.read(self.apps_list), FRESH)
        self.assertTrue(any("isn't a directory" in m for m in self.stderr))

    @unittest.skipIf(os.geteuid() == 0, "root writes everywhere")
    def test_a_read_only_directory_is_not_listed_and_says_why(self):
        self.write(self.apps_list, FRESH)
        os.chmod(self.shared, 0o555)
        self.addCleanup(os.chmod, self.shared, 0o755)
        self.assertEqual(steam_library.register(self.home, self.shared), [])
        self.assertEqual(self.read(self.apps_list), FRESH)
        self.assertTrue(any("writable" in m for m in self.stderr))

    def test_a_list_that_doesnt_parse_is_left_alone_and_the_other_still_done(self):
        self.write(self.apps_list, '"libraryfolders" {')
        self.write(self.config_list, FRESH)
        changed = steam_library.register(self.home, self.shared)
        self.assertEqual(changed, [self.config_list])
        self.assertEqual(self.read(self.apps_list), '"libraryfolders" {')
        self.assertTrue(any("leaving it alone" in m for m in self.stderr))

    def test_no_temp_file_is_left_behind(self):
        self.write(self.apps_list, FRESH)
        steam_library.register(self.home, self.shared)
        self.assertEqual(os.listdir(os.path.dirname(self.apps_list)), ["libraryfolders.vdf"])


class CommandTests(Scratch):
    def run_script(self, *args, **env):
        return subprocess.run(
            [sys.executable, SCRIPT, *args],
            env={**os.environ, **env},
            capture_output=True,
            text=True,
        )

    def test_as_start_steam_runs_it(self):
        home = self.path("home")
        shared = self.path("share")
        os.makedirs(shared)
        lists = os.path.join(home, steam_library.STEAM_DIR, "steamapps", "libraryfolders.vdf")
        self.write(lists, FRESH)
        done = self.run_script(shared, HOME=home)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn("listed", done.stderr)
        self.assertIn(shared, self.read(lists))
        again = self.run_script(shared, HOME=home)
        self.assertEqual(again.returncode, 0)
        self.assertEqual(again.stderr, "")

    def test_a_missing_argument_is_a_usage_error(self):
        self.assertEqual(self.run_script().returncode, 2)


if __name__ == "__main__":
    unittest.main()
