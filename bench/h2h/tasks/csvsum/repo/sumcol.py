import sys


def column_sum(path, column):
    """Sum the numeric column named `column` in a CSV file with a header row."""
    with open(path) as f:
        lines = f.read().splitlines()
    header = lines[0].split(",")
    idx = header.index(column)
    total = 0
    for line in lines:
        total += float(line.split(",")[idx])
    return total


if __name__ == "__main__":
    print(column_sum(sys.argv[1], sys.argv[2]))
