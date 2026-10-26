# Naming

The product is **Freya NLE**, shortened to **Freya** in prose. The backronym is
*FREe Yet Another Non-Linear Video Editor*, and it belongs in a FAQ entry at
most; the marketing name is Freya, not the expansion.

The repository is `freya-nle`. Older links to `AviQtl-Plus` keep working through
GitHub's rename redirect, so the name should not be handed to another project.

## Identifiers

| Surface | Value |
| --- | --- |
| Binary | `freya-nle` |
| Rust crates | `freya-nle-core`, `-app`, `-audio`, `-carla`, `-export`, `-media`, `-preview`, `-render`, `-slint` |
| Crate directories | `rust/freya-nle-*` |
| macOS bundle identifier | `io.github.gt610.freya-nle` |
| User data directory | `Freya NLE` under the platform data root |
| Portable settings file | `freya_settings.json` |
| Project file | `.freya`, with `.aviqtl` still readable |
| Fragment file | `.freyafrag`, with `.aviqtlfrag` still readable |
| Lua script namespace | `freya`, with `aviqtl` kept as a deprecated alias |

The `io.github.gt610` prefix follows the convention for projects published on
GitHub; it is not tied to a domain the project owns.

## What keeps the old spelling

The Qt frontend is being removed, so anything only it can see keeps its current
name until the code is deleted. That covers the C ABI between `freya-nle-core`
and `core/include/rust_core_abi.hpp` (`aviqtl_*` functions, `AviQtl*` types,
`AVIQTL_*` macros), the CMake targets, the Qt translation files in `i18n/*.ts`,
and the `AviQtl::Core::...` context strings that `rust/freya-nle-slint/build.rs`
reads out of those files.

Renaming the C ABI now would mean editing both sides of a contract that is
scheduled for deletion, so it waits for the Qt removal.

## Names that are not ours

`AviUtl`, `AviUtl2`, `ExEdit`, and `.exo` belong to other projects. References
to them keep their spelling, including "AviUtl 1.10", "ExEdit 0.92", and the
object-file import.
