# Pleat

<p align="center">
  <img src="assets/readme/pleat-hero.png" width="100%" alt="Pleat — 基于 Kitter 的 Agent 技能管理应用">
</p>

<p align="center"><a href="README.md">English</a> · <strong>简体中文</strong></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-444444" alt="Apache-2.0"></a>
  <img src="https://img.shields.io/badge/UI-Rust%20%2B%20GPUI-6C5CE7" alt="Rust 与 GPUI">
  <img src="https://img.shields.io/badge/CLI-kitter-444444" alt="兼容 kitter CLI">
</p>

**集中维护技能，为每个项目选择合适的组合。**

Pleat 是基于 [Kitter](https://github.com/what1f/kitter) 的桌面应用与 CLI，使用 Rust 和 GPUI 构建。它把技能来源、项目安装、Agent 可发现的内容和预计上下文开销放到同一处管理。

本项目由 [caaaptaintop/Pleat](https://github.com/caaaptaintop/Pleat) 维护。Kitter 的技能库和多 Agent 管理能力是基础；Pleat 在其上补充原生项目发现、Proma 工作区管理、本机修改保护、跨项目批量操作和存储迁移。

> 图片说明：顶部主视觉由 ImageGen 基于既有 Pleat 图标制作，是品牌效果图；下方为真实 macOS 应用截图，使用隔离的示例数据，界面中的“示例”项目不代表真实用户资料。

## 基于 Kitter 做了哪些改动

本 fork 从 Kitter `v0.1.10`（`e953940`）继续开发。以下是本仓库的主要增量；上游的技能导入、统一技能库、按项目安装和 Agent 发现能力继续保留。

| 方向 | Pleat 的更新 |
| --- | --- |
| 品牌与入口 | Pleat 名称、折面图标、桌面窗口、关于页面和 macOS 安装包；保留 `kitter` CLI 与应用技术标识。 |
| 原生项目发现 | 读取 Codex 本地项目和 Proma 原生工作区；合并指向同一真实目录的项目，保留工作区归属，支持归档、隐藏与恢复视图。 |
| Proma 安装管理 | 按原生工作区管理用户技能副本；区分当前托管副本、外部链接、应用集成及本机变体。用户技能安装/卸载，可信内置预设启用/停用。 |
| 跨项目操作 | 在技能库、项目和技能安装详情中管理多技能、多 Agent、多项目；先查看影响清单，执行后列出失败项。 |
| 上下文与搜索 | 区分已安装和未安装 Agent；名称搜索、可选描述匹配、来源分类及自动/手动技能与 token 估算，操作沿用当前项目与工作区。 |
| 更新与本机适配 | 按来源基线、本机文件和新版来源做三方比较；保留未冲突的本机改动，冲突时停止。保护 `local-data/`，支持独立的 `local-data/ADAPTATION.md`。 |
| 存储与兼容 | 新装使用 Pleat 数据目录；高级设置支持迁移当前库、切换已有库和迁移旧 Kitter 数据根。移动保留旧路径兼容链接，校验目录身份与事务状态。 |

## 看看实际界面

### 一个技能库，查看每处安装

从本地文件夹、GitHub、skills.sh 兼容来源或 Claude 插件来源添加技能，也可以检查并纳管已有安装。分组组织技能，选中后查看来源、内容和项目安装情况。

![Pleat 技能库及安装详情，隔离示例数据](assets/readme/pleat-library.png)

### 按项目和工作区管理

项目页面区分直接安装、继承来源、全局技能和插件。Codex 与 Proma 可以关联同一个项目，同时保留各自的安装位置和工作区。

![Pleat 项目与 Agent 技能概览，隔离示例数据](assets/readme/pleat-project.png)

上下文数字来自目录扫描与静态估算，用于比较已发现的技能；它们不证明 Agent 本轮实际加载了哪些技能，也不代表实际 token 消耗。Proma 副本的安装状态和运行时加载分开判断。

### 批量操作先看影响范围

按项目选择技能、Agent 和 Proma 原生工作区，确认目标路径与操作后执行。普通 Agent 使用受管链接；Proma 使用带来源登记的工作区副本，更新需要显式同步。

![Pleat 多项目技能管理面板，隔离示例数据](assets/readme/pleat-management.png)

## 构建与使用

**本仓库尚未发布 Pleat GitHub Release。** 当前可从源码构建；上游 [Kitter Releases](https://github.com/what1f/kitter/releases) 提供的是 Kitter，不是本 fork 的 Pleat 安装包。

```sh
git clone https://github.com/caaaptaintop/Pleat.git
cd Pleat

# 核心与 CLI
cargo build --release --locked --bin kitter

# 启动桌面应用
cargo run --release --locked --features desktop --bin kitter-desktop
```

构建需要 Rust 和 Git；桌面平台依赖见 [CI 配置](.github/workflows/ci.yml)。macOS 打包另需 `just` 和系统命令行工具：

```sh
just package
# target/release/Pleat.app
# target/release/Pleat-<version>-macos-<arch>.dmg
```

本机 macOS 打包脚本使用 ad hoc 签名，尚无 Apple Developer ID 签名与公证。

CLI 命令继续使用 `kitter`。下例假定已将构建好的 `target/release/kitter` 加入 `PATH`：

```sh
kitter list
kitter projects --json
kitter add npx https://github.com/owner/repository --skill skill-a
kitter install skill-a --project /path/to/project --target codex
kitter project /path/to/project --agent codex
kitter check
kitter update skill-a
```

共享安装使用 `--target universal`，目标是 `.agents/skills`。Agent 专用目录按各 Agent 的规则处理。Proma 仅对已登记的原生工作区操作：

```sh
kitter proma workspaces --json
kitter proma status --workspace <workspace-slug> --json
```

更多参数使用 `kitter --help`、`kitter project --help` 和 `kitter proma --help` 查看。内置技能入口仍为 [`kitter`](resources/skills/kitter)。

## 数据位置与旧路径兼容

新安装的默认技能库：

| 平台 | 默认位置 |
| --- | --- |
| macOS | `~/Library/Application Support/Pleat/skills` |
| Windows | `%LOCALAPPDATA%\Pleat\skills` |
| Linux | `$XDG_DATA_HOME/Pleat/skills` 或 `~/.local/share/Pleat/skills` |

已有 Kitter 数据会继续使用原位置，直到用户明确执行迁移。通过 **设置 → 高级存储设置** 查看迁移计划、迁移当前库或使用已有库；移动后保留原路径兼容链接，让既有安装和本机环境继续访问同一份数据。

迁移拒绝跨磁盘移动、已有冲突数据、目录嵌套及身份不符的目标。未完成的元数据事务会阻止再次开库，需按保存的恢复材料处理，不自动覆盖。`local-data/` 留在原技能实体内，不作为公开技能代码复制到 Proma。

```sh
kitter library
# 切换到已有的非空技能库；移入空目录请使用桌面迁移功能
kitter library --set /absolute/path/to/existing/library
```

`KITTER_HOME` 仍可显式指定应用数据根。bundle ID `dev.kitter.app`、内部 `Kitter` 可执行文件、Cargo 包名、配置格式和兼容变量名继续保留；仓库和产品名称的变化不要求批量改写技能正文或宿主配置。

## 平台与验证范围

| 平台 | 已有证据 |
| --- | --- |
| macOS ARM64 | 本机桌面运行、CLI、打包与隔离示例操作已验证；本文截图来自此平台。 |
| macOS Intel | CI 核心测试与桌面编译检查通过。 |
| Windows x64 | CI 核心测试与桌面编译检查通过。 |
| Linux x64 | CI 核心测试与桌面编译检查通过。 |

Windows、Linux 及 Intel Mac 的 CI 结果不替代原生桌面体验验收。查看 [GitHub Actions](https://github.com/caaaptaintop/Pleat/actions) 获取具体提交的状态。

## 上游、贡献与许可

Pleat 基于 [what1f/kitter](https://github.com/what1f/kitter)；感谢 Kitter 上游作者与贡献者提供基础实现。Pleat 的增量通过本仓库提交历史保留，不代表由上游发布或背书。

本项目问题与改进请提交到 [Pleat Issues](https://github.com/caaaptaintop/Pleat/issues)。

沿用 [Apache License 2.0](LICENSE)，保留上游版权与许可证声明。字体、图标及其他第三方素材见 [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。[品牌母版与导出](assets/brand/README.md)及 [README 图片来源](assets/readme/README.md)另有说明。
