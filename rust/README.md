# Freya NLE Rust workspace

The workspace contains nine crates and builds the only application frontend:

| Crate | Responsibility |
| --- | --- |
| `freya-nle-core` | Domain rules, authoritative timeline state, documents, script runtime, and shared bake data |
| `freya-nle-app` | Application/workspace commands, history, persistence, recovery, catalogs, MOD hosting, and packages |
| `freya-nle` | Desktop windows, callbacks, and presentation models; builds the application binary |
| `freya-nle-preview` | GUI-neutral frame planning and asynchronous preview decoding |
| `freya-nle-export` | Export jobs, progress, cancellation, rendering handoff, and output cleanup |
| `freya-nle-render` | wgpu composition, visual effects, text, and generated objects |
| `freya-nle-media` | Media decoding and encoding through FFmpeg |
| `freya-nle-audio` | Mixing, output, and plugin processing |
| `freya-nle-carla` | External Carla library and plugin ABI integration |

`python3 BUILD.py` builds and packages `freya-nle`. Native FFmpeg, Carla, graphics, and audio
dependencies are separate from the application's own code.

## Ownership boundary

The workspace owns the behavior of the application:

- project, settings, preset, package, effect, script, plugin, and keyframe documents;
- project migrations, defaults, schema normalization, and deterministic serialization;
- authoritative project/timeline state, reversible edit patches, scene and clip mutation, timeline
  edit planning, selection, scene settings, snapping, duration limits, and ID allocation;
- keyframe normalization, mutation, easing names, interpolation, typed numeric evaluation, and
  batch evaluation;
- audio resampling, mixing, metering, and batch-mix policy;
- media time/duration calculations, permission lookup, recovery identifier validation, package
  safety decisions, and catalog ordering/filtering;
- hardware-independent media probing, decoding, and encoding plans, including the software fallback
  every export uses;
- wgpu composition, the built-in effect catalog, text rasterization, and generated objects;
- the Slint windows, their menus, shortcuts, accessibility roles, and persistence.

When adding behavior, prefer one of the existing document or batch APIs. A change that needs a
native handle belongs behind a small platform module rather than in a window callback, and a
conversion loop at an API edge is an adapter, not an alternative domain implementation.

Scene, clip, effect, and audio-plugin edits are expressed as targeted timeline-state requests so
the core validates the authoritative state and produces reversible patches without rebuilding the
whole project document.

## No C ABI

The raw C ABI that used to live next to the crates is gone. The Qt frontend was its only
consumer, so every export was retired with it, the header and its contract checker left
the tree, and the plain data the Rust pipelines share now lives in `freya-nle-core`'s
`types` module. New integration work calls the Rust API; the
[retirement audit](../audits/2026-10-26-qt-retirement.md) records how the surface was
retired module by module.

## Toolchain and validation

The workspace uses Rust edition 2024. `rust-toolchain.toml` pins Rust 1.97.1, and the crates'
minimum supported Rust version matches that toolchain. Changes must pass:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --all-features -- -D warnings
cargo test --manifest-path rust/Cargo.toml --locked --workspace --all-features
```

The checks that need FFmpeg, a graphics adapter, or installed Carla libraries are listed in
[CONTRIBUTING](../CONTRIBUTING.md#adding-tests) and are run separately. `tests/test_build_version.py`
runs alongside them in CI.
