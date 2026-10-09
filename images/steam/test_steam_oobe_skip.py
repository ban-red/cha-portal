"""Tests for steam-oobe-skip: `python3 -m unittest discover -s images/steam`."""

import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-oobe-skip")

# What Steam wrote in a home after a language was picked.
SAMPLE = """"Registry"
{
	"HKLM"
	{
		"Software"
		{
			"Valve"
			{
				"Steam"
				{
					"SteamPID"		"1347"
				}
			}
		}
	}
	"HKCU"
	{
		"Software"
		{
			"Valve"
			{
				"Steam"
				{
					"language"		"english"
					"GamescopeEnableAppTargetRefreshRate2"		"1"
				}
			}
		}
	}
}
"""


def run(content):
    """Runs the script on a registry.vdf holding `content` (None: no file).
    Returns (exit code, file's text afterwards or None, stderr)."""
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, ".steam", "registry.vdf")
        if content is not None:
            os.makedirs(os.path.dirname(path))
            with open(path, "w") as f:
                f.write(content)
        done = subprocess.run(
            [sys.executable, SCRIPT, path], capture_output=True, text=True, check=False
        )
        text = None
        if os.path.exists(path):
            with open(path) as f:
                text = f.read()
    return done.returncode, text, done.stderr


class OobeSkip(unittest.TestCase):
    def test_adds_the_flag_and_keeps_the_rest(self):
        code, text, err = run(SAMPLE)
        self.assertEqual(code, 0)
        self.assertIn('\t\t\t\t\t"CompletedOOBE"\t\t"1"\n', text)
        self.assertIn('"language"\t\t"english"', text)
        self.assertIn('"SteamPID"\t\t"1347"', text)
        self.assertIn("marked", err)
        # HKLM's Steam block didn't get it
        self.assertEqual(text.count("CompletedOOBE"), 1)
        # unchanged but for the one line
        self.assertEqual(text.replace('\t\t\t\t\t"CompletedOOBE"\t\t"1"\n', ""), SAMPLE)

    def test_a_second_run_changes_nothing(self):
        _, once, _ = run(SAMPLE)
        code, twice, err = run(once)
        self.assertEqual(code, 0)
        self.assertEqual(twice, once)
        self.assertEqual(err, "")

    def test_replaces_a_zero(self):
        code, text, _ = run(SAMPLE.replace('"language"', '"CompletedOOBE"\t\t"0"\n\t\t\t\t\t"language"'))
        self.assertEqual(code, 0)
        self.assertEqual(text.count("CompletedOOBE"), 1)
        self.assertIn('"CompletedOOBE"\t\t"1"', text)

    def test_makes_the_file_when_there_is_none(self):
        code, text, _ = run(None)
        self.assertEqual(code, 0)
        self.assertIn('"Registry"', text)
        self.assertIn('"CompletedOOBE"\t\t"1"', text)

    def test_adds_missing_blocks(self):
        code, text, _ = run('"Registry"\n{\n\t"HKLM"\n\t{\n\t}\n}\n')
        self.assertEqual(code, 0)
        self.assertIn('"HKCU"', text)
        self.assertIn('"CompletedOOBE"\t\t"1"', text)

    def test_leaves_a_file_it_cannot_read(self):
        broken = '"Registry"\n{\n\t"HKCU"\n\t{\n'
        code, text, err = run(broken)
        self.assertEqual(code, 0)
        self.assertEqual(text, broken)
        self.assertIn("can't read", err)

    def test_escaped_quotes_survive(self):
        src = SAMPLE.replace('"english"', '"say \\"hi\\""')
        code, text, _ = run(src)
        self.assertEqual(code, 0)
        self.assertIn('"language"\t\t"say \\"hi\\""', text)


if __name__ == "__main__":
    unittest.main()
