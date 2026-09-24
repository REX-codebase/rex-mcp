import unittest, pathlib
from shop import pricing
from report import summary
class T(unittest.TestCase):
    def test_renamed(self):
        self.assertTrue(hasattr(pricing, "line_total"))
        self.assertFalse(hasattr(pricing, "calc"))
        self.assertEqual(summary([(2.5, 2), (1, 3)]), "first=5.0 total=8.0")
        for f in pathlib.Path(".").rglob("*.py"):
            if f.name != "hidden_test.py":
                self.assertNotIn("calc(", f.read_text(), str(f))
unittest.main()
