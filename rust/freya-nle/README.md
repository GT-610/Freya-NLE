# Freya NLE frontend

This crate is the Rust desktop frontend for Freya NLE. It preserves the existing layer/object,
start/end parameter, independent-window, project ownership, and save-confirmation workflows.
The September 2026 workflow improvements deliberately extend the Qt baseline with AviUtl-style
numeric scrubbing and layer selection, standard Slint menu shortcuts, compact object settings,
and optional workspace layouts. See [the implementation status](../../audits/2026-09-24-slint-workflow-implementation.md)
for the supported behavior, validation, and deferred architecture work.

Standard widgets use the active platform palette and metrics. Timeline clips, waveforms,
keyframes, drag grips, and splitter hit regions remain custom Slint editing surfaces.

Editor chrome — panel headers, window footers, ruler timecode, and layer rows — uses the
explicit tokens in `ui/theme.slint` instead of per-window pixel values. They follow AviUtl2's
`style.conf` tables with light and dark values; `ui/chrome.slint` holds the shared `PanelHeader`,
`FooterBar`, and `TimeFormat`. The rationale and the remaining batches are in
[the AviUtl2 UI analysis](../../audits/2026-11-05-aviutl2-ui-analysis.zh-CN.md), and
`tools/ui-snapshot` renders the windows offscreen for design review.

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
metadata is owned by the `catalog/` definitions; the build extracts the matching English and
Simplified Chinese translations from `i18n/effect-metadata.json` and translates that metadata only
when projecting it into UI models.

`bakeStrategy` and `onDemandPrefetchFrames` remain persisted for compatibility with settings files
written by older versions, but they do not pretend to control a copied `BakeController`: the Rust
`SceneRenderPlan` caches typed clip
and effect metadata when the document revision changes and evaluates the requested frame directly.
There is currently no separate per-frame bake cache for those two controls to tune.

Catalog inventory tests read `catalog/effects` and `catalog/objects` and require all 44 built-in
effects and all 17 built-in objects to reach an explicit production preview route. Transform, clipping, and blend-layer
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

Domain, application, production preview, and export code live in GUI-neutral crates. The parity
gate's behavior tests are the comparison baseline for Slint acceptance.

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
| `logging`, `log_window` | The `log`/`env_logger` pipeline, panic capture, the file writer, and the Tools -> Log window |
| `batch`, `gpu` | Batch export driving and the adapter/device gate |
| `packages`, `projection` | Background package/plugin discovery and stable shared UI models |
| `tests` | Existing frontend behavior regressions |

`ui/app.slint` is the Rust-facing export surface. `theme.slint` and `models.slint` own shared globals
and data types. Window files own their layouts; `widgets.slint`, `timeline-items.slint`, and
`object-controls.slint` hold reusable controls. Keep component names and the root exports stable
when moving presentation code so callbacks and translation contexts retain their identities.

### Ownership and behavior

- The [Qt UI parity gate](../../audits/2026-10-23-slint-ui-parity-gate.md) is the behavioral
  reference; the Qt frontend itself was retired on 2026-10-26
  ([audit](../../audits/2026-10-26-qt-retirement.md)).
- Keep preview, timeline, object settings, launcher, settings, recovery, package manager, and export
  flows as separately owned native windows, as the reference did.
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
- Treat the [parity gate](../../audits/2026-10-23-slint-ui-parity-gate.md) as the acceptance
  record. Compile success and a visually plausible mockup do not establish parity.

## Running

### GPU requirement

Freya NLE requires a **GPU-backed wgpu adapter** and ships no CPU or software
renderer, the same position Premiere takes after removing its
"Mercury Playback Engine Software Only" option.

A CPU adapter is not merely slow: on a host whose only adapter is
`Microsoft Basic Render Driver` (WARP, `DeviceType::Cpu`), compiling the
compositor shader kills the process instead of returning an error, so `gpu.rs`
checks for a GPU-backed adapter first and exits naming the adapters it found.
[`../../no-gpu-probes/README.md`](../../no-gpu-probes/README.md) is the record of
what was measured, layer by layer, and worth reading before changing that gate.

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

Runtime effects, objects, plugins, effect packages, and repository metadata
are resolved from the shared resource directories during development.

The validation project exercises the Slint/wgpu integration across the editor windows, real
timeline models, and the production preview planning/decoding/compositing bridge:

```sh
cargo run --release -p freya-nle -- --validate-gpu --frames 120
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
cargo run --release -p freya-nle -- --validate-gpu --frames 120
git diff --check
```

Performance samples must record the active display and refresh rate. Samples taken on different
display configurations are not directly comparable. Functional MCP checks may still be compared
when logical window sizes and scale factors are recorded.

### wgpu 29 baseline

One wgpu version, 29.0.x, serves the whole workspace: Slint 1.17 exposes `unstable-wgpu-29` and has
no wgpu 30 equivalent, so the frontend shares devices and textures with `freya-nle-render`,
`freya-nle-preview`, and `freya-nle-export` on the same types. `rust/Cargo.lock` carries a single
`wgpu` entry; the per-crate `29.0.1` requirement resolves to it.

A 2026-08 probe ran the workspace against wgpu 30.0.1 out of tree and reported a clean offscreen
Metal render (640x360 readback, checksum `61979074`). Moving the stack over is one change, not a
gradual one: `BufferSlice::get_mapped_range()` returns a `Result` in 30 and
`RequestAdapterOptions` gains `apply_limit_buckets`, so the render, preview, and export crates move
together with Slint's feature and bridge calls. A hand-written cross-version bridge would duplicate
devices and copy textures for no gain, so 29 stays until Slint ships matching public types.
