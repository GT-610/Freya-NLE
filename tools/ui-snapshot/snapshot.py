#!/usr/bin/env python3
"""Convert the raw RGBA frames written by ui-snapshot into PNG files.

The Rust tool writes `/tmp/<name>.rgba` plus a `/tmp/<name>.size` sidecar with
the frame dimensions, so this script only needs the window name.
"""

import sys
from pathlib import Path

from PIL import Image


def convert(name: str) -> Path:
    raw = Path(f"/tmp/{name}.rgba")
    size_file = Path(f"/tmp/{name}.size")
    if not raw.exists() or not size_file.exists():
        raise SystemExit(f"missing {raw} or {size_file}; run `cargo run -- {name}` first")
    width, height = (int(value) for value in size_file.read_text().split())
    data = raw.read_bytes()
    expected = width * height * 4
    if len(data) != expected:
        raise SystemExit(f"{raw} holds {len(data)} bytes, expected {expected}")
    out = Path(f"/tmp/{name}.png")
    Image.frombytes("RGBA", (width, height), data).save(out)
    print(out)
    return out


if __name__ == "__main__":
    names = sys.argv[1:] or ["timeline", "main"]
    for name in names:
        convert(name)
