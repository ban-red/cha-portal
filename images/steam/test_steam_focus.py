"""Tests for steam-focus: `python3 -m unittest discover -s images/steam`.

Only the decision is tested here; the X side ran against Balatro on the node.
"""

import importlib.machinery
import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
loader = importlib.machinery.SourceFileLoader("steam_focus", os.path.join(HERE, "steam-focus"))
spec = importlib.util.spec_from_loader("steam_focus", loader)
steam_focus = importlib.util.module_from_spec(spec)
loader.exec_module(steam_focus)

STEAM, OTHER, NONE = steam_focus.STEAM, steam_focus.OTHER, steam_focus.NONE
kind = steam_focus.kind
should = steam_focus.should_take_focus


class Kind(unittest.TestCase):
    def test_steams_own_windows(self):
        self.assertEqual(kind(769, False), STEAM)
        # The overlay flag makes it Steam's, whatever app id it carries.
        self.assertEqual(kind(2379780, True), STEAM)
        self.assertEqual(kind(None, True), STEAM)

    def test_a_game(self):
        self.assertEqual(kind(2379780, False), OTHER)

    def test_no_app_id(self):
        self.assertEqual(kind(None, False), NONE)


class Decision(unittest.TestCase):
    def test_the_menu_closing_over_a_game(self):
        self.assertTrue(should(STEAM, OTHER, True, True))

    def test_only_on_a_change(self):
        self.assertFalse(should(STEAM, OTHER, False, True))

    def test_only_from_steam(self):
        self.assertFalse(should(OTHER, OTHER, True, True))
        self.assertFalse(should(NONE, OTHER, True, True))

    def test_only_to_a_game(self):
        self.assertFalse(should(OTHER, STEAM, True, True))
        self.assertFalse(should(STEAM, NONE, True, True))

    def test_only_a_window_that_asks_for_it(self):
        self.assertFalse(should(STEAM, OTHER, True, False))


if __name__ == "__main__":
    unittest.main()
