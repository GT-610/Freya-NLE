# Third-party licences

Freya NLE is released under the GNU Affero General Public License version 3
(see `LICENSE.md`). A packaged build also contains the components below, each
under its own licence. Every release archive ships this file, `LICENSE.md`, and
the GPL-2.0 and LGPL-2.1 texts under `licenses/`, because an archive that
redistributes these binaries without their notices is not compliant.

## Shipped as binaries

| Component | Licence | Role in the build |
| :--- | :--- | :--- |
| FFmpeg | LGPL-2.1-or-later, built with `--enable-gpl` | Decoding, encoding, muxing, resampling. The GPL flag makes the shipped binaries GPL-2.0-or-later. |
| x264 | GPL-2.0-or-later | H.264 encoder reachable through FFmpeg. |
| x265 | GPL-2.0-or-later | HEVC encoder reachable through FFmpeg. |
| Carla | GPL-2.0-or-later | Optional VST3/LV2 host. Only `carla-discovery-native` is bundled, and only when the build host provides it. |

FFmpeg is linked dynamically on every platform, so a user may replace the
bundled libraries with their own build. The source for the exact FFmpeg used by
a release is identified by version and commit in `build-info.json`.

## Windows runtime

The Windows archive bundles `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll`, and
`MSVCP140.dll` when a binary in the archive imports it, so the build runs
without a separately installed redistributable. These files are redistributed
under the Microsoft Visual Studio licence terms; the archive does not modify
them.

## Linked at build time

| Component | Licence |
| :--- | :--- |
| Slint | GPL-3.0-only (used under the GPLv3 option) |
| wgpu, winit | MIT OR Apache-2.0 |
| Rust standard library | MIT OR Apache-2.0 |
| luna-core | MIT OR Apache-2.0 |
| Remix Icon | Remix Icon License |

The full dependency list with versions is reproducible from `rust/Cargo.lock`
at the commit recorded in `build-info.json`.

## Assets

`assets/remixicon.ttf` is from [Remix Icon](https://remixicon.com/) and is used
under the [Remix Icon License](https://raw.githubusercontent.com/Remix-Design/RemixIcon/refs/heads/master/License).
