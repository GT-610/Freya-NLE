# Freya NLE Rust workspace

The workspace contains nine crates and builds the only application frontend:

| Crate | Responsibility |
| --- | --- |
| `freya-nle-core` | Domain rules, authoritative timeline state, documents, script runtime, and the retained C ABI |
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

## C ABI

`freya-nle-core/include/rust_core_abi.hpp` declares the raw C ABI that `freya-nle-core` still
exports. Its only consumer was the retired Qt frontend, so the surface is going module by
module: 97 of the 141 exports are already gone, and the 44 left stay with the contract
check until their APIs can be reshaped onto safe types (see
[the retirement audit](../audits/2026-10-26-qt-retirement.md)).
`tests/check_rust_abi.py` keeps the declaration and the exports in sync meanwhile.

The ABI keeps the pre-rename spelling (`aviqtl_*` functions, `AviQtl*` types, `AVIQTL_*` macros).
The crates, the binary, and the file formats use the current name.

- Callers must compare `aviqtl_core_abi_version()` with `AVIQTL_RUST_CORE_ABI_VERSION` before relying on the ABI and may inspect `aviqtl_core_capabilities()` for optional operations.
- The boundary contains only fixed-layout scalar fields, pointers, and lengths.
- Every byte and fixed-layout buffer is owned by the caller. The timeline-state API is the sole
  documented exception: Rust returns an opaque handle from `aviqtl_timeline_state_create`, and the
  caller must release it exactly once with `aviqtl_timeline_state_destroy`. The handle never exposes
  Rust memory and all JSON output still uses caller-owned buffers.
- A null pointer is accepted only when its matching length is zero. Non-empty buffers must be correctly aligned and valid for the duration of the call.
- Mutable output ranges must not overlap any input or other output range. Violations return `AVIQTL_RUST_CORE_STATUS_OVERLAPPING_BUFFERS` where the operation has a status result.
- Adding or reordering fields, changing a function signature, or changing a discriminant requires an ABI version increment. Additive functions or capabilities that leave existing layouts intact may use a new capability bit.

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
and the C ABI checker run alongside them in CI.
