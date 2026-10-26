# Pleat v0.1.11

## Updates

- Include the verified Agent project-discovery, management and adaptation paths from the local compatibility stage. WorkBuddy remains partial; this release does not claim complete native support for all 17 Agents. See [the scope record](docs/agent-compatibility-stage.json).
- Add opt-in Proma built-in skill following across all registered workspaces. Apply a reviewed first-use plan to enable it; subsequent checks preserve active/inactive locations and local edits.
- Backport selected Kitter v0.2.2 fixes while keeping Pleat's current Agent adapters and UI. The full upstream plugin engine is not included. See [backport details](docs/upstream-backports-v0.1.11.json).
- Preserve SSH and HTTPS repository URLs; compare CRLF/LF text conservatively; retain known update status when a source check fails or lacks a hash. Keep update caches with the selected library and reinstall only the requested skill.
- Show Agent names on icon hover and matching/total skill counts when filtering.
- Use the static MSVC runtime for Windows builds and remove generated Python bytecode from the source tree.

## Distribution

This prerelease provides the Apple Silicon macOS installer. It uses ad hoc signing and is not notarized. [First-launch instructions](README.md#if-macos-blocks-the-first-launch) remain applicable.
