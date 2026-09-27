"""Run with python -m unittest discover -s tests -p 'test_visual_region_review.py'."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("visual_region_review", Path(__file__).resolve().parents[1] / "scripts/visual-region-review.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
try:
    from PIL import Image
except ImportError:
    Image = None


@unittest.skipIf(Image is None, "optional Pillow not installed")
class RegionReviewTest(unittest.TestCase):
    def test_local_subject_change_not_stable_anchor(self):
        a = Image.new("RGB", (4, 2), (100, 100, 100))
        b = a.copy()
        b.putpixel((0, 0), (130, 100, 100))
        result = module.inspect(a, b, {"regions": {"subject": [0, 0, 2, 2], "anchor": [2, 0, 4, 2]}}, 20)
        self.assertEqual(result["subject"]["changed_fraction"], .25)
        self.assertEqual(result["anchor"]["changed_fraction"], 0)

    def test_no_resampling_or_silent_invalid_region(self):
        a = Image.new("RGB", (4, 2))
        with self.assertRaisesRegex(ValueError, "Different image dimensions"):
            module.inspect(a, Image.new("RGB", (5, 2)), {"regions": {"s": [0, 0, 1, 1]}}, 20)
        with self.assertRaisesRegex(ValueError, "Invalid/out-of-frame"):
            module.inspect(a, a, {"regions": {"s": [3, 0, 5, 1]}}, 20)

    def test_threshold_is_strict_and_channel_max(self):
        a = Image.new("RGB", (1, 1), (100, 100, 100))
        b = Image.new("RGB", (1, 1), (120, 100, 100))
        m = {"regions": {"s": [0, 0, 1, 1]}}
        self.assertEqual(module.inspect(a, b, m, 20)["s"]["changed_pixels"], 0)
        self.assertEqual(module.inspect(a, b, m, 19)["s"]["changed_pixels"], 1)


if __name__ == "__main__":
    unittest.main()
