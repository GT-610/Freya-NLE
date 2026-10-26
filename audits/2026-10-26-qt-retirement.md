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
and object: `every_qt_effect_catalog_entry_has_an_explicit_preview_route` enumerates the
44 effect definitions and `every_qt_object_catalog_entry_has_an_explicit_preview_route`
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
check survives without `core/`. The exports themselves are unchanged; see "Still open".

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

## Still open

Ordered roughly by user impact. Each is Rust-side work; the Qt implementation that
documented the behavior is in git history.

1. **Lua expression parameters.** Qt evaluated a parameter whose string value starts with
   `=` as Lua with `time`, `index`, `value`, and `v` bound
   (`core/include/effect_model.hpp:266-273,306-312`, `scripting/lua_host.cpp:97-125`).
   No Rust path evaluates expressions; such a parameter is read as a literal string, so a
   project that relies on one renders a frozen value. Implementing this needs a
   thread-local, cached Lua VM in the bake plan plus expression-aware parameter reads.
2. **Hardware video decode and encode.** Qt probed cuda/vaapi/d3d11va/dxva2/videotoolbox
   and created device contexts and hardware frame pools
   (`core/src/video_decoder.cpp:183-206`, `core/src/video_encoder.cpp:233-342`). The
   Rust media path decodes in software (`freya-nle-media/src/lib.rs:447-471`) and lists
   hardware encoder names but only builds software frames
   (`freya-nle-media/src/encoder.rs:19-34,493-498`). Preview and export still work;
   high-bitrate footage and hardware-accelerated export do not. Until this lands, the
   hardware encoder names should either be implemented or hidden so an export option is
   never offered that cannot run.
3. **Compute-shader effects.** Qt's `ComputeEffect` exposed work-group sizes and extra
   input textures (`core/src/compute_effect.cpp:54,94,189`) and 10 shipped `.comp`
   shaders used them. The native package contract allows one fragment pass, up to 16
   `vec4` uniforms, and no extra textures (`freya-nle-render/src/lib.rs:91-134`). The
   built-in effects have wgpu equivalents, but third-party compute packages cannot be
   authored.
4. **Text justification.** Alignment value 8 maps to `TextAlignment::Justify`
   (`freya-nle-core/src/bake_plan.rs:1417-1421`) and rasterizes left-aligned
   (`freya-nle-render/src/text.rs:462`).
5. **Unrouted catalog entries.** A definition that passes metadata validation but has no
   render route is listed and then renders nothing. The built-in catalog is covered by
   tests; a third-party native package with a valid WGSL runtime is rendered by the
   native path, so this is a diagnostics gap rather than a rendering one.
6. **SF2/SFZ soundfont discovery.** Qt's format table scanned them
   (`engine/plugin/audio_plugin_manager.cpp:371-375`); the Carla mapping omits them
   (`freya-nle-carla/src/lib.rs:550-559`). Qt could list but not host them, so the impact
   is the browser listing only.
7. **Camera `layerCount` timeline affordance.** The parameter is read for GroupControl
   but not for camera clips, so the timeline curtain Qt drew for a camera layer range is
   absent.
8. **Qt-era user-facing validation strings** in `freya-nle-app/src/object_settings.rs`
   are still Japanese while the rest of the interface follows the selected language.
9. **The C ABI and its contract check.** 141 exports across 21 modules and
   `freya-nle-core/include/rust_core_abi.hpp` have no consumer left in the tree. Retiring
   them touches every module's state handles and status codes, so it is a separate
   change; `tests/check_rust_abi.py` keeps the header and the exports in sync meanwhile.
10. **Documentation site.** `docs/` is a separate repository (`docs/docs/developer/
    building.md`, `docs/docs/zh-CN/developer/building.md`) and still documents
    `--frontend qt`, CMake, and the C ABI as a build requirement. That repository needs
    its own update.

## Verification

Run on macOS 26 (Apple M5, Metal) on the branch:

- `cargo fmt --manifest-path rust/Cargo.toml --all -- --check`
- `cargo test --manifest-path rust/Cargo.toml --locked --workspace --all-features`
- `cargo clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets --all-features -- -D warnings`
  is also clean on the pinned 1.97.1 toolchain. The host used for this audit has
  Homebrew Rust 1.99.1 without rustup, whose newer clippy reports two pre-existing
  `Atomic::fetch_update` deprecations in `freya-nle-audio`; they predate this change
  and the pinned toolchain does not flag them.
- `python -m unittest tests.test_build_version` and `python tests/test_rust_abi_contract.py`
- `python tests/check_rust_abi.py --rust-src rust/freya-nle-core/src --header rust/freya-nle-core/include/rust_core_abi.hpp`
- `python3 BUILD.py --help` (no `--frontend`/`--qt-dir`)
- `rust/target/debug/freya-nle --frames 120` reports the Apple M5 Metal device, loads the
  61 catalog definitions and 4 script plugins, and prints
  `SLINT_WGPU_COMPATIBILITY` with `main_same_device: true`, `timeline_same_device: true`,
  and no wgpu errors.

Not verified here: the deferred native desktop suite on Windows and Linux (save/close
ordering, IME, VoiceOver, dialogs, export cancellation) recorded in
[the parity gate](2026-10-23-slint-ui-parity-gate.md), and rendered-output comparison
against the removed Qt renderer. Both remain release-platform acceptance work rather than
migration blockers.
