from shop import pricing
from shop.cart import cart_total


def summary(lines):
    first = pricing.line_total(*lines[0]) if lines else 0
    return f"first={first} total={cart_total(lines)}"
