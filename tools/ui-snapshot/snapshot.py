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


def check_top(
    base_name: str, tall_name: str, footer: int = 64, tolerance: float = 0.005
) -> int:
    """Compares the shared top region of two renders of one window.

    Growing a dialog must add space at the bottom, so the top of the taller
    render has to look exactly like the shorter one. A window that instead
    redistributes the extra height into its rows (the stretched combo box this
    check was written for) shows up as differing pixels here.
    """
    # Re-convert both frames first: the PNGs are derived from the raw captures,
    # and a rerender that was never converted would be compared against the
    # previous run's image and pass while the current one regressed.
    base_path = convert(base_name)
    tall_path = convert(tall_name)
    base = Image.open(base_path).convert("RGBA")
    tall = Image.open(tall_path).convert("RGBA")
    if tall.width != base.width or tall.height <= base.height:
        # Strictly taller: an equal height means the size override did not take
        # effect, and identical images would then pass without testing anything.
        raise SystemExit(
            f"{tall_name} is {tall.width}x{tall.height}, "
            f"expected the same width and more than {base.height} rows"
        )
    # A footer strip is ignored: a button pinned to the bottom of the shorter
    # window legitimately moves down when the window grows.
    if base.height <= footer:
        raise SystemExit(
            f"{base_name} is only {base.height} rows tall, so there is no top "
            f"region left after ignoring the {footer}-row footer"
        )
    compared_height = base.height - footer
    shared = tall.crop((0, 0, base.width, compared_height))
    base = base.crop((0, 0, base.width, compared_height))
    base_pixels = base.tobytes()
    shared_pixels = shared.tobytes()
    differing = sum(
        1
        for index in range(0, len(base_pixels), 4)
        if base_pixels[index : index + 4] != shared_pixels[index : index + 4]
    )
    total = base.width * compared_height
    ratio = differing / total
    print(
        f"{base_name} vs {tall_name}: {differing}/{total} pixels differ "
        f"({ratio:.2%})"
    )
    return 0 if ratio <= tolerance else 1


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
    if len(sys.argv) == 4 and sys.argv[1] == "--check-top":
        raise SystemExit(check_top(sys.argv[2], sys.argv[3]))
    if len(sys.argv) > 1:
        names = sys.argv[1:]
    elif os.environ.get("UI_SNAPSHOT_THEME") == "light":
        names = ["timeline-light", "main-light"]
    else:
        names = ["timeline", "main"]
    for name in names:
        convert(name)
