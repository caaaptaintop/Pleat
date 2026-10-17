# Pleat

<p align="center">
  <img src="assets/readme/pleat-hero.png" width="100%" alt="Pleat — an Agent skill manager based on Kitter">
</p>

<p align="center"><strong>English</strong> · <a href="README.zh-CN.md">简体中文</a></p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-444444" alt="Apache-2.0"></a>
  <img src="https://img.shields.io/badge/UI-Rust%20%2B%20GPUI-6C5CE7" alt="Rust and GPUI">
  <img src="https://img.shields.io/badge/CLI-kitter-444444" alt="Compatible kitter CLI">
</p>

**Maintain skills in one place. Choose the right combination for each project.**

Pleat is a desktop app and CLI based on [Kitter](https://github.com/what1f/kitter), built with Rust and GPUI. It brings skill sources, project installations, Agent discovery and estimated context cost into one place.

Maintained at [caaaptaintop/Pleat](https://github.com/caaaptaintop/Pleat), this fork extends Kitter with native project discovery, Proma workspace management, protection for local edits, operations across projects and guarded storage migration.

> Image notes: the hero is an ImageGen brand illustration based on the existing Pleat icon. The screenshots below show the actual macOS app with isolated demo data. Project names prefixed with “示例” mean “example”; they contain no real user projects.

## What changed from Kitter

This fork continues from Kitter `v0.1.10` (`e953940`). The upstream skill import, central library, project installation and Agent discovery features remain the foundation. Main additions in this repository:

| Area | Pleat additions |
| --- | --- |
| Brand and entry points | Pleat name, folded icon, desktop window, About page and macOS package; retain the `kitter` CLI and application identifiers. |
| Native projects | Read Codex local projects and native Proma workspaces; merge directory identities while retaining workspace associations, with archive, hide and restore views. |
| Proma workspaces | Manage user skill copies per native workspace; distinguish managed copies, external links, app integrations and local variants. Install/remove user skills and enable/disable verified built-in presets. |
| Operations across projects | Select multiple skills, Agents and projects from the library, project view or installation details; inspect the impact list before execution and see individual failures. |
| Context and search | Distinguish installed Agents; search names with optional description matches; inspect source categories, automatic/manual skills and token estimates while retaining the current project and workspace. |
| Updates and local adaptations | Compare the recorded source baseline, local files and new source. Preserve non-conflicting edits and stop on conflicts. Protect `local-data/` and support a separate `local-data/ADAPTATION.md`. |
| Storage and compatibility | Use Pleat data directories for new installations. Preview library moves, existing-library switches and legacy Kitter data-root moves; keep compatibility links and validate directory identities and transaction state. |

## See the app

### One library, every installation

Add skills from local folders, GitHub, skills.sh-compatible sources or Claude plugin sources, or inspect and adopt existing installations. Organize skills into groups and inspect their source, content and project installations.

![Pleat library and installation details with isolated demo data](assets/readme/pleat-library.png)

### Projects and native workspaces

The project view separates direct installations, inherited sources, global skills and plugins. Codex and Proma can share a project while retaining their own installation paths and native workspaces.

![Pleat project and Agent overview with isolated demo data](assets/readme/pleat-project.png)

Context numbers come from directory scans and static estimates. They describe discovered skills, not what an Agent actually loaded in a particular turn or its actual token usage. Proma installation checks are separate from runtime-loading verification.

### Inspect the scope before a batch operation

Choose skills, Agents and native Proma workspaces, then review the target paths and operations. Ordinary Agent installations use managed links; Proma uses workspace copies with recorded origins that require an explicit sync to update.

![Pleat management panel with isolated demo data](assets/readme/pleat-management.png)

## Build and use

**No Pleat GitHub Release has been published in this repository yet.** Build from source for now. [Upstream Kitter Releases](https://github.com/what1f/kitter/releases) distribute Kitter, not this Pleat fork.

```sh
git clone https://github.com/caaaptaintop/Pleat.git
cd Pleat

# Core and CLI
cargo build --release --locked --bin kitter

# Run the desktop app
cargo run --release --locked --features desktop --bin kitter-desktop
```

The build requires Rust and Git; see the [CI configuration](.github/workflows/ci.yml) for desktop platform dependencies. Packaging on macOS also requires `just` and the system command-line tools:

```sh
just package
# target/release/Pleat.app
# target/release/Pleat-<version>-macos-<arch>.dmg
```

The local macOS packaging script uses ad hoc signing. Apple Developer ID signing and notarization are not provided.

The CLI is still named `kitter`. These examples assume the built `target/release/kitter` is on your `PATH`:

```sh
kitter list
kitter projects --json
kitter add npx https://github.com/owner/repository --skill skill-a
kitter install skill-a --project /path/to/project --target codex
kitter project /path/to/project --agent codex
kitter check
kitter update skill-a
```

Use `--target universal` for the shared `.agents/skills` location. Provider-specific targets follow each Agent's directory rules. Proma operations are scoped to registered native workspaces:

```sh
kitter proma workspaces --json
kitter proma status --workspace <workspace-slug> --json
```

See `kitter --help`, `kitter project --help` and `kitter proma --help` for more options. The bundled Agent skill remains [`kitter`](resources/skills/kitter).

## Data locations and legacy compatibility

Default skill libraries for new installations:

| Platform | Default location |
| --- | --- |
| macOS | `~/Library/Application Support/Pleat/skills` |
| Windows | `%LOCALAPPDATA%\Pleat\skills` |
| Linux | `$XDG_DATA_HOME/Pleat/skills` or `~/.local/share/Pleat/skills` |

Existing Kitter data stays in place until an explicit migration. Open **Settings → Advanced storage settings** to preview a move, move the current library or use an existing library. Moves retain old-path compatibility links so existing installations and local environments can still reach the same data.

Migration refuses cross-volume moves, conflicting data, nested directories and mismatched identities. An incomplete metadata transaction blocks reopening until the saved recovery material is resolved; it is not overwritten automatically. `local-data/` remains with its source skill and is not copied into Proma as public skill code.

```sh
kitter library
# Switch to an existing nonempty library; use the desktop action to move into an empty folder.
kitter library --set /absolute/path/to/existing/library
```

`KITTER_HOME` remains an explicit application-data override. Bundle ID `dev.kitter.app`, the internal `Kitter` executable, Cargo package name, configuration format and compatibility variable names remain unchanged. Renaming the product and repository does not require rewriting skill content or host configuration.

## Platform and verification scope

| Platform | Existing evidence |
| --- | --- |
| macOS ARM64 | Local desktop, CLI, packaging and isolated demo operations verified; screenshots in this README are from this platform. |
| macOS Intel | Core tests and desktop compilation checked in CI. |
| Windows x64 | Core tests and desktop compilation checked in CI. |
| Linux x64 | Core tests and desktop compilation checked in CI. |

CI does not replace native desktop-experience testing on Windows, Linux or Intel Mac. Check [GitHub Actions](https://github.com/caaaptaintop/Pleat/actions) for the status of a specific commit.

## Upstream, contributing and license

Pleat is based on [what1f/kitter](https://github.com/what1f/kitter). Thanks to the upstream authors and contributors for the foundation. Pleat changes are recorded in this repository's commit history; they are not an upstream release or endorsement.

Please use [Pleat Issues](https://github.com/caaaptaintop/Pleat/issues) for this fork.

Distributed under [Apache License 2.0](LICENSE), retaining upstream copyright and license notices. See [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for bundled material, [brand assets](assets/brand/README.md) for icon sources and [README image provenance](assets/readme/README.md) for the visuals.
