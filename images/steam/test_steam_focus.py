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
    def test_a_game_that_gains_the_focus(self):
        # At launch, after Steam's menu, or after no window at all.
        self.assertTrue(should(OTHER, True, False, None))

    def test_once_while_it_keeps_the_focus(self):
        self.assertFalse(should(OTHER, True, True, None))

    def test_not_again_within_a_second(self):
        # Wine moving the focus itself can't make a loop.
        self.assertFalse(should(OTHER, True, False, 0.3))
        self.assertTrue(should(OTHER, True, False, 1.0))

    def test_never_steams_own_windows_or_none(self):
        self.assertFalse(should(STEAM, True, False, None))
        self.assertFalse(should(NONE, True, False, None))

    def test_only_a_window_that_asks_for_it(self):
        # Until its WM_PROTOCOLS arrive, then on a later poll.
        self.assertFalse(should(OTHER, False, False, None))


if __name__ == "__main__":
    unittest.main()
