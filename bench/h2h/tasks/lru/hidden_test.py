import unittest
from cache import Cache
class T(unittest.TestCase):
    def test_lru(self):
        c = Cache(2)
        c.put("a", 1); c.put("b", 2)
        self.assertEqual(c.get("a"), 1)
        c.put("c", 3)
        self.assertIsNone(c.get("b"))
        self.assertEqual(c.get("a"), 1)
        self.assertEqual(c.get("c"), 3)
        c.put("a", 9); c.put("d", 4)
        self.assertIsNone(c.get("c"))
        self.assertEqual(len(c), 2)
    def test_bad(self):
        with self.assertRaises(ValueError):
            Cache(0)
unittest.main()
