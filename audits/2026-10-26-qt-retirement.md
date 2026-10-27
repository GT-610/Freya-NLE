# Retiring the Qt frontend

Date: 2026-10-26
Branch: `chore/qt-retirement` (from `main` at `33e0df31`)
Scope: decide which Qt features the Rust + Slint frontend had fully replaced, remove the
replaced Qt code, carry over everything the replacement still read, and record what is
deliberately different or still open.

The Qt tree referenced below was removed in this change; file and line citations into
`ui/`, `core/`, `engine/`, and `scripting/` describe commit `33e0df31` and are kept as the
evidence for each decision.

## Verdict

The user-visible surface of the Qt frontend is projected by the Slint frontend, and the
two exceptions are recorded below. The catalog and renderer cover every shipped effect
and object: `every_effect_catalog_entry_has_an_explicit_preview_route` enumerates the
44 effect definitions and `every_object_catalog_entry_reaches_its_production_render_route`
the 17 objects, and the startup log reports `Effect catalog · 61 definitions`. Removing
the Qt tree therefore removed a behavior reference, not a running feature - with the
caveat that the remaining differences in the "Deliberate deviations" and "Still open"
sections are now Rust-side work items instead of a second implementation to compare
against.

What was checked:

- **Windows, dialogs, and overlays** (13 windows plus every modal and chooser) - all had
  a Slint counterpart; the window-geometry ids and defaults match. The startup splash
  was the only Qt window with no Slint analogue.
- **Menu actions** - all 15 Qt menu items exist in the Slint menu bar, which also adds
  fragments, `.exo` import, the trim tools, workspace layouts, and the media library.
- **Keyboard shortcuts** - the 34 configurable actions and their defaults are identical
  (`ui/qml/MainWindow.qml:146-611` against `rust/freya-nle/src/shortcuts.rs:236-356`).
  Qt bound 20 of them with `Qt.ApplicationShortcut`, which the per-window Slint event
  model does not reproduce; see the deviations.
- **Catalog and rendering** - 44 effects and 17 objects have explicit Rust routes, and
  every object parameter in the shipped metadata is read by `freya-nle-core`'s bake
  plan.
- **Core services** - project normalization, packages, presets, permissions, settings,
  themes, audio planning, media probing, and export planning are Rust-owned; Qt already
  called into them through the C ABI.

## What moved out of Qt-owned paths

The Slint build still read four things from the legacy directories. Each moved to a
workspace-owned path in the first commit of this change:

| Data | Old location | New location | Consumers updated |
| --- | --- | --- | --- |
| 44 effect + 17 object definitions | `ui/qml/effects/*.json`, `ui/qml/objects/*.json` | `catalog/effects/`, `catalog/objects/` | `freya-nle-app` dev resource root, `freya-nle-preview` tests, `included_bytes!` fixtures, `BUILD.py` packaging |
| Icon font for the text rasterizer test | `ui/resources/remixicon.ttf` | `assets/remixicon.ttf` | `freya-nle-render` test module |
| Effect-name translations | `i18n/AviQtl_{en_US,zh_CN}.ts` (`AviQtl::Core::EffectRegistry` context, 293 pairs) | `i18n/effect-metadata.json` | `rust/freya-nle/build.rs` |
| Easing golden for the keyframe test | `tests/data/keyframe_easing_cpp_golden.txt` | `rust/freya-nle-core/tests/fixtures/keyframe_easing_golden.txt` | `freya-nle-core` unit test |

The built-in metadata no longer names a QML component. It carries `"builtin": true`
instead, and `normalize_metadata` requires either that marker or a native
`aviqtl-wgsl-v1` runtime; a definition that only names a QML component is reported by
name when a package is installed. `BUILD.py` still stages `catalog/effects` and
`catalog/objects` as the bundle-level `effects/` and `objects/` directories, so the
packaged layout and the CI checks that look for it did not change.

The C ABI header moved to `rust/freya-nle-core/include/rust_core_abi.hpp` so its contract
check would survive without `core/`; the whole surface was later deleted once its modules
were retired (see [C ABI retirement](#c-abi-retirement)).

## Deliberate deviations

These are behaviors that Qt had and the Slint frontend does not, accepted on 2026-10-26.
None of them needs the Qt source to be reimplemented.

- **Startup splash.** Qt opened a frameless splash window sized by the `splashSize`
  setting (`core/src/main.cpp:110-122`, `ui/qml/SplashView.qml`) while the QML engine and
  services initialized. The Slint frontend does its startup in one synchronous pass
  before any window can be shown, so a splash window would be mapped and hidden without
  ever painting a frame. The splash is not implemented and its now-dead "Splash image
  size" row is removed from system settings; `splashSize` stays a persisted key like
  `bakeStrategy`.
- **Application-wide shortcuts.** Qt bound New/Open/Save/Save As/Export/Quit/Preferences/
  Undo/Redo/Add Scene and the transport keys with `Qt.ApplicationShortcut`, so they fired
  while any window had focus. Slint delivers keys per window; the editor windows install
  the handler, the other eleven windows do not. Extending that safely needs a
  "a text field has focus" query the Slint runtime does not expose, and Qt's own
  `_isInputFocused` guard disabled even the accelerators while typing, which is not worth
  copying. The commands remain reachable from the editor windows and the menu bar.
- **QML effect and object packages.** Qt installed packages whose metadata named a QML
  component and compiled GLSL to `.qsb` (`core/src/package_manager.cpp:747,910-931`).
  The Slint edition has no QML runtime; native JSON + WGSL (`aviqtl-wgsl-v1`) is the
  package contract and QML packages are rejected by name at installation. This was
  already true before the retirement and is documented in `docs/docs/developer/effects.md`.

## Filled after the removal

These were recorded as gaps by the audit and are implemented on the same branch:

- **Lua expression parameters.** A parameter stored as a string beginning with `=`
  is evaluated as Lua with `time` (seconds from the object start), `t`, `index`,
  `value`, and `v` in scope (`freya-nle-core/src/expression.rs`). The evaluator is
  a lazy, per-thread Lua state with compiled-expression caching and an instruction
  budget, and the bake plan applies it over the keyframe value for every parameter
  it hands to the renderer, the exporter, and the audio planner
  (`freya-nle-core/src/bake_plan.rs`, `CachedEffect::expression_value`). A broken
  expression keeps the literal fallback instead of zeroing the parameter.
- **Hardware encoding on macOS.** `h264_videotoolbox` and `hevc_videotoolbox` joined the
  encoder catalog and the export dialog, with `libx264`/`libx265` as their fallback. A
  real three-frame encode was run against the linked FFmpeg on Apple Silicon.
- **Text justification.** `Text.AlignJustify` now distributes the leftover width
  over the word gaps of every line except the last one
  (`freya-nle-render/src/text.rs`, `justified_glyphs`).
- **Validation messages follow the interface language.** The object-settings
  number, integer, and non-text errors are composed with `status_message!` and
  rendered through the same localization path as the status bar.
- **The application calls itself Freya NLE everywhere.** `--version`, the usage
  text, the missing-GPU message, the error-dialog title, the update notice, and
  the About window's project link no longer say "AviQtl Plus" or open the
  upstream Qt project's page; the link now matches the URL the About window
  displays.

## Still open

Ordered roughly by user impact. Each is Rust-side work; the Qt implementation that
documented the behavior is in git history.

1. **Hardware video decode, and hardware frames for encode.** Qt probed
   cuda/vaapi/d3d11va/dxva2/videotoolbox and created device contexts and hardware frame
   pools (`core/src/video_decoder.cpp:183-206`, `core/src/video_encoder.cpp:233-342`).
   The Rust media path decodes in software (`freya-nle-media/src/lib.rs:447-471`).
   Encoding now offers the Apple VideoToolbox encoders next to the NVENC/AMF/QSV/VAAPI
   entries and was verified end-to-end on macOS; an encoder that cannot open - VAAPI
   without a hardware frames context, for one - falls back to
   `libx264`/`libx265`/`libaom-av1` through `export_codec_fallback`, so the export still
   completes. What is left of Qt's path is the explicit `AVHWDeviceContext` /
   `hw_frames_ctx` setup: hardware-frame decoding, and a hardware upload path for
   VAAPI/QSV instead of the software fallback.

   Blocking constraint: `freya-nle-media/Cargo.toml` sets `unsafe_code = "forbid"`,
   like the other application crates. `ffmpeg-the-third` does not expose device
   creation, `get_format` negotiation or `av_hwframe_transfer_data`, so a hardware
   decoder needs a small unsafe FFI module in that crate. The design is understood
   (create a VideoToolbox/VAAPI device, prefer its pixel format during decode,
   transfer frames to system memory before the scaler), but relaxing a crate-level
   safety policy is a project decision rather than a missing implementation.
2. **Compute-shader effects, partly carried over.** Qt's `ComputeEffect` exposed
   work-group sizes and extra input textures (`core/src/compute_effect.cpp:54,94,189`) and
   10 shipped `.comp` shaders used them. Native packages can now declare
   `"mode": "compute"` with a `workgroup` tile: the host binds the layer texture, the
   parameter block and an `rgba8unorm` output storage texture, dispatches one compute pass
   per effect, and rejects metadata whose tile does not match the shader's
   `@workgroup_size`. `"dispatch": n` (1–8) chains the shader over its own output with
   `aviqtl_pass()`, matching Qt's `dispatchCount`/`passIndex` ping-pong. The source-level
   validator accepted either contract so installation still works before metadata is
   paired. Compute shaders also read the layer as it entered the effect chain
   (`aviqtl_original_texel`), covering Qt's `sourceProxy` extra texture for effects such as
   border blur and drop shadow. What is not carried over is binding a *different* layer as
   an input (`syncExtraTextures` with another item), which needs preview-side routing
   before it means anything.
3. **Unrouted catalog entries.** A definition that passes metadata validation but has no
   render route is listed and then renders nothing. The preview planner now reports an
   effect id without a renderer, and an object clip type that is not in the catalog the
   app loaded, once per document instead of dropping them silently. It stays quiet when
   no catalog was supplied, because then it cannot tell a missing object from an unloaded
   one.
4. **SF2/SFZ soundfont discovery, deliberately not carried over.** Qt's format table
   scanned them (`engine/plugin/audio_plugin_manager.cpp:371-375`) while its host mapping
   had no SF2/SFZ case, so the browser offered plugins that could not load. The Rust
   catalog omits the formats (`freya-nle-carla/src/lib.rs:550-559`) instead of repeating
   that; a soundfont format needs a host path before it gets a scan path.
5. **Documentation site.** `docs/` is a separate repository (submodule on its
   `release/0.7.0-rc.1` branch) and its sources still describe the Qt build:
   `developer/building.md` and `developer/release-checklist.md` name CMake, the
   `--frontend qt` flag and `tests/check_rust_abi.py`; `developer/index.md` still lists
   "C++23 / Qt 6 + Qt Quick" as the application UI; `guide/start-here/about.md` and
   `guide/help-and-reference/faq.md` tell the pre-migration story. Those need a
   docs-repository change and a submodule bump, not a change on this branch.

## C ABI retirement

`rust/freya-nle-core/include/rust_core_abi.hpp` and its exports existed so the Qt frontend
could call the Rust core. With that consumer gone the surface was dead weight, so it was
retired module by module: the thin `extern "C"` shims first, then the raw-pointer
protocols after their callers moved onto the safe Rust APIs. **All 141 exports are gone**,
and the header, `tests/check_rust_abi.py`, its contract test, the CI step, the static
library crate type and the ABI version/capability table left with them.

| module | exports retired | what was behind them |
| --- | --- | --- |
| `policy` | 18 | media timing, permission tables, package and recovery id validation |
| `timeline_state` | 13 | create/reset/snapshot, edit planning, transactions and id hints |
| `settings` | 12 | the settings document handle behind `SettingsState` |
| `export` | 11 | video, image-sequence, audio-frame and progress planning |
| `permission` | 11 | the permission-state handle behind `PluginPermissionState` |
| `package` | 11 | the catalog-state handle and the package-document dispatcher |
| `timeline_edit` | 10 | vacancy search, batch/delta moves, drag, resize, layers, clipboard, split |
| `timeline_domain` | 9 | scene settings, snapping, duration, selection and reorder planning |
| `effect` | 7 | effect metadata normalization and the ABI-only catalog registry |
| `plugin` | 7 | the plugin-document dispatcher and the script-plugin catalog registry |
| `audio` | 5 | batch mixing, playback planning and waveform sampling |
| `bake_plan` | 5 | the opaque scene-plan handle and its full-bake evaluation |
| `lib` (easing) | 4 | easing name and curve entry points |
| `preset` | 3 | preset document build/normalize |
| `recovery` | 3 | recovery metadata inspect/build and snapshot naming |
| `keyframe` | 3 | numeric interpolation names, typed and batch track evaluation |
| `keyframe_document` | 2 | typed keyframe-document apply and evaluate wrappers |
| `project` | 2 | project version and document normalization |
| `timeline` | 2 | render and audio bake helpers |
| `script` | 1 | script metadata parsing |

The functions behind them stay where the Rust frontend or the tests use them:
`parse_metadata`, `normalize_project_json`, `bake_render`/`bake_audio`,
`evaluate_document_track`, `evaluate_numeric_segment`, `normalize_metadata`,
`mix_stereo_tracks`, `resample_stereo`, `plan_delta_move`, `plan_video_export`,
`SettingsState`, `PluginPermissionState`, `api::PackageCatalog`, `SceneRenderPlan` and the
timeline state machine are still exercised directly.

Two consequences worth recording:

- **What left with the wrappers.** Buffer-protocol tests (size queries, overlapping
  ranges, null and misaligned pointers) had nothing left to guard. So did logic only the
  wrappers reached: the ABI-only effect and plugin catalogs, the C-side playback and
  waveform planners that `freya-nle` implements itself, and the bake plan's full-bake
  output mode, whose scene-plan JSON parser survives as a `#[cfg(test)]` fixture parser
  because the typed tests are written against that format.
- **The remaining data types.** `abi.rs` became `freya-nle-core/src/types.rs`: the plain
  `EasingParameters`, `AudioMixParameters`/`AudioMeter`, `RenderBakeInput`/`Output`,
  `AudioBakeInput`/`Output`, `TimelineClipGeometry`, `TimelineMoveInput`,
  `SceneSettings` and `IdAllocation` that the bake, timeline and audio pipelines pass
  around. They lost their `#[repr(C)]` layouts and the `AviQtl*` spelling, kept only the
  fields the Rust callers read, and `timeline_edit` reports its planning errors through a
  small `EditError` enum instead of the old status codes.

## Verification

Run on macOS 26 (Apple M5, Metal) on the branch:

- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`
- `cargo test --manifest-path rust/Cargo.toml --locked --workspace --all-features`
- `cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --all-features -- -D warnings`
  is also clean on the pinned 1.97.1 toolchain. The host used for this audit has
  Homebrew Rust 1.99.1 without rustup, whose newer clippy reports three pre-existing
  lints in `freya-nle-audio` (two `Atomic::fetch_update` deprecations and a
  `chunks_exact` suggestion); they predate this change and the pinned toolchain does
  not flag them.
- `python -m unittest tests.test_build_version`
- `python3 BUILD.py --xcode --offline` after the compute-package work assembles, signs and
  verifies `build/Freya NLE.app` (44 `Resources/effects`, 17 `Resources/objects`,
  `effect-packages`, `plugins`, `repos`), writes
  `dist/AviQtl-macOS-Xcode-arm64.zip`, and the packaged
  `Contents/MacOS/freya-nle --frames 60` loads all 61 definitions and 4 script plugins,
  reports `main_same_device: true`, `timeline_same_device: true` and no wgpu errors, and
  exits 0
- `python3 BUILD.py --help` (no `--frontend`/`--qt-dir`)
- `rust/target/debug/freya-nle --frames 120` reports the Apple M5 Metal device, loads the
  61 catalog definitions and 4 script plugins, and prints
  `SLINT_WGPU_COMPATIBILITY` with `main_same_device: true`, `timeline_same_device: true`,
  and no wgpu errors.
- `cargo test --locked -p freya-nle-media -p freya-nle-audio -- --ignored` and
  `-p freya-nle-preview -- --ignored` pass, including a real decode/mix/encode round trip
  and an H.264 plus HEVC VideoToolbox encode.
- `python3 BUILD.py --xcode --offline` builds, signs, and archives a release:
  `build/Freya NLE.app` contains `Resources/effects` (44), `Resources/objects` (17),
  `plugins`, `effect-packages`, and `repos`, the archive lands in `dist/`, and running
  `Contents/MacOS/freya-nle --frames 120` from the bundle loads all 61 definitions with a
  shared Metal device and no wgpu errors.

Not verified here: the deferred native desktop suite on Windows and Linux (save/close
ordering, IME, VoiceOver, dialogs, export cancellation) recorded in
[the parity gate](2026-10-23-slint-ui-parity-gate.md), and rendered-output comparison
against the removed Qt renderer. Both remain release-platform acceptance work rather than
migration blockers.
