import csv
import sys


def column_sum(path, column):
    total = 0.0
    with open(path, newline="") as f:
        for row in csv.DictReader(f):
            v = (row.get(column) or "").strip()
            if v:
                total += float(v)
    return total


if __name__ == "__main__":
    print(column_sum(sys.argv[1], sys.argv[2]))
