from shop.pricing import line_total


def cart_total(lines):
    return round(sum(line_total(p, q) for p, q in lines), 2)
