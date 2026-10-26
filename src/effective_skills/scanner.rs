use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;

use super::SkillRoot;

#[derive(Clone, Copy)]
pub(super) enum ScanProfile {
    DirectChildren,
    ProviderChildren,
    DimRecursive,
    Recursive {
        max_depth: usize,
        max_directories: usize,
        max_entries: usize,
    },
    PiIgnored,
    PromaActive,
    WorkBuddy,
}

pub(super) fn scan(root: &SkillRoot, profile: ScanProfile) -> Vec<PathBuf> {
    if machine_data_path(&root.path) {
        return Vec::new();
    }
    if let Some(path) = &root.exact_skill_file {
        if machine_data_path(path) {
            return Vec::new();
        }
        return path
            .is_file()
            .then(|| vec![path.clone()])
            .unwrap_or_default();
    }
    if !root.path.is_dir() {
        return Vec::new();
    }
    if root.flat_markdown_only {
        return flat_markdown(root);
    }
    if matches!(profile, ScanProfile::ProviderChildren) {
        return provider_children(root);
    }
    if root.direct_children_only {
        return direct_children(root);
    }
    let mut files = match profile {
        ScanProfile::DirectChildren => direct_children(root),
        ScanProfile::ProviderChildren => provider_children(root),
        ScanProfile::Recursive {
            max_depth,
            max_directories,
            max_entries,
        } => recursive(root, max_depth, max_directories, max_entries, true, false),
        ScanProfile::DimRecursive => recursive(root, 11, 2_000, 20_000, false, true),
        ScanProfile::PiIgnored => pi_ignored(root),
        ScanProfile::PromaActive => proma_active(root),
        ScanProfile::WorkBuddy => workbuddy(root),
    };
    files.sort();
    files.dedup();
    files
}

fn flat_markdown(root: &SkillRoot) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(&root.path) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            !machine_data_path(path)
                && path.is_file()
                && path.extension().is_some_and(|extension| extension == "md")
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn provider_children(root: &SkillRoot) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(&root.path) else {
        return Vec::new();
    };
    let mut files = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path().join("SKILL.md"))
        .filter(|path| !machine_data_path(path) && path.is_file())
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn direct_children(root: &SkillRoot) -> Vec<PathBuf> {
    let root_manifest = root.path.join("SKILL.md");
    let mut result = (root_manifest.is_file() && !machine_data_path(&root_manifest))
        .then(|| vec![root.path.join("SKILL.md")])
        .unwrap_or_default();
    let Ok(entries) = fs::read_dir(&root.path) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if machine_data_path(&path) {
            continue;
        }
        if path.is_dir() {
            let manifest = path.join("SKILL.md");
            if manifest.is_file() && !machine_data_path(&manifest) {
                result.push(manifest);
            }
        } else if root.include_root_markdown
            && path.extension().is_some_and(|extension| extension == "md")
            && !machine_data_path(&path)
        {
            result.push(path);
        }
    }
    result
}

fn recursive(
    root: &SkillRoot,
    max_depth: usize,
    max_directories: usize,
    max_entries: usize,
    include_root_manifest: bool,
    include_hidden: bool,
) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![(root.path.clone(), 0usize)];
    let mut visited = HashSet::new();
    let mut directories = 0usize;
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if directories >= max_directories || entries_seen >= max_entries {
            break;
        }
        let canonical = fs::canonicalize(&directory).unwrap_or_else(|_| directory.clone());
        if machine_data_path(&canonical) {
            continue;
        }
        if !visited.insert(canonical) {
            continue;
        }
        directories += 1;
        let manifest = directory.join("SKILL.md");
        if (depth > 0 || include_root_manifest)
            && manifest.is_file()
            && !machine_data_path(&manifest)
        {
            result.push(manifest);
            continue;
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().rev() {
            entries_seen += 1;
            if entries_seen > max_entries {
                break;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !include_hidden && (name.starts_with('.') || name == "node_modules") {
                continue;
            }
            let path = entry.path();
            if directory == root.path
                && root.include_root_markdown
                && path.is_file()
                && path.extension().is_some_and(|extension| extension == "md")
                && !machine_data_path(&path)
            {
                result.push(path);
            } else if path.is_dir()
                && (root.follow_directory_symlinks
                    || !entry.file_type().is_ok_and(|kind| kind.is_symlink()))
            {
                stack.push((path, depth + 1));
            }
        }
    }
    result
}

/// WorkBuddy counts scanner calls, rather than filesystem components: the
/// `skills/` directory inside a found skill advances one level. The root's own
/// manifest is not an entry. Hidden directories are included, machine data not.
fn workbuddy(root: &SkillRoot) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![(root.path.clone(), 0usize)];
    let mut visited = HashSet::new();
    let mut entries_seen = 0usize;
    while let Some((directory, depth)) = stack.pop() {
        if depth > 5 || visited.len() >= 2_000 || entries_seen >= 20_000 { continue; }
        let canonical = fs::canonicalize(&directory).unwrap_or_else(|_| directory.clone());
        if machine_data_path(&canonical) || !visited.insert(canonical) { continue; }
        let Ok(children) = fs::read_dir(&directory) else { continue; };
        let mut children = children.take(20_001 - entries_seen).flatten().collect::<Vec<_>>();
        children.sort_by_key(|entry| entry.file_name());
        for child in children.into_iter().rev() {
            entries_seen += 1;
            if entries_seen > 20_000 { break; }
            let path = child.path();
            if !path.is_dir() || machine_data_path(&path) { continue; }
            let manifest = path.join("SKILL.md");
            if manifest.is_file() && !machine_data_path(&manifest) {
                result.push(manifest);
                let nested = path.join("skills");
                if nested.is_dir() { stack.push((nested, depth + 1)); }
            } else {
                stack.push((path, depth + 1));
            }
        }
    }
    result
}

fn pi_ignored(root: &SkillRoot) -> Vec<PathBuf> {
    let root_manifest = root.path.join("SKILL.md");
    if root_manifest.is_file() && !machine_data_path(&root_manifest) {
        return vec![root_manifest];
    }
    let mut result = Vec::new();
    let mut builder = WalkBuilder::new(&root.path);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .follow_links(true)
        .filter_entry(|entry| !machine_data_path(entry.path()));
    for entry in builder.build().flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.file_name().is_some_and(|name| name == "SKILL.md")
            || (root.include_root_markdown
                && path.parent() == Some(root.path.as_path())
                && path.extension().is_some_and(|extension| extension == "md"))
        {
            result.push(path.to_path_buf());
        }
    }
    result
}

fn proma_active(root: &SkillRoot) -> Vec<PathBuf> {
    let Ok(active) = root.path.canonicalize() else {
        return Vec::new();
    };
    let mut builder = WalkBuilder::new(&active);
    let boundary = active.clone();
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .follow_links(true)
        .filter_entry(move |entry| {
            !machine_data_path(entry.path())
                && entry.path().canonicalize().is_ok_and(|path| path.starts_with(&boundary))
        });
    let mut files = builder.build().flatten()
        .filter(|entry| entry.path().file_name().is_some_and(|name| name == "SKILL.md"))
        .filter_map(|entry| entry.path().canonicalize().ok())
        .filter(|path| path.is_file() && path.starts_with(&active) && !machine_data_path(path))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();
    files
}

fn machine_data_path(path: &Path) -> bool {
    let reserved = |path: &Path| {
        path.components()
            .any(|part| part.as_os_str() == "local-data")
    };
    reserved(path) || fs::canonicalize(path).is_ok_and(|path| reserved(&path))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::effective_skills::{SkillRoot, SkillScope};

    #[test]
    fn every_profile_excludes_machine_data_and_aliases() {
        let temp = tempdir().unwrap();
        let private = temp.path().join("local-data/private");
        fs::create_dir_all(&private).unwrap();
        fs::write(private.join("SKILL.md"), "fixture").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&private, temp.path().join("alias")).unwrap();
        let root = SkillRoot::new(temp.path().to_path_buf(), SkillScope::User);
        let profiles = [
            ScanProfile::DirectChildren,
            ScanProfile::ProviderChildren,
            ScanProfile::DimRecursive,
            ScanProfile::PiIgnored,
            ScanProfile::Recursive {
                max_depth: 4,
                max_directories: 100,
                max_entries: 100,
            },
        ];
        for profile in profiles {
            assert!(scan(&root, profile).is_empty());
            let data_root = SkillRoot::new(private.clone(), SkillScope::User);
            assert!(scan(&data_root, profile).is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_aliases_to_machine_data_are_excluded_in_every_profile() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let private = temp.path().join("local-data/private/SKILL.md");
        fs::create_dir_all(private.parent().unwrap()).unwrap();
        fs::write(&private, "private fixture").unwrap();
        let public = temp.path().join("public");
        fs::create_dir_all(public.join("demo")).unwrap();
        symlink(&private, public.join("SKILL.md")).unwrap();
        symlink(&private, public.join("demo/SKILL.md")).unwrap();
        symlink(&private, public.join("extra.md")).unwrap();
        let mut root = SkillRoot::new(public, SkillScope::User);
        root.include_root_markdown = true;
        let profiles = [
            ScanProfile::DirectChildren,
            ScanProfile::ProviderChildren,
            ScanProfile::DimRecursive,
            ScanProfile::PiIgnored,
            ScanProfile::Recursive {
                max_depth: 4,
                max_directories: 100,
                max_entries: 100,
            },
        ];
        for profile in profiles {
            assert!(scan(&root, profile).is_empty());
        }
    }

    #[test]
    fn direct_children_do_not_recurse() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join("direct")).unwrap();
        fs::create_dir_all(temp.path().join("group/nested")).unwrap();
        fs::write(temp.path().join("direct/SKILL.md"), "---\n---").unwrap();
        fs::write(temp.path().join("group/nested/SKILL.md"), "---\n---").unwrap();
        let root = SkillRoot::new(temp.path().to_path_buf(), SkillScope::User);
        let files = scan(&root, ScanProfile::DirectChildren);
        assert_eq!(files, vec![temp.path().join("direct/SKILL.md")]);
    }

    #[test]
    fn pi_scan_honors_ignore_files() {
        let temp = tempdir().unwrap();
        fs::create_dir_all(temp.path().join("kept")).unwrap();
        fs::create_dir_all(temp.path().join("ignored")).unwrap();
        fs::write(temp.path().join(".ignore"), "ignored/\n").unwrap();
        fs::write(temp.path().join("kept/SKILL.md"), "---\n---").unwrap();
        fs::write(temp.path().join("ignored/SKILL.md"), "---\n---").unwrap();
        let root = SkillRoot::new(temp.path().to_path_buf(), SkillScope::User);
        let files = scan(&root, ScanProfile::PiIgnored);
        assert_eq!(files, vec![temp.path().join("kept/SKILL.md")]);
    }

    #[cfg(unix)]
    #[test]
    fn proma_active_scan_rejects_cross_root_links_and_private_data() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let active = temp.path().join("skills");
        for directory in ["skills/kept", "skills/file-alias", "skills/local-data/private",
            "skills/ignored", "skills-inactive/disabled", "external"] {
            fs::create_dir_all(temp.path().join(directory)).unwrap();
            fs::write(temp.path().join(directory).join("SKILL.md"), "fixture").unwrap();
        }
        fs::remove_file(active.join("file-alias/SKILL.md")).unwrap();
        symlink(temp.path().join("external/SKILL.md"), active.join("file-alias/SKILL.md")).unwrap();
        symlink(temp.path().join("external"), active.join("external-alias")).unwrap();
        symlink(active.join("local-data/private"), active.join("private-alias")).unwrap();
        symlink(active.join("kept"), active.join("internal-alias")).unwrap();
        fs::write(active.join(".ignore"), "ignored/\n").unwrap();
        fs::write(active.join("root.md"), "fixture").unwrap();
        let root = SkillRoot::new(active.clone(), SkillScope::Repository);
        assert_eq!(scan(&root, ScanProfile::PromaActive), vec![active.join("kept/SKILL.md").canonicalize().unwrap()]);
    }
}
