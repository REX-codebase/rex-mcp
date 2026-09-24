from shop.pricing import calc


def cart_total(lines):
    return round(sum(calc(p, q) for p, q in lines), 2)
