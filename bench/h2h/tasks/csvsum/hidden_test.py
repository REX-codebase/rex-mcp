import unittest, os, tempfile
from sumcol import column_sum
class T(unittest.TestCase):
    def test_sales(self):
        self.assertAlmostEqual(column_sum("sales.csv", "amount"), 20.0)
    def test_blank_and_empty(self):
        d = tempfile.mkdtemp()
        p = os.path.join(d, "x.csv")
        with open(p, "w") as f:
            f.write('a,b\n1,"2"\n\n3,\n,4\n')
        self.assertAlmostEqual(column_sum(p, "b"), 6.0)
        self.assertAlmostEqual(column_sum(p, "a"), 4.0)
unittest.main()
