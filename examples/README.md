# Freya NLE Examples

Self-contained examples demonstrating each supported extension type.

## Index

| Example | Type | Demonstrates |
|---------|------|-------------|
| [hello_mod](hello_mod/) | Lua plugin | Lifecycle hooks, APIs, permissions |

Effect and object packages are demonstrated by the validated
[`effect-packages/`](../effect-packages/) directory instead: they are native
JSON + WGSL packages, the only package runtime the Slint edition supports.

## Quick Start

### Lua Plugin

1. Copy `hello_mod/` to `<data>/plugins/hello_mod/`
2. Restart Freya NLE
3. Open the Lua console and type `hello()`

## Extension Types

| Type | Files | Runtime |
|------|-------|---------|
| Effect | `manifest.json` + JSON metadata + `.wgsl` | `aviqtl-wgsl-v1` (wgpu) |
| Object | `manifest.json` + JSON metadata + `.wgsl` | `aviqtl-wgsl-v1` (wgpu) |
| Mod | `manifest.lua` + `main.lua` | Lua (sandboxed) |

QML is not a package runtime: a package whose metadata names a QML component is
rejected at installation time.

## Documentation

- [Effects and objects](https://aviqtl.gt610.dpdns.org/developer/effects) — extension model and JSON metadata
- [Plugin development](https://aviqtl.gt610.dpdns.org/developer/plugins) — Lua automation and permissions
