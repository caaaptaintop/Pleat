use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
#[cfg(not(unix))]
use sha2::{Digest, Sha256};
#[cfg(not(unix))]
use std::io::Read;
#[cfg(not(unix))]
use walkdir::WalkDir;

// Stored in the existing registry, never inside a skill or its private data.
pub(crate) type Manifest = BTreeMap<String, Node>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Node {
    Directory { mode: u32 },
    File { sha256: String, mode: u32 },
    Symlink { target: String, directory: bool },
}

#[cfg(not(unix))]
fn mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        u32::from(metadata.permissions().readonly())
    }
}

pub(crate) fn snapshot(root: &Path) -> Result<Manifest> {
    #[cfg(unix)]
    {
        return crate::package_fs::Directory::open(root)?.snapshot();
    }
    #[cfg(not(unix))]
    {
        snapshot_portable(root)
    }
}

#[cfg(not(unix))]
fn snapshot_portable(root: &Path) -> Result<Manifest> {
    if !fs::symlink_metadata(root)?.is_dir() {
        bail!("更新保护要求实体目录，不能跟随根链接");
    }
    let mut result = Manifest::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0 || (e.file_name() != "local-data" && e.file_name() != ".git")
        })
    {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .context("无法保护非 UTF-8 文件路径")?
            .to_owned();
        let metadata = fs::symlink_metadata(path)?;
        let node = if metadata.file_type().is_symlink() {
            let target = fs::read_link(path)?
                .to_str()
                .context("无法保护非 UTF-8 链接目标")?
                .to_owned();
            #[cfg(windows)]
            let directory = {
                use std::os::windows::fs::FileTypeExt;
                metadata.file_type().is_symlink_dir()
            };
            #[cfg(not(windows))]
            let directory = false;
            Node::Symlink { target, directory }
        } else if metadata.is_dir() {
            Node::Directory {
                mode: mode(&metadata),
            }
        } else if metadata.is_file() {
            let mut hash = Sha256::new();
            let mut file = open_regular_file(root, path)?;
            let mut buffer = [0u8; 65536];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            Node::File {
                sha256: format!("{:x}", hash.finalize()),
                mode: mode(&metadata),
            }
        } else {
            bail!("无法安全更新特殊文件：{relative}");
        };
        result.insert(relative, node);
    }
    Ok(result)
}

#[cfg(not(unix))]
fn open_regular_file(root: &Path, path: &Path) -> Result<fs::File> {
    crate::package_fs::Directory::open(root)?.read(path.strip_prefix(root)?)
}

fn merge(base: &Manifest, current: &Manifest, incoming: &Manifest) -> Result<Manifest> {
    let paths = base
        .keys()
        .chain(current.keys())
        .chain(incoming.keys())
        .collect::<BTreeSet<_>>();
    let mut desired = Manifest::new();
    let mut conflicts = Vec::new();
    for path in paths {
        let old = base.get(path);
        let local = current.get(path);
        let new = incoming.get(path);
        let chosen = if local == old {
            new
        } else if new == old || local == new {
            local
        } else if matches!((local, new),
            (Some(Node::File { sha256: a, mode: x }), Some(Node::File { sha256: b, mode: y }))
                if a == b && (x & 0o111 != 0) == (y & 0o111 != 0))
        {
            // Git does not retain private read/write bits (e.g. 0600 vs 0644).
            // Identical content and execution intent can keep local permissions.
            local
        } else {
            conflicts.push(path.clone());
            continue;
        };
        if let Some(node) = chosen {
            desired.insert(path.clone(), node.clone());
        }
    }
    if !conflicts.is_empty() {
        bail!(
            "本机文件与来源更新冲突，未替换技能：{}",
            conflicts.join("、")
        );
    }
    // A locally added child survives removal of its unchanged upstream parent.
    // A file/link replacing that parent is a conflict, never a traversal target.
    for path in desired.keys().cloned().collect::<Vec<_>>() {
        let mut parent = Path::new(&path).parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            let name = p.to_str().context("无法保护文件路径")?;
            match desired.get(name) {
                Some(Node::Directory { .. }) => {}
                Some(_) => bail!("本机文件与来源目录类型冲突，未替换技能：{name}"),
                None => match current.get(name) {
                    Some(node @ Node::Directory { .. }) => {
                        desired.insert(name.to_owned(), node.clone());
                    }
                    _ => bail!("更新会覆盖本机目录删除，未替换技能：{name}"),
                },
            }
            parent = p.parent();
        }
    }
    Ok(desired)
}

// Only the disposable staging tree is modified. Live files are never written here.
pub(crate) fn prepare(
    current_root: &Path,
    staging: &Path,
    base: &Manifest,
    current: &Manifest,
) -> Result<Manifest> {
    let output = crate::package_fs::Directory::open(staging)?;
    let input = crate::package_fs::Directory::open(current_root)?;
    let incoming = snapshot(staging)?;
    let desired = merge(base, current, &incoming)?;
    // A few native Npx sources use skill.md; macOS resolves SKILL.md to it.
    // Keep their original filename in the manifest and in the source hash.
    if !desired.iter().any(|(path, node)| path.eq_ignore_ascii_case("SKILL.md") && matches!(node, Node::File { .. })) {
        bail!("技能入口已被本机删除或改为链接，未替换技能");
    }
    // Staging may contain read-only source directories. Temporarily make only
    // these disposable directories writable, then restore the chosen modes.
    for (path, node) in &incoming {
        if let Node::Directory { mode } = node {
            #[cfg(unix)]
            let writable = *mode | 0o700;
            #[cfg(not(unix))]
            let writable = 0;
            output.chmod_directory(Path::new(path), writable)?;
        }
    }
    let mut paths = incoming.keys().collect::<Vec<_>>();
    paths.sort_by_key(|p| std::cmp::Reverse(Path::new(p).components().count()));
    for path in paths {
        let old = &incoming[path];
        let replace_type = desired
            .get(path)
            .is_some_and(|new| std::mem::discriminant(old) != std::mem::discriminant(new));
        if !desired.contains_key(path) || replace_type {
            output.remove(Path::new(path), old)?;
        }
    }
    // BTree ordering puts parents before their children.
    for (path, node) in &desired {
        let target = staging.join(path);
        if let Node::Directory { .. } = node {
            output.mkdir(Path::new(path))?;
            continue;
        }
        if incoming.get(path) == Some(node) {
            continue;
        }
        if let Some(old) = incoming.get(path) {
            if fs::symlink_metadata(&target).is_ok() {
                output.remove(Path::new(path), old)?;
            }
        }
        match node {
            Node::File { mode, .. } => {
                let mut source = input.read(Path::new(path))?;
                let mut file = output.create_file(Path::new(path), *mode)?;
                std::io::copy(&mut source, &mut file)?;
            }
            Node::Symlink { target, directory } => {
                output.symlink(Path::new(path), target, *directory)?;
            }
            Node::Directory { .. } => unreachable!(),
        }
    }
    for (path, node) in desired.iter().rev() {
        if let Node::Directory { mode } = node {
            output.chmod_directory(Path::new(path), *mode)?;
        }
    }
    if snapshot(staging)? != desired {
        bail!("准备期间文件发生变化，未替换技能");
    }
    // The next baseline describes the source, not retained local changes.
    Ok(incoming)
}

pub(crate) fn verify_unchanged(root: &Path, expected: &Manifest) -> Result<()> {
    if snapshot(root)? != *expected {
        bail!("更新期间本机文件发生变化，未完成替换");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn identical_new_file_keeps_private_local_permissions() {
        let local = entries(&[(
            "new.py",
            Node::File {
                sha256: "same".into(),
                mode: 0o600,
            },
        )]);
        let incoming = entries(&[("new.py", file("same"))]);
        assert_eq!(merge(&Manifest::new(), &local, &incoming).unwrap(), local);
    }

    #[test]
    fn permission_compatibility_does_not_hide_content_or_execution_conflicts() {
        let local = entries(&[(
            "new.py",
            Node::File {
                sha256: "local".into(),
                mode: 0o600,
            },
        )]);
        assert!(
            merge(
                &Manifest::new(),
                &local,
                &entries(&[("new.py", file("remote"))])
            )
            .is_err()
        );
        let executable = entries(&[(
            "new.py",
            Node::File {
                sha256: "local".into(),
                mode: 0o755,
            },
        )]);
        assert!(merge(&Manifest::new(), &local, &executable).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_local_file_is_retained_while_baseline_describes_source() {
        let current = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        fs::write(current.path().join("SKILL.md"), "entry").unwrap();
        fs::write(staging.path().join("SKILL.md"), "entry").unwrap();
        let base = snapshot(current.path()).unwrap();
        fs::write(current.path().join("new.py"), "same").unwrap();
        fs::set_permissions(
            current.path().join("new.py"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(staging.path().join("new.py"), "same").unwrap();
        fs::set_permissions(
            staging.path().join("new.py"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let local = snapshot(current.path()).unwrap();
        let source = snapshot(staging.path()).unwrap();
        let baseline = prepare(current.path(), staging.path(), &base, &local).unwrap();
        assert_eq!(baseline, source);
        assert_eq!(snapshot(staging.path()).unwrap(), local);
        fs::set_permissions(
            current.path().join("new.py"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(verify_unchanged(current.path(), &local).is_err());
    }
    fn file(value: &str) -> Node {
        Node::File {
            sha256: value.into(),
            mode: 0o644,
        }
    }
    fn entries(values: &[(&str, Node)]) -> Manifest {
        values
            .iter()
            .map(|(p, n)| (p.to_string(), n.clone()))
            .collect()
    }

    #[test]
    fn native_lowercase_skill_entry_keeps_its_source_filename() {
        let current = tempfile::tempdir().unwrap();
        let staging = tempfile::tempdir().unwrap();
        fs::write(current.path().join("skill.md"), "original").unwrap();
        fs::write(staging.path().join("skill.md"), "updated").unwrap();
        let base = snapshot(current.path()).unwrap();
        let incoming = prepare(current.path(), staging.path(), &base, &base).unwrap();
        assert!(incoming.contains_key("skill.md"));
        assert_eq!(
            fs::read(staging.path().join("skill.md")).unwrap(),
            b"updated"
        );
    }

    #[test]
    fn local_edits_additions_deletions_survive_unrelated_source_updates() {
        let base = entries(&[
            ("SKILL.md", file("old")),
            ("script.py", file("old")),
            ("obsolete", file("old")),
        ]);
        let local = entries(&[
            ("SKILL.md", file("mine")),
            ("script.py", file("old")),
            ("notes", file("mine")),
        ]);
        let new = entries(&[
            ("SKILL.md", file("old")),
            ("script.py", file("new")),
            ("obsolete", file("old")),
        ]);
        assert_eq!(
            merge(&base, &local, &new).unwrap(),
            entries(&[
                ("SKILL.md", file("mine")),
                ("script.py", file("new")),
                ("notes", file("mine"))
            ])
        );
    }

    #[test]
    fn conflicting_edits_removals_additions_and_types_are_rejected() {
        for (old, local, new) in [
            (Some(file("old")), Some(file("mine")), Some(file("new"))),
            (Some(file("old")), Some(file("mine")), None),
            (Some(file("old")), None, Some(file("new"))),
            (None, Some(file("mine")), Some(file("new"))),
            (
                Some(file("old")),
                Some(Node::Directory { mode: 0o755 }),
                Some(file("new")),
            ),
        ] {
            let make =
                |node: Option<Node>| node.map(|n| entries(&[("path", n)])).unwrap_or_default();
            assert!(merge(&make(old), &make(local), &make(new)).is_err());
        }
    }

    #[test]
    fn source_parent_removal_retains_local_child_but_type_change_conflicts() {
        let base = entries(&[
            ("refs", Node::Directory { mode: 0o755 }),
            ("refs/old", file("old")),
        ]);
        let mut local = base.clone();
        local.insert("refs/mine".into(), file("mine"));
        let result = merge(&base, &local, &Manifest::new()).unwrap();
        assert!(result.contains_key("refs/mine"));
        assert!(!result.contains_key("refs/old"));
        assert!(merge(&base, &local, &entries(&[("refs", file("new"))])).is_err());
    }
}
