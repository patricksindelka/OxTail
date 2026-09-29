#!/usr/bin/env python3
"""Regenerates the icon assets from the same geometry as
crates/oxtail-gui/src/icon.rs (64x64 design grid), with supersampling.

Outputs (next to this script): oxtail-<size>.png, oxtail.ico, oxtail.icns.
oxtail.svg is maintained by hand with the same shapes. Pure standard library.
Run:  python3 packaging/icons/make_icons.py
"""
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
BG = (0x1E, 0x2A, 0x3A)
GREY = (0x9A, 0xA5, 0xB8)
RED = (0xF7, 0x76, 0x8E)
GREEN = (0x9E, 0xCE, 0x6A)
# (x0, y0, x1, y1, colour) on the 64 grid; painted over the tile in order.
RECTS = [
    (10, 14, 50, 19, GREY),
    (10, 24, 40, 29, GREY),
    (10, 34, 54, 39, RED),
    (10, 44, 32, 49, GREY),
    (36, 43, 42, 50, GREEN),
]
RADIUS = 12.0


def sample(x, y):
    """Colour (r, g, b, a) of the design at grid coordinates (x, y)."""
    cx = min(max(x, RADIUS), 64 - RADIUS)
    cy = min(max(y, RADIUS), 64 - RADIUS)
    if (x - cx) ** 2 + (y - cy) ** 2 > RADIUS**2:
        return (0, 0, 0, 0)
    c = BG
    for x0, y0, x1, y1, col in RECTS:
        if x0 <= x < x1 and y0 <= y < y1:
            c = col
    return (*c, 255)


def render(size, ss=3):
    scale = 64.0 / size
    rows = []
    for py in range(size):
        row = bytearray([0])  # PNG filter type 0
        for px in range(size):
            r = g = b = a = 0
            for sy in range(ss):
                for sx in range(ss):
                    s = sample((px + (sx + 0.5) / ss) * scale, (py + (sy + 0.5) / ss) * scale)
                    # premultiplied accumulation
                    r += s[0] * s[3]
                    g += s[1] * s[3]
                    b += s[2] * s[3]
                    a += s[3]
            n = ss * ss
            if a:
                row += bytes((round(r / a), round(g / a), round(b / a), round(a / n)))
            else:
                row += bytes((0, 0, 0, 0))
        rows.append(bytes(row))
    return png(size, b"".join(rows))


def png(size, raw):
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(images):
    """images: {size: png bytes}; PNG-compressed entries (Vista+)."""
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    body = b""
    for size, data in sorted(images.items()):
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(body))
        body += data
    return out + body


def icns(entries):
    """entries: [(type, png bytes)]."""
    body = b"".join(t + struct.pack(">I", 8 + len(d)) + d for t, d in entries)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main():
    sizes = [16, 32, 48, 64, 128, 256, 512]
    pngs = {}
    for s in sizes:
        pngs[s] = render(s)
        with open(os.path.join(HERE, f"oxtail-{s}.png"), "wb") as f:
            f.write(pngs[s])
    with open(os.path.join(HERE, "oxtail.ico"), "wb") as f:
        f.write(ico({s: pngs[s] for s in (16, 32, 48, 64, 128, 256)}))
    with open(os.path.join(HERE, "oxtail.icns"), "wb") as f:
        f.write(
            icns(
                [
                    (b"icp4", pngs[16]),
                    (b"icp5", pngs[32]),
                    (b"icp6", pngs[64]),
                    (b"ic07", pngs[128]),
                    (b"ic08", pngs[256]),
                    (b"ic09", pngs[512]),
                ]
            )
        )


if __name__ == "__main__":
    main()
