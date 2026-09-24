def paginate(items, page, size):
    if size <= 0 or page < 1:
        raise ValueError("bad page or size")
    start = (page - 1) * size
    return items[start:start + size]


def page_count(total, size):
    return -(-total // size)
