//! The production seam for effective-Skill inspection.
//!
//! Each Agent adapter owns the static scan and metadata profile used before
//! its entry rules render the initial model catalog. Runtime loading is not
//! part of this module.

use std::collections::HashMap;

use super::additional_agents::{DimAgentAdapter, OmpAdapter};
use super::desktop_agents::{TraePolicy, WorkBuddyPolicy};
use super::scanner::ScanProfile;
use super::{
    AgentContextEstimate, AmpPolicy, AntigravityPolicy, ClaudeCodeAdapter, CodexAdapter,
    CopilotPolicy, CursorPolicy, DiscoveryContext, DroidPolicy, EffectiveSkill, GrokAdapter,
    HermesPolicy, MetadataProfile, OpenClawPolicy, OpenCodePolicy, PiAdapter, PromaAdapter, SkillScope, discover,
    estimate_skills, estimate_with_policy, estimate_with_profile,
};

trait AgentAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate;
}

macro_rules! legacy_adapter {
    ($adapter:ident, $policy:ident) => {
        struct $adapter;
        impl AgentAdapter for $adapter {
            fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
                estimate_with_policy(&$policy, context)
            }
        }
    };
}

impl AgentAdapter for CodexAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        estimate_with_profile(
            self,
            context,
            ScanProfile::Recursive {
                max_depth: 6,
                max_directories: 2_000,
                max_entries: 20_000,
            },
            MetadataProfile::StrictFrontmatter,
        )
    }
}

impl AgentAdapter for ClaudeCodeAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        estimate_with_profile(
            self,
            context,
            ScanProfile::DirectChildren,
            MetadataProfile::BodyFallback,
        )
    }
}

impl AgentAdapter for PiAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        estimate_with_profile(
            self,
            context,
            ScanProfile::PiIgnored,
            MetadataProfile::StrictFrontmatter,
        )
    }
}

impl AgentAdapter for PromaAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        let mut skills = discover(
            self,
            context,
            ScanProfile::PromaActive,
            MetadataProfile::StrictFrontmatter,
        );
        for skill in &mut skills {
            skill.when_to_use = None;
        }
        estimate_skills(self, skills)
    }
}

impl AgentAdapter for OmpAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        let skills = discover(
            self,
            context,
            ScanProfile::ProviderChildren,
            MetadataProfile::ProviderFrontmatter,
        );
        estimate_skills(
            self,
            super::additional_agents::resolve_omp_collisions(skills, context),
        )
    }
}

impl AgentAdapter for DimAgentAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        let mut skills = discover(
            self,
            context,
            ScanProfile::DimRecursive,
            MetadataProfile::StrictFrontmatter,
        );
        // Apply disabled IDs after resolving name shadowing, as DimAgent does.
        skills.retain(|skill| skill.visibility != super::SkillVisibility::ManualOnly);
        estimate_skills(self, skills)
    }
}

impl AgentAdapter for GrokAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        match super::grok_selection::read(context) {
            Ok((catalog, retention)) => estimate_grok_catalog_with_retention(
                catalog, retention, super::grok_metadata::read),
            Err(failure) => {
                let mut estimate = estimate_skills(self, Vec::new());
                estimate.diagnostic = Some(super::SkillDiscoveryDiagnostic {
                    code: "grok-native-inspect-unavailable".into(), message: failure.message().into(),
                    metadata_only: Vec::new(),
                });
                estimate
            }
        }
    }
}

/// Native supplies trust, merged configuration, canonical deduplication and
/// collision names. Only invocation flags absent from inspect need a bounded
/// header read; this function never calls the source trigger writer.
pub(super) fn estimate_grok_catalog(_context: &DiscoveryContext,
    catalog: super::grok_state::NativeCatalog) -> AgentContextEstimate
{
    estimate_grok_catalog_with_retention(catalog, super::grok_selection::Retention::default(),
        super::grok_metadata::read)
}

// Injectable metadata flags keep selection/count fixtures independent of native
// execution and filesystem header reads. Only the current catalog is rendered.
pub(super) fn estimate_grok_catalog_with_retention(
    catalog: super::grok_state::NativeCatalog,
    retention: super::grok_selection::Retention,
    mut read_flags: impl FnMut(&std::path::Path) -> Option<super::grok_metadata::Flags>,
) -> AgentContextEstimate {
    use super::{SkillScope, SkillSource, SkillVisibility};
    use super::grok_state::SourceKind;
    let mut unknown = catalog.has_unknown_sources();
    let mut skills = Vec::new();
    let mut metadata_only = Vec::new();
    let retain_metadata = super::grok_selection::metadata;
    for native in catalog.skills {
        let Some(path) = native.source.path.clone() else {
            unknown = true; metadata_only.push(retain_metadata(native)); continue;
        };
        let scope = match native.source.kind {
            SourceKind::Project if path.starts_with(&catalog.cwd) => SkillScope::Local,
            SourceKind::Project => SkillScope::Repository,
            SourceKind::User | SourceKind::Server => SkillScope::User,
            SourceKind::ConfigToml if catalog.project_root.as_ref().is_some_and(|root| path.starts_with(root)) => SkillScope::Repository,
            SourceKind::ConfigToml => SkillScope::User,
            SourceKind::Bundled => SkillScope::System,
            _ => { unknown = true; metadata_only.push(retain_metadata(native)); continue; }
        };
        if native.source.kind == SourceKind::Project && !catalog.project_trusted {
            unknown = true; metadata_only.push(retain_metadata(native)); continue;
        }
        let source = if native.source.kind == SourceKind::Bundled { SkillSource::Builtin }
            else { SkillSource::Filesystem };
        let flags = read_flags(&path);
        let visibility = if native.disabled { SkillVisibility::Disabled }
            else if let Some(flags) = flags.filter(|flags| flags.user_invocable == native.user_invocable) {
                if flags.disable_model_invocation {
                    if native.user_invocable { SkillVisibility::ManualOnly } else { SkillVisibility::Disabled }
                } else { SkillVisibility::Automatic }
            } else { unknown = true; SkillVisibility::Unverified };
        let root_path = path.parent().and_then(|dir| {
            if path.file_name().is_some_and(|name| name == "SKILL.md") { dir.parent() }
            else { Some(dir) }
        }).map(std::path::Path::to_path_buf);
        // inspect 1.0.50 omits invocableAs. Keep a scope-qualified identity for
        // native sources whose namespace is known, so manual entry cannot hit
        // a same-named builtin command. ConfigToml alone does not prove scope.
        let invocation_id = native.invocable_as.unwrap_or_else(|| {
            let prefix = match native.source.kind {
                SourceKind::User => Some("user"),
                SourceKind::Server => Some("server"),
                SourceKind::Project if scope == SkillScope::Local => Some("local"),
                SourceKind::Project => Some("repo"),
                _ => None,
            };
            prefix.map(|prefix| format!("{prefix}:{}", native.name))
                .unwrap_or_else(|| native.name.clone())
        });
        skills.push(EffectiveSkill {
            id: invocation_id, name: native.name,
            description: native.description, when_to_use: None,
            prompt_path: Some(path.display().to_string()), path, root_path,
            scope, source, visibility, user_invocable: native.user_invocable,
        });
    }
    let mut estimate = estimate_skills(&GrokAdapter, skills);
    // Append provenance after computing the current native model/manual counts.
    // These sources were reported at home, not loaded in the current project.
    for native in retention.shadowed {
        let Some(path) = native.source.path.clone() else {
            unknown = true; metadata_only.push(retain_metadata(native)); continue;
        };
        estimate.skills.push(EffectiveSkill {
            id: native.invocable_as.unwrap_or_else(|| native.name.clone()), name: native.name,
            description: native.description, when_to_use: None,
            root_path: None, prompt_path: None, path,
            scope: SkillScope::User, source: SkillSource::Filesystem,
            visibility: SkillVisibility::Shadowed, user_invocable: native.user_invocable,
        });
    }
    estimate.discovered_count = estimate.skills.len();
    metadata_only.extend(retention.metadata_only);
    if unknown || retention.unknown {
        estimate.diagnostic = Some(super::SkillDiscoveryDiagnostic {
            code: if retention.unknown { "grok-native-global-projection-unverified" }
                else { "grok-native-metadata-unverified" }.into(),
            message: if retention.unknown { "Grok 全局来源投影或覆盖关系尚未核验，统计未知；请查看来源并刷新" }
                else { "Grok 部分原生来源或调用元数据尚未核验，统计未知；请查看来源并刷新" }.into(),
            metadata_only,
        });
    }
    estimate
}

#[cfg(test)]
mod grok_manual_identity_tests {
    use super::*;
    use super::super::grok_state::{NativeCatalog, NativeSkill, NativeSource, SourceKind};

    #[test]
    fn builtin_like_names_keep_source_namespaces_and_config_scope_is_not_invented() {
        let cases = [
            (SourceKind::User, "/home/.grok/skills/review/SKILL.md", None, "user:review"),
            (SourceKind::Server, "/home/.grok/server-skills/review/SKILL.md", None, "server:review"),
            (SourceKind::Project, "/project/sub/.grok/skills/review/SKILL.md", None, "local:review"),
            (SourceKind::Project, "/project/.grok/skills/review/SKILL.md", None, "repo:review"),
            (SourceKind::ConfigToml, "/project/custom/review/SKILL.md", None, "review"),
            (SourceKind::Project, "/project/sub/.grok/skills/review/SKILL.md", Some("local:host-renamed"), "local:host-renamed"),
        ];
        for (kind, path, advertised, expected) in cases {
            let catalog = NativeCatalog {
                grok_version: "1.0.50".into(), cwd: "/project/sub".into(),
                project_root: Some("/project".into()), project_trusted: true,
                skills: vec![NativeSkill {
                    name: "review".into(), description: "native identity fixture".into(),
                    source: NativeSource { kind, source_type: format!("{kind:?}"),
                        path: Some(path.into()), plugin_name: None },
                    user_invocable: true, disabled: false, collides_with: None,
                    invocable_as: advertised.map(str::to_owned),
                }],
            };
            let estimate = estimate_grok_catalog_with_retention(catalog,
                super::super::grok_selection::Retention::default(),
                |_| Some(super::super::grok_metadata::Flags::default()));
            assert_eq!(estimate.skills[0].id, expected);
            assert_eq!(estimate.skills[0].name, "review");
            assert_eq!(estimate.skills[0].path, std::path::Path::new(path));
        }
    }
}

legacy_adapter!(CursorAdapter, CursorPolicy);
legacy_adapter!(OpenCodeAdapter, OpenCodePolicy);
struct CopilotAdapter;
impl AgentAdapter for CopilotAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        inspect_copilot_roots(context, super::copilot_roots(context))
    }
}

/// The production estimate boundary also accepts isolated root-resolution results.
/// Failure is unknown and never enters the scanner or retries default roots.
pub(super) fn inspect_copilot_roots(
    context: &DiscoveryContext,
    roots: anyhow::Result<Vec<super::SkillRoot>>,
) -> AgentContextEstimate {
    inspect_copilot_roots_after_grouping(context, roots, || {})
}

// The synchronous hook is a deterministic test seam after identity capture and
// before any default-header open; production supplies a no-op.
pub(super) fn inspect_copilot_roots_after_grouping(
    context: &DiscoveryContext,
    roots: anyhow::Result<Vec<super::SkillRoot>>,
    after_grouping: impl FnOnce(),
) -> AgentContextEstimate {
    match roots {
        Ok(mut roots) => {
            let standard_roots = copilot_standard_roots(context, &roots);
            if standard_roots {
                // Canonical deduplication must retain standard provenance even
                // when an alias root arrives before the two evidenced roots.
                roots.sort_by_key(|root| {
                    if root.path == context.cwd.join(".github/skills") { 0 }
                    else if root.path == context.home.join(".copilot/skills") { 1 }
                    else { 2 }
                });
            }
            let policy = CopilotPolicy { roots };
            let estimate = estimate_with_policy(&policy, context);
            let (identities, mut skills): (Vec<_>, Vec<EffectiveSkill>) = super::group_effective_skills_with_identity(
                estimate.skills.iter().map(|skill| (super::AgentKind::Copilot, skill)),
            ).into_iter().map(|(canonical, group)| {
                (canonical.and_then(CopilotSourceIdentity::capture), group.entries()[0].1.clone())
            }).unzip();
            after_grouping();
            let mut names = HashMap::<String, Vec<usize>>::new();
            for (index, skill) in skills.iter().enumerate() {
                names.entry(skill.name.clone()).or_default().push(index);
            }
            let mut unknown = false;
            for indices in names.values().filter(|indices| indices.len() > 1) {
                let project = context.cwd.join(".github/skills");
                let personal = context.home.join(".copilot/skills");
                let verified_pair = standard_roots && indices.len() == 2
                    && indices.iter().all(|&index| {
                        let skill = &skills[index];
                        skill.source == super::SkillSource::Filesystem
                            && skill.visibility == super::SkillVisibility::Automatic
                            && skill.user_invocable
                    })
                    && indices.iter().any(|&index| skills[index].root_path.as_ref() == Some(&project)
                        && skills[index].scope == SkillScope::Repository)
                    && indices.iter().any(|&index| skills[index].root_path.as_ref() == Some(&personal)
                        && skills[index].scope == SkillScope::User)
                    && copilot_pair_metadata(&skills, indices, &identities);
                for &index in indices {
                    skills[index].visibility = if verified_pair {
                        if skills[index].scope == SkillScope::Repository {
                            super::SkillVisibility::Automatic
                        } else {
                            super::SkillVisibility::Shadowed
                        }
                    } else if matches!(skills[index].visibility,
                        super::SkillVisibility::Automatic | super::SkillVisibility::NameOnly)
                    {
                        super::SkillVisibility::Unverified
                    } else {
                        skills[index].visibility
                    };
                }
                unknown |= !verified_pair;
            }
            let mut estimate = estimate_skills(&policy, skills);
            if unknown {
                estimate.diagnostic = Some(super::SkillDiscoveryDiagnostic {
                    code: "copilot_same_name_selection_unknown".into(),
                    message: "发现同名技能的不同来源；当前来源或调用模式不在已验证的标准双源选择范围内".into(),
                    metadata_only: Vec::new(),
                });
            }
            estimate
        },
        Err(_) => {
            let mut estimate = estimate_skills(&CopilotPolicy { roots: Vec::new() }, Vec::new());
            estimate.diagnostic = Some(super::SkillDiscoveryDiagnostic {
                code: "copilot_roots_unavailable".into(),
                message: "Copilot 技能目录配置无法解析或读取；技能数量未知".into(),
                metadata_only: Vec::new(),
            });
            estimate
        }
    }
}
// Compare exact root declarations, not path fragments or canonical targets:
// custom/inherited sources cannot acquire standard provenance through an alias.
fn copilot_standard_roots(context: &DiscoveryContext, roots: &[super::SkillRoot]) -> bool {
    if context.repository_root.as_ref() != Some(&context.cwd) {
        return false;
    }
    let Ok(expected) = super::copilot_roots_from(context, &context.home.join(".copilot"),
        &serde_json::json!({}), None, true) else { return false; };
    roots.len() == expected.len() && expected.iter().all(|expected| {
        roots.iter().filter(|root| root.path == expected.path
            && root.scope == expected.scope && root.source == expected.source
            && root.include_root_markdown == expected.include_root_markdown
            && root.flat_markdown_only == expected.flat_markdown_only
            && root.direct_children_only == expected.direct_children_only
            && root.follow_directory_symlinks == expected.follow_directory_symlinks
            && root.exact_skill_file == expected.exact_skill_file
            && root.openclaw_boundary.is_none()).count() == 1
    })
}

struct CopilotSourceIdentity {
    canonical: std::path::PathBuf,
    file_id: (u64, u64),
}

fn copilot_file_id(metadata: &std::fs::Metadata) -> Option<(u64, u64)> {
    if !metadata.is_file() { return None; }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    }
    // Without a reliable descriptor identity this bounded rule stays unknown.
    #[cfg(not(unix))]
    { None }
}

impl CopilotSourceIdentity {
    fn capture(canonical: std::path::PathBuf) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(&canonical).ok()?;
        let file_id = copilot_file_id(&metadata)?;
        if canonical.canonicalize().ok()? != canonical { return None; }
        Some(Self { canonical, file_id })
    }

    fn matches_path(&self, path: &std::path::Path) -> bool {
        path.canonicalize().ok().as_ref() == Some(&self.canonical)
            && std::fs::metadata(path).ok().and_then(|metadata| copilot_file_id(&metadata)) == Some(self.file_id)
            && std::fs::symlink_metadata(&self.canonical).ok().and_then(|metadata| copilot_file_id(&metadata)) == Some(self.file_id)
            && path.canonicalize().ok().as_ref() == Some(&self.canonical)
    }

    fn matches_file(&self, path: &std::path::Path, file: &std::fs::File) -> bool {
        file.metadata().ok().and_then(|metadata| copilot_file_id(&metadata)) == Some(self.file_id)
            && self.matches_path(path)
    }
}

fn copilot_pair_metadata(
    skills: &[EffectiveSkill], indices: &[usize], identities: &[Option<CopilotSourceIdentity>],
) -> bool {
    let [left, right] = indices else { return false; };
    let (Some(left_id), Some(right_id)) = (&identities[*left], &identities[*right])
        else { return false; };
    if left_id.canonical == right_id.canonical || left_id.file_id == right_id.file_id { return false; }
    let left_skill = &skills[*left];
    let right_skill = &skills[*right];
    if !left_id.matches_path(&left_skill.path) || !right_id.matches_path(&right_skill.path) { return false; }
    let Some(left_file) = copilot_default_metadata(left_skill) else { return false; };
    if !left_id.matches_file(&left_skill.path, &left_file) { return false; }
    let Some(right_file) = copilot_default_metadata(right_skill) else { return false; };
    // Recheck both mappings after BOTH reads and retain the descriptors through
    // this decision. These are observed checkpoints, not an atomic FS snapshot.
    left_id.matches_file(&left_skill.path, &left_file)
        && right_id.matches_file(&right_skill.path, &right_file)
}

fn copilot_default_metadata(skill: &EffectiveSkill) -> Option<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    // Follow the existing managed aliases, but check the opened descriptor so
    // replacing a path with a FIFO cannot block or pass the regular-file gate.
    let mut file = options.open(&skill.path).ok()?;
    if !file.metadata().ok()?.is_file() { return None; }
    copilot_default_metadata_header(&mut file, skill).then_some(file)
}

const COPILOT_HEADER_LIMIT: usize = 64 * 1024;

fn copilot_default_metadata_header(reader: &mut impl std::io::Read, skill: &EffectiveSkill) -> bool {
    let mut header = Vec::new();
    let mut line_start = 0;
    let mut opened = false;
    let mut closed = false;
    // Deliberately avoid buffered read-ahead into the body. The cap includes
    // delimiters, and the first closing delimiter ends reads immediately.
    for byte in std::io::Read::bytes(std::io::Read::take(reader, COPILOT_HEADER_LIMIT as u64)) {
        let Ok(byte) = byte else { return false; };
        header.push(byte);
        if byte != b'\n' { continue; }
        let delimiter = matches!(&header[line_start..], b"---\n" | b"---\r\n");
        if !opened {
            if !delimiter { return false; }
            opened = true;
        } else if delimiter {
            closed = true;
            break;
        }
        line_start = header.len();
    }
    // A delimiter at actual EOF is also supported; reaching the budget alone
    // cannot establish EOF, so that case stays unknown.
    closed |= opened && header.len() < COPILOT_HEADER_LIMIT && &header[line_start..] == b"---";
    if !closed { return false; }
    let Ok(content) = std::str::from_utf8(&header) else { return false; };
    let Some(frontmatter) = super::yaml_frontmatter(content) else { return false; };
    let Ok(serde_yaml::Value::Mapping(fields)) = serde_yaml::from_str(frontmatter)
        else { return false; };
    fields.len() == 2
        && fields.get(&serde_yaml::Value::String("name".into())).and_then(|value| value.as_str()) == Some(skill.name.as_str())
        && fields.get(&serde_yaml::Value::String("description".into())).and_then(|value| value.as_str()) == Some(skill.description.as_str())
}

#[cfg(test)]
mod copilot_metadata_tests {
    use super::*;
    use std::io::Cursor;

    fn skill(path: std::path::PathBuf) -> EffectiveSkill {
        EffectiveSkill {
            id: "sentinel".into(), name: "sentinel".into(), description: "fixture".into(),
            when_to_use: None, path, root_path: None, prompt_path: None,
            scope: SkillScope::User, visibility: super::super::SkillVisibility::Automatic,
            user_invocable: true, source: super::super::SkillSource::Filesystem,
        }
    }

    #[test]
    fn copilot_header_requires_two_matching_string_fields() {
        let skill = skill("unused".into());
        for fields in ["name: sentinel", "description: fixture", "{}",
            "name: 42\ndescription: fixture", "name: sentinel\ndescription: false",
            "name: changed\ndescription: fixture", "name: sentinel\ndescription: changed",
            "name: sentinel\ndescription: fixture\nenabled: true"] {
            let mut reader = Cursor::new(format!("---\n{fields}\n---\n"));
            assert!(!copilot_default_metadata_header(&mut reader, &skill), "{fields}");
        }
        for header in ["---\nname: sentinel\ndescription: fixture\n---\n",
            "---\r\nname: sentinel\r\ndescription: fixture\r\n---\r\n",
            "---\nname: sentinel\ndescription: fixture\n---"] {
            assert!(copilot_default_metadata_header(&mut Cursor::new(header), &skill));
        }
    }

    #[test]
    fn copilot_header_budget_includes_delimiters() {
        let skill = skill("unused".into());
        let prefix = "---\nname: sentinel\ndescription: fixture\n#";
        let suffix = "\n---\n";
        for (length, accepted) in [(COPILOT_HEADER_LIMIT, true), (COPILOT_HEADER_LIMIT + 1, false)] {
            let header = format!("{prefix}{}{suffix}", "x".repeat(length - prefix.len() - suffix.len()));
            let mut reader = Cursor::new(header);
            assert_eq!(copilot_default_metadata_header(&mut reader, &skill), accepted);
            assert!(reader.position() <= COPILOT_HEADER_LIMIT as u64);
        }
        let mut unterminated = Cursor::new(format!("{prefix}{}", "x".repeat(COPILOT_HEADER_LIMIT)));
        assert!(!copilot_default_metadata_header(&mut unterminated, &skill));
        assert_eq!(unterminated.position(), COPILOT_HEADER_LIMIT as u64);
    }

    #[test]
    fn copilot_header_stops_before_large_non_utf8_body_and_rejects_read_errors() {
        let skill = skill("unused".into());
        let header = b"---\nname: sentinel\ndescription: fixture\n---\n";
        let mut contents = header.to_vec();
        contents.extend(vec![0xff; 2 * 1024 * 1024]);
        let mut reader = Cursor::new(contents);
        assert!(copilot_default_metadata_header(&mut reader, &skill));
        assert_eq!(reader.position(), header.len() as u64);
        struct FailedReader;
        impl std::io::Read for FailedReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            }
        }
        assert!(!copilot_default_metadata_header(&mut FailedReader, &skill));
    }

    #[cfg(unix)]
    #[test]
    fn copilot_group_identity_binds_descriptor_and_postread_path_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let source_a = temp.path().join("a");
        let source_b = temp.path().join("b");
        for path in [&source_a, &source_b] {
            std::fs::write(path, b"---\nname: sentinel\ndescription: fixture\n---\n").unwrap();
        }
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&source_a, &alias).unwrap();
        let grouped_key = alias.canonicalize().unwrap();
        let identity = CopilotSourceIdentity::capture(grouped_key).unwrap();
        let opened_a = copilot_default_metadata(&skill(alias.clone())).unwrap();
        let opened_b = copilot_default_metadata(&skill(source_b.clone())).unwrap();
        assert!(identity.matches_file(&alias, &opened_a));
        // Path remains A, but an FD for identical-header B must not pass.
        assert!(!identity.matches_file(&alias, &opened_b));
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&source_b, &alias).unwrap();
        // FD remains A, but the postread path now maps to B.
        assert!(!identity.matches_file(&alias, &opened_a));
        // Replacing the file at the same canonical string must also fail.
        std::fs::rename(&source_b, &source_a).unwrap();
        assert!(!identity.matches_file(&source_a, &opened_a));
    }

    #[cfg(unix)]
    #[test]
    fn copilot_metadata_file_gate_allows_directory_alias_but_rejects_fifo_and_stale_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let path = source.join("SKILL.md");
        std::fs::write(&path, b"---\nname: sentinel\ndescription: fixture\n---\n").unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        let aliased = skill(alias.join("SKILL.md"));
        assert!(copilot_default_metadata(&aliased).is_some());
        std::fs::write(&path, b"---\nname: sentinel\ndescription: changed\n---\n").unwrap();
        assert!(copilot_default_metadata(&aliased).is_none());
        assert!(copilot_default_metadata(&skill(source)).is_none());
        assert!(copilot_default_metadata(&skill(temp.path().join("missing"))).is_none());
        let fifo = temp.path().join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let fifo_name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        // Exercise only this new descriptor gate, never the shared scanner.
        assert!(copilot_default_metadata(&skill(fifo)).is_none());
    }
}

legacy_adapter!(AntigravityAdapter, AntigravityPolicy);
legacy_adapter!(AmpAdapter, AmpPolicy);
legacy_adapter!(DroidAdapter, DroidPolicy);
legacy_adapter!(OpenClawAdapter, OpenClawPolicy);
legacy_adapter!(HermesAdapter, HermesPolicy);
legacy_adapter!(TraeAdapter, TraePolicy);

struct WorkBuddyAdapter;
impl AgentAdapter for WorkBuddyAdapter {
    fn inspect(&self, context: &DiscoveryContext) -> AgentContextEstimate {
        estimate_with_profile(&WorkBuddyPolicy, context, ScanProfile::WorkBuddy, MetadataProfile::WorkBuddy)
    }
}

pub(super) fn inspect_project(
    context: &DiscoveryContext,
    include_global_only: bool,
    agents: &[crate::agents::AgentIconInfo],
) -> Vec<AgentContextEstimate> {
    let dimagent = DimAgentAdapter {
        home: super::additional_agents::dim_home(context),
    };
    let adapters: [(&str, &dyn AgentAdapter); 15] = [
        ("codex", &CodexAdapter),
        ("claude-code", &ClaudeCodeAdapter),
        ("cursor", &CursorAdapter),
        ("opencode", &OpenCodeAdapter),
        ("copilot", &CopilotAdapter),
        ("antigravity", &AntigravityAdapter),
        ("amp", &AmpAdapter),
        ("droid", &DroidAdapter),
        ("pi", &PiAdapter),
        ("omp", &OmpAdapter),
        ("dimagent", &dimagent),
        ("grok", &GrokAdapter),
        ("trae", &TraeAdapter),
        ("workbuddy", &WorkBuddyAdapter),
        ("hermes", &HermesAdapter),
    ];
    let installed = |id| agents.iter().any(|agent| agent.id == id);
    let mut estimates = adapters
        .into_iter()
        .filter(|(id, _)| installed(*id))
        .map(|(_, adapter)| adapter.inspect(context))
        .collect::<Vec<_>>();
    if installed("proma") && !super::proma_workspaces(context).is_empty() {
        estimates.push(PromaAdapter.inspect(context));
    }
    if include_global_only {
        for (id, adapter) in [("openclaw", &OpenClawAdapter as &dyn AgentAdapter)] {
            if installed(id) { estimates.push(adapter.inspect(context)); }
        }
    }
    estimates
}

#[cfg(test)]
pub(super) fn inspect_dimagent(
    context: &DiscoveryContext,
    home: std::path::PathBuf,
) -> AgentContextEstimate {
    DimAgentAdapter { home }.inspect(context)
}
