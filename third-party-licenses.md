# Third-party licences

Freya NLE is released under the GNU Affero General Public License version 3
(see `LICENSE`). A packaged build also contains the components below, each
under its own licence. Every release archive ships this file, `LICENSE`, and
the GPL-2.0, GPL-3.0 and LGPL-2.1 texts under `licenses/`.

## Shipped as binaries

| Component | Licence | Role in the build |
| :--- | :--- | :--- |
| FFmpeg | LGPL-2.1-or-later, built with GPL components | Decoding, encoding, muxing, resampling. The Linux AppImage and macOS recipe enables GPL and version 3; the Windows recipe enables GPL. The Arch archive uses the distribution's FFmpeg. |
| x264 | GPL-2.0-or-later | H.264 encoder reachable through FFmpeg. |
| x265 | GPL-2.0-or-later | HEVC encoder reachable through FFmpeg. |
| Carla | GPL-2.0-or-later | Optional VST3/LV2 host. Only `carla-discovery-native` is bundled, and only when the build host provides it. |

FFmpeg is linked dynamically on every platform, so a user may replace the
bundled libraries with their own build. The linked FFmpeg version is recorded
in `build-info.json`; a source revision is included when it can be established
from the installed headers.

## Windows runtime

The Windows archive bundles `VCRUNTIME140.dll` and `VCRUNTIME140_1.dll`, and
`MSVCP140.dll` when a binary in the archive imports it, so the build runs
without a separately installed redistributable. These files are redistributed
under the Microsoft Visual Studio licence terms; the archive does not modify
them.

## Linked at build time

| Component | Licence |
| :--- | :--- |
| Slint (and `slint-build`) | GPL-3.0-only (used under the GPLv3 option) |
| wgpu, naga, serde, serde_json, log, env_logger, chrono, url, ureq, semver | MIT OR Apache-2.0 |
| winit (through Slint's `backend-winit`) | Apache-2.0 |
| tiny-skia | BSD-3-Clause |
| cpal, pollster | Apache-2.0 |
| rfd, fontdb, rustybuzz, tracing | MIT |
| libloading | ISC |
| crc32fast, flate2, sha2, rtrb, tempfile, unicode-casefold, webbrowser, sys-locale | MIT OR Apache-2.0 |
| truce-rack | MIT OR Apache-2.0 |
| luna-core | MIT OR Apache-2.0 |
| ffmpeg-the-third (Rust bindings) | WTFPL |
| Rust standard library | MIT OR Apache-2.0 |
| Remix Icon | Remix Icon License |

The full dependency list with versions is reproducible from `rust/Cargo.lock`
at the commit recorded in `build-info.json`.

## Assets

`assets/remixicon.ttf` is from [Remix Icon](https://remixicon.com/) and is used
under the [Remix Icon License](https://raw.githubusercontent.com/Remix-Design/RemixIcon/refs/heads/master/License).
