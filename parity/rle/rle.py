"""PackBits decoder, extracted verbatim (type hints added) from pietern/brother-scand rle_to_tiff.py (MIT)."""


def rle_decode(data: bytes) -> bytes:
    """Decodes PackBits encoded data."""
    i = 0
    output = bytearray()
    while i < len(data):
        val = data[i]
        i += 1
        if val == 0x80:
            continue
        if val > 0x80:
            repeats = 0x101 - val
            output += data[i:i + 1] * repeats
            i += 1
        else:
            output += data[i:i + val + 1]
            i += val + 1
    return output
