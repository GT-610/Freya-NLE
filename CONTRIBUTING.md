Thank you for your interest in contributing to Freya NLE!

## AI/LLM Contributions

You may use AI/LLMs. They are tools, like a compiler or an autocomplete
engine, and the person who runs them is the author: if you wrote a change with
an LLM, you still wrote it, and you are the one who answers for it.

Four rules follow from that:

- **You are the author.** Understand the change and test it before you open a
  pull request. "The model wrote it" is not an answer to a review question,
  and a patch you cannot explain is not ready to merge however well it
  compiles.
- **Do not sign an AI's name.** Do not name a model or AI tool as an author or
  co-author, put one in a `Signed-off-by` line, or add a "generated with" note
  to a commit or pull request — some tools append one automatically, so check
  before you push. Every name in a signature has to be a person, because a
  signature means someone is accountable for the change and a model cannot be.
  `Co-authored-by:` keeps its normal meaning, a person who worked on the
  change and can answer for their part; adding a human co-author under an AI
  author is the same false signature, not a workaround.
- **A pull request needs a person behind it.** Open it yourself, or from an
  account that answers for it. A bot may not stand in for you.
- **AI review does not count.** A model reading the diff, or answering on a
  pull request, is no more an approval than a linter's output. Read it as a
  hint from an untrusted source, not as a review, and wait for a person to
  approve the change.

Two things you owe because the change is yours:

- **Check where the code came from.** Model output sometimes reproduces code
  it was trained on. Make sure what you submit is yours to license under the
  AGPLv3, and that it brings no code, header, or attribution you cannot
  honour.
- **Say so when it helps the reviewer.** If much of the change was generated,
  say so in the pull request so the reviewer knows where to read closely. For
  a routine edit it adds nothing, and it is never held against you.

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

Qt is not required: the Qt Quick frontend was retired in 0.8.

## Code Style

- **Rust**: `cargo fmt` and `cargo clippy` (both are CI gates)
- **Slint**: match existing patterns in `rust/freya-nle/ui/`
- **Commits**: use concise, descriptive English messages

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
and every entry must declare the `aviqtl-wgsl-v1` runtime, either as a fragment
effect or (with `"mode": "compute"` and a `workgroup` tile) a compute dispatch.
QML is not a package runtime: metadata that names a QML component is rejected at
installation time.

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
# Installed Carla libraries; the plugin-processing test also needs AVIQTL_CARLA_TEST_LADSPA.
cargo test --manifest-path rust/Cargo.toml --locked -p freya-nle-carla -- --ignored
```

Carla tests without its libraries remain unverified, not passed. The GPU compositing
tests that used to sit in `freya-nle-preview` were removed: they asked for an adapter no
CI runner provides, so they never ran anywhere. Check
the compositor by running the editor and exporting a frame.

## Versions and Releases

`rust/Cargo.toml` owns the project version (`[workspace.package] version`); no
other file may invent one. `BUILD.py` reads it from there, and the release
workflow rejects a tag that is not exactly `v<version>`.

Versions follow SemVer 2.0.0. A pre-release uses the dotted counter form, so the
first release candidate of 0.8.0 is version `0.8.0-rc.1` with tag
`v0.8.0-rc.1`:

- `0.8.0-rc.1`, `0.8.0-rc.2`, … `0.8.0` sort in the order they are built,
  because SemVer compares dot-separated identifiers and `rc.10` is newer than
  `rc.9`.
- `0.8.0-rc1` and `0.8.0-rc-1` are rejected. Each parses as one identifier, so
  `rc10` sorts before `rc9`, and every tool that expects the counter form has to
  special-case it.

`tests/test_build_version.py` fails when the version is not SemVer, when the
suffix is not `rc.<n>`, or when `CHANGELOG.md` has no section for the workspace
version, so bump them together.

Artifact names carry no toolchain: the toolchain, the commit, and the minimum
system a build needs belong in the `build-info.json` inside the archive.

## Reporting Issues

Please report bugs via [GitHub Issues](https://github.com/GT-610/freya-nle/issues).

## License

By contributing, you agree that your contributions will be licensed under the AGPLv3.
