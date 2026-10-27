<p align="center">
  <img src="./assets/splash.svg" width="256">
</p>

<p align="center"><b>A video editor with AviUtl's editing model, rebuilt on Rust, Slint, and wgpu</b></p>

<p align="center">
  <a href="https://github.com/GT-610/freya-nle">GitHub</a>
  / <a href="https://github.com/GT-610/freya-nle/releases">Releases</a>
</p>

<p align="center">
  <b>English</b> / <a href="./i18n/README/README.ja.md">日本語</a> / <a href="./i18n/README/README.zh_CN.md">简体中文</a>
</p>

> [!NOTE]
> Freya NLE was called AviQtl-Plus until 0.8. Old links redirect, `.aviqtl`
> projects and `.aviqtlfrag` fragments still open, and the CHANGELOG lists what
> each identifier became.

> [!IMPORTANT]
> Freya NLE is a fork of [taisho-guy/NeoUtl](https://codeberg.org/taisho-guy/NeoUtl) that continues the **Qt Quick + QRhi + ECS** development path after the upstream project was rebuilt with **Rust + Slint + wgpu**. The frontend has since migrated the same way, and the Qt frontend was retired in 0.8: **Rust + Slint + wgpu is the only build**. See [About the project](https://aviqtl.gt610.dpdns.org/guide/start-here/about) for the full story.

## What is Freya NLE?

<img src="./assets/screenshot.webp" alt="Freya NLE screenshot">

A video editor built around the operability of **AviUtl 1.10** and **ExEdit 0.92**, with **performance that goes past what AviUtl can do**.

See [AviUtl Operability Targets](https://aviqtl.gt610.dpdns.org/developer/operability-targets) for the editing-model compatibility goals that guide development.

### Key Features

- UI closely resembling AviUtl
- **Fragments (.freyafrag)** save a block of clips and insert it elsewhere, losslessly; `.aviqtlfrag` files still open
- **AviUtl ExEdit object files (.exo)** can be imported, bringing existing AviUtl material into a project
- **Fast and powerful GPU-accelerated effects**
- Support for **audio effects** such as VST3 and LV2
- **Lua plugin system** with package management, declarative parameters, and permission control
- Cross-platform: **Linux**, **Windows**, **macOS**

> [!NOTE]
> Freya NLE requires a **GPU-backed graphics adapter**. There is no CPU or software rendering mode: preview compositing, export, and hardware codec acceleration all run on the GPU. Hosts that expose only a software adapter such as the Microsoft Basic Render Driver (WARP) cannot run Freya NLE and are reported as unsupported at startup instead of failing later.

## Documentation

- [Installation](https://aviqtl.gt610.dpdns.org/guide/start-here/installation)
- [User guide](https://aviqtl.gt610.dpdns.org/guide/start-here/)
- [Build from source](https://aviqtl.gt610.dpdns.org/developer/building)
- [Developer documentation](https://aviqtl.gt610.dpdns.org/developer/)
- [Frequently asked questions](https://aviqtl.gt610.dpdns.org/guide/help-and-reference/faq)
- [About the project](https://aviqtl.gt610.dpdns.org/guide/start-here/about)

## Effect Packages

Freya NLE features a modular effect system. The `effect-packages/` directory contains ready-to-use effect packs that demonstrate the extension system:

| Package | Type | Contents |
|---------|------|----------|
| [weather-objects](effect-packages/weather-objects/) | Object | Rain, snow animations |
| [color-effects](effect-packages/color-effects/) | Effect | Color tint effect |

These packages serve as both useful additions and developer references for creating custom effects. See [effect-packages/README.md](effect-packages/README.md) for details.

## Related Links

Freya NLE stands on the shoulders of many wonderful projects.

| Project | License | Role |
| :--- | :--- | :--- |
| AviUtl | Non-free | Respected origin |
| AviQtl | AGPLv3 | Original Qt Quick project; `aviqtl` branch of the upstream |
| NeoUtl | AGPLv3 | New Rust + Slint + wgpu version by the original author |
| Freya NLE | AGPLv3 | This project — Rust + Slint + wgpu, grown out of a Qt Quick + QRhi + ECS codebase |
| Carla | GPLv2+ | Audio effect host (VST3/LV2 etc.) |
| FFmpeg | GPLv2+ | Video/audio decoding & encoding |
| luna-core | MIT OR Apache-2.0 | Lua 5.1 virtual machine for plugins |
| Slint | GPLv3 | UI framework |
| Zrythm | AGPLv3 | Reference for audio plugin implementation |
| Remix Icon | Remix Icon License | Symbol icons |

## License

Freya NLE is released under the [GNU Affero General Public License](https://www.gnu.org/licenses/agpl-3.0.txt).

[Remix Icon](https://remixicon.com/) used within Freya NLE is provided under the [Remix Icon License](https://raw.githubusercontent.com/Remix-Design/RemixIcon/refs/heads/master/License).
