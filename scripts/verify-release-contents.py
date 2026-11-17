#!/usr/bin/env python3
"""Verify the provenance and licence files in an unpacked release artifact."""

import argparse
import json
from pathlib import Path


SOURCE = Path(__file__).resolve().parent.parent
LICENSES = {
    "LICENSE": SOURCE / "LICENSE",
    "third-party-licenses.md": SOURCE / "third-party-licenses.md",
    "GPL-2.0.txt": SOURCE / "licenses" / "GPL-2.0.txt",
    "GPL-3.0.txt": SOURCE / "licenses" / "GPL-3.0.txt",
    "LGPL-2.1.txt": SOURCE / "licenses" / "LGPL-2.1.txt",
}


def verify(root: Path, version: str, commit: str, platform: str, architecture: str, variant: str | None) -> None:
    info = json.loads((root / "build-info.json").read_text(encoding="utf-8"))
    expected = {
        "name": "Freya NLE", "version": version, "commit": commit,
        "platform": platform, "architecture": architecture, "variant": variant,
        "profile": "Release",
    }
    for key, value in expected.items():
        if info.get(key) != value:
            raise ValueError(f"{root}: {key} is {info.get(key)!r}, expected {value!r}")
    ffmpeg = info.get("ffmpeg")
    if not isinstance(ffmpeg, dict) or not isinstance(ffmpeg.get("version"), str) or not ffmpeg["version"].strip():
        raise ValueError(f"{root}: FFmpeg version is missing")
    if ffmpeg.get("source") not in ("system", "ffmpeg-dir"):
        raise ValueError(f"{root}: FFmpeg source is missing or invalid")
    for name, source in LICENSES.items():
        if (root / "licenses" / name).read_bytes() != source.read_bytes():
            raise ValueError(f"{root}: bundled {name} differs from the source commit")
    print(f"Verified {root}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("version")
    parser.add_argument("commit")
    parser.add_argument("platform", choices=("linux", "macos", "windows"))
    parser.add_argument("architecture", choices=("arm64", "x86_64"))
    parser.add_argument("--variant", default=None)
    args = parser.parse_args()
    verify(args.root, args.version, args.commit, args.platform, args.architecture, args.variant)


if __name__ == "__main__":
    main()
