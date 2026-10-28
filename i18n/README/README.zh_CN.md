<p align="center">
  <img src="../../assets/splash.svg" width="256">
</p>

<p align="center"><b>沿用 AviUtl 编辑方式、基于 Rust、Slint 与 wgpu 重建的视频编辑软件</b></p>

<p align="center">
  <a href="https://github.com/GT-610/freya-nle">GitHub</a>
  / <a href="https://github.com/GT-610/freya-nle/releases">发行版</a>
</p>

<p align="center">
  <a href="../../README.md">English</a> / <a href="./README.ja.md">日本語</a> / <b>简体中文</b>
</p>

> [!NOTE]
> Freya NLE 在 0.8 之前叫 AviQtl-Plus。旧链接会重定向，`.aviqtl` 项目与 `.aviqtlfrag` 片段仍然可以打开，每一处标识符改动都记在 CHANGELOG 里。

> [!IMPORTANT]
> Freya NLE 是 [TeamNeoUtl/NeoUtl](https://codeberg.org/TeamNeoUtl/NeoUtl) 的 fork，最初沿用 **Qt Quick + QRhi + ECS** 开发路线。上游 NeoUtl 后来改用 **Rust + egui + wgpu** 重建；Freya NLE 则在 0.8 中移除了 Qt 前端，并迁移到 **Rust + Slint + wgpu**：**现在只有这一种构建**。关于完整来龙去脉，请参见[关于项目](https://aviqtl.gt610.dpdns.org/zh-CN/guide/start-here/about)。

## 什么是 [Freya NLE](https://github.com/GT-610/freya-nle)？

<img src="../../assets/screenshot.webp" alt="Freya NLE 界面截图">

一个旨在继承 **AviUtl 1.10** 和 **ExEdit 0.92** 的操作体验，同时拥有**超越 AviUtl 性能**的视频编辑软件项目。

指导开发的编辑模型兼容目标见 [AviUtl 操作体验目标](https://aviqtl.gt610.dpdns.org/zh-CN/developer/operability-targets)。

### 主要特性

- 与 AviUtl 极为相似的用户界面
- 可以导入 **AviUtl 扩展编辑物件文件（.exo）**
- 使用 GPU 实现的**高速强大特效**
- 支持 VST3、LV2 等**音频特效**
- **Lua 插件系统**，支持包管理、声明式参数和权限控制
- 跨平台：**Linux**、**Windows**、**macOS**

## 文档

- [安装](https://aviqtl.gt610.dpdns.org/zh-CN/guide/start-here/installation)
- [用户手册](https://aviqtl.gt610.dpdns.org/zh-CN/guide/start-here/)
- [从源码构建](https://aviqtl.gt610.dpdns.org/zh-CN/developer/building)
- [开发者文档](https://aviqtl.gt610.dpdns.org/zh-CN/developer/)
- [常见问题](https://aviqtl.gt610.dpdns.org/zh-CN/guide/help-and-reference/faq)
- [关于项目](https://aviqtl.gt610.dpdns.org/zh-CN/guide/start-here/about)

## 效果包

Freya NLE 拥有模块化效果系统。`effect-packages/` 目录包含即用型效果包，展示扩展系统：

| 包 | 类型 | 内容 |
|---|------|------|
| [weather-objects](../../effect-packages/weather-objects/) | 对象 | 雨、雪动画 |

这些效果包既是实用的附加组件，也是开发者创建自定义效果的参考。详见 [effect-packages/README.md](../../effect-packages/README.md)。

## 相关链接

Freya NLE 站在众多优秀项目的肩膀上。

| 项目 | 许可证 | 角色 |
| :--- | :--- | :--- |
| AviUtl | 非自由 | 致敬的原型 |
| AviQtl | AGPLv3 | 原 Qt Quick 版本；上游的 `aviqtl` 分支 |
| NeoUtl | AGPLv3 | 原作者的新 Rust + egui + wgpu 版本 |
| Freya NLE | AGPLv3 | 本项目 — 脱胎于 Qt Quick + QRhi + ECS 的 Rust + Slint + wgpu 版本 |
| Carla | GPLv2+ | 音频特效宿主（VST3/LV2等） |
| FFmpeg | GPLv2+ | 视频/音频编解码 |
| luna-core | MIT OR Apache-2.0 | 供插件使用的 Lua 5.1 虚拟机 |
| Slint | GPLv3 | UI 框架 |
| Zrythm | AGPLv3 | 音频插件实现参考 |
| Remix Icon | Remix Icon License | 符号图标 |

## 许可证

Freya NLE 基于 [GNU Affero General Public License](https://www.gnu.org/licenses/agpl-3.0.txt) 发布。

Freya NLE 中使用的 [Remix Icon](https://remixicon.com/) 遵循 [Remix Icon License](https://raw.githubusercontent.com/Remix-Design/RemixIcon/refs/heads/master/License)。
