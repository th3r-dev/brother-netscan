import random
from rle import rle_decode
random.seed(1)
data = bytes(random.randrange(256) for _ in range(2_000_000))
for _ in range(3):
    rle_decode(data)
