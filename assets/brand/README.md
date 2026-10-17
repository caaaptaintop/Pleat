# Pleat 品牌资源

本仓库 `caaaptaintop/Pleat` 基于 [Kitter](https://github.com/what1f/kitter)。产品显示名称为 **Pleat**，上游 Apache-2.0 许可证和贡献者署名保持。名称尚未核查商标或域名可用性。

## 设计来源与母版

- 原始参考：`Pleat-icon-reference-20261017.png`，1086×1448，保留用户原始 PNG；仅采用左侧 Pleat，右侧 Folio 未采用。
- 可编辑母版：`pleat-icon.svg`，1024×1024，无嵌入位图；`ivory-tile`、`back-plane`、`front-plane` 分别可编辑。
- 轮廓按参考重建：米白圆角底板、两片石墨折面及中央留白。没有加入文字、符号或额外装饰；渐变与轻微底板阴影来自参考的材质感。
- 图标外缘保留透明留距，底板占 824×824；不对参考图做截屏式裁切，不把对比图或标题排版装进图标。
- 安装器背景母版：`installer-background.svg`，米白底和拖入 Applications 的引导箭头；不再沿用上游动物插图。`preview.png` 展示同一图标的浅深背景和小尺寸效果。

## 可复现导出

```sh
# 需要 Python 3 与 rsvg-convert（librsvg），macOS ICNS 使用系统 iconutil。
just brand-icons
# 旧入口仍可使用，同样从 Pleat SVG 生成，不再读取旧 Kitter.icon。
just macos-icon
```

`scripts/export-brand-assets.py` 用同一 SVG 直接渲染各尺寸 PNG，标准库打包 PNG 格式 ICO；macOS 通过 iconutil 输出 ICNS。无需 Pillow 或新的 Rust 依赖。

| 生产资源 | 用途 |
| --- | --- |
| `assets/app-icon.png` | 1024×1024 RGBA，应用内关于区域与 Linux 桌面资源 |
| `assets/app-icon.ico` | Windows 图标，16 / 24 / 32 / 48 / 64 / 128 / 256 |
| `assets/macos/app-icon.png` | macOS 打包源，1024×1024 RGBA |
| `assets/macos/app-icon.icns` | macOS 原生多尺寸图标，16 至 1024 |

`just package` 生成 `target/release/Pleat.app` 和 `Pleat-<version>-macos-<arch>.dmg`，卷名为 `Pleat Installer`。旧 `assets/macos/Kitter.icon` 和 Swift 导出辅助保留作上游设计历史，已不参与生产导出。

## 兼容性边界

品牌资源覆盖产品显示、品牌图形和桌面安装包名称。bundle identifier 仍为 `dev.kitter.app`，内部可执行文件仍为 `Kitter`，Cargo 包与 CLI 命令仍为 `kitter`，签名仍沿用原脚本。新装默认使用 `Pleat` 数据目录；已有 Kitter 数据保留原位，直到用户明确迁移，迁移后保留旧路径兼容链接。技能、钥匙串、配置格式与既有来源/下载地址不因品牌改名而自动重写。

`LICENSE` 与 `THIRD_PARTY_LICENSES.md` 保持原文，打包时随应用分发；关于区域的署名简化为“基于 Kitter”，下方“项目主页”指向本项目 GitHub，不显示裸网址。本地构建不代表公开发布，也不代表三端运行验收已完成。
