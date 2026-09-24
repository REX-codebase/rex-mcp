import unittest
from timeparse import parse_duration as p
class T(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(p("1h30m"), 5400)
        self.assertEqual(p("45s"), 45)
        self.assertEqual(p(" 2h "), 7200)
        self.assertEqual(p("1h5m10s"), 3910)
        self.assertEqual(p("0s"), 0)
    def test_bad(self):
        for bad in ["", "10", "5d", "30m1h", "1h1h", "h", "1.5h", "m5"]:
            with self.assertRaises(ValueError, msg=bad):
                p(bad)
unittest.main()
