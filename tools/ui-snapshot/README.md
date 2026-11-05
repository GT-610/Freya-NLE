# ui-snapshot

Renders the Freya NLE Slint windows without a window server or GPU, so a layout
change can be reviewed as an image instead of by launching the editor.

```bash
cd tools/ui-snapshot
cargo run -- timeline   # writes /tmp/timeline.rgba and /tmp/timeline.size
cargo run -- main       # writes /tmp/main.rgba and /tmp/main.size
python3 snapshot.py     # converts both to /tmp/timeline.png and /tmp/main.png
```

The build script compiles the shipping UI from
`rust/freya-nle/ui/app.slint`, so the render always matches the code. The sample
scenes, clips, and media rows live in `src/main.rs`; extend them when a new
surface needs covering.

Notes:

- The crate is standalone (its own `[workspace]`) and pins Slint to the version
  the editor uses, because the software renderer and the compiler must match.
- Strings are rendered from the source `@tr` text (English); the editor's own
  build script is the one that loads the `.po` catalogs.
- Frames are rendered with Slint's software renderer at 1:1 scale, so DPI
  differences are not covered by the snapshot.
