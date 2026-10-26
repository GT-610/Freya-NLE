# Freya NLE frontend

This crate is the Rust desktop frontend for Freya NLE. It preserves the existing layer/object,
start/end parameter, independent-window, project ownership, and save-confirmation workflows.
The September 2026 workflow improvements deliberately extend the Qt baseline with AviUtl-style
numeric scrubbing and layer selection, standard Slint menu shortcuts, compact object settings,
and optional workspace layouts. See [the implementation status](../../audits/2026-09-24-slint-workflow-implementation.md)
for the supported behavior, validation, and deferred architecture work.

Standard widgets use the active platform palette and metrics. Timeline clips, waveforms,
keyframes, drag grips, and splitter hit regions remain custom Slint editing surfaces.

## Architecture

The Slint frontend is intentionally thin:

- `freya-nle-core` owns project documents, timeline transactions, geometry planning, rendering plans,
  effect metadata, settings schemas, recovery data, and other domain rules.
- `freya-nle-app` owns framework-neutral application and workspace state: open projects, scene and clip
  selection, clipboard commands, undo/redo, transport, media import, missing media, presets, and
  settings persistence.
- `freya-nle-preview` owns the production frame planner, asynchronous media
  decoding, nested-scene handling, and wgpu compositor without depending on a GUI framework.
- `freya-nle-export` owns GUI-neutral export jobs, decoded-frame handoff, wgpu composition/readback,
  image-sequence output, video/audio encoding, progress, cancellation, and partial-output cleanup.
- `freya-nle-render`, `freya-nle-media`, `freya-nle-audio`, and `freya-nle-carla` own the lower-level
  renderer, media, audio, and plugin integration.
- `freya-nle` owns native windows, declarative layout, input hit regions, menus, accessibility,
  and translation between Slint models/callbacks and `freya-nle-app` commands.

Open and Save As use `rfd` platform dialogs. On Linux its default backend is the XDG portal rather
than GTK, preserving the migration goal of not reintroducing a large native GUI build dependency.
Save/Discard/Cancel sequencing remains in `freya-nle-app`; the native chooser only returns a path.

Project recovery follows the same split. `freya-nle-app` owns recovery IDs, metadata validation,
periodic scheduling, a single background file worker, stale cleanup, and Save/Close/Quit cleanup.
Slint only presents the launcher-owned recovery list and forwards Recover or Discard. Recovering
always creates a dirty pathless project, so Save opens Save As and never overwrites the original.

Settings follow the same ownership boundary. `SettingsStore` remains the persisted source of truth;
Slint windows only hold drafts and forward Apply, Reload, OK, or Close. Runtime settings update the
application model immediately after a successful atomic save, including quit confirmation,
automatic recovery, recovery interval, undo limits, new-project defaults, timeline skimming and
dimensions, the configured 1-512 layer limit, object-settings sidebar placement, the Dark/Light/System
theme, System/English/Simplified Chinese/Japanese interface language, preview render scale, and
preview MSAA. Project settings stay outside the undo stack as in Qt,
while scene creation retains Qt's separate add and settings-update undo steps.

The system-settings window exposes Qt's nine categories and persists the same scalar, plugin-path,
and 34-shortcut keys. Plugin and shortcut rows are stable host-owned models; applying a draft also
refreshes timeline zoom bounds and the live shortcut resolver without rebuilding the window.
`previewRenderScale` renders into a smaller physical wgpu target while keeping the scene's logical
coordinates and camera, and `previewMsaaSamples` selects a real 1x/2x/4x/8x multisample compositor
with resolve for fixed-function and complex blend paths, including nested scenes. Export deliberately
remains single-sampled and full-resolution, matching the Qt separation between preview quality and
output quality.

English is the source language for Slint `@tr` strings and the fallback for unsupported locales.
The System language preference uses the operating system's preferred locale, maps Chinese locales
to Simplified Chinese and Japanese locales to Japanese, and otherwise selects English. The bundled
`zh_CN` and `ja_JP` gettext catalogs update live after Apply or OK. Built-in effect and object
metadata remains owned by the shared Qt JSON definitions; the Slint build extracts the matching
English and Simplified Chinese translations from the existing Qt TS catalogs and translates that
metadata only when projecting it into UI models.

`bakeStrategy` and `onDemandPrefetchFrames` remain persisted for Qt settings compatibility, but they
do not pretend to control a copied Qt `BakeController`: the Rust `SceneRenderPlan` caches typed clip
and effect metadata when the document revision changes and evaluates the requested frame directly.
There is currently no separate per-frame bake cache for those two controls to tune.

Catalog inventory tests read the shipped Qt metadata and require all 44 built-in effects and all 17
built-in objects to reach an explicit production preview route. Transform, clipping, and blend-layer
handling are asserted separately because they belong to geometry/crop/compositor planning rather
than the visual-effect pass.

Missing-media handling follows that boundary as well. `freya-nle-app` resolves project-relative media,
validates type-safe replacements, and commits relinks through the normal timeline command stack so
dirty state, revision tracking, Undo, and Redo remain consistent. Slint only presents the conditional
File-menu entry, missing-media status, manager, and filtered native replacement chooser.

Window placement is stored under Qt's existing `windowGeometry_*` keys. The Slint host restores and
persists logical position, logical size, and maximized state for each migrated editor window without
letting unopened hidden windows erase an older saved geometry.

Custom timeline and editor surfaces declare accessibility roles, names, descriptions, selection or
value state, and supported actions explicitly. Standard Slint widgets keep their native semantics,
and form controls receive labels instead of relying on adjacent visual text. Native VoiceOver
traversal remains part of the deferred unlocked-desktop suite.

Domain, application, production preview, and export code live in GUI-neutral crates. This workspace
has no egui frontend; the retained Qt implementation and the behavior tests provide the comparison
baseline for Slint acceptance.

The export window keeps the Qt draft and close workflow: it refreshes available codecs when opened,
uses project settings and the active scene range, pauses playback before rendering, rejects project
tab changes during a job, reports frame progress and ETA, confirms cancellation, and removes partial
video or image-sequence output after cancellation. Slint imports the same shared wgpu device used by
the preview path, so exported frames are composed by the production renderer before CPU readback and
encoding.

## UI rules

### Source layout

`src/main.rs` creates the windows, shared runtimes, and event-loop timers. Host behavior is split
into modules with explicit imports:

| Module | Responsibility |
| --- | --- |
| `callbacks` | Editor callback registration and file-drop integration |
| `lifecycle`, `dialogs` | Window ownership, save/close/recovery flows, native pickers, and geometry |
| `playback`, `gpu`, `export` | Preview/audio/waveform coordination, shared GPU setup, and export UI |
| `object_settings`, `easing` | Metadata projection and object/easing editor commands |
| `settings`, `shortcuts`, `localization` | Settings drafts, input routing, and language selection |
| `packages`, `projection` | Background package/plugin discovery and stable shared UI models |
| `tests` | Existing frontend behavior regressions |

`ui/app.slint` is the Rust-facing export surface. `theme.slint` and `models.slint` own shared globals
and data types. Window files own their layouts; `widgets.slint`, `timeline-items.slint`, and
`object-controls.slint` hold reusable controls. Keep component names and the root exports stable
when moving presentation code so callbacks and translation contexts retain their identities.

### Ownership and behavior

- Qt/QML is the behavioral source of truth until a separate product change is approved.
- Keep preview, timeline, object settings, launcher, settings, recovery, package manager, and export
  flows as separately owned native windows where Qt does.
- Do not move business rules into `.slint` files. Slint forwards intent; `freya-nle-app` commits state.
- Keep long-lived `VecModel` instances and update rows in place. Replacing a model during a pointer
  callback can destroy the delegate that owns an active drag or native context menu.
- Prefer the active Slint `Palette` and `StyleMetrics` for custom surfaces. Add a semantic token to
  `AppTheme` when an editor-specific color is needed instead of scattering platform-independent
  literals through the UI.
- Let tabs size to their content, use standard widgets for ordinary form controls, and reserve
  custom pointer surfaces for editor interactions that standard widgets cannot express.
- Assign explicit geometry to small overlay hit regions such as resize handles; implicit placement
  is not accepted for timeline input surfaces.
- Treat [`../../audits/2026-10-23-slint-ui-parity-gate.md`](../../audits/2026-10-23-slint-ui-parity-gate.md) as the acceptance gate. Compile success and a visually plausible mockup do
  not establish parity.

## Running

### GPU requirement

Freya NLE requires a **GPU-backed wgpu adapter**. It does not ship a software or CPU
renderer, matching how Premiere documents GPU requirements and removed its
"Mercury Playback Engine Software Only" option, and how DaVinci Resolve requires
a discrete GPU with sufficient VRAM.

A CPU adapter is not merely slow. On a host whose only adapter is
`Microsoft Basic Render Driver` (WARP, `DeviceType::Cpu`), the compositor's
41-case effect shader cannot be compiled by the FXC path that WARP uses, so
pipeline creation dies with an access violation (`0xC0000005`) instead of
returning an error. The application therefore checks for a GPU-backed adapter
before it initializes wgpu and exits with an actionable message naming the
adapters it found, rather than crashing. See
[`../../no-gpu-probes/README.md`](../../no-gpu-probes/README.md) for the
measured evidence.

### Commands

From the `rust` directory:

```sh
cargo run -p freya-nle
```

From the repository root, `BUILD.py` builds and packages Slint by default. On Windows with
Visual Studio 2022 and the configured dependencies:

```sh
python BUILD.py --msvc
```

`--frontend qt` selects the retained Qt/CMake development build. See
[`../../CONTRIBUTING.md`](../../CONTRIBUTING.md#removing-qt) for the criteria that retire it.

Runtime effects, objects, plugins, effect packages, and repository metadata
are resolved from the shared resource directories during development.

The current validation project exercises the two-window Slint/wgpu integration, real timeline
models, and the production preview planning/decoding/compositing bridge:

```sh
cargo run --release -p freya-nle-slint -- --validate-gpu --frames 120
```

For Slint MCP interaction checks, build with debug metadata and enable the feature only on the
command line:

```sh
SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315 \
  cargo run -p freya-nle --features slint/mcp -- \
  --validate-gpu --frames 100000
```

## Validation

Before a migration checkpoint:

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run --release -p freya-nle-slint -- --validate-gpu --frames 120
git diff --check
```

Performance samples must record the active display and refresh rate. Samples taken on different
display configurations are not directly comparable. Functional MCP checks may still be compared
when logical window sizes and scale factors are recorded.

### wgpu 29 baseline

Slint 1.17 exposes `unstable-wgpu-29` and has no wgpu 30 equivalent, so the production frontend
shares devices and textures on 29 while the renderer, preview, and export crates also build on
their own against 30.0.1. An isolated probe ran 297 workspace tests plus an offscreen Metal render
(640x360 readback, checksum `61979074`) on wgpu 30, with strict Clippy clean outside the Slint
binary. Moving the whole stack over needs `freya-nle-render`, `freya-nle-preview`, and
`freya-nle-export` to change together with Slint's feature and bridge calls, because
`BufferSlice::get_mapped_range()` now returns a `Result` and `RequestAdapterOptions` gains
`apply_limit_buckets`. A hand-written cross-version bridge would add duplicate-device and
texture-copy complexity for no gain, so 29 stays the production baseline until Slint ships
matching public types.
