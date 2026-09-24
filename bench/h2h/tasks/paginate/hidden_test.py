import unittest
from pager import paginate, page_count
class T(unittest.TestCase):
    def test_pages(self):
        xs = list(range(10))
        self.assertEqual(paginate(xs, 1, 3), [0, 1, 2])
        self.assertEqual(paginate(xs, 4, 3), [9])
        self.assertEqual(paginate(xs, 5, 3), [])
        with self.assertRaises(ValueError):
            paginate(xs, 0, 3)
    def test_count(self):
        self.assertEqual(page_count(10, 3), 4)
        self.assertEqual(page_count(9, 3), 3)
        self.assertEqual(page_count(0, 3), 0)
unittest.main()
