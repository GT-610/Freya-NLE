#!/usr/bin/env python3
"""Convert the raw RGBA frames written by ui-snapshot into PNG files.

The Rust tool writes `<crate>/target/ui-snapshot/<name>.rgba` plus a `.size`
sidecar with the frame dimensions, so this script only needs the window name.
Pass names explicitly, or leave them out to convert the default set (with the
`-light` suffix when `UI_SNAPSHOT_THEME=light`).
"""

import os
import sys
from pathlib import Path

from PIL import Image

OUTPUT_DIR = Path(__file__).resolve().parent / "target" / "ui-snapshot"


def output_dir() -> Path:
    """The shared frame directory, validated the same way the Rust side does."""
    if OUTPUT_DIR.is_symlink():
        raise SystemExit(f"{OUTPUT_DIR} must not be a symlink")
    if OUTPUT_DIR.exists() and not OUTPUT_DIR.is_dir():
        raise SystemExit(f"{OUTPUT_DIR} is not a directory")
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    if hasattr(os, "geteuid") and OUTPUT_DIR.stat().st_uid != os.geteuid():
        raise SystemExit(f"{OUTPUT_DIR} is not owned by the current user")
    return OUTPUT_DIR


def convert(name: str) -> Path:
    directory = output_dir()
    raw = directory / f"{name}.rgba"
    size_file = directory / f"{name}.size"
    if not raw.exists() or not size_file.exists():
        raise SystemExit(f"missing {raw} or {size_file}; run `cargo run -- {name}` first")
    width, height = (int(value) for value in size_file.read_text().split())
    data = raw.read_bytes()
    expected = width * height * 4
    if len(data) != expected:
        raise SystemExit(f"{raw} holds {len(data)} bytes, expected {expected}")
    out = directory / f"{name}.png"
    Image.frombytes("RGBA", (width, height), data).save(out)
    print(out)
    return out


if __name__ == "__main__":
    if len(sys.argv) > 1:
        names = sys.argv[1:]
    elif os.environ.get("UI_SNAPSHOT_THEME") == "light":
        names = ["timeline-light", "main-light"]
    else:
        names = ["timeline", "main"]
    for name in names:
        convert(name)
