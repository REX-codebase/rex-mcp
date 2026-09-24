def paginate(items, page, size):
    """Return the items on 1-based page `page` with `size` items per page."""
    if size <= 0:
        raise ValueError("size must be positive")
    start = page * size
    return items[start:start + size]


def page_count(total, size):
    return total // size
