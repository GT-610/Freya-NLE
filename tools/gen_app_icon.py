#!/usr/bin/env python3
"""Regenerate `assets/icon.ico` from the geometry in `assets/icon.svg`.

The icon is a filmstrip plate whose centre is a transparent notch, with an
oblique **F** sitting in it. Windows needs raster sizes inside the ICO
container, and no build step produces them, so the raster frames are generated
here and committed.

Run from the repository root:

    python tools/gen_app_icon.py

Requires Pillow (`python -m pip install pillow`). The SVG stays the source of
truth: edit it first, then mirror the change in `PLATE_RECTS`, `NOTCH` and
`F_POLY` below and rerun this script.
"""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "assets" / "icon.ico"

BRAND = (0x8A, 0xD2, 0xF2)
SUPERSAMPLE = 8
SIZES = [16, 24, 32, 48, 64, 128, 256]

# Plate rectangles (x0, y0, x1, y1) in the 128x128 icon frame.
PLATE_RECTS = [
    (28, 0, 100, 128),
    (32, 0, 96, 28),
    (32, 100, 96, 128),
    (124, 0, 128, 128),
    (104, 116, 120, 128), (104, 96, 120, 112), (104, 76, 120, 92),
    (104, 56, 120, 72), (104, 36, 120, 52), (104, 16, 120, 32),
    (104, 0, 120, 12),
    (0, 0, 4, 128),
    (8, 116, 24, 128), (8, 96, 24, 112), (8, 76, 24, 92),
    (8, 56, 24, 72), (8, 36, 24, 52), (8, 16, 24, 32),
    (8, 0, 24, 12),
]

NOTCH = [(52, 32), (96, 32), (96, 76), (76, 96), (32, 96), (32, 52)]

F_POLY = [
    (58, 42), (90, 42), (87, 53), (68, 53), (66, 60),
    (83, 60), (80, 71), (64, 71), (61, 86), (49, 86),
]


def render(size: int) -> Image.Image:
    scale = size * SUPERSAMPLE / 128.0
    image = Image.new("RGBA", (size * SUPERSAMPLE, size * SUPERSAMPLE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    for x0, y0, x1, y1 in PLATE_RECTS:
        draw.rectangle([x0 * scale, y0 * scale, x1 * scale - 1, y1 * scale - 1], fill=BRAND)
    # Punch the notch out, then set the F back into it.
    draw.polygon([(x * scale, y * scale) for x, y in NOTCH], fill=(0, 0, 0, 0))
    draw.polygon([(x * scale, y * scale) for x, y in F_POLY], fill=BRAND)
    return image.resize((size, size), Image.LANCZOS)


def main() -> int:
    payloads = []
    for size in SIZES:
        buffer = io.BytesIO()
        render(size).save(buffer, format="PNG", optimize=True)
        payloads.append((size, buffer.getvalue()))

    header = struct.pack("<HHH", 0, 1, len(payloads))
    offset = len(header) + 16 * len(payloads)
    entries = b""
    body = b""
    for size, data in payloads:
        # A dimension of 0 means 256 in the ICO format.
        dimension = 0 if size >= 256 else size
        entries += struct.pack(
            "<BBBBHHII", dimension, dimension, 0, 0, 1, 32, len(data), offset
        )
        body += data
        offset += len(data)

    OUT.write_bytes(header + entries + body)
    print(f"wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes, {len(payloads)} sizes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
