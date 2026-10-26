//! Retain native User sources omitted from the current Grok catalog.
//! No directory discovery, config parsing, process runner or name-based identity.
//! Source basis: xai-org/grok-build@2bdd1d6a, prompt/skills.rs:66-127,
//! 191-242, 382-468; inspect/mod.rs:911-946, 994-1004. That commit is NOT
//! established as the build commit of the pinned 1.0.50 artifact.

use std::path::{Path, PathBuf};

use super::{DiscoveryContext, NativeSkillMetadata};
use super::grok_state::{self, NativeCatalog, NativeSkill, SourceKind};

#[derive(Default)]
pub(super) struct Retention {
    pub shadowed: Vec<NativeSkill>,
    pub metadata_only: Vec<NativeSkillMetadata>,
    pub unknown: bool,
}

impl Retention {
    fn unavailable() -> Self { Self { unknown: true, ..Self::default() } }
}

fn same_cwd_retention(current: &NativeCatalog, same_settings: bool) -> Retention {
    if !same_settings || current.project_root.is_some() || current.skills.iter().any(|skill|
        matches!(skill.source.kind, SourceKind::Project | SourceKind::ConfigToml))
    { Retention::unavailable() } else { Retention::default() }
}

pub(super) fn metadata(native: NativeSkill) -> NativeSkillMetadata {
    NativeSkillMetadata {
        name: native.name, description: native.description,
        source_type: native.source.source_type, source_path: native.source.path,
        source_identity: native.source.plugin_name, invocation_name: native.invocable_as,
        user_invocable: native.user_invocable, disabled: native.disabled,
    }
}

// The real 1.0.50 evidence establishes omission for matching installation
// directory names. Do not extrapolate that to arbitrary equal header labels.
// This is a collision precondition, never a source identity or discovery root.
fn installation_key(skill: &NativeSkill) -> Option<&std::ffi::OsStr> {
    let path = skill.source.path.as_ref()?;
    if path.file_name()? != "SKILL.md" { return None; }
    path.parent()?.file_name()
}

enum ConfigIdentity {
    Absent,
    File(PathBuf),
    Unknown,
}

// Official project_config.rs:41-85: exclude the selected user config by
// exact/canonical identity. Any other existing config can change skills.ignore;
// its contents and the presence of ConfigToml skill rows are irrelevant.
fn config_is_global_or_absent(
    candidate: &Path,
    user_config: &Path,
    mut inspect: impl FnMut(&Path) -> ConfigIdentity,
) -> bool {
    if candidate == user_config { return true; }
    match inspect(candidate) {
        ConfigIdentity::Absent => true,
        ConfigIdentity::File(candidate) => match inspect(user_config) {
            ConfigIdentity::File(user) => candidate == user,
            _ => false,
        },
        ConfigIdentity::Unknown => false,
    }
}

// The native report's projectRoot comes from git2 workdir discovery, not a
// skill directory or the host's project picker. Match official RepoDirChain:
// no repo (including a home git root) -> cwd only; otherwise cwd through the
// worktree root, stopping by canonical identity. Never infer .git/worktrees
// paths. Unresolvable/non-ancestor boundaries or an excessive chain are Unknown.
fn project_config_candidates(
    catalog: &NativeCatalog,
    home: &Path,
    mut canonical_dir: impl FnMut(&Path) -> Option<PathBuf>,
) -> Option<Vec<PathBuf>> {
    let home_key = canonical_dir(home)?;
    let root_key = match catalog.project_root.as_deref() {
        Some(root) => Some(canonical_dir(root)?).filter(|root| root != &home_key),
        None => None,
    };
    let mut dir = catalog.cwd.as_path();
    let mut candidates = Vec::new();
    for _ in 0..256 {
        let key = canonical_dir(dir)?;
        candidates.push(dir.join(".grok/config.toml"));
        if root_key.as_ref().is_none_or(|root| root == &key) {
            candidates.reverse(); // Official project config order: root first.
            return Some(candidates);
        }
        dir = dir.parent()?;
    }
    None
}

fn project_settings_match(
    catalog: &NativeCatalog,
    home: &Path,
    user_config: &Path,
    canonical_dir: impl FnMut(&Path) -> Option<PathBuf>,
    mut inspect: impl FnMut(&Path) -> ConfigIdentity,
) -> bool {
    project_config_candidates(catalog, home, canonical_dir).is_some_and(|candidates|
        candidates.iter().all(|candidate|
            config_is_global_or_absent(candidate, user_config, &mut inspect)))
}

#[cfg(all(unix, not(test)))]
fn canonical_directory(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok().filter(|path| path.is_dir())
}

#[cfg(all(unix, not(test)))]
fn config_identity(path: &Path) -> ConfigIdentity {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigIdentity::Absent,
        Err(_) => ConfigIdentity::Unknown,
        Ok(_) => match path.canonicalize() {
            Ok(canonical) if canonical.is_file() => ConfigIdentity::File(canonical),
            _ => ConfigIdentity::Unknown,
        },
    }
}

/// All execution remains inside the existing pinned, bounded native reader.
/// Calls are sequential and never supply --trust. Root observations detect
/// changes at checkpoints; they do not claim an atomic environment snapshot.
#[cfg(all(unix, not(test)))]
pub(super) fn read(context: &DiscoveryContext)
    -> Result<(NativeCatalog, Retention), grok_state::Diagnostic>
{
    let selected_root = crate::agents::grok::grok_config_root(&context.home)
        .map_err(|_| grok_state::Diagnostic::ConfigRootUnknown)?;
    let current = grok_state::read(context)?;
    let root_unchanged = || crate::agents::grok::grok_config_root(&context.home)
        .is_ok_and(|root| root == selected_root);
    let Some(home) = context.home.canonicalize().ok().filter(|path| path.is_dir()) else {
        return Ok((current, Retention::unavailable()));
    };
    if !root_unchanged() { return Ok((current, Retention::unavailable())); }
    let user_config = selected_root.join("config.toml");
    let home_settings_match = config_is_global_or_absent(
        &home.join(".grok/config.toml"), &user_config, config_identity);
    let current_settings_match = project_settings_match(
        &current, &home, &user_config, canonical_directory, config_identity);
    if current.cwd == home {
        // The current catalog already IS the home projection. A project entry
        // here cannot be disambiguated by repeating the identical native read.
        let retention = same_cwd_retention(&current, home_settings_match && current_settings_match);
        return Ok((current, retention));
    }
    let home_context = DiscoveryContext {
        cwd: home.clone(), home: context.home.clone(), repository_root: None,
    };
    let retention = match grok_state::read(&home_context) {
        Ok(global) => retain(&current, global, &home,
            root_unchanged() && home_settings_match && current_settings_match
                && config_is_global_or_absent(&home.join(".grok/config.toml"), &user_config, config_identity)
                && project_settings_match(&current, &home, &user_config, canonical_directory, config_identity), |path|
            path.canonicalize().ok().filter(|_| path.is_file())),
        Err(_) => Retention::unavailable(),
    };
    Ok((current, retention))
}

// Preserve the reader's cfg(test) prohibition on starting any installed Agent.
#[cfg(any(test, not(unix)))]
pub(super) fn read(context: &DiscoveryContext)
    -> Result<(NativeCatalog, Retention), grok_state::Diagnostic>
{
    grok_state::read(context).map(|current| (current, Retention::default()))
}

/// Pure metadata selection; canonical identities are supplied by the caller.
/// A home inspect is not a native "global only" API: trust/project settings
/// there can change selection. Such projections retain metadata, never assert
/// shadowing. Inspect labels may also be re-keyed display names, so aliases
/// and collision annotations cannot establish a bare-name override.
pub(super) fn retain(
    current: &NativeCatalog,
    global: NativeCatalog,
    home: &Path,
    same_settings: bool,
    mut canonical_file: impl FnMut(&Path) -> Option<PathBuf>,
) -> Retention {
    let clean_projection = same_settings && global.cwd == home && current.cwd != global.cwd
        && global.grok_version == current.grok_version && current.grok_version == "1.0.50"
        && global.project_root.is_none()
        && !global.skills.iter().any(|skill|
            matches!(skill.source.kind, SourceKind::Project | SourceKind::ConfigToml));
    let mut result = Retention { unknown: !clean_projection, ..Retention::default() };
    let identities = current.skills.iter().map(|skill|
        skill.source.path.as_deref().and_then(&mut canonical_file)).collect::<Vec<_>>();
    let all_current_identities_known = identities.iter().all(Option::is_some);
    let mut seen = identities.iter().flatten().cloned().collect::<std::collections::HashSet<_>>();
    let name_counts = |catalog: &NativeCatalog| {
        let mut counts = std::collections::HashMap::<String, usize>::new();
        for skill in &catalog.skills { *counts.entry(skill.name.clone()).or_default() += 1; }
        counts
    };
    let current_names = name_counts(current);
    let global_names = name_counts(&global);
    for native in global.skills {
        // Never import the home's project, plugins, config paths or bundled
        // entries as User sources. Unknown native labels remain diagnostic.
        if !matches!(native.source.kind, SourceKind::User | SourceKind::Unknown) { continue; }
        let identity = native.source.path.as_deref().and_then(&mut canonical_file);
        if let Some(identity) = &identity {
            if !seen.insert(identity.clone()) { continue; }
        }
        // A re-keyed SKILL keeps its old display label alongside the original
        // name winner. Require unique labels in BOTH reports. Commands can
        // have independent display labels, so their omissions stay unknown.
        let bare_name = |skill: &NativeSkill| skill.collides_with.is_none() && skill.invocable_as.is_none()
            && installation_key(skill).is_some();
        let winner = current.skills.iter().zip(&identities).any(|(skill, key)|
            skill.source.kind == SourceKind::Project && current.project_trusted
                && skill.name == native.name && bare_name(skill)
                && installation_key(skill) == installation_key(&native)
                && current_names.get(&skill.name) == Some(&1)
                && key.is_some() && key != &identity);
        if clean_projection && all_current_identities_known && identity.is_some()
            && native.source.kind == SourceKind::User && bare_name(&native) && winner
            && global_names.get(&native.name) == Some(&1)
            // Disabled is an observed home status, not a current-project
            // status. Keep its explicit flag in diagnostic metadata rather
            // than erase it in EffectiveSkill's single visibility field.
            && !native.disabled
        {
            result.shadowed.push(native);
        } else {
            result.unknown = true;
            result.metadata_only.push(metadata(native));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn catalog(cwd: &str, trusted: bool, skills: Value) -> NativeCatalog {
        grok_state::parse_json(&serde_json::to_vec(&json!({
            "grokVersion":"1.0.50", "cwd":cwd, "projectRoot":null,
            "projectTrusted":trusted, "skills":skills,
        })).unwrap()).unwrap()
    }

    fn skill(kind: &str, path: &str, name: &str) -> Value {
        json!({"name":name,"description":"fixture metadata",
            "source":{"type":kind,"path":path},"userInvocable":true})
    }

    fn identity(path: &Path) -> Option<PathBuf> { Some(path.to_path_buf()) }

    #[test]
    fn project_override_retains_distinct_native_user_and_flags() {
        let current = catalog("/project", true, json!([skill("project", "/project/same/SKILL.md", "same")]));
        let mut user = skill("user", "/custom-global/same/SKILL.md", "same");
        user["userInvocable"] = json!(false);
        let global = catalog("/home", true, json!([user]));
        let result = retain(&current, global, Path::new("/home"), true, identity);
        assert!(!result.unknown);
        assert_eq!(result.shadowed.len(), 1);
        assert!(!result.shadowed[0].disabled && !result.shadowed[0].user_invocable);
    }

    #[test]
    fn canonical_aliases_never_add_a_second_source_even_with_another_name() {
        let current = catalog("/project", true, json!([skill("project", "/alias/shared/SKILL.md", "same")]));
        let global = catalog("/home", false, json!([
            skill("user", "/canonical/shared/SKILL.md", "different-name"),
            skill("user", "/new/shared/SKILL.md", "same"),
            skill("user", "/new-alias/shared/SKILL.md", "third-name")
        ]));
        let result = retain(&current, global, Path::new("/home"), true, |path|
            Some(PathBuf::from(match path.to_str().unwrap() {
                "/alias/shared/SKILL.md" => "/canonical/shared/SKILL.md",
                "/new-alias/shared/SKILL.md" => "/new/shared/SKILL.md", other => other,
            })));
        assert!(!result.unknown); assert_eq!(result.shadowed.len(), 1);
        assert!(result.metadata_only.is_empty());
    }

    #[test]
    fn contaminated_or_nonindependent_home_projection_retains_only_unknown_metadata() {
        let current = catalog("/project", true, json!([skill("project", "/project/same/SKILL.md", "same")]));
        for case in 0..6 {
            let mut global = catalog("/home", false, json!([skill("user", "/global/same/SKILL.md", "same")]));
            match case {
                0 => global.project_root = Some(PathBuf::from("/home")),
                1 => global.cwd = current.cwd.clone(),
                2 => global.cwd = PathBuf::from("/wrong"),
                3 => global.grok_version = "unbound".into(),
                4 => global.skills.push(catalog("/home", false, json!([
                    skill("project", "/home/local/SKILL.md", "local")])).skills.remove(0)),
                _ => {}
            }
            let result = retain(&current, global, Path::new("/home"), case != 5, identity);
            assert!(result.unknown, "case {case}");
            assert!(result.shadowed.is_empty()); assert_eq!(result.metadata_only.len(), 1);
        }
    }

    #[test]
    fn missing_identity_unmatched_name_alias_or_untrusted_project_cannot_prove_shadowing() {
        for case in 0..7 {
            let mut winner = skill("project", "/project/same/SKILL.md", "same");
            let mut user = skill("user", "/global/same/SKILL.md", "same");
            match case {
                0 => winner["name"] = json!("other"),
                1 => winner["invocableAs"] = json!("alias"),
                2 => user["collidesWith"] = json!("same"),
                3 => winner["source"]["type"] = json!("configToml"),
                _ => {}
            }
            let current = catalog("/project", case != 4, json!([winner]));
            let global = catalog("/home", false, json!([user]));
            let result = retain(&current, global, Path::new("/home"), true, |path|
                if (case == 5 && path.starts_with("/global")) || (case == 6 && path.starts_with("/project"))
                { None } else { identity(path) });
            assert!(result.unknown, "case {case}");
            assert!(result.shadowed.is_empty()); assert_eq!(result.metadata_only.len(), 1);
        }
    }

    #[test]
    fn unknown_native_source_keeps_original_hints_and_nonuser_entries_are_not_imported() {
        let current = catalog("/project", true, json!([]));
        let global = catalog("/home", false, json!([
            skill("future", "/opaque/SKILL.md", "opaque"),
            skill("user", "relative-invalid", "missing-path"),
            skill("bundled", "/bundle/SKILL.md", "bundle"),
            skill("configToml", "/configured/SKILL.md", "configured")
        ]));
        let result = retain(&current, global, Path::new("/home"), true, identity);
        assert!(result.unknown); assert!(result.shadowed.is_empty());
        assert_eq!(result.metadata_only.len(), 2);
        assert_eq!(result.metadata_only[0].source_type, "future");
        assert_eq!(result.metadata_only[0].source_path, Some(PathBuf::from("/opaque/SKILL.md")));
        assert!(result.metadata_only[1].source_path.is_none());
    }

    #[test]
    fn unavailable_projection_is_unknown_and_test_entry_never_starts_native() {
        let result = Retention::unavailable();
        assert!(result.unknown && result.shadowed.is_empty() && result.metadata_only.is_empty());
        let context = DiscoveryContext { cwd: "/synthetic-project".into(), home: "/synthetic-home".into(), repository_root: None };
        assert!(read(&context).is_err());
    }

    #[test]
    fn home_equals_cwd_keeps_default_native_user_without_duplicate_read_or_project_inference() {
        // Mirrors the controller's fresh unset-GROK_HOME observation: a trusted
        // home report still classifies ~/.grok/skills as native User.
        let current = catalog("/home", true, json!([
            skill("user", "/home/.grok/skills/shared/SKILL.md", "pleat-default-global-identity")
        ]));
        let result = same_cwd_retention(&current, true);
        assert!(!result.unknown && result.shadowed.is_empty());
        assert!(same_cwd_retention(&current, false).unknown);
        let project = catalog("/home", true, json!([
            skill("project", "/home/project/SKILL.md", "project")
        ]));
        assert!(same_cwd_retention(&project, true).unknown);
    }

    #[test]
    fn display_rekey_ambiguity_keeps_all_distinct_sources_as_metadata() {
        let current = catalog("/project", true, json!([
            skill("project", "/project/shared/SKILL.md", "same")
        ]));
        let global = catalog("/home", true, json!([
            skill("user", "/global/first/SKILL.md", "same"),
            skill("user", "/global/rekeyed/SKILL.md", "same")
        ]));
        let result = retain(&current, global, Path::new("/home"), true, identity);
        assert!(result.unknown && result.shadowed.is_empty());
        assert_eq!(result.metadata_only.len(), 2);
        assert_ne!(result.metadata_only[0].source_path, result.metadata_only[1].source_path);
    }

    #[test]
    fn two_native_projection_shape_keeps_current_counts_rendering_and_invocation_flags() {
        use super::super::{adapters::estimate_grok_catalog_with_retention, grok_metadata::Flags,
            SkillVisibility};
        // Same labels / distinct canonical sources, matching the controller's
        // immutable real-native observation; directory names need not equal names.
        let mut disabled = skill("project", "/project/disabled/SKILL.md", "disabled");
        disabled["disabled"] = json!(true);
        let current = catalog("/project", true, json!([
            skill("project", "/project/.grok/skills/shared/SKILL.md", "pleat-owned-shared-name"),
            skill("project", "/project/manual/SKILL.md", "manual"), disabled,
        ]));
        let mut manual = skill("user", "/custom-grok/manual/SKILL.md", "manual");
        manual["userInvocable"] = json!(false);
        let global_disabled = skill("user", "/custom-grok/disabled/SKILL.md", "disabled");
        let global = catalog("/home", true, json!([
            skill("user", "/custom-grok/skills/shared/SKILL.md", "pleat-owned-shared-name"),
            manual, global_disabled,
        ]));
        let flags = |path: &Path| Some(Flags {
            user_invocable: true, disable_model_invocation: path.starts_with("/project/manual"),
        });
        let baseline = estimate_grok_catalog_with_retention(current.clone(), Retention::default(), flags);
        let retained = retain(&current, global, Path::new("/home"), true, identity);
        assert!(!retained.unknown); assert_eq!(retained.shadowed.len(), 3);
        let mut reads = 0;
        let estimate = estimate_grok_catalog_with_retention(current, retained, |path| {
            assert!(path.starts_with("/project")); reads += 1; flags(path)
        });
        assert_eq!(reads, 3); assert_eq!(estimate.skills.len(), 6);
        assert!(estimate.diagnostic.is_none());
        assert_eq!(estimate.discovered_count, baseline.discovered_count + 3);
        assert_eq!(estimate.model_visible_count, 1);
        assert_eq!(estimate.manual_only_count, 1);
        assert_eq!(estimate.estimated_tokens, baseline.estimated_tokens);
        assert!(estimate.skills[3..].iter().all(|skill|
            skill.visibility == SkillVisibility::Shadowed && skill.scope == super::super::SkillScope::User
                && skill.prompt_path.is_none() && skill.root_path.is_none()));
        assert!(!estimate.skills[4].user_invocable);
    }

    #[test]
    fn native_home_disabled_status_is_preserved_without_asserting_current_invocation() {
        let current = catalog("/project", true, json!([
            skill("project", "/project/shared/SKILL.md", "same")
        ]));
        let mut disabled = skill("user", "/global/shared/SKILL.md", "same");
        disabled["disabled"] = json!(true); disabled["userInvocable"] = json!(false);
        let result = retain(&current, catalog("/home", true, json!([disabled])), Path::new("/home"), true, identity);
        assert!(result.unknown && result.shadowed.is_empty());
        assert!(result.metadata_only[0].disabled && !result.metadata_only[0].user_invocable);
    }

    #[test]
    fn existing_two_same_header_project_rows_stay_two_without_name_merging() {
        use super::super::{adapters::estimate_grok_catalog_with_retention, grok_metadata::Flags};
        let current = catalog("/project", true, json!([
            skill("project", "/project/.grok/skills/different-alias-0/SKILL.md", "pleat-shared-header-name"),
            skill("project", "/project/.grok/skills/different-alias-1/SKILL.md", "pleat-shared-header-name")
        ]));
        let retained = retain(&current, catalog("/home", true, json!([])), Path::new("/home"), true, identity);
        assert!(!retained.unknown && retained.shadowed.is_empty());
        let estimate = estimate_grok_catalog_with_retention(current, retained, |_| Some(Flags::default()));
        assert_eq!(estimate.skills.len(), 2); assert_eq!(estimate.model_visible_count, 2);
        assert!(estimate.diagnostic.is_none());
        assert_ne!(estimate.skills[0].path, estimate.skills[1].path);
        assert_eq!(estimate.skills[0].name, estimate.skills[1].name);
    }

    #[test]
    fn four_compatibility_aliases_of_one_canonical_file_never_add_sources() {
        let current = catalog("/project", true, json!([
            skill("project", "/project/.grok/skills/different-alias-0/SKILL.md", "same")
        ]));
        let aliases = [".grok", ".agents", ".claude", ".cursor"].iter().enumerate()
            .map(|(index, root)| skill("user", &format!("/home/{root}/skills/different-alias-{index}/SKILL.md"), "same"))
            .collect::<Vec<_>>();
        let result = retain(&current, catalog("/home", true, json!(aliases)), Path::new("/home"), true,
            |_| Some(PathBuf::from("/source/one/SKILL.md")));
        assert!(!result.unknown && result.shadowed.is_empty() && result.metadata_only.is_empty());
    }

    #[test]
    fn equal_headers_with_different_installation_directories_do_not_prove_an_override() {
        let current = catalog("/project", true, json!([
            skill("project", "/project/first/SKILL.md", "same")
        ]));
        let result = retain(&current, catalog("/home", true, json!([
            skill("user", "/global/other/SKILL.md", "same")
        ])), Path::new("/home"), true, identity);
        assert!(result.unknown && result.shadowed.is_empty());
        assert_eq!(result.metadata_only.len(), 1);
    }

    #[test]
    fn grg1_current_ignore_only_config_keeps_user_metadata_unknown_without_config_skill_rows() {
        use super::super::{adapters::estimate_grok_catalog_with_retention, grok_metadata::Flags};
        // Counterexample report shape: project config only sets skills.ignore
        // for /global/shared. Filtering precedes merging, so neither report
        // needs any ConfigToml skill row. We inspect only config identity.
        for (cwd, root, independent_config) in [
            ("/project", None, "/project/.grok/config.toml"),
            ("/repo/sub/work", Some("/repo"), "/repo/.grok/config.toml"),
            ("/repo/sub/work", Some("/repo"), "/repo/sub/.grok/config.toml"),
            ("/repo/sub/work", Some("/repo"), "/repo/sub/work/.grok/config.toml"),
        ] {
            let mut current = catalog(cwd, true, json!([
                skill("project", &format!("{cwd}/shared/SKILL.md"), "same")
            ]));
            current.project_root = root.map(PathBuf::from);
            assert!(current.skills.iter().all(|skill| skill.source.kind == SourceKind::Project));
            for unreadable in [false, true] {
                let same_settings = project_settings_match(&current, Path::new("/home"),
                    Path::new("/global/config.toml"), identity, |path| {
                        if path == Path::new(independent_config) {
                            if unreadable { ConfigIdentity::Unknown }
                            else { ConfigIdentity::File(path.to_path_buf()) }
                        } else if path == Path::new("/global/config.toml") {
                            ConfigIdentity::File(path.to_path_buf())
                        } else { ConfigIdentity::Absent }
                    });
                assert!(!same_settings);
                let retained = retain(&current, catalog("/home", true, json!([
                    skill("user", "/global/shared/SKILL.md", "same")
                ])), Path::new("/home"), same_settings, identity);
                assert!(retained.unknown && retained.shadowed.is_empty());
                assert_eq!(retained.metadata_only.len(), 1);
                assert_eq!(retained.metadata_only[0].source_path, Some(PathBuf::from("/global/shared/SKILL.md")));
                let estimate = estimate_grok_catalog_with_retention(current.clone(), retained, |_| Some(Flags::default()));
                assert_eq!(estimate.skills.len(), 1);
                let serialized = serde_json::to_value(estimate).unwrap();
                for field in ["discovered_count", "model_visible_count", "manual_only_count",
                    "name_only_count", "conditional_count", "estimated_tokens"] {
                    assert!(serialized[field].is_null(), "{independent_config}: {field}");
                }
            }
        }
    }

    #[test]
    fn grg1_config_candidates_follow_native_worktree_no_repo_and_home_boundaries() {
        let mut current = catalog("/worktrees/live/sub/work", true, json!([]));
        let home = Path::new("/home");
        assert_eq!(project_config_candidates(&current, home, identity).unwrap(),
            vec![PathBuf::from("/worktrees/live/sub/work/.grok/config.toml")]);
        current.project_root = Some(PathBuf::from("/worktree-alias"));
        assert_eq!(project_config_candidates(&current, home, |path|
            if path == Path::new("/worktree-alias") { Some(PathBuf::from("/worktrees/live")) }
            else { identity(path) }).unwrap(), [
                "/worktrees/live/.grok/config.toml", "/worktrees/live/sub/.grok/config.toml",
                "/worktrees/live/sub/work/.grok/config.toml",
            ].map(PathBuf::from));
        // RepoDirChain excludes a canonical HOME git root; no ancestor walk.
        current.cwd = PathBuf::from("/home/repo/sub");
        current.project_root = Some(PathBuf::from("/home-alias"));
        assert_eq!(project_config_candidates(&current, home, |path|
            if path == Path::new("/home-alias") { Some(home.to_path_buf()) }
            else { identity(path) }).unwrap().len(), 1);
        current.cwd = PathBuf::from("/worktrees/live/sub/work");
        current.project_root = Some(PathBuf::from("/unrelated"));
        assert!(project_config_candidates(&current, home, identity).is_none());
        current.project_root = Some(PathBuf::from("/worktrees/live"));
        assert!(project_config_candidates(&current, home, |path|
            if path == Path::new("/worktrees/live/sub") { None } else { identity(path) }).is_none());
        assert!(!project_settings_match(&current, home, Path::new("/global/config.toml"),
            |_| None, |_| panic!("uncertain boundary must not be treated as absent configs")));
    }

    #[test]
    fn grg1_user_config_exact_or_canonical_identity_is_excluded_but_uncertainty_is_not() {
        let candidate = Path::new("/project/.grok/config.toml");
        let user = Path::new("/global/config.toml");
        assert!(config_is_global_or_absent(candidate, candidate,
            |_| panic!("exact selected user config is not an independent overlay")));
        assert!(config_is_global_or_absent(candidate, user,
            |_| ConfigIdentity::File(PathBuf::from("/source/user-config.toml"))));
        assert!(config_is_global_or_absent(candidate, user, |_| ConfigIdentity::Absent));
        assert!(!config_is_global_or_absent(candidate, user, |path|
            if path == candidate { ConfigIdentity::File(PathBuf::from("/independent/config.toml")) }
            else { ConfigIdentity::File(PathBuf::from("/source/user-config.toml")) }));
        assert!(!config_is_global_or_absent(candidate, user, |path|
            if path == candidate { ConfigIdentity::File(PathBuf::from("/source/user-config.toml")) }
            else { ConfigIdentity::Unknown }));
        assert!(!config_is_global_or_absent(candidate, user, |_| ConfigIdentity::Unknown));
    }

    #[test]
    fn grg1_clean_current_chain_keeps_shadowed_positive_and_ignores_outside_worktree_configs() {
        let mut current = catalog("/worktrees/live/sub", true, json!([
            skill("project", "/worktrees/live/sub/shared/SKILL.md", "same")
        ]));
        current.project_root = Some(PathBuf::from("/worktrees/live"));
        let mut inspected = Vec::new();
        let same_settings = project_settings_match(&current, Path::new("/home"),
            Path::new("/global/config.toml"), identity, |path| {
                inspected.push(path.to_path_buf());
                match path.to_str().unwrap() {
                    "/worktrees/live/.grok/config.toml" | "/worktrees/live/sub/.grok/config.toml" => ConfigIdentity::Absent,
                    _ => panic!("must use native workdir chain, not main checkout, gitdir or outer ancestors"),
                }
            });
        assert!(same_settings); assert_eq!(inspected.len(), 2);
        let global = || catalog("/home", true, json!([
            skill("user", "/global/shared/SKILL.md", "same")
        ]));
        let result = retain(&current, global(), Path::new("/home"), same_settings, identity);
        assert!(!result.unknown); assert_eq!(result.shadowed.len(), 1);
        // Once an independent config appears at a later checkpoint, the same
        // two reports no longer establish a priority override.
        let later_settings = project_settings_match(&current, Path::new("/home"),
            Path::new("/global/config.toml"), identity,
            |path| ConfigIdentity::File(path.to_path_buf()));
        assert!(!later_settings);
        let result = retain(&current, global(), Path::new("/home"), same_settings && later_settings, identity);
        assert!(result.unknown && result.shadowed.is_empty()); assert_eq!(result.metadata_only.len(), 1);
        // Canonical User/Project aliases still do not become a second source.
        let result = retain(&current, global(), Path::new("/home"), false,
            |_| Some(PathBuf::from("/source/shared/SKILL.md")));
        assert!(result.shadowed.is_empty() && result.metadata_only.is_empty());
    }

    #[test]
    fn unverified_current_or_projection_keeps_serialized_counts_null_and_safe_metadata() {
        use super::super::{adapters::estimate_grok_catalog_with_retention, grok_metadata::Flags};
        for projection_unavailable in [true, false] {
            let current = catalog("/project", true, json!([
                skill("project", "/project/shared/SKILL.md", "same")
            ]));
            let retention = if projection_unavailable { Retention::unavailable() } else {
                retain(&current, catalog("/home", true, json!([
                    skill("user", "/global/other/SKILL.md", "unmatched")
                ])), Path::new("/home"), true, identity)
            };
            let estimate = estimate_grok_catalog_with_retention(current, retention, |_| Some(Flags::default()));
            let value = serde_json::to_value(&estimate).unwrap();
            for field in ["discovered_count", "model_visible_count", "manual_only_count", "estimated_tokens"] {
                assert!(value[field].is_null());
            }
            assert_eq!(estimate.diagnostic.as_ref().unwrap().code, "grok-native-global-projection-unverified");
            if !projection_unavailable {
                let diagnostic = estimate.diagnostic.as_ref().unwrap();
                assert_eq!(diagnostic.metadata_only[0].source_path, Some(PathBuf::from("/global/other/SKILL.md")));
                assert_eq!(estimate.skills.len(), 1);
            }
        }
        let current = catalog("/project", true, json!([
            skill("project", "/project/shared/SKILL.md", "same")
        ]));
        let estimate = estimate_grok_catalog_with_retention(current, Retention::default(), |_| None);
        assert_eq!(estimate.skills[0].visibility, super::super::SkillVisibility::Unverified);
        assert!(serde_json::to_value(estimate).unwrap()["discovered_count"].is_null());
    }
}
