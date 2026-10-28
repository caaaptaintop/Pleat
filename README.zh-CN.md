# Pleat

<p align="center">
  <img src="assets/readme/pleat-hero.png" width="100%" alt="Pleat — 基于 Kitter 的 Agent 技能管理工具">
</p>

<p align="center"><a href="README.md">English</a> · <strong>简体中文</strong></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-444444" alt="Apache-2.0"></a>
  <img src="https://img.shields.io/badge/UI-Rust%20%2B%20GPUI-6C5CE7" alt="Rust 与 GPUI">
  <img src="https://img.shields.io/badge/CLI-kitter-444444" alt="兼容 kitter CLI">
</p>

Pleat 是基于 [Kitter](https://github.com/what1f/kitter) 开发的技能管理工具，提供桌面界面和 `kitter` CLI。用它统一维护技能文件，查看各项目装了什么、哪些 Agent 能发现这些技能。

## 在 Kitter 基础上扩展了什么

Kitter 已提供技能导入、统一技能库、按 Agent 安装和上下文估算。Pleat 从 `v0.1.10` 继续开发，主要补充了以下功能：

- 自动读取 Codex 本地项目和 Proma 原生工作区，合并指向同一目录的项目。项目列表支持归档、隐藏与恢复。
- 按 Proma 工作区安装和同步技能副本，识别托管来源、外部链接及本机变体。用户技能可以安装、卸载；可信内置预设可以启用、停用。
- 一次为多个项目和 Agent 管理技能。执行前查看目标路径与影响清单，执行后逐项报告失败。
- 按来源整理项目技能，补充名称和描述搜索、Agent 安装识别；管理操作保留当前选中的项目与工作区。
- 更新前比较来源基线、本机文件和新版来源，保留未冲突的本机修改，遇到冲突时停止替换。技能内的 `local-data/` 和本机适配说明会保留。
- 迁移技能库、切换已有库，或将旧 Kitter 数据迁入 Pleat 目录。移动后保留旧路径兼容链接，已有安装继续访问同一份文件。

## 界面与操作

### 查看技能装在哪里

从本地文件夹、GitHub、skills.sh 兼容来源或 Claude 插件来源添加技能，也可以纳管已有安装。选中技能后，右侧列出使用它的项目和 Agent。

![技能库与安装详情](assets/readme/pleat-library.png)

### 查看项目能用哪些技能

项目页面区分项目安装、全局继承和插件，并标明来源。Codex 和 Proma 可以关联同一个项目，各自保留安装位置和工作区。

![项目、Agent 与技能来源](assets/readme/pleat-project.png)

技能数和 token 来自目录扫描与静态估算，不代表 Agent 本轮实际加载的内容或消耗。

### 管理多个项目

选择技能、Agent 和目标项目，再预览操作范围。普通 Agent 使用受管链接；Proma 使用工作区副本，来源更新后需同步副本。

![跨项目技能管理面板](assets/readme/pleat-management.png)

## 获取 Pleat

### macOS 安装包

1. 从 [GitHub Release v0.1.12](https://github.com/caaaptaintop/Pleat/releases/tag/v0.1.12) 下载 `Pleat-0.1.12-macos-arm64.dmg`，适用于 Apple Silicon Mac。
2. 打开 DMG，将 **Pleat** 拖入“应用程序”。
3. 从“应用程序”打开 Pleat。

安装包采用 ad hoc 签名，未经 Apple 公证。

本次仅提供 Apple Silicon macOS 安装包。Windows 核心 CI 尚未通过，不作为已验证平台。

#### 首次打开被拦截时

确认安装包来自本仓库的 Releases 后，按以下步骤操作：

1. 在“应用程序”中双击 **Pleat**。若提示无法验证开发者或无法检查是否包含恶意软件，点“完成”或“好”关闭提示。
2. 点屏幕左上角的 ** → 系统设置 → 隐私与安全性**。
3. 向下滚动到“安全性”，找到 Pleat 被阻止打开的提示，点 **“仍要打开”**；部分 macOS 版本需先点“打开”，再点“仍要打开”。
4. 若系统要求验证，输入 **Mac 登录密码** 完成确认；后续提示中点 **“打开”** 或 **“好”**。

之后可直接从“应用程序”打开 Pleat。若看不到“仍要打开”，先再次双击 Pleat 触发提示，再回到“隐私与安全性”查看。

### 从源码构建

需要 Rust 和 Git，桌面平台依赖见 [CI 配置](.github/workflows/ci.yml)。

```sh
git clone https://github.com/caaaptaintop/Pleat.git
cd Pleat

# 构建 CLI
cargo build --release --locked --bin kitter

# 启动桌面应用
cargo run --release --locked --features desktop --bin kitter-desktop
```

macOS 可用 `just` 和系统命令行工具生成 App 与 DMG：

```sh
just package
# target/release/Pleat.app
# target/release/Pleat-<version>-macos-<arch>.dmg
```

## CLI

命令名继续使用 `kitter`。将构建好的 `target/release/kitter` 加入 `PATH` 后，可直接盘点技能和项目：

```sh
kitter list
kitter projects --json
kitter project /path/to/project
```

安装、更新时，将示例中的 `skill-name` 和项目路径替换为实际值：

```sh
kitter install skill-name --project /path/to/project --target codex
kitter update skill-name
```

共享安装使用 `--target universal`，目录为 `.agents/skills`。Proma 操作针对已登记的原生工作区：

```sh
kitter proma workspaces --json
kitter proma status --workspace workspace-slug --json
```

macOS 上可先预览全部登记工作区的 Proma 内置技能更新，检查保存的计划后再应用：

```sh
kitter proma builtin-follow > proma-builtin-plan.json
kitter proma builtin-follow --apply proma-builtin-plan.json
```

首次应用计划后，Pleat 运行期间会继续检查内置内容；关闭期间的更新在下次启动补齐。各副本保留原启用／停用位置，未知或本地修改的副本保留待处理，不补装缺失技能。

更多参数见 `kitter --help`。内置 Agent 技能入口仍为 [`kitter`](resources/skills/kitter)。

## 数据与兼容

新安装的默认技能库：

| 平台 | 位置 |
| --- | --- |
| macOS | `~/Library/Application Support/Pleat/skills` |
| Windows | `%LOCALAPPDATA%\Pleat\skills` |
| Linux | `$XDG_DATA_HOME/Pleat/skills` 或 `~/.local/share/Pleat/skills` |

已有 Kitter 数据保持原位，直到用户在 **设置 → 高级存储设置** 中明确迁移。迁移保留旧路径兼容链接；跨磁盘移动、目标冲突或目录身份不符时会停止。

本机适配可放在技能内的 `local-data/ADAPTATION.md`，由宿主指令加载。`local-data/` 随技能实体保留，不会复制进 Proma 的技能包。

<details>
<summary>自定义数据位置</summary>

```sh
kitter library
# 切换到已有的非空技能库；移入空目录请用桌面迁移功能
kitter library --set /absolute/path/to/existing/library
```

`KITTER_HOME` 可指定应用数据根。未完成的元数据事务会阻止再次开库，需按保存的恢复材料处理。

</details>

## 平台状态

| 平台 | 验证情况 |
| --- | --- |
| macOS ARM64 | 本机桌面、CLI、打包及隔离示例操作已验证。 |
| macOS Intel | CI 核心测试与桌面编译检查通过。 |
| Windows x64 | 仅部分基础功能；原生适配受管写入尚未支持，不作为已验证发布平台。 |
| Linux x64 | CI 核心测试与桌面编译检查通过。 |

Windows、Linux 和 Intel Mac 的原生桌面体验仍需验证。各提交的检查结果见 [GitHub Actions](https://github.com/caaaptaintop/Pleat/actions)。

## 上游与许可

基础实现来自 [what1f/kitter](https://github.com/what1f/kitter)，本仓库的修改保存在提交历史中。问题和建议请提交到 [Pleat Issues](https://github.com/caaaptaintop/Pleat/issues)。

沿用 [Apache License 2.0](LICENSE)，保留上游版权和许可证声明。第三方素材见 [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md)。
