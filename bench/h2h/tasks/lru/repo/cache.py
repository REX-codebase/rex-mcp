class Cache:
    """A tiny key/value cache."""

    def __init__(self, capacity):
        self.capacity = capacity
        self.data = {}

    def get(self, key, default=None):
        return self.data.get(key, default)

    def put(self, key, value):
        self.data[key] = value
