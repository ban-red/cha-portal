"""Tests for steam-upscalers: `python3 -m unittest discover -s images/steam`."""

import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "steam-upscalers")

DEFAULTS = {
    "PROTON_ENABLE_NGX_UPDATER": "1",
    "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE": "on",
    "DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE_RENDER_PRESET_SELECTION": "render_preset_latest",
    "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE": "on",
    "DXVK_NVAPI_DRS_NGX_DLSS_RR_OVERRIDE_RENDER_PRESET_SELECTION": "render_preset_latest",
    "DXVK_NVAPI_VKREFLEX": "1",
}


def run(nvidia, **env):
    """The environment a shell has after eval'ing the script's output: the
    variables it exports (with `env`'s own), as a dict, and its output."""
    with tempfile.TemporaryDirectory() as d:
        ctl = os.path.join(d, "nvidiactl")
        if nvidia:
            open(ctl, "w").close()
        base = {"PATH": os.environ["PATH"], "CHA_NVIDIA_CTL": ctl, **env}
        out = subprocess.run([SCRIPT], env=base, capture_output=True, text=True, check=True).stdout
        shown = subprocess.run(
            ["sh", "-c", f'eval "$({SCRIPT})"; env'],
            env=base,
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    seen = dict(line.split("=", 1) for line in shown.splitlines() if "=" in line)
    return {k: seen[k] for k in DEFAULTS if k in seen}, out


class SteamUpscalersTest(unittest.TestCase):
    def test_an_nvidia_node_gets_the_newest_dlss_models_and_reflex(self):
        got, _ = run(nvidia=True)
        self.assertEqual(got, DEFAULTS)

    def test_frame_generation_is_never_forced(self):
        _, out = run(nvidia=True)
        self.assertNotIn("FG_OVERRIDE", out)

    def test_a_node_without_nvidia_gets_nothing(self):
        got, out = run(nvidia=False)
        self.assertEqual(got, {})
        self.assertEqual(out, "")

    def test_a_variable_already_set_is_left_alone(self):
        got, out = run(nvidia=True, DXVK_NVAPI_VKREFLEX="0", PROTON_ENABLE_NGX_UPDATER="")
        self.assertEqual(got["DXVK_NVAPI_VKREFLEX"], "0")
        self.assertEqual(got["PROTON_ENABLE_NGX_UPDATER"], "")
        self.assertNotIn("DXVK_NVAPI_VKREFLEX", out)
        self.assertEqual(got["DXVK_NVAPI_DRS_NGX_DLSS_SR_OVERRIDE"], "on")

    def test_the_off_switch_sets_none(self):
        got, out = run(nvidia=True, CHA_GAME_UPSCALERS="0")
        self.assertEqual(got, {})
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
