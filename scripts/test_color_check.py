"""scripts/color-check.py: the patch layout agrees with the streamer's, and
the matrix and curve checks find what an encoder did, through a real
encode and decode where ffmpeg (with libx264) is installed."""

import importlib.util
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("color_check", HERE / "color-check.py")
cc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cc)


class Layout(unittest.TestCase):
    def test_the_patches_are_the_streamers(self):
        rust = (HERE.parent / "crates/cha-streamer/src/encoder/patches.rs").read_text()
        table = rust[rust.index("pub const PATCHES") : rust.index("];\n\n")]
        triples = [[int(v) for v in t] for t in re.findall(r"\[(\d+), (\d+), (\d+)\]", table)]
        self.assertEqual(triples, [c for row in cc.PATCHES for c in row])

    def test_the_cells_fill_the_picture_as_the_streamer_lays_them_out(self):
        # 1366 / 8 = 170: the last column takes the 6 pixels left over.
        self.assertEqual(cc.cell_rect(0, 7, 1366, 768)[2], 1366 - (1366 - 1190) // 4)
        bgrx = cc.pattern_bgrx(64, 30)
        # Row 0, column 0 is red; row 2's last cell the brightest near-black.
        self.assertEqual(bgrx[0:4], bytes((0, 0, 255, 255)))
        self.assertEqual(bgrx[-4:], bytes((28, 28, 28, 255)))


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "needs ffmpeg")
class ThroughAnEncoder(unittest.TestCase):
    W, H = 320, 180

    def encode(self, matrix):
        """The pattern through ffmpeg's RGB to YCbCr with `matrix`, then x264 at QP 0."""
        tmp = tempfile.mkdtemp()
        src, out = Path(tmp) / "patches.bgrx", Path(tmp) / f"patches-{matrix}.h264"
        src.write_bytes(cc.pattern_bgrx(self.W, self.H))
        subprocess.run(
            ["ffmpeg", "-v", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr0", "-s", f"{self.W}x{self.H}",
             "-i", str(src), "-frames:v", "3",
             "-vf", f"scale=out_color_matrix={matrix}:out_range=tv,format=yuv420p",
             "-c:v", "libx264", "-qp", "0", str(out)],
            check=True,
        )
        return str(out)

    def test_it_tells_bt709_from_bt601(self):
        for matrix, name in (("bt709", "BT.709"), ("bt601", "BT.601")):
            report = cc.analyse(cc.decode_last_frame(self.encode(matrix), self.W, self.H), self.W, self.H)
            self.assertEqual(report["matrix"], name, report["errors"])
            # The right matrix gives the colours back within a few codes, the wrong one doesn't.
            self.assertLess(report["errors"][name], 4)
            other = "BT.601" if name == "BT.709" else "BT.709"
            self.assertGreater(report["errors"][other], 6)

    def test_an_encoder_that_only_changes_the_matrix_keeps_the_curve(self):
        report = cc.analyse(cc.decode_last_frame(self.encode("bt709"), self.W, self.H), self.W, self.H)
        worst = max(abs(got - want) for _, got, want in report["greys"])
        self.assertLess(worst, 3)


if __name__ == "__main__":
    unittest.main()
