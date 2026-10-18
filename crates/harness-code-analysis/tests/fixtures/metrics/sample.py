"""Order helpers."""
import math


def score(values, *, weight=1.0, **extra):
    """Score the values."""
    total = 0
    for v in values:
        if v > 0 and v < 10 or v == 100:
            total += v
        elif v < 0:
            total -= v
        else:
            pass
    squares = [x * x for x in values if x]
    return total * weight if values else 0


class Account:
    """An account."""

    rate = 0.1

    def __init__(self, owner):
        self.owner = owner
        self.balance = 0

    def deposit(self, amount):
        try:
            self.balance += amount
        except TypeError:
            raise

    def report(self):
        return f"{self.owner}: {self.balance}"

    @staticmethod
    def parse(text):
        key = lambda s: s.strip()
        return key(text)

    def audit(this, log):
        match log:
            case "a":
                return 1
            case _:
                return 2
