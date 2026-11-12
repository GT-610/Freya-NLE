"""Keeps diagnostics on the logging facade.

Every workspace crate reports through `log`. Direct `println!`/`eprintln!`
calls bypass the level filter, the rotating file, and the panic-safe sinks,
and `eprintln!` panics when stderr is closed - which is exactly the Windows
GUI launch this application must survive.

The two allowed sites are protocols, not diagnostics:

- `--validate-gpu` writes `SLINT_WGPU_COMPATIBILITY {json}` to stdout for the
  release checklist to parse.
- `--help` prints its text to stdout.

Build scripts are out of scope: `build.rs` speaks the `cargo:` protocol.
"""

import re
import unittest
from pathlib import Path

SOURCE_DIR = Path(__file__).resolve().parent.parent
RUST_DIR = SOURCE_DIR / "rust"

PRINT_PATTERN = re.compile(r"\b(eprintln|println|print|dbg)!\s*\(")

# (crate-relative path, substring that identifies the allowed call)
ALLOWED = (
    ("freya-nle/src/gpu.rs", "SLINT_WGPU_COMPATIBILITY"),
    ("freya-nle/src/main.rs", 'print!("{text}")'),
)


def rust_sources(root: Path = RUST_DIR):
    for crate in sorted(root.glob("freya-nle*")):
        source = crate / "src"
        if source.is_dir():
            yield from sorted(source.rglob("*.rs"))


def violations(root: Path = RUST_DIR):
    found = []
    for path in rust_sources(root):
        relative = path.relative_to(root).as_posix()
        lines = path.read_text().splitlines()
        for index, line in enumerate(lines):
            if not PRINT_PATTERN.search(line):
                continue
            # The `--validate-gpu` JSON is two lines: `println!(` then the
            # `SLINT_WGPU_COMPATIBILITY` payload, so look at the call site.
            call_site = "\n".join(lines[index : index + 3])
            allowed = any(
                relative == allowed_relative and marker in call_site
                for allowed_relative, marker in ALLOWED
            )
            if not allowed:
                found.append(f"{relative}:{index + 1}: {line.strip()}")
    return found


class LoggingHygieneTest(unittest.TestCase):
    def test_no_direct_print_calls_outside_protocols(self):
        found = violations()
        self.assertEqual(
            [],
            found,
            "\n".join(
                [
                    "Diagnostics must use log::info!/warn!/error! so they reach stderr",
                    "and the rotating file. Allowed protocol sites:",
                    *[f"  {path}: {marker}" for path, marker in ALLOWED],
                    "",
                    *found,
                ]
            ),
        )

    def test_allowlist_matches_real_code(self):
        # A stale entry would silently widen the rule the moment the protocol
        # line moves, so each exemption must still exist.
        for relative, marker in ALLOWED:
            text = (RUST_DIR / relative).read_text()
            self.assertIn(marker, text, f"stale allowlist entry: {relative}")


if __name__ == "__main__":
    unittest.main()
