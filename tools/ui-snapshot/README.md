# ui-snapshot

Renders the Freya NLE Slint windows without a window server or GPU, so a layout
change can be reviewed as an image instead of by launching the editor.

```bash
cd tools/ui-snapshot
cargo run -- timeline   # writes target/ui-snapshot/timeline.rgba (+ .size)
cargo run -- main       # writes target/ui-snapshot/main.rgba (+ .size)
python3 snapshot.py     # converts both to target/ui-snapshot/*.png
```

Converting PNGs needs Pillow:

```bash
python3 -m pip install pillow
```

Set `UI_SNAPSHOT_THEME=light` to render the light palette; the Rust tool then
writes `timeline-light.*`, and `python3 snapshot.py` with no arguments picks the
same names up. Other modes: `settings`, `system`, `confirm`, `mark`, `layer`, `export`,
`packages`. `UI_SNAPSHOT_SIZE=680x600` overrides the window size, and
`UI_SNAPSHOT_IMAGE=/path/to.png` supplies a stand-in preview frame.

The build script compiles the shipping UI from
`rust/freya-nle/ui/app.slint`, so the render always matches the code. The sample
scenes, clips, and media rows live in `src/main.rs`; extend them when a new
surface needs covering.

Notes:

- Frames go to this crate's `target/ui-snapshot/` (git-ignored) rather than a
  predictable name in the system temp directory; both the Rust tool and the
  script validate that the directory is not a symlink and belongs to the user.
- The crate is standalone (its own `[workspace]`) and pins Slint to the version
  the editor uses, because the software renderer and the compiler must match.
- Strings are rendered from the source `@tr` text (English); the editor's own
  build script is the one that loads the `.po` catalogs.
- Frames are rendered at 1:1 scale, so DPI differences are not covered.
