# Contributing to Freya NLE

Thank you for your interest in contributing!

## Getting Started

1. Fork the repository, clone your fork, and create a branch from `main`
2. Build the project using `python3 BUILD.py` (see the [build documentation](https://aviqtl.gt610.dpdns.org/developer/building) for platform-specific instructions)
3. Make your changes
4. Run the checks below
5. Submit a pull request

## Development Environment

- **Rust**: the version pinned in `rust-toolchain.toml`
- **Frontend**: Rust, Slint, wgpu
- **Build**: `python3 BUILD.py` drives Cargo for every platform
- **Dependencies**: FFmpeg, Carla, a GPU-backed graphics adapter, and a working audio output device

Qt is not required: the Qt Quick frontend was retired in 0.8. See [Qt retirement](#qt-retirement).

## Code Style

- **Rust**: `cargo fmt` and `cargo clippy` (both are CI gates)
- **Slint**: match existing patterns in `rust/freya-nle/ui/`
- **Commits**: use concise, descriptive messages

## Project Structure

```
catalog/       - built-in effect and object metadata, staged beside the binary
rust/          - the Rust workspace and the application binary
plugins/       - example Lua plugins used by the plugin host
effect-packages/ - native, installable effect and object packages
assets/        - icons, screenshots, and other shipped resources
i18n/          - effect-metadata translations and translated READMEs
tests/         - build metadata checks (Python)
tools/         - repository utilities
no-gpu-probes/ - standalone probes used to measure GPU-support decisions
```

## Adding Effects or Objects

Effects and objects are native JSON + WGSL packages. Each package contains a
`manifest.json`, one or more metadata JSON files, and the shaders they declare,
and every entry must declare the `aviqtl-wgsl-v1` runtime. QML is not a package
runtime: metadata that names a QML component is rejected at installation time.

See the [effects and objects documentation](https://aviqtl.gt610.dpdns.org/developer/effects)
for the metadata schema, the shader contract, and the package layout. The
[`effect-packages/`](effect-packages/) directory contains validated examples.

## Adding Tests

Rust tests live next to their implementations and under `rust/freya-nle-core/tests`.
Run them from the repository root with the platform's native dependency environment loaded:

```sh
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --all-features -- -D warnings
cargo test --manifest-path rust/Cargo.toml --locked --workspace --all-features
python -m unittest tests.test_build_version
```

On Windows, use the MSVC developer environment and the same `FFMPEG_DIR`,
`LIBCLANG_PATH`, and runtime DLL paths configured by `BUILD.py --msvc`.
An existing packaged executable alone does not provide the headers needed by Cargo.

The following checks require additional runtime capabilities and are ignored by
the default Rust test command. Run each separately so a missing device or plugin
is reported explicitly:

```sh
# FFmpeg CLI on PATH; linked FFmpeg must provide libx264 and aac.
cargo test --manifest-path rust/Cargo.toml --locked -p freya-nle-media -p freya-nle-audio -- --ignored
# A graphics adapter; the preview tests enable DX12, Vulkan, or Metal by platform.
cargo test --manifest-path rust/Cargo.toml --locked -p freya-nle-preview scaled_msaa_preview -- --ignored
# Installed Carla libraries; the plugin-processing test also needs AVIQTL_CARLA_TEST_LADSPA.
cargo test --manifest-path rust/Cargo.toml --locked -p freya-nle-carla -- --ignored
```

Carla/GPU tests without their dependencies remain unverified, not passed.

## Qt retirement

The Qt Quick + QRhi frontend and its C++ support library were removed in 0.8. The
[Rust + Slint UI parity gate](audits/2026-10-23-slint-ui-parity-gate.md) records
the behavior it was held to, and the
[retirement audit](audits/2026-10-26-qt-retirement.md) records what was migrated,
what deliberately changed, and what is still open.

The C ABI is gone with the Qt consumer: no `aviqtl_*` exports, header, or contract
check remain. `freya-nle-core` exposes Rust APIs only, and the shared bake/timeline data
types live in its `types` module.

## Versions and Releases

`rust/Cargo.toml` owns the project version (`[workspace.package] version`); no
other file may invent one. `BUILD.py` reads it from there, and the release
workflow rejects a tag that is not exactly `v<version>`.

Versions follow SemVer 2.0.0. A pre-release uses the dotted counter form, so the
first release candidate of 0.7.0 is version `0.7.0-rc.1` with tag
`v0.7.0-rc.1`:

- `0.7.0-rc.1`, `0.7.0-rc.2`, … `0.7.0` sort in the order they are built,
  because SemVer compares dot-separated identifiers and `rc.10` is newer than
  `rc.9`.
- `0.7.0-rc1` and `0.7.0-rc-1` are rejected. Each parses as one identifier, so
  `rc10` sorts before `rc9`, and every tool that expects the counter form has to
  special-case it.

`tests/test_build_version.py` fails when the version is not SemVer, when the
suffix is not `rc.<n>`, or when `CHANGELOG.md` has no section for the workspace
version, so bump them together.

To prepare a release:

1. Set `[workspace.package] version` in `rust/Cargo.toml` and refresh
   `rust/Cargo.lock` with `cargo update --workspace --offline`.
2. Move the `Unreleased` entries of `CHANGELOG.md` into a
   `## [<version>] - <YYYY-MM-DD>` section and leave `Unreleased` empty.
3. Run the checks listed in [Adding Tests](#adding-tests), then build and smoke
   test a package with `python3 BUILD.py`.
4. Tag the validated commit `v<version>`. A tag containing `-` is published as a
   GitHub pre-release and does not replace the latest stable release.

## Reporting Issues

Please report bugs via [GitHub Issues](https://github.com/GT-610/freya-nle/issues).

## License

By contributing, you agree that your contributions will be licensed under the AGPLv3.
