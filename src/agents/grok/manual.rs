//! Read-only formatter for a verified native catalog entry. No document body is
//! read. Checkpoints bind this observation, not a later native process's FDs.
//! Qualified names follow fixed official 1.0.45 source; native 1.0.50 execution
//! of these qualified outputs has not been verified here.

use std::path::Path;

use crate::effective_skills::{EffectiveSkill, SkillScope, SkillSource, SkillVisibility};

/// Native input and the filesystem checkpoints used to construct it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Invocation {
    pub text: String,
    pub source_identity: String,
    pub source_revision: String,
    pub context_identity: String,
    pub includes_adaptation: bool,
}

/// Caller must preserve verified native namespace provenance in id: Server uses
/// server:<id> even under display scope User. Exclude unknown ConfigToml
/// namespaces upstream; neither paths nor display names establish a namespace.
pub fn invocation(
    project: &Path,
    skill: &EffectiveSkill,
    installed_alias: Option<&Path>,
) -> anyhow::Result<Option<Invocation>> {
    if !matches!(skill.source, SkillSource::Filesystem)
        || !skill.user_invocable
        || !matches!(skill.visibility, SkillVisibility::Automatic | SkillVisibility::ManualOnly)
    {
        return Ok(None);
    }
    let scope_prefix = match skill.scope {
        SkillScope::Local => "local",
        SkillScope::Repository => "repo",
        SkillScope::User => "user",
        SkillScope::System => return Ok(None),
    };
    #[cfg(unix)]
    {
        // Discard the underlying error chain, including paths and OS messages.
        unix::format(project, skill, installed_alias, scope_prefix)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("Grok 手动调用无法安全构造"))
    }
    #[cfg(not(unix))]
    {
        let _ = (project, installed_alias, scope_prefix);
        Ok(None)
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::{Result, ensure};
    use crate::package_fs::{Directory, is_not_found};
    use std::{fs::{File, Metadata}, os::unix::fs::MetadataExt, path::{Component, PathBuf}};

    #[cfg(test)]
    thread_local! {
        pub(super) static AFTER_BIND: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            std::cell::RefCell::new(None);
    }

    // Only ENOENT means MissingOptional; links, wrong node types and access
    // failures must reach the fixed public error, never a slash-only fallback.
    fn optional<T>(result: Result<T>) -> Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error) if is_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn same_file(a: &Metadata, b: &Metadata) -> bool {
        a.is_file() && b.is_file() && a.dev() == b.dev() && a.ino() == b.ino()
            && a.len() == b.len() && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec() && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec() && a.nlink() == b.nlink()
    }

    fn normal_absolute(path: &Path) -> bool {
        path.is_absolute() && path.components().all(|component|
            matches!(component, Component::RootDir | Component::Normal(_)))
    }

    fn relative_path(base: &Path, target: &Path) -> Option<PathBuf> {
        if !normal_absolute(base) || !normal_absolute(target) { return None; }
        let base = base.components().collect::<Vec<_>>();
        let target = target.components().collect::<Vec<_>>();
        let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
        if common == 0 { return None; }
        let mut relative = PathBuf::new();
        for _ in &base[common..] { relative.push(".."); }
        for component in &target[common..] { relative.push(component.as_os_str()); }
        Some(relative)
    }

    fn safe_reference(path: &Path) -> bool {
        path.to_str().is_some_and(|token| !token.is_empty()
            && !token.chars().any(|c| c.is_whitespace() || c.is_control()))
    }

    fn adaptation_reference(project: &Path, logical_root: &Path, expected: &Path) -> Result<PathBuf> {
        let suffix = Path::new("local-data/ADAPTATION.md");
        let logical = logical_root.join(suffix);
        let native_reference = |reference: PathBuf| {
            // The collector and FileReference parser each consume an optional
            // leading @. A ./ prefix keeps a real @ directory name intact.
            if reference.as_os_str().as_encoded_bytes().first() == Some(&b'@') {
                Path::new(".").join(reference)
            } else { reference }
        };
        let resolves = |reference: &Path| safe_reference(reference)
            && project.join(reference).canonicalize().is_ok_and(|path| path == expected);
        if normal_absolute(project) && normal_absolute(logical_root) {
            // Prefer the logical alias only after checking native join semantics;
            // generated .. must not silently ascend a symlink's lexical parent.
            if let Some(relative) = relative_path(project, &logical) {
                let relative = native_reference(relative);
                if resolves(&relative) { return Ok(relative); }
            }
            // Native cwd follows the actual directory. Resolve only the alias's
            // parent, preserving the skill link itself in the emitted reference.
            // Never lexically cancel a symlink or a caller-supplied ParentDir.
            let base = project.canonicalize()?;
            let entry = match (logical_root.parent(), logical_root.file_name()) {
                (Some(parent), Some(name)) => parent.canonicalize()?.join(name),
                _ => logical_root.to_path_buf(),
            };
            if let Some(relative) = relative_path(&base, &entry.join(suffix)) {
                let relative = native_reference(relative);
                if resolves(&relative) { return Ok(relative); }
            }
        }
        Ok(logical)
    }

    struct FileProof {
        file: File,
        metadata: Metadata,
    }

    impl FileProof {
        fn new(file: File) -> Result<Self> {
            let metadata = file.metadata()?;
            ensure!(metadata.is_file(), "invalid file");
            Ok(Self { file, metadata })
        }

        fn verify(&self, parent: &Directory, name: &Path) -> Result<()> {
            ensure!(same_file(&self.metadata, &self.file.metadata()?)
                && same_file(&self.metadata, &parent.read(name)?.metadata()?),
                "file changed");
            Ok(())
        }
    }

    pub(super) fn format(
        project: &Path,
        skill: &EffectiveSkill,
        installed_alias: Option<&Path>,
        scope_prefix: &str,
    ) -> Result<Invocation> {
        // id is the native invocation ID supplied by discovery, never name.
        let (invocation_prefix, bare_id) = match skill.id.split_once(':') {
            Some((prefix, bare)) => {
                ensure!(prefix == scope_prefix
                    || (skill.scope == SkillScope::User && prefix == "server"),
                    "invocation scope mismatch");
                (prefix, bare)
            }
            None => (scope_prefix, skill.id.as_str()),
        };
        ensure!(!bare_id.is_empty() && bare_id.bytes().all(|byte|
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')),
            "invalid invocation id");
        // Bare names can select builtins. Always use the canonical native scope.
        let invocation_name = format!("{invocation_prefix}:{bare_id}");
        ensure!(project.is_absolute() && skill.path.file_name().is_some_and(|name| name == "SKILL.md"),
            "invalid source path");
        let context_path = project.canonicalize()?;
        let context = Directory::open_anchored(&context_path)?;
        let context_identity = context.identity()?;
        let verify_context = || -> Result<()> {
            ensure!(project.canonicalize()? == context_path, "project mapping changed");
            context.verify_location(&context_path)?;
            ensure!(context.identity()? == context_identity, "project changed");
            Ok(())
        };
        verify_context()?;
        let skill_path = if skill.path.is_absolute() { skill.path.clone() }
            else { project.join(&skill.path) };
        let logical_source = skill_path.parent().ok_or_else(|| anyhow::anyhow!("missing source"))?;
        let source_path = logical_source.canonicalize()?;
        let source = Directory::open_anchored(&source_path)?;
        let source_identity = source.identity()?;
        let logical_alias = installed_alias.map(|path|
            if path.is_absolute() { path.to_path_buf() } else { project.join(path) });
        let verify_sources = || -> Result<()> {
            ensure!(logical_source.canonicalize()? == source_path, "source mapping changed");
            if let Some(alias) = &logical_alias {
                ensure!(alias.canonicalize()? == source_path, "alias mapping changed");
            }
            source.verify_location(&source_path)?;
            ensure!(source.identity()? == source_identity, "source changed");
            Ok(())
        };
        verify_sources()?;
        let skill_name = Path::new("SKILL.md");
        let skill_file = FileProof::new(source.read(skill_name)?)?;
        let metadata = &skill_file.metadata;
        let source_revision = format!("{}:{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(), metadata.ino(), metadata.len(), metadata.mtime(),
            metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec(), metadata.nlink());
        let local_name = Path::new("local-data");
        let adaptation_name = Path::new("ADAPTATION.md");
        let local = optional(source.child(local_name))?;
        let local_identity = local.as_ref().map(Directory::identity).transpose()?;
        let adaptation = match &local {
            Some(local) => optional(local.read(adaptation_name))?.map(FileProof::new).transpose()?,
            None => None,
        };
        let reference = if adaptation.is_some() {
            // Keep the caller's logical alias. The canonical library path may
            // contain spaces even when a project-relative reference is usable.
            Some(adaptation_reference(project, logical_alias.as_deref().unwrap_or(logical_source),
                &source_path.join(local_name).join(adaptation_name))?)
        } else { None };
        let verify_reference = || -> Result<()> {
            if let Some(reference) = &reference {
                ensure!(project.join(reference).canonicalize()?
                    == source_path.join(local_name).join(adaptation_name), "reference mapping changed");
            }
            Ok(())
        };
        verify_reference()?;
        let text = if let Some(reference) = &reference {
            let token = reference.to_str().ok_or_else(|| anyhow::anyhow!("invalid reference"))?;
            ensure!(safe_reference(reference), "unrepresentable reference");
            // Raw native references have no quoting or escaping layer.
            format!("/{} @{}", invocation_name, token)
        } else {
            format!("/{}", invocation_name)
        };

        #[cfg(test)]
        AFTER_BIND.with(|hook| {
            let callback = hook.borrow_mut().take();
            if let Some(callback) = callback { callback(); }
        });

        skill_file.verify(&source, skill_name)?;
        match &local {
            Some(local) => {
                local.verify_location(&source_path.join(local_name))?;
                ensure!(Some(local.identity()?) == local_identity, "local data changed");
                match &adaptation {
                    Some(file) => file.verify(local, adaptation_name)?,
                    None => ensure!(optional(local.read(adaptation_name))?.is_none(), "adaptation appeared"),
                }
                local.verify_location(&source_path.join(local_name))?;
            }
            None => ensure!(optional(source.child(local_name))?.is_none(), "local data appeared"),
        }
        skill_file.verify(&source, skill_name)?;
        verify_sources()?;
        verify_reference()?;
        verify_context()?;
        Ok(Invocation { text, source_identity, source_revision, context_identity,
            includes_adaptation: adaptation.is_some() })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::package_fs::Directory;
    use std::{fs, os::unix::{ffi::OsStrExt, fs::symlink}, path::PathBuf};

    const SECRET: &str = "SECRET_SENTINEL_MUST_NOT_ESCAPE";

    fn skill(path: PathBuf) -> EffectiveSkill {
        EffectiveSkill {
            id: "user:manual_1-test.v2".into(), name: "Display Name Is Not The ID".into(),
            description: SECRET.into(), when_to_use: None, path,
            root_path: None, prompt_path: None, scope: SkillScope::User,
            visibility: SkillVisibility::Automatic, user_invocable: true,
            source: SkillSource::Filesystem,
        }
    }

    fn fixture(root: &Path, adaptation: bool) -> EffectiveSkill {
        fs::create_dir_all(root).unwrap();
        fs::write(root.join("SKILL.md"), SECRET).unwrap();
        if adaptation {
            fs::create_dir(root.join("local-data")).unwrap();
            fs::write(root.join("local-data/ADAPTATION.md"), SECRET).unwrap();
        }
        skill(root.join("SKILL.md"))
    }

    fn rejected(result: anyhow::Result<Option<Invocation>>) {
        let error = result.unwrap_err();
        assert_eq!(format!("{error:#}"), "Grok 手动调用无法安全构造");
        assert!(!format!("{error:?}").contains(SECRET));
        assert_eq!(error.chain().count(), 1);
    }

    fn assert_reference(project: &Path, entry: &EffectiveSkill, result: &Invocation) {
        let token = result.text.split_once(" @").unwrap().1;
        assert!(!token.chars().any(|c| c.is_whitespace() || c.is_control()));
        let root = entry.path.parent().unwrap().canonicalize().unwrap();
        assert_eq!(project.join(token).canonicalize().unwrap(), root.join("local-data/ADAPTATION.md"));
        assert_eq!(result.source_identity, Directory::open_anchored(&root).unwrap().identity().unwrap());
        assert!(result.includes_adaptation);
    }

    #[test]
    fn project_alias_preserves_a_safe_relative_reference_and_real_source_identity() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("中文 项目");
        let source = temp.path().join("技能 源库");
        let original = fixture(&source, true);
        let alias = project.join(".grok/skills/manual");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink(&source, &alias).unwrap();
        for (entry, installed) in [
            (original.clone(), Some(alias.as_path())),
            (skill(alias.join("SKILL.md")), None),
        ] {
            let result = invocation(&project, &entry, installed).unwrap().unwrap();
            assert_eq!(result.text, "/user:manual_1-test.v2 @.grok/skills/manual/local-data/ADAPTATION.md");
            assert!(result.includes_adaptation);
            let metadata = fs::metadata(&source).unwrap();
            use std::os::unix::fs::MetadataExt;
            assert_eq!(result.source_identity, format!("{}:{}", metadata.dev(), metadata.ino()));
            assert_eq!(result.source_identity,
                Directory::open_anchored(&source.canonicalize().unwrap()).unwrap().identity().unwrap());
        }
    }

    #[test]
    fn global_reference_resolves_through_the_supplied_alias_or_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let entry = fixture(&source, true);
        let alias = temp.path().join("global-alias");
        symlink(&source, &alias).unwrap();
        let project = temp.path().join("elsewhere");
        fs::create_dir(&project).unwrap();
        for installed in [Some(alias.as_path()), None] {
            let result = invocation(&project, &entry, installed).unwrap().unwrap();
            let expected = installed.unwrap_or(&source);
            assert_reference(&project, &entry, &result);
            assert_eq!(result.text, format!("/{} @../{}/local-data/ADAPTATION.md",
                entry.id, expected.file_name().unwrap().to_str().unwrap()));
        }
    }

    #[test]
    fn repository_parent_alias_is_relative_to_the_nested_native_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("仓库 空格");
        let project = repository.join("subdir");
        fs::create_dir_all(&project).unwrap();
        let mut entry = fixture(&temp.path().join("技能源库 空格"), true);
        entry.scope = SkillScope::Repository;
        entry.id = "repo:manual".into();
        let alias = repository.join(".grok/skills/x");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink(entry.path.parent().unwrap(), &alias).unwrap();
        let result = invocation(&project, &entry, Some(&alias)).unwrap().unwrap();
        assert_eq!(result.text, "/repo:manual @../.grok/skills/x/local-data/ADAPTATION.md");
        assert_reference(&project, &entry, &result);
    }

    #[test]
    fn different_global_and_cwd_branches_share_a_safe_relative_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let common = temp.path().join("共同 祖先");
        let project = common.join("workspace/subdir");
        fs::create_dir_all(&project).unwrap();
        let entry = fixture(&common.join("技能源 库"), true);
        let alias = common.join("global/skills/x");
        fs::create_dir_all(alias.parent().unwrap()).unwrap();
        symlink(entry.path.parent().unwrap(), &alias).unwrap();
        let result = invocation(&project, &entry, Some(&alias)).unwrap().unwrap();
        assert_eq!(result.text, "/user:manual_1-test.v2 @../../global/skills/x/local-data/ADAPTATION.md");
        assert_reference(&project, &entry, &result);
    }

    #[test]
    fn symlink_cwd_and_alias_parent_use_physical_parents_without_resolving_the_skill_link() {
        let temp = tempfile::tempdir().unwrap();
        let common = temp.path().join("实际 根");
        let physical_cwd = common.join("workspace/subdir");
        fs::create_dir_all(&physical_cwd).unwrap();
        let project = temp.path().join("cwd-link");
        symlink(&physical_cwd, &project).unwrap();
        let entry = fixture(&common.join("技能源 空格"), true);
        let physical_alias = common.join("global/skills/x");
        fs::create_dir_all(physical_alias.parent().unwrap()).unwrap();
        symlink(entry.path.parent().unwrap(), &physical_alias).unwrap();
        let alias_parent = temp.path().join("install-link");
        symlink(common.join("global"), &alias_parent).unwrap();
        let alias = alias_parent.join("skills/x");
        let result = invocation(&project, &entry, Some(&alias)).unwrap().unwrap();
        assert_eq!(result.text, "/user:manual_1-test.v2 @../../global/skills/x/local-data/ADAPTATION.md");
        assert_reference(&project, &entry, &result);
    }

    #[test]
    fn safe_repository_alias_is_preserved_when_its_physical_parent_contains_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("仓库 空格");
        let project = repository.join("subdir");
        fs::create_dir_all(&project).unwrap();
        let entry = fixture(&temp.path().join("源库 空格"), true);
        let physical_parent = temp.path().join("安装 父级");
        fs::create_dir_all(physical_parent.join("skills")).unwrap();
        symlink(&physical_parent, repository.join(".grok")).unwrap();
        symlink(entry.path.parent().unwrap(), physical_parent.join("skills/x")).unwrap();
        let alias = repository.join(".grok/skills/x");
        let result = invocation(&project, &entry, Some(&alias)).unwrap().unwrap();
        assert_eq!(result.text, "/user:manual_1-test.v2 @../.grok/skills/x/local-data/ADAPTATION.md");
        assert_reference(&project, &entry, &result);
    }

    #[test]
    fn caller_parent_components_are_not_lexically_folded_to_make_a_safe_token() {
        let temp = tempfile::tempdir().unwrap();
        let entry = fixture(&temp.path().join("source"), true);
        let alias = temp.path().join("alias");
        symlink(entry.path.parent().unwrap(), &alias).unwrap();
        for name in ["bridge", "bridge space"] {
            let parent = temp.path().join(name);
            fs::create_dir(&parent).unwrap();
            let logical = parent.join("../alias");
            let result = invocation(temp.path(), &entry, Some(&logical));
            if name == "bridge" {
                let result = result.unwrap().unwrap();
                let token = result.text.split_once(" @").unwrap().1;
                assert!(Path::new(token).is_absolute());
                assert!(Path::new(token).components().any(|c| c == std::path::Component::ParentDir));
                assert_reference(temp.path(), &entry, &result);
            } else { rejected(result); }
        }
    }

    #[test]
    fn absent_optional_directory_or_file_returns_only_the_native_slash() {
        let temp = tempfile::tempdir().unwrap();
        let entry = fixture(&temp.path().join("source with spaces"), false);
        for visibility in [SkillVisibility::Automatic, SkillVisibility::ManualOnly] {
            let mut entry = entry.clone();
            entry.visibility = visibility;
            let result = invocation(temp.path(), &entry, None).unwrap().unwrap();
            assert_eq!(result.text, format!("/{}", entry.id));
            assert!(!result.includes_adaptation);
        }
        fs::create_dir(entry.path.parent().unwrap().join("local-data")).unwrap();
        assert!(!invocation(temp.path(), &entry, None).unwrap().unwrap().includes_adaptation);
    }

    #[test]
    fn unsupported_states_and_sources_are_none_without_filesystem_access() {
        let temp = tempfile::tempdir().unwrap();
        let mut entry = skill(temp.path().join("nonexistent/SECRET/SKILL.md"));
        for visibility in [SkillVisibility::Disabled, SkillVisibility::Shadowed,
            SkillVisibility::Unverified, SkillVisibility::NameOnly, SkillVisibility::Conditional]
        {
            entry.visibility = visibility;
            assert!(invocation(temp.path(), &entry, None).unwrap().is_none());
        }
        entry.visibility = SkillVisibility::Automatic;
        entry.user_invocable = false;
        assert!(invocation(temp.path(), &entry, None).unwrap().is_none());
        entry.user_invocable = true;
        for source in [SkillSource::Builtin, SkillSource::Plugin { id: SECRET.into(), display_name: SECRET.into() }] {
            entry.source = source;
            assert!(invocation(temp.path(), &entry, None).unwrap().is_none());
        }
        entry.source = SkillSource::Filesystem;
        entry.scope = SkillScope::System;
        assert!(invocation(temp.path(), &entry, None).unwrap().is_none());
    }

    #[test]
    fn canonical_scope_names_qualify_bare_ids_and_preserve_matching_qualified_ids() {
        let temp = tempfile::tempdir().unwrap();
        for adaptation in [false, true] {
            let directory = if adaptation { "with-adaptation" } else { "without-adaptation" };
            let original = fixture(&temp.path().join(directory), adaptation);
            for (scope, prefix) in [(SkillScope::Local, "local"),
                (SkillScope::Repository, "repo"), (SkillScope::User, "user")]
            {
                for bare in ["review", "help", "always-approve", "manual_1-test.v2"] {
                    for id in [bare.to_owned(), format!("{prefix}:{bare}")] {
                        let mut entry = original.clone();
                        entry.scope = scope;
                        entry.id = id;
                        let result = invocation(temp.path(), &entry, None).unwrap().unwrap();
                        let suffix = if adaptation {
                            format!(" @{directory}/local-data/ADAPTATION.md")
                        } else { String::new() };
                        assert_eq!(result.text, format!("/{prefix}:{bare}{suffix}"));
                        assert_eq!(result.includes_adaptation, adaptation);
                    }
                }
            }
        }
    }

    #[test]
    fn qualified_server_ids_preserve_the_native_namespace_under_user_display_scope() {
        let temp = tempfile::tempdir().unwrap();
        let mut entry = fixture(&temp.path().join("source"), true);
        entry.id = "server:review".into();
        let result = invocation(temp.path(), &entry, None).unwrap().unwrap();
        assert_eq!(result.text, "/server:review @source/local-data/ADAPTATION.md");
        assert!(result.includes_adaptation);
        // A bare id under the same display scope still selects User, not Server.
        entry.id = "review".into();
        assert_eq!(invocation(temp.path(), &entry, None).unwrap().unwrap().text,
            "/user:review @source/local-data/ADAPTATION.md");
    }

    #[test]
    fn qualified_ids_with_a_different_scope_are_fixed_errors() {
        let temp = tempfile::tempdir().unwrap();
        let mut entry = fixture(&temp.path().join("source"), false);
        for (scope, expected) in [(SkillScope::Local, "local"),
            (SkillScope::Repository, "repo"), (SkillScope::User, "user")]
        {
            entry.scope = scope;
            for prefix in ["local", "repo", "user", "server", "plugin", "User", ""] {
                if prefix == expected || (scope == SkillScope::User && prefix == "server") { continue; }
                entry.id = format!("{prefix}:review");
                rejected(invocation(temp.path(), &entry, None));
            }
        }
    }

    #[test]
    fn invalid_ids_are_fixed_errors_without_display_name_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let mut entry = fixture(&temp.path().join("source"), false);
        for id in ["", "/manual", "a/b", "manual args", "manual\n", "manual\0", "中文",
            "manual\u{a0}args", "@manual", "manual\\args", "manual%20args", "\"manual\"",
            "user:", "user:user:help", "user:help:extra", "user:/help", "user:help args",
            "server:", "server:server:help", "server:/help", "server:help args"]
        {
            entry.id = id.into();
            rejected(invocation(temp.path(), &entry, None));
        }
    }

    #[test]
    fn wrong_alias_same_name_and_non_skill_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let entry = fixture(&temp.path().join("one/manual"), false);
        let other = fixture(&temp.path().join("two/manual"), false);
        let alias = temp.path().join(SECRET);
        symlink(other.path.parent().unwrap(), &alias).unwrap();
        rejected(invocation(temp.path(), &entry, Some(&alias)));
        rejected(invocation(temp.path(), &entry, Some(&entry.path)));
        let mut wrong = entry.clone();
        wrong.path = entry.path.parent().unwrap().to_path_buf();
        rejected(invocation(temp.path(), &wrong, None));
        wrong.path = temp.path().join(SECRET).join("missing/SKILL.md");
        rejected(invocation(temp.path(), &wrong, None));
    }

    #[test]
    fn skill_updates_change_the_revision_without_adopting_a_new_source_directory() {
        let temp = tempfile::tempdir().unwrap();
        let entry = fixture(&temp.path().join("source"), false);
        let before = invocation(temp.path(), &entry, None).unwrap().unwrap();
        fs::write(&entry.path, "a different native header and body length").unwrap();
        let after = invocation(temp.path(), &entry, None).unwrap().unwrap();
        assert_eq!(before.source_identity, after.source_identity);
        assert_eq!(before.context_identity, after.context_identity);
        assert_ne!(before.source_revision, after.source_revision);
    }

    #[test]
    fn leading_at_directories_keep_the_bound_source_after_both_native_parser_layers() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let entry = fixture(&temp.path().join("library/source"), true);
        // A competing, readable file must never be selected by stripping @.
        fixture(&project.join("chosen"), true);
        for name in ["@chosen", "@@chosen"] {
            let alias = project.join(name);
            symlink(entry.path.parent().unwrap(), &alias).unwrap();
            let result = invocation(&project, &entry, Some(&alias)).unwrap().unwrap();
            assert!(result.text.contains(" @./@"));
            let collector_token = result.text.split_once(" @").unwrap().1;
            // Relevant portion of the saved native FileReference regex: its
            // optional leading @ is consumed a second time after collection.
            let parsed_path = collector_token.strip_prefix('@').unwrap_or(collector_token);
            assert_eq!(project.join(parsed_path).canonicalize().unwrap(),
                entry.path.parent().unwrap().join("local-data/ADAPTATION.md").canonicalize().unwrap());
            assert_ne!(project.join(parsed_path).canonicalize().unwrap(),
                project.join("chosen/local-data/ADAPTATION.md").canonicalize().unwrap());
        }
    }

    #[test]
    fn replacing_the_current_project_during_construction_is_rejected_without_adaptation() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir(&project).unwrap();
        let entry = fixture(&temp.path().join("source"), false);
        let original = temp.path().join("original-project");
        let changed = project.clone();
        unix::AFTER_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            fs::rename(&changed, original).unwrap();
            fs::create_dir(changed).unwrap();
        })));
        rejected(invocation(&project, &entry, None));
    }

    fn fifo(path: &Path) {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn links_and_non_regular_nodes_never_become_optional_absence() {
        for node in ["SKILL.md", "local-data", "local-data/ADAPTATION.md"] {
            for kind in ["link", "dangling", "fifo", "wrong-type"] {
                let temp = tempfile::tempdir().unwrap();
                let source = temp.path().join("source");
                let entry = fixture(&source, false);
                if node == "SKILL.md" { fs::remove_file(&entry.path).unwrap(); }
                if node.starts_with("local-data/") { fs::create_dir(source.join("local-data")).unwrap(); }
                let node_path = source.join(node);
                let target = temp.path().join(SECRET);
                match kind {
                    "link" | "dangling" => {
                        if kind == "link" {
                            if node == "local-data" { fs::create_dir(&target).unwrap(); }
                            else { fs::write(&target, SECRET).unwrap(); }
                        }
                        symlink(&target, &node_path).unwrap();
                    }
                    "fifo" => fifo(&node_path),
                    _ if node == "local-data" => fs::write(&node_path, SECRET).unwrap(),
                    _ => fs::create_dir(&node_path).unwrap(),
                }
                rejected(invocation(temp.path(), &entry, None));
            }
        }
    }

    #[test]
    fn unsafe_reference_tokens_are_errors_without_escaping_or_canonical_fallback() {
        for name in ["has space", "has\tcontrol", "has\ncontrol", "has\u{a0}space", "has\u{2003}space", "has\u{7f}control"] {
            let temp = tempfile::tempdir().unwrap();
            let entry = fixture(&temp.path().join(name), true);
            rejected(invocation(temp.path(), &entry, None));
            // A safe canonical target must not replace an unsafe selected alias.
            let safe = fixture(&temp.path().join("safe"), true);
            let alias = temp.path().join(format!("alias-{name}"));
            symlink(safe.path.parent().unwrap(), &alias).unwrap();
            rejected(invocation(temp.path(), &safe, Some(&alias)));
        }
        let temp = tempfile::tempdir().unwrap();
        use std::os::unix::ffi::OsStringExt;
        let entry = fixture(&temp.path().join("safe"), true);
        // APFS cannot create this name. Exercise rejection as caller input,
        // without requiring the filesystem to represent a non-UTF-8 fixture.
        let invalid = temp.path().join(std::ffi::OsString::from_vec(b"invalid-\xff".to_vec()));
        let mut invalid_source = entry.clone();
        invalid_source.path = invalid.join("SKILL.md");
        rejected(invocation(temp.path(), &invalid_source, None));
        rejected(invocation(temp.path(), &entry, Some(&invalid)));
    }

    #[test]
    fn replacements_at_the_final_checkpoint_are_rejected() {
        for target in ["source", "alias", "SKILL.md", "local-data", "local-data/ADAPTATION.md"] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let entry = fixture(&source, true);
            let alias = temp.path().join("alias");
            symlink(&source, &alias).unwrap();
            let path = match target {
                "source" => source.clone(),
                "alias" => alias.clone(),
                _ => source.join(target),
            };
            let displaced = temp.path().join("displaced");
            let is_directory = matches!(target, "source" | "local-data");
            unix::AFTER_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                fs::rename(&path, &displaced).unwrap();
                if is_directory { fs::create_dir(&path).unwrap(); }
                else { fs::write(&path, SECRET).unwrap(); }
            })));
            rejected(invocation(temp.path(), &entry, Some(&alias)));
        }
    }

    #[test]
    fn optional_absence_is_rechecked_before_returning() {
        for missing_directory in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let entry = fixture(&source, false);
            let local = source.join("local-data");
            if !missing_directory { fs::create_dir(&local).unwrap(); }
            unix::AFTER_BIND.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
                if missing_directory { fs::create_dir(&local).unwrap(); }
                else { fs::write(local.join("ADAPTATION.md"), SECRET).unwrap(); }
            })));
            rejected(invocation(temp.path(), &entry, None));
        }
    }
}
